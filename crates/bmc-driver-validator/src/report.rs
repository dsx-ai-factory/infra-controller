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

//! What the run prints: the platform, the driver map, the plan, each
//! result as it finishes, and the summary.

mod activity;
mod terminal;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bmc_platform::{Capability, PlatformIdentity};
use colored::{ColoredString, Colorize};
use strum::IntoEnumIterator;
pub(crate) use terminal::Terminal;

use crate::checks::{CheckId, CheckResult, Kind, Outcome, Planned, Reporter};
use crate::ownership::Ownership;
use crate::selection::Selection;

/// Columns of the label in key/value rows.
const KEY_WIDTH: usize = 14;

/// Columns of the check id in check rows.
const CHECK_WIDTH: usize = 34;

/// Columns before the detail text in check rows.
const DETAIL_COLUMN: usize = 2 + 2 + CHECK_WIDTH + 1 + 7 + 2;

/// Columns of the driver id in plan rows.
const DRIVER_WIDTH: usize = 30;

/// Narrowest a wrapped text column gets, however wide the columns before it.
const MIN_TEXT_WIDTH: usize = 30;

/// Spaces between a field's name and its value.
const FIELD_GAP: usize = 2;

/// Spaces between fields that share a row.
const FIELD_GUTTER: usize = 4;

impl Reporter for Terminal {
    fn started(&self, check: CheckId, started: Instant) {
        self.activity.start(check.as_str(), started);
    }

    fn step(&self, check: CheckId, started: Instant, step: &str) {
        self.trace(check.as_str(), started, step);
    }

    fn finished(&self, check: CheckId) {
        self.activity.finish(check.as_str());
    }

    fn result(&self, result: &CheckResult) {
        let ended = Ended::of(&result.outcome);
        self.activity.above(|| {
            let columns = format!(
                "  {} {:<CHECK_WIDTH$} {:>7}  ",
                ended.symbol(),
                result.id,
                seconds(result.elapsed).dimmed(),
            );
            self.print_in_column(&columns, DETAIL_COLUMN, detail(&result.outcome), |line| {
                ended.tint(line)
            });
            if let Outcome::Pass(detail) = &result.outcome {
                self.print_fields(DETAIL_COLUMN, &detail.fields);
            }
        });
    }

    fn secret(&self, secret: &str) {
        self.add_secret(secret);
    }
}

impl Terminal {
    pub(crate) fn taking_ownership(&self, address: SocketAddr) {
        self.line(
            &format!("Replacing the factory password of {address} …")
                .dimmed()
                .to_string(),
        );
    }

    pub(crate) fn print_ownership(&self, ownership: &Ownership) {
        self.heading("Ownership");
        self.row("password", &ownership.summary);
        if let Some(driver) = &ownership.accounts_driver {
            self.row(
                "accounts",
                &format!("{driver}, selected from the ServiceRoot alone"),
            );
        }
    }

    pub(crate) fn discovering(&self, address: SocketAddr) {
        self.line(&format!("Discovering {address} …").dimmed().to_string());
    }

    pub(crate) fn warn(&self, message: &str) {
        self.line("");
        self.print_in_column(&format!("  {} ", "!".yellow().bold()), 4, message, |line| {
            line.yellow()
        });
    }

