/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

//! `nico-pre`: pre-flight checks an Infrastructure Provider runs at each
//! stage of a NICo deployment. One binary; the checklists are subcommands, and
//! a hard link or copy named `nico-pre-<checklist>` runs that checklist directly.

mod catalog;
mod check;
mod checklists;
mod gate;
mod harness;
mod register;
mod report;
mod setup_cmd;
// The schema is the foundation; the checklists that read every field fill in behind it.
#[allow(
    dead_code,
    reason = "schema fields are read by checklists that have not landed yet"
)]
mod sitedef;
mod template;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chrono::Utc;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::check::{ExitCode, Row, RunResult};
use crate::checklists::Checklist;
use crate::register::Register;

#[derive(Parser)]
#[command(
    name = "nico-pre",
    version,
    about = "Pre-flight checks for a NICo site, one checklist per deployment step",
    long_about = "Every checklist first validates the site definition (exit 3 with the offending rows \
                  when it fails), then runs its own checks. Exit 0: every check passed; 1: warnings \
                  only; 2: at least one check blocks; 3: the tool could not run a check."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Step 1: the collaterals against each other and the site definition
    Config(ChecklistArgs),
    /// Step 2a: the build host can produce the ISO and the images
    Build(ChecklistArgs),
    /// Step 2b: the staging host can run setup.sh and reach every site controller and BMC
    Stage(ChecklistArgs),
    /// Step 3: each site controller over Redfish, before the control-plane DPU is provisioned
    Host(ChecklistArgs),
    /// Step 4: the fabric from a site controller and its DPU, against the datacenter worksheet
    Fabric(ChecklistArgs),
    /// Step 5: the Kubernetes cluster before setup.sh
    Platform(ChecklistArgs),
    /// Step 6: NICo as a system after setup.sh, nothing ingested
    Install(ChecklistArgs),
    /// Step 7: ingestion readiness before the first host is approved
    Ingest(ChecklistArgs),
    /// Parse a site definition and run the gate alone: schema, decisions, rules, arithmetic
    Validate(ValidateArgs),
    /// The decision register: list it, or write the `decisions` section of a site definition
    Decisions(DecisionsArgs),
    /// The check catalogue: every registered check by id, implemented or not
    Checks(ChecksArgs),
    /// Print the tool version
    Version,
}

