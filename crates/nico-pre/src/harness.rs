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

//! The check harness: a checklist is a table of checks, each one function that
//! looks at the site and returns an outcome. The harness runs the table,
//! times each check, turns an error or a panic into a row for that check
//! alone, honours `--only` and `--skip`, and hands back the rows the report
//! prints and the result file records. A coder writes the function and the
//! table entry; nothing else.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::check::{Row, Status};
use crate::gate::EffectiveDecisions;
use crate::sitedef::SiteDef;

/// What a check sees: the site definition, where it was read from (so
/// collateral paths resolve), the effective decisions after the register's
/// rules, and the evidence directory when the operator asked for one.
pub(crate) struct Ctx<'a> {
    pub(crate) sd: &'a SiteDef,
    pub(crate) site_def_path: &'a Path,
    pub(crate) decisions: &'a EffectiveDecisions,
    #[allow(dead_code, reason = "no check keeps evidence yet")]
    pub(crate) evidence: Option<&'a Path>,
}

impl Ctx<'_> {
    /// The effective value(s) of a decision, by register id.
    #[allow(dead_code, reason = "for checks that branch on a multi-select")]
    pub(crate) fn decision(&self, id: &str) -> Option<&[String]> {
        self.decisions.values.get(id).map(Vec::as_slice)
    }

    /// True when the decision holds the option (case-insensitive short name).
    pub(crate) fn decided(&self, id: &str, option: &str) -> bool {
        self.decision(id)
            .is_some_and(|vs| vs.iter().any(|v| v.eq_ignore_ascii_case(option)))
    }

    /// A path from the site definition, resolved against its directory.
    pub(crate) fn resolve(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.site_def_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(p)
        }
    }
}

/// What one check concluded. The harness supplies id and expected
/// from the table entry; the check supplies only what it observed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Observed value agrees with the expectation.
    Pass(String),
    /// NICo tolerates what was observed; recorded, never blocking.
    Warn { observed: String, fix: String },
    /// Blocks the next step until fixed or waived.
    Fail { observed: String, fix: String },
    /// The check does not apply to this site (a decision rules it out).
    #[allow(dead_code, reason = "no landed check is ruled out by a decision yet")]
    Skip(String),
    /// Registered, not written yet.
    Todo,
}

pub(crate) type CheckFn = fn(&Ctx) -> eyre::Result<Outcome>;

/// One table entry: the row's identity and expectation, and the function.
pub(crate) struct Check {
    /// `<checklist>.<area>.<name>`, stable: it is what `--only`/`--skip` and
    /// the result files refer to.
    pub(crate) id: &'static str,
    /// The expected value or condition, as the row prints it.
    pub(crate) expected: &'static str,
    /// The spec of the check, one part per line, each starting with its label:
    /// `Reads:` the fields, files, decisions and endpoints it consults; `Pass:`
    /// the condition and the observed text; `Warn:` (optional) and `Fail:` each
    /// condition with what observed names and the fix; `Skip:` the decision that
    /// rules it out, or `never`; `Test:` the fixture and the minimum cases. Written
    /// before the function exists, so a coder implements against it; `CHECKS.md`
    /// prints it. Tests in `checklists/mod.rs` enforce the shape.
    pub(crate) detail: &'static str,
    pub(crate) run: CheckFn,
}

impl Check {
    /// False while the entry still points at [`todo`].
    pub(crate) fn implemented(&self) -> bool {
        !std::ptr::fn_addr_eq(self.run, todo as CheckFn)
    }
}

/// The function of a registered check that is not written yet. Keep it the
/// only function in the crate with this body: [`Check::implemented`] compares
/// addresses, and the compiler may merge identical functions.
pub(crate) fn todo(_: &Ctx) -> eyre::Result<Outcome> {
    Ok(Outcome::Todo)
}

/// Runs a table against a site. `only` non-empty restricts to those ids;
/// `skip` removes ids. Each check gets its own row whatever happens inside it.
pub(crate) fn run(checks: &[Check], ctx: &Ctx, only: &[String], skip: &[String]) -> Vec<Row> {
    // An id that names no check is a tool error, not a quiet no-op: a misspelled
    // `--only` would otherwise run nothing and exit 0.
    let mut rows: Vec<Row> = only
        .iter()
        .chain(skip)
        .filter(|id| !checks.iter().any(|c| c.id == id.as_str()))
        .map(|id| {
            Row::error(
                id.clone(),
                "a registered check id in --only or --skip",
                "unknown check id",
            )
            .with_fix("`nico-pre checks` lists the ids")
        })
        .collect();
    // An id in both lists would silently run nothing; say so instead.
    rows.extend(only.iter().filter(|id| skip.contains(id)).map(|id| {
        Row::error(
            id.clone(),
            "a check id in --only or in --skip, not both",
            "named in both --only and --skip",
        )
        .with_fix("drop it from one of the two lists")
    }));
    rows.extend(
        checks
            .iter()
            .filter(|c| only.is_empty() || only.iter().any(|o| o == c.id))
            .filter(|c| !skip.iter().any(|s| s == c.id))
            .map(|c| run_one(c, ctx)),
    );
    rows
}

