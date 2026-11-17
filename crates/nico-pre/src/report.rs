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

//! Rendering of a run result: a report for people (what needs attention as paragraphs,
//! what passed as one line each), the four-column table for wide panes, JSON for scripts.

use std::fmt::Write as _;

use eyre::Result;

use crate::check::{Row, RunResult, Status};

/// How the text report is laid out.
pub(crate) struct Layout {
    /// Columns available; lines wrap inside it.
    pub(crate) width: usize,
    /// Passed and skipped checks as full paragraphs too, not one line each.
    pub(crate) verbose: bool,
}

/// Indent of a paragraph's text: two spaces, the eight-character label, two spaces.
const INDENT: usize = 12;
const LABEL: usize = 8;

fn header(result: &RunResult) -> String {
    let count = |s: Status| result.rows.iter().filter(|r| r.status == s).count();
    let mut counts = format!(
        "{} passed, {} warnings, {} failed",
        count(Status::Pass),
        count(Status::Warn),
        count(Status::Fail)
    );
    for (status, word) in [(Status::Skip, "skipped"), (Status::Error, "errors")] {
        let n = count(status);
        if n > 0 {
            let _ = write!(counts, ", {n} {word}");
        }
    }
    format!(
        "{} {} · site {} · {} checks · {} · exit {}",
        result.tool,
        result.checklist,
        result.site.replace(['\n', '\r'], " ⏎ "),
        result.rows.len(),
        counts,
        result.exit
    )
}

/// The report: failures, errors and warnings first, each as a paragraph with its
/// expected, observed and fix lines wrapped to `layout.width`; then the skipped and the
/// passed checks, one line each (or as paragraphs too when verbose).
pub(crate) fn text(result: &RunResult, layout: &Layout) -> String {
    let mut out = header(result);
    out.push('\n');
    if result.rows.is_empty() {
        return out;
    }
    let width = layout.width.max(INDENT + 20);
    let rank = |s: Status| match s {
        Status::Error => 0,
        Status::Fail => 1,
        Status::Warn => 2,
        Status::Skip => 3,
        Status::Pass => 4,
    };
    let mut rows: Vec<&Row> = result.rows.iter().collect();
    rows.sort_by_key(|r| rank(r.status));
    let attention = |r: &&Row| !matches!(r.status, Status::Pass | Status::Skip);
    for row in rows.iter().filter(|r| layout.verbose || attention(r)) {
        out.push('\n');
        paragraph(&mut out, row, width);
    }
    if !layout.verbose {
        for (status, title) in [(Status::Skip, "skipped"), (Status::Pass, "passed")] {
            let group: Vec<&&Row> = rows.iter().filter(|r| r.status == status).collect();
            if group.is_empty() {
                continue;
            }
            let _ = write!(out, "\n{title} {}\n", group.len());
            let id_width = group
                .iter()
                .map(|r| r.id.chars().count())
                .max()
                .unwrap_or(0);
            for row in group {
                one_line(&mut out, row, id_width, width);
            }
        }
    }
    out
}

fn paragraph(out: &mut String, row: &Row, width: usize) {
    let _ = writeln!(out, "{} {}", row.status.glyph(), row.id);
    labelled(out, "expected", &[row.expected.as_str()], width);
    labelled(out, "observed", &items(&row.observed), width);
    if let Some(fix) = &row.fix {
        labelled(out, "fix", &[fix.as_str()], width);
    }
}

/// `  label     text`, each item wrapped to the width and starting a line of its own.
fn labelled(out: &mut String, label: &str, items: &[&str], width: usize) {
    let mut first = true;
    for item in items {
        for line in wrap(item, width - INDENT) {
            if first {
                let _ = writeln!(out, "  {label:<LABEL$}  {line}");
                first = false;
            } else {
                let _ = writeln!(out, "{:INDENT$}{line}", "");
            }
        }
    }
    if first {
        let _ = writeln!(out, "  {label:<LABEL$}");
    }
}

