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

//! Generates the `docs/manuals/nico-admin-cli` markdown reference from the live clap command tree.
//!
//! The per-command pages are derived from the same man pages the
//! `generate-man` subcommand produces: we render every command's roff man page
//! with `clap_mangen`, convert each to GitHub-flavored markdown with `pandoc`,
//! and reorganize them into one directory per top-level command. Using the man
//! pages (rather than hand-walking clap's `about`/`after_long_help`) is what
//! gives each page its full SYNOPSIS and per-flag OPTIONS detail.
//!
//! Layout: `commands/<command>/<command>.md` is the base page for a top-level
//! command, and each (flattened) descendant gets `commands/<command>/<command>-<sub>.md`
//! beside it, cross-linked via a breadcrumb and a Subcommands table. The four
//! domain index pages (Hardware/Network/Tenant/Admin) are written from
//! [`crate::cfg::cli_options::domain_for_command`]. Hand-authored pages
//! (`README.md`, `workflows.md`, `setup.md`, `rest-cli-parity.md`) are not
//! touched — operator workflows are editorial and cannot be machine-generated.

use std::fmt::Write as _;
use std::path::Path;

use clap::{Command, CommandFactory};

use crate::cfg::cli_options::{CliDomain, CliOptions, domain_for_command};
use crate::errors::{CarbideCliError, CarbideCliResult};

const BIN: &str = "nico-admin-cli";
const DOMAINS: [CliDomain; 4] = [
    CliDomain::Hardware,
    CliDomain::Network,
    CliDomain::Tenant,
    CliDomain::Admin,
];

pub(super) fn generate(out_dir: &Path) -> CarbideCliResult<()> {
    let root = CliOptions::command().name(BIN);

    // Render every command's roff man page into a scratch directory. clap_mangen
    // names each file by the full command path joined with '-' (e.g.
    // `nico-admin-cli-vpc-show.1`), which we reconstruct per node below.
    let man_dir = std::env::temp_dir().join("nico-admin-cli-cli-docs-man");
    let _ = std::fs::remove_dir_all(&man_dir);
    std::fs::create_dir_all(&man_dir)?;
    clap_mangen::generate_to(root.clone(), &man_dir)?;

    // One directory per top-level command, holding that command's page plus a
    // page for each (flattened) descendant. Rebuilt from scratch so a renamed or
    // removed command can't leave a stale page behind.
    let commands_dir = out_dir.join("commands");
    let _ = std::fs::remove_dir_all(&commands_dir);
    std::fs::create_dir_all(&commands_dir)?;

    // (name, one-line description, domain) for every visible top-level command,
    // used to build the domain index pages.
    let mut top_level: Vec<(String, String, CliDomain)> = Vec::new();

    for sub in root.get_subcommands() {
        if sub.is_hide_set() || sub.get_name() == "help" {
            continue;
        }
        let name = sub.get_name().to_string();
        let about = first_line(&styled(sub.get_about()));
        let domain = domain_for_command(&name).ok_or_else(|| {
            CarbideCliError::GenericError(format!(
                "command `{name}` has no CLI-docs domain; add it to \
                 crates/admin-cli/cli_domains.yaml"
            ))
        })?;

        let dir = commands_dir.join(&name);
        std::fs::create_dir_all(&dir)?;
        render_node(
            sub,
            vec![BIN.to_string(), name.clone()],
            domain,
            &man_dir,
            &dir,
        )?;

        top_level.push((name, about, domain));
    }

    top_level.sort_by(|a, b| a.0.cmp(&b.0));

    for domain in DOMAINS {
        let rows: Vec<&(String, String, CliDomain)> =
            top_level.iter().filter(|c| c.2 == domain).collect();
        std::fs::write(
            out_dir.join(format!("{}.md", slug(domain))),
            render_domain_index(domain, &rows),
        )?;
    }

    let _ = std::fs::remove_dir_all(&man_dir);
    Ok(())
}