fn run_one(c: &Check, ctx: &Ctx) -> Row {
    let started = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| (c.run)(ctx)));
    let mut row = match outcome {
        Ok(Ok(Outcome::Pass(observed))) => Row::pass(c.id, c.expected, observed),
        Ok(Ok(Outcome::Warn { observed, fix })) => {
            Row::new(c.id, Status::Warn, c.expected, observed).with_fix(fix)
        }
        Ok(Ok(Outcome::Fail { observed, fix })) => {
            Row::fail(c.id, c.expected, observed).with_fix(fix)
        }
        Ok(Ok(Outcome::Skip(why))) => Row::new(
            c.id,
            Status::Skip,
            c.expected,
            format!("not applicable: {why}"),
        ),
        Ok(Ok(Outcome::Todo)) => Row::new(c.id, Status::Warn, c.expected, "not implemented yet"),
        Ok(Err(e)) => Row::error(c.id, c.expected, format!("{e:#}"))
            .with_fix("the check could not run; see observed"),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".to_string());
            Row::error(c.id, c.expected, format!("check panicked: {msg}"))
                .with_fix("a bug in the check, not in the site; report it")
        }
    };
    row.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::Register;

    fn ctx_for<'a>(sd: &'a SiteDef, dec: &'a EffectiveDecisions) -> Ctx<'a> {
        Ctx {
            sd,
            site_def_path: Path::new("crates/nico-pre/examples/site-def.yaml"),
            decisions: dec,
            evidence: None,
        }
    }

    fn passes(_: &Ctx) -> eyre::Result<Outcome> {
        Ok(Outcome::Pass("ok".into()))
    }
    fn errs(_: &Ctx) -> eyre::Result<Outcome> {
        Err(eyre::eyre!("could not reach the BMC"))
    }
    fn panics(_: &Ctx) -> eyre::Result<Outcome> {
        panic!("index out of range")
    }

    const TABLE: &[Check] = &[
        Check {
            id: "t.a",
            expected: "a",
            detail: "",
            run: passes,
        },
        Check {
            id: "t.b",
            expected: "b",
            detail: "",
            run: errs,
        },
        Check {
            id: "t.c",
            expected: "c",
            detail: "",
            run: panics,
        },
        Check {
            id: "t.d",
            expected: "d",
            detail: "",
            run: todo,
        },
    ];

    #[test]
    fn every_outcome_becomes_its_own_row() {
        let sd = crate::sitedef::parse(include_str!("../examples/site-def.yaml")).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = ctx_for(&sd, &dec);
        let rows = run(TABLE, &ctx, &[], &[]);
        let statuses: Vec<_> = rows.iter().map(|r| (r.id.as_str(), r.status)).collect();
        assert_eq!(
            statuses,
            [
                ("t.a", Status::Pass),
                ("t.b", Status::Error),
                ("t.c", Status::Error),
                ("t.d", Status::Warn),
            ]
        );
        assert!(rows[1].observed.contains("could not reach the BMC"));
        assert!(rows[2].observed.contains("panicked: index out of range"));
        assert_eq!(rows[3].observed, "not implemented yet");
        assert!(rows.iter().all(|r| r.duration_ms.is_some()));
    }

    #[test]
    fn only_and_skip_select_checks_before_they_run() {
        let sd = crate::sitedef::parse(include_str!("../examples/site-def.yaml")).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = ctx_for(&sd, &dec);
        let ids = |rows: Vec<Row>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(run(TABLE, &ctx, &["t.a".into(), "t.d".into()], &[])),
            ["t.a", "t.d"]
        );
        assert_eq!(
            ids(run(TABLE, &ctx, &[], &["t.b".into(), "t.c".into()])),
            ["t.a", "t.d"]
        );
        let rows = run(TABLE, &ctx, &["t.zzz".into()], &[]);
        assert_eq!(rows[0].id, "t.zzz");
        assert_eq!(
            rows[0].status,
            Status::Error,
            "an unknown id is a tool error"
        );
        let rows = run(TABLE, &ctx, &["t.a".into()], &["t.a".into()]);
        assert_eq!(
            rows[0].status,
            Status::Error,
            "an id in both lists is a tool error"
        );
        assert!(rows[0].observed.contains("both"));
    }
}