/// The findings of an observed value, one per `; ` or line break.
fn items(text: &str) -> Vec<&str> {
    text.lines()
        .flat_map(|l| l.split("; "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Greedy wrap on spaces; a word longer than the width stays whole on its own line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// `  ✓ id   observed`, the observed cut to the width with an ellipsis.
fn one_line(out: &mut String, row: &Row, id_width: usize, width: usize) {
    let observed = row.observed.replace(['\n', '\r'], " ⏎ ");
    let prefix = format!("  {} {:<id_width$}  ", row.status.glyph(), row.id);
    let room = width.saturating_sub(prefix.chars().count()).max(8);
    let shown = if observed.chars().count() > room {
        let mut s: String = observed.chars().take(room - 1).collect();
        s.push('…');
        s
    } else {
        observed
    };
    let _ = writeln!(out, "{}", format!("{prefix}{shown}").trim_end());
}

/// The four-column table: every check as one row with its expected, observed and fix.
pub(crate) fn table(result: &RunResult) -> String {
    let mut out = header(result);
    out.push('\n');
    if result.rows.is_empty() {
        return out;
    }
    let cells: Vec<[String; 4]> = result
        .rows
        .iter()
        .map(|r| {
            // One row per check: a line break inside a value would print as a line
            // without its check id, so it is shown as a mark instead.
            let one_line = |s: &str| s.replace(['\n', '\r'], " ⏎ ");
            [
                format!("{} {}", r.status.glyph(), one_line(&r.id)),
                one_line(&r.expected),
                one_line(&r.observed),
                one_line(r.fix.as_deref().unwrap_or_default()),
            ]
        })
        .collect();
    let header = ["check", "expected", "observed", "fix"];
    let mut widths = header.map(str::len);
    for row in &cells {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cols: [&str; 4]| -> String {
        let mut s = String::new();
        for (i, (col, w)) in cols.iter().zip(widths).enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            let pad = w.saturating_sub(col.chars().count());
            s.push_str(col);
            if i + 1 < cols.len() {
                s.extend(std::iter::repeat_n(' ', pad));
            }
        }
        s.trim_end().to_string()
    };
    let _ = writeln!(out, "{}", line(header));
    for row in &cells {
        let _ = writeln!(out, "{}", line([&row[0], &row[1], &row[2], &row[3]]));
    }
    out
}

pub(crate) fn json(result: &RunResult) -> Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> RunResult {
        let mut fail = Row::new(
            "config.sitedef.agreement",
            Status::Fail,
            "every quantity present in both equal, key for key",
            "network.nameServer 10.0.0.1 vs site.yaml nameServer 8.8.8.8; network.fnn.routeTargets no entry 65034:901 vs site.yaml fnn.routeTargetsToImport 65034:901",
        );
        fail.fix = Some("pick one as the source and copy the value to the other".to_string());
        let mut warn = Row::new(
            "config.consistency.site-controllers",
            Status::Warn,
            "at least three site controllers",
            "3 controllers; sizing table is the placeholder",
        );
        warn.fix = Some("treat the check as a floor".to_string());
        let rows = vec![
            Row::new("sitedef.site.name", Status::Pass, "a site name", "`site1`"),
            warn,
            Row::new(
                "config.metallb.rules",
                Status::Skip,
                "MetalLB pools hold every VIP",
                "no metallbConfig named",
            ),
            fail,
            Row::new(
                "config.consistency.pools",
                Status::Pass,
                "pools sized to counts",
                "control plane: 32 for 6 addresses (a /31 per site controller); service VIPs: 32 for 13 VIPs; DPU ASNs: 95 for 36 dpus",
            ),
        ];
        let mut r = RunResult::new("config", "site1", rows);
        r.exit = 2;
        r
    }

    /// Failures and warnings come first as paragraphs, findings one per line, text
    /// wrapped; skipped and passed checks follow as one line each, cut to the width.
    #[test]
    fn report_puts_attention_first_and_passes_last() {
        let out = text(
            &result(),
            &Layout {
                width: 72,
                verbose: false,
            },
        );
        let want = "\
nico-pre config · site site1 · 5 checks · 2 passed, 1 warnings, 1 failed, 1 skipped · exit 2

✗ config.sitedef.agreement
  expected  every quantity present in both equal, key for key
  observed  network.nameServer 10.0.0.1 vs site.yaml nameServer 8.8.8.8
            network.fnn.routeTargets no entry 65034:901 vs site.yaml
            fnn.routeTargetsToImport 65034:901
  fix       pick one as the source and copy the value to the other

⚠ config.consistency.site-controllers
  expected  at least three site controllers
  observed  3 controllers
            sizing table is the placeholder
  fix       treat the check as a floor

skipped 1
  – config.metallb.rules  no metallbConfig named

passed 2
  ✓ sitedef.site.name         `site1`
  ✓ config.consistency.pools  control plane: 32 for 6 addresses (a /31 …
";
        assert_eq!(out, want);
    }

    /// Verbose renders every check as a paragraph, passes included, in severity order.
    #[test]
    fn verbose_expands_the_passes() {
        let out = text(
            &result(),
            &Layout {
                width: 100,
                verbose: true,
            },
        );
        assert!(
            out.contains("\n✓ sitedef.site.name\n  expected  a site name\n  observed  `site1`\n")
        );
        assert!(!out.contains("passed 2"));
        let fail = out.find("✗ config").unwrap();
        let pass = out.find("✓ sitedef").unwrap();
        assert!(fail < pass, "{out}");
    }

    /// The table keeps one row per check with the four columns.
    #[test]
    fn table_keeps_the_four_columns() {
        let out = table(&result());
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].starts_with("check  "), "{}", lines[1]);
        assert!(lines[1].contains("expected") && lines[1].ends_with("fix"));
        assert_eq!(lines.len(), 2 + 5);
    }

    /// A word longer than the width stays whole; nothing is split inside a word.
    #[test]
    fn wrap_keeps_long_words_whole() {
        let lines = wrap("a bbbbbbbbbbbbbbbbbbbbbbbbbbbb c", 10);
        assert_eq!(lines, ["a", "bbbbbbbbbbbbbbbbbbbbbbbbbbbb", "c"]);
        assert_eq!(wrap("", 10), [""]);
    }
}