#[derive(Args, Clone)]
struct ChecklistArgs {
    /// Path of the site definition
    #[arg(long = "site-def", value_name = "PATH")]
    site_def: PathBuf,
    /// Output format
    #[arg(short = 'o', long = "output", value_enum, default_value_t = Output::Text)]
    output: Output,
    /// Run only these check ids (comma-separated); the gate always runs
    #[arg(long, value_delimiter = ',', value_name = "ID")]
    only: Vec<String>,
    /// Skip these check ids (comma-separated); the gate always runs
    #[arg(long, value_delimiter = ',', value_name = "ID")]
    skip: Vec<String>,
    /// Directory for the raw material behind rows. Only checks that keep evidence
    /// write there; none of the implemented checks does yet, so it stays empty.
    #[arg(long, value_name = "DIR")]
    evidence: Option<PathBuf>,
    /// Where the result file goes; default: the site definition's `results`
    #[arg(long, value_name = "DIR")]
    results: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct ValidateArgs {
    /// Path of the site definition
    #[arg(long = "site-def", value_name = "PATH")]
    site_def: PathBuf,
    /// Output format
    #[arg(short = 'o', long = "output", value_enum, default_value_t = Output::Text)]
    output: Output,
}

#[derive(Args, Clone)]
struct DecisionsArgs {
    /// Write the `decisions` section instead of listing the register
    #[arg(long)]
    template: bool,
    /// Carry this site definition's current decisions into the template
    #[arg(long = "site-def", value_name = "PATH")]
    site_def: Option<PathBuf>,
}

#[derive(Args, Clone)]
struct ChecksArgs {
    /// Output format: a text list, or the Markdown of CHECKS.md
    #[arg(long, value_enum, default_value_t = CatalogFormat::Text)]
    format: CatalogFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CatalogFormat {
    Text,
    Md,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Output {
    Text,
    Json,
}

fn main() -> std::process::ExitCode {
    // A usage error is a tool failure (3), not "a check blocks" (2), which clap's own
    // exit status would say; help and version output stay exit 0.
    let cli = match Cli::try_parse_from(expand_alias(std::env::args_os())) {
        Ok(cli) => cli,
        Err(e) => {
            let shown = e.print().is_ok();
            return std::process::ExitCode::from(if e.use_stderr() || !shown {
                ExitCode::ToolFailure.code()
            } else {
                ExitCode::Pass.code()
            });
        }
    };
    let code = match cli.command {
        Command::Config(a) => run_checklist(Checklist::Config, &a),
        Command::Build(a) => run_checklist(Checklist::Build, &a),
        Command::Stage(a) => run_checklist(Checklist::Stage, &a),
        Command::Host(a) => run_checklist(Checklist::Host, &a),
        Command::Fabric(a) => run_checklist(Checklist::Fabric, &a),
        Command::Platform(a) => run_checklist(Checklist::Platform, &a),
        Command::Install(a) => run_checklist(Checklist::Install, &a),
        Command::Ingest(a) => run_checklist(Checklist::Ingest, &a),
        Command::Validate(a) => validate(&a),
        Command::Decisions(a) => decisions(&a),
        Command::Checks(a) => {
            print!(
                "{}",
                match a.format {
                    CatalogFormat::Text => catalog::text(),
                    CatalogFormat::Md => catalog::markdown(),
                }
            );
            ExitCode::Pass
        }
        Command::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::Pass
        }
    };
    std::process::ExitCode::from(code.code())
}

/// `nico-pre-config ...` behaves as `nico-pre config ...`.
fn expand_alias(args: impl Iterator<Item = OsString>) -> Vec<OsString> {
    let mut args: Vec<OsString> = args.collect();
    let checklist = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|f| f.to_str())
        .and_then(|f| f.strip_prefix("nico-pre-"))
        .and_then(Checklist::from_name);
    if let Some(checklist) = checklist {
        args.insert(1, OsString::from(checklist.name()));
    }
    args
}

fn run_checklist(checklist: Checklist, args: &ChecklistArgs) -> ExitCode {
    let started = Utc::now();
    let reg = Register::builtin();
    let (site, rows, gate_blocked, results_dir) = match sitedef::load(&args.site_def) {
        Err(e) => (
            String::from("?"),
            vec![Row::fail(
                "sitedef.schema",
                "a site definition that parses with no unknown keys",
                format!("{e:#}"),
            )],
            true,
            args.results.clone(),
        ),
        Ok(sd) => {
            let mut rows = gate::run(&sd, reg);
            let blocked = check::blocks(&rows);
            if !blocked {
                let decisions = gate::effective_decisions(&sd, reg);
                let ctx = harness::Ctx {
                    sd: &sd,
                    site_def_path: &args.site_def,
                    decisions: &decisions,
                    evidence: args.evidence.as_deref(),
                };
                rows.extend(checklist.run(&ctx, &args.only, &args.skip));
            }
            let dir = args
                .results
                .clone()
                .unwrap_or_else(|| sd.results_dir(&args.site_def));
            (sd.site.name, rows, blocked, Some(dir))
        }
    };
    let mut result = RunResult::new(checklist.name(), &site, rows).started_at(started);
    // The saved file records the status of the run it describes: the rows' worst, or 3
    // when the gate blocked. A failed write then raises the process status to 3 as well,
    // since a caller must not take the run as recorded.
    let exit = if gate_blocked {
        ExitCode::ToolFailure
    } else {
        result.exit_code()
    };
    result = result.with_exit(exit);
    let mut exit = exit;
    if let Some(dir) = results_dir
        && let Err(e) = write_result(&dir, &result)
    {
        eprintln!("nico-pre: result file not written: {e:#}");
        exit = ExitCode::ToolFailure;
        result = result.with_exit(exit);
    }

    match args.output {
        Output::Text => print!("{}", report::text(&result, exit)),
        Output::Json => match report::json(&result) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("nico-pre: {e:#}");
                return ExitCode::ToolFailure;
            }
        },
    }

    exit
}