/// Writes the markdown page for `cmd` (whose full path from the binary is
/// `path`, e.g. `["nico-admin-cli", "vpc", "show"]`) and recurses into its
/// visible subcommands. Every page for one top-level command lives flat in
/// `dir`, named by the path below the binary joined with '-' (so `vpc show`
/// becomes `vpc-show.md`).
fn render_node(
    cmd: &Command,
    path: Vec<String>,
    domain: CliDomain,
    man_dir: &Path,
    dir: &Path,
) -> CarbideCliResult<()> {
    // The man page body, minus the sections we re-render ourselves: SUBCOMMANDS
    // (we emit a linked table) and EXTRA (the after_long_help examples, which
    // roff mangles — we re-render them from the clap command instead).
    let man_file = man_dir.join(format!("{}.1", path.join("-")));
    let body = strip_sections(&man_to_markdown(&man_file)?, &["SUBCOMMANDS", "EXTRA"]);
    let body = format_markdown_links(&body);

    let children: Vec<&Command> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .collect();

    std::fs::write(
        dir.join(format!("{}.md", stem(&path))),
        render_command_page(cmd, &path, domain, &body, &children),
    )?;

    for child in &children {
        let mut child_path = path.clone();
        child_path.push(child.get_name().to_string());
        render_node(child, child_path, domain, man_dir, dir)?;
    }
    Ok(())
}

fn render_command_page(
    cmd: &Command,
    path: &[String],
    domain: CliDomain,
    body: &str,
    children: &[&Command],
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# `{}`\n", path.join(" "));
    let _ = writeln!(s, "{}\n", breadcrumb(path, domain));

    // SYNOPSIS / DESCRIPTION / OPTIONS lifted from the man page.
    let _ = writeln!(s, "{}\n", body.trim_end());

    if let Some(examples) = example_commands(cmd) {
        let _ = writeln!(s, "## Examples\n");
        let _ = writeln!(s, "```sh\n{examples}\n```\n");
    }

    if !children.is_empty() {
        let _ = writeln!(s, "## Subcommands\n");
        let _ = writeln!(s, "| Subcommand | Description |");
        let _ = writeln!(s, "|---|---|");
        // Children live beside this page; their stem extends this command's.
        let prefix = stem(path);
        for child in children {
            let child_name = child.get_name();
            let _ = writeln!(
                s,
                "| [`{child_name}`](./{prefix}-{child_name}.md) | {} |",
                first_line(&styled(child.get_about()))
            );
        }
        let _ = writeln!(s);
    }

    let _ = writeln!(s, "---\n");
    let _ = writeln!(
        s,
        "**See also:** [{} commands](../../{}.md) · [CLI reference index](../../index.md)",
        domain.title(),
        slug(domain),
    );
    s
}

fn render_domain_index(domain: CliDomain, rows: &[&(String, String, CliDomain)]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# CLI reference — {}\n", domain.title());
    let _ = writeln!(s, "{}\n", intro(domain));
    let _ = writeln!(
        s,
        "For global flags, setup, and task-oriented documentation, see \
         [the CLI reference overview](./index.md).\n"
    );
    let _ = writeln!(s, "| Command | Description |");
    let _ = writeln!(s, "|---|---|");
    for (name, about, _) in rows {
        let about = if about.is_empty() {
            String::new()
        } else if about.ends_with('.') {
            about.clone()
        } else {
            format!("{about}.")
        };
        let _ = writeln!(s, "| [`{name}`](./commands/{name}/{name}.md) | {about} |");
    }
    s
}

// ---- helpers ----

/// The page stem for a command path: the path below the binary joined with '-'
/// (`["nico-admin-cli", "vpc", "show"]` -> `"vpc-show"`). Both the file name
/// and the in-directory links use this.
fn stem(path: &[String]) -> String {
    path[1..].join("-")
}

/// A one-line navigation trail: the domain index, then each ancestor command as
/// a link, then the current command in bold.
fn breadcrumb(path: &[String], domain: CliDomain) -> String {
    let mut parts = vec![format!(
        "[{} commands](../../{}.md)",
        domain.title(),
        slug(domain)
    )];
    // Ancestors are everything between the binary and this command; each links
    // to its own page (which shares this directory).
    for depth in 2..path.len() {
        let ancestor = &path[depth - 1];
        let ancestor_stem = path[1..depth].join("-");
        parts.push(format!("[{ancestor}](./{ancestor_stem}.md)"));
    }
    parts.push(format!(
        "**{}**",
        path.last()
            .expect("a command path always has at least the binary")
    ));
    format!("*{}*", parts.join(" › "))
}