    pub(crate) fn print_identity(&self, address: SocketAddr, identity: &PlatformIdentity) {
        self.heading(&format!("Platform  {}", address.to_string().dimmed()));
        let root = &identity.service_root;
        let oem = (!root.oem_keys.is_empty()).then(|| format!("oem {}", root.oem_keys.join(", ")));
        self.row(
            "service root",
            &joined([root.vendor.clone(), root.product.clone(), oem]),
        );
        self.row(
            "system",
            &identity
                .system
                .as_ref()
                .map_or("none listed".to_string(), |system| {
                    joined([
                        Some(system.id.clone()),
                        joined_words([system.manufacturer.as_deref(), system.model.as_deref()]),
                        system.sku.as_ref().map(|sku| format!("sku {sku}")),
                        system
                            .part_number
                            .as_ref()
                            .map(|part| format!("part {part}")),
                        system
                            .bios_version
                            .as_ref()
                            .map(|bios| format!("bios {bios}")),
                    ])
                }),
        );
        self.row(
            "manager",
            &identity
                .manager
                .as_ref()
                .map_or("none listed".to_string(), |manager| {
                    joined([
                        Some(manager.id.clone()),
                        manager.model.as_ref().map(|model| format!("model {model}")),
                        manager
                            .firmware
                            .as_ref()
                            .map(|firmware| format!("firmware {firmware}")),
                    ])
                }),
        );
        for chassis in &identity.chassis {
            self.row(
                "chassis",
                &joined([
                    Some(chassis.id.clone()),
                    joined_words([chassis.manufacturer.as_deref(), chassis.model.as_deref()]),
                    chassis
                        .part_number
                        .as_ref()
                        .map(|part| format!("part {part}")),
                ]),
            );
        }
    }

    pub(crate) fn print_selection(&self, selection: &Selection) {
        self.heading("Drivers");
        let driver_width = Capability::iter()
            .map(|capability| selection.driver(capability).len())
            .max()
            .unwrap_or_default();
        for capability in Capability::iter() {
            let driver = format!("{:<driver_width$}", selection.driver(capability));
            let driver = if selection.is_supported(capability) {
                driver.cyan()
            } else {
                driver.dimmed()
            };
            self.line(&format!(
                "  {:<KEY_WIDTH$} {driver}  {}",
                capability.as_str(),
                selection.source(capability).dimmed()
            ));
        }
        self.line("");
        for (id, matchers) in selection.deciding_rules() {
            self.row(&format!("rule {id}"), &matchers);
        }
        for capability in selection.overridden() {
            self.row_wrapped(
                "override",
                &format!(
                    "{}: {}",
                    capability.as_str(),
                    selection.override_note(capability)
                ),
            );
        }
        let quirks = selection.quirks();
        self.row(
            "quirks",
            &if quirks.is_empty() {
                "none".to_string()
            } else {
                quirks.join(", ")
            },
        );
        self.row("rules hash", &selection.hash());
    }

    pub(crate) fn print_plan(&self, title: &str, plan: &[Planned], selection: &Selection) {
        self.heading(title);
        for planned in plan {
            let kind = match planned.check.kind {
                Kind::Read => "read ",
                Kind::Apply => "apply",
            };
            let (symbol, note) = match &planned.skip {
                Some(outcome) => (Ended::of(outcome).symbol(), detail(outcome)),
                None => ("▸".normal(), ""),
            };
            let columns = format!(
                "  {symbol} {:<CHECK_WIDTH$} {} {:<DRIVER_WIDTH$} ",
                planned.check.id,
                kind.dimmed(),
                selection.driver(planned.check.capability)
            );
            self.print_in_column(
                &columns,
                2 + 2 + CHECK_WIDTH + 1 + 5 + 1 + DRIVER_WIDTH + 1,
                note,
                |line| line.dimmed(),
            );
        }
    }

    pub(crate) fn checks_heading(&self) {
        self.heading("Checks");
    }

