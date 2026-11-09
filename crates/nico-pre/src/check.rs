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

//! Check rows, run results, and exit codes shared by every checklist.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Outcome of one check row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// The check does not apply to this site; never affects the exit code.
    Skip,
    Pass,
    /// NICo tolerates it; recorded, never blocking.
    Warn,
    /// Blocks the next step until fixed or waived.
    Fail,
    /// The tool could not run the check.
    Error,
}

impl Status {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Status::Skip => "–",
            Status::Pass => "✓",
            Status::Warn => "⚠",
            Status::Fail => "✗",
            Status::Error => "!",
        }
    }
}

/// One row of a checklist's table: what was expected, what was observed, and
/// on a failure how to fix it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) status: Status,
    pub(crate) expected: String,
    pub(crate) observed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) doc: Option<String>,
    /// Wall time of the check, set by the harness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
}

impl Row {
    pub(crate) fn new(
        id: impl Into<String>,
        status: Status,
        expected: impl Into<String>,
        observed: impl Into<String>,
    ) -> Self {
        Row {
            id: id.into(),
            status,
            expected: expected.into(),
            observed: observed.into(),
            fix: None,
            doc: None,
            duration_ms: None,
        }
    }

    pub(crate) fn pass(
        id: impl Into<String>,
        expected: impl Into<String>,
        observed: impl Into<String>,
    ) -> Self {
        Row::new(id, Status::Pass, expected, observed)
    }

    pub(crate) fn fail(
        id: impl Into<String>,
        expected: impl Into<String>,
        observed: impl Into<String>,
    ) -> Self {
        Row::new(id, Status::Fail, expected, observed)
    }

    pub(crate) fn error(
        id: impl Into<String>,
        expected: impl Into<String>,
        observed: impl Into<String>,
    ) -> Self {
        Row::new(id, Status::Error, expected, observed)
    }

    pub(crate) fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Exit codes of every checklist, as the provider guide states them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitCode {
    /// Every check passed.
    Pass = 0,
    /// Warnings only.
    Degraded = 1,
    /// At least one check blocks.
    Blocked = 2,
    /// The tool could not run a check, or the site definition failed the gate.
    ToolFailure = 3,
}

impl ExitCode {
    pub(crate) fn code(self) -> u8 {
        self as u8
    }
}

/// One run of one checklist: the result file.
#[derive(Debug, Serialize)]
pub(crate) struct RunResult {
    pub(crate) tool: &'static str,
    pub(crate) version: &'static str,
    pub(crate) checklist: String,
    pub(crate) site: String,
    pub(crate) started_at: DateTime<Utc>,
    /// The exit status the command returns, which may be worse than the rows alone say
    /// (a blocked gate, an unwritten result file).
    pub(crate) exit: u8,
    pub(crate) rows: Vec<Row>,
}

impl RunResult {
    pub(crate) fn new(checklist: &str, site: &str, rows: Vec<Row>) -> Self {
        let mut result = RunResult {
            tool: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            checklist: checklist.to_string(),
            site: site.to_string(),
            started_at: Utc::now(),
            exit: 0,
            rows,
        };
        result.exit = result.exit_code().code();
        result
    }

    /// When the run began, for a result built after its rows were produced.
    pub(crate) fn started_at(mut self, started_at: DateTime<Utc>) -> Self {
        self.started_at = started_at;
        self
    }

    /// The status the command returns, when it is worse than the rows alone say.
    pub(crate) fn with_exit(mut self, exit: ExitCode) -> Self {
        self.exit = exit.code();
        self
    }

    pub(crate) fn exit_code(&self) -> ExitCode {
        match self.rows.iter().map(|r| r.status).max() {
            Some(Status::Error) => ExitCode::ToolFailure,
            Some(Status::Fail) => ExitCode::Blocked,
            Some(Status::Warn) => ExitCode::Degraded,
            Some(Status::Pass | Status::Skip) | None => ExitCode::Pass,
        }
    }
}

/// True when any row is a ✗ or a tool failure.
pub(crate) fn blocks(rows: &[Row]) -> bool {
    rows.iter().any(|r| r.status >= Status::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(statuses: &[Status]) -> Vec<Row> {
        statuses
            .iter()
            .map(|s| Row::new("t", *s, "e", "o"))
            .collect()
    }

    #[test]
    fn exit_code_is_the_worst_status() {
        let cases: &[(&[Status], ExitCode)] = &[
            (&[], ExitCode::Pass),
            (&[Status::Pass, Status::Pass], ExitCode::Pass),
            (&[Status::Pass, Status::Warn], ExitCode::Degraded),
            (&[Status::Warn, Status::Fail], ExitCode::Blocked),
            (&[Status::Fail, Status::Error], ExitCode::ToolFailure),
        ];
        for (statuses, expected) in cases {
            let result = RunResult::new("m", "s", rows(statuses));
            assert_eq!(result.exit_code(), *expected, "{statuses:?}");
        }
    }
}
