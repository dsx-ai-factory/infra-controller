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

//! Rendering of a run result: the text table for people, JSON for scripts.

use std::fmt::Write as _;

use eyre::Result;

use crate::check::{ExitCode, RunResult};

/// The text report; `exit` is the status the command returns, which the header prints
/// (a blocked gate or an unwritten result file raises it above the rows' own worst).
pub(crate) fn text(result: &RunResult, exit: ExitCode) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {} · site {} · {} rows · exit {}",
        result.tool,
        result.checklist,
        result.site.replace(['\n', '\r'], " ⏎ "),
        result.rows.len(),
        exit.code()
    );
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