    /// Prints the counts by how checks ended, then every result that did not
    /// pass, grouped the same way, and the changes left unrestored.
    pub(crate) fn summary(
        &self,
        results: &[CheckResult],
        unrestored: &[String],
        elapsed: Duration,
    ) {
        let of = |ended: Ended| -> Vec<&CheckResult> {
            results
                .iter()
                .filter(|result| Ended::of(&result.outcome) == ended)
                .collect()
        };
        let counts = Ended::ALL.map(|ended| {
            let count = of(ended).len();
            let text = format!("{count} {}", ended.name());
            if count == 0 && ended == Ended::Failed {
                text
            } else {
                ended.paint(&text).to_string()
            }
        });
        self.heading(&format!(
            "Summary  {}  {}",
            counts.join(" · "),
            seconds(elapsed).dimmed()
        ));

        let failures = of(Ended::Failed);
        if !failures.is_empty() {
            self.heading(&Ended::Failed.paint("Failed").to_string());
            for result in failures {
                self.line(&format!(
                    "  {} {}  {}",
                    Ended::Failed.symbol(),
                    result.id.as_str().bold(),
                    result.driver.dimmed()
                ));
                self.print_in_column("      ", 6, detail(&result.outcome), |line| line.normal());
            }
        }
        for (ended, title) in [
            (Ended::Unsupported, "Unsupported"),
            (Ended::Skipped, "Skipped"),
            (Ended::Blocked, "Blocked"),
        ] {
            let matching = of(ended);
            if matching.is_empty() {
                continue;
            }
            self.heading(title);
            for result in matching {
                let columns = format!("  {} {:<CHECK_WIDTH$} ", ended.symbol(), result.id);
                self.print_in_column(
                    &columns,
                    2 + 2 + CHECK_WIDTH + 1,
                    detail(&result.outcome),
                    |line| line.dimmed(),
                );
            }
        }

        if !unrestored.is_empty() {
            self.heading(&"Not restored".red().bold().to_string());
            for change in unrestored {
                self.print_in_column(&format!("  {} ", "!".red().bold()), 4, change, |line| {
                    line.normal()
                });
            }
        }
    }
}

/// How a check ended, as results are colored, counted, and grouped.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Ended {
    Passed,
    Failed,
    Unsupported,
    Skipped,
    Blocked,
}

impl Ended {
    const ALL: [Self; 5] = [
        Self::Passed,
        Self::Failed,
        Self::Unsupported,
        Self::Skipped,
        Self::Blocked,
    ];

    fn of(outcome: &Outcome) -> Self {
        match outcome {
            Outcome::Pass(_) => Self::Passed,
            Outcome::Fail(_) | Outcome::Unreachable(_) => Self::Failed,
            Outcome::Unsupported(_) => Self::Unsupported,
            Outcome::Skipped(_) => Self::Skipped,
            Outcome::Blocked(_) => Self::Blocked,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }

    /// `text` in this ending's color, as its symbol and count are drawn.
    fn paint(self, text: &str) -> ColoredString {
        match self {
            Self::Passed => text.green(),
            Self::Failed => text.red().bold(),
            Self::Unsupported => text.yellow(),
            Self::Skipped | Self::Blocked => text.dimmed(),
        }
    }

    fn symbol(self) -> ColoredString {
        self.paint(match self {
            Self::Passed => "✔",
            Self::Failed => "✘",
            Self::Unsupported => "○",
            Self::Skipped | Self::Blocked => "–",
        })
    }

    /// A result's detail text: plain when it passed, red when it failed.
    fn tint(self, text: &str) -> ColoredString {
        match self {
            Self::Passed => text.normal(),
            Self::Failed => text.red(),
            Self::Unsupported | Self::Skipped | Self::Blocked => text.dimmed(),
        }
    }
}

fn detail(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Pass(detail) => &detail.summary,
        Outcome::Fail(detail)
        | Outcome::Unreachable(detail)
        | Outcome::Unsupported(detail)
        | Outcome::Skipped(detail)
        | Outcome::Blocked(detail) => detail,
    }
}

fn seconds(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

/// The present parts, separated by ` · `.
fn joined<const N: usize>(parts: [Option<String>; N]) -> String {
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// The present words, separated by spaces, or `None` when there are none.
fn joined_words<const N: usize>(words: [Option<&str>; N]) -> Option<String> {
    let words: Vec<&str> = words.into_iter().flatten().collect();
    (!words.is_empty()).then(|| words.join(" "))
}