/// Removes the named top-level (`## `) sections from pandoc's markdown, heading
/// included, up to the next `## ` heading or end of document. Matching is
/// case-insensitive on the section title (man sections are single words like
/// `SUBCOMMANDS`).
fn strip_sections(md: &str, names: &[&str]) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in md.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            skipping = names.iter().any(|n| heading.trim().eq_ignore_ascii_case(n));
            if skipping {
                continue;
            }
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Turns repository-relative documentation paths used in terminal help into
/// links that work from generated command-reference pages. Terminal help keeps
/// the plain path, while the Markdown reference gains a reader-friendly label.
fn format_markdown_links(md: &str) -> String {
    md.replace(
        "docs/configuration/templated-ipxe-operating-systems.md",
        "[Templated iPXE Operating Systems](../../../../configuration/templated-ipxe-operating-systems.md)",
    )
    .replace(
        "https://docs.rs/duration-str/latest/duration_str/",
        "[duration-str documentation](https://docs.rs/duration-str/latest/duration_str/)",
    )
    .replace(
        "https://grafana.example.com",
        "[https://grafana.example.com](https://grafana.example.com)",
    )
    .replace(
        "https://github.com/NVIDIA/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/managed-host/managed-host-set-primary-interface.md",
        "[primary-interface command documentation](https://github.com/NVIDIA/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/managed-host/managed-host-set-primary-interface.md)",
    )
    .replace(
        "https://host:50051",
        "[https://host:50051](https://host:50051)",
    )
}

/// Converts a roff man page to GitHub-flavored markdown via pandoc, demoting the
/// man page's `# NAME`/`# OPTIONS`/… sections to `## ` so each generated page
/// can own the `# ` title (the full command invocation).
fn man_to_markdown(man_file: &Path) -> CarbideCliResult<String> {
    let mut pandoc = pandoc::new();
    pandoc.add_input(man_file);
    pandoc.set_input_format(pandoc::InputFormat::Other("man".to_string()), Vec::new());
    pandoc.set_output_format(pandoc::OutputFormat::Other("gfm".to_string()), Vec::new());
    pandoc.add_option(pandoc::PandocOption::ShiftHeadingLevelBy(1));
    pandoc.set_output(pandoc::OutputKind::Pipe);

    match pandoc.execute() {
        Ok(pandoc::PandocOutput::ToBuffer(markdown)) => Ok(markdown),
        Ok(_) => Err(CarbideCliError::GenericError(format!(
            "pandoc returned non-text output converting man page {}",
            man_file.display()
        ))),
        Err(err) => Err(CarbideCliError::GenericError(format!(
            "while converting man page {} to markdown with pandoc: {err}",
            man_file.display()
        ))),
    }
}

// A clap `StyledStr` rendered to plain text.
fn styled(s: Option<&clap::builder::StyledStr>) -> String {
    s.map(|s| s.to_string()).unwrap_or_default()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

// The example command lines from a command's `after_long_help` EXAMPLES block:
// the lines beginning with the `$ ` shell prompt, with the prompt stripped.
fn example_commands(cmd: &Command) -> Option<String> {
    let help = cmd.get_after_long_help()?.to_string();
    let cmds: Vec<String> = help
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("$ "))
        .map(str::to_string)
        .collect();
    (!cmds.is_empty()).then(|| cmds.join("\n"))
}

fn slug(d: CliDomain) -> &'static str {
    match d {
        CliDomain::Hardware => "hardware",
        CliDomain::Network => "network",
        CliDomain::Tenant => "tenant",
        CliDomain::Admin => "admin",
    }
}

fn intro(d: CliDomain) -> &'static str {
    match d {
        CliDomain::Hardware => {
            "Live hardware and lifecycle operations: machines, BMC, DPUs, firmware \
             and component lifecycle, attestation, low-level passthrough (Redfish, \
             RMS, MLX), and operator utilities."
        }
        CliDomain::Network => {
            "VPCs, peerings, prefixes, network segments and devices, security groups, \
             IB/NVLink fabric partitions, IP/domain lookups, and resource pools."
        }
        CliDomain::Tenant => {
            "Tenants and tenant keysets, instances and instance types, compute \
             allocations, the declarative `expected-*` inventory, operating systems \
             and OS images, iPXE templates, extension services, and the site explorer."
        }
        CliDomain::Admin => "CLI and system utilities.",
    }
}

#[cfg(test)]
mod tests {
    use super::format_markdown_links;

    #[test]
    fn repository_doc_paths_become_links_in_generated_markdown() {
        let markdown = "See docs/configuration/templated-ipxe-operating-systems.md.";
        assert_eq!(
            format_markdown_links(markdown),
            "See [Templated iPXE Operating Systems](../../../../configuration/templated-ipxe-operating-systems.md)."
        );
    }

    #[test]
    fn bare_urls_become_valid_markdown_links() {
        let markdown = "See https://docs.rs/duration-str/latest/duration_str/, https://grafana.example.com, or https://host:50051.";
        assert_eq!(
            format_markdown_links(markdown),
            "See [duration-str documentation](https://docs.rs/duration-str/latest/duration_str/), [https://grafana.example.com](https://grafana.example.com), or [https://host:50051](https://host:50051)."
        );
    }
}