/// Writes the result as `<checklist>-<UTC stamp>.json`, never over an earlier run: a
/// second run in the same second gets a `-1`, `-2`, ... suffix.
fn write_result(dir: &Path, result: &RunResult) -> eyre::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let json = report::json(result)?;
    // Written whole to a temporary name first, then published under the result name by a
    // hard link, which fails rather than replaces when an earlier run holds that name: a
    // reader never sees a partial file under a valid result name.
    // The temporary name is created exclusively and written through its own handle, so a
    // pre-planted file or symlink of the same name in a shared directory is refused, not
    // followed.
    let tmp = {
        use std::io::Write as _;
        let mut attempt = 0u32;
        loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let candidate = dir.join(format!(
                ".{}-{stamp}-{}-{nanos}-{attempt}.json.tmp",
                result.checklist,
                std::process::id()
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600); // a result names hosts and addresses; owner-only
            }
            match options.open(&candidate) {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(json.as_bytes()) {
                        drop(f);
                        let _ = std::fs::remove_file(&candidate);
                        return Err(e.into());
                    }
                    break candidate;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => {
                    attempt += 1;
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    let published = (0..1000u32).find_map(|n| {
        let name = if n == 0 {
            format!("{}-{stamp}.json", result.checklist)
        } else {
            format!("{}-{stamp}-{n}.json", result.checklist)
        };
        let path = dir.join(name);
        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => Some(Ok(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
            Err(e) => Some(Err(e)),
        }
    });
    let _ = std::fs::remove_file(&tmp);
    match published {
        Some(Ok(path)) => Ok(path),
        Some(Err(e)) => Err(e.into()),
        None => eyre::bail!(
            "a thousand result files already carry {stamp} in {}",
            dir.display()
        ),
    }
}

/// The site-definition gate on its own: exit 0 when the file is usable by
/// every checklist, 3 when it is not. Writes no result file.
fn validate(args: &ValidateArgs) -> ExitCode {
    let started = Utc::now();
    let reg = Register::builtin();
    let (site, rows) = match sitedef::load(&args.site_def) {
        Err(e) => (
            String::from("?"),
            vec![Row::fail(
                "sitedef.schema",
                "a site definition that parses with no unknown keys",
                format!("{e:#}"),
            )],
        ),
        Ok(sd) => {
            let rows = gate::run(&sd, reg);
            (sd.site.name, rows)
        }
    };
    let blocked = check::blocks(&rows);
    let exit = if blocked {
        ExitCode::ToolFailure
    } else {
        ExitCode::Pass
    };
    let result = RunResult::new("validate", &site, rows)
        .started_at(started)
        .with_exit(exit);
    match args.output {
        Output::Text => print!("{}", report::text(&result, exit)),
        Output::Json => match report::json(&result) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("nico-pre: {e:#}");
                return ExitCode::ToolFailure;
            }
        },
    }
    exit
}

fn decisions(args: &DecisionsArgs) -> ExitCode {
    let reg = Register::builtin();
    if !args.template {
        print!("{}", template::register_table(reg));
        return ExitCode::Pass;
    }
    let current = match &args.site_def {
        None => indexmap::IndexMap::new(),
        Some(path) => match sitedef::load(path) {
            Ok(sd) => sd.decisions,
            Err(e) => {
                eprintln!("nico-pre: {e:#}");
                return ExitCode::ToolFailure;
            }
        },
    };
    print!("{}", template::decisions_section(&current, reg));
    ExitCode::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn checklist_alias_in_argv0_selects_the_checklist() {
        let cases: &[(&str, &[&str])] = &[
            (
                "/usr/local/bin/nico-pre-config",
                &[
                    "/usr/local/bin/nico-pre-config",
                    "config",
                    "--site-def",
                    "x",
                ],
            ),
            ("nico-pre", &["nico-pre", "--site-def", "x"]),
            ("nico-pre-unknown", &["nico-pre-unknown", "--site-def", "x"]),
        ];
        for (argv0, expected) in cases {
            let got = expand_alias([argv0, "--site-def", "x"].into_iter().map(OsString::from));
            let expected: Vec<OsString> = expected.iter().map(OsString::from).collect();
            assert_eq!(got, expected, "{argv0}");
        }
    }
}
