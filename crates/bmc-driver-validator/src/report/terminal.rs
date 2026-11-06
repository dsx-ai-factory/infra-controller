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

//! The one writer of terminal output, and the layout primitives over it.
//! Every line passes the redactor on its way out.

use std::io::IsTerminal;
use std::time::Instant;

use colored::{ColoredString, Colorize};

use super::activity::Activity;
use super::{FIELD_GAP, FIELD_GUTTER, KEY_WIDTH, MIN_TEXT_WIDTH};
use crate::redact::Redactor;

/// Width output is fitted to when the terminal does not say.
const DEFAULT_WIDTH: usize = 120;

pub(crate) struct Terminal {
    redactor: Redactor,
    /// The live line of running checks, without `--verbose` on a terminal.
    pub(super) activity: Activity,
    /// Whether check steps are printed to stderr rather than shown on the
    /// activity line.
    verbose: bool,
}

impl Terminal {
    /// Colors output only on a terminal; `NO_COLOR` and `CLICOLOR_FORCE`
    /// still apply.
    pub(crate) fn new(redactor: Redactor, verbose: bool) -> Self {
        let interactive = std::io::stdout().is_terminal();
        if !interactive {
            colored::control::set_override(false);
        }
        Self {
            redactor,
            activity: Activity::new(!verbose && interactive),
            verbose,
        }
    }

    pub(super) fn add_secret(&self, secret: &str) {
        self.redactor.add(secret);
    }

    pub(crate) fn error(&self, message: &str) {
        eprintln!("{} {}", "error:".red().bold(), self.redactor.apply(message));
    }

    /// Reports one step of `check`, which started at `started`: printed to
    /// stderr with `--verbose`, shown on the activity line otherwise.
    pub(super) fn trace(&self, check: &'static str, started: Instant, step: &str) {
        let step = self.redactor.apply(step);
        if !self.verbose {
            self.activity.step(check, step);
            return;
        }
        eprintln!(
            "    {} {step}",
            format!("{check} +{:.1}s", started.elapsed().as_secs_f64()).dimmed(),
        );
    }

    pub(super) fn line(&self, text: &str) {
        println!("{}", self.redactor.apply(text));
    }

    pub(super) fn heading(&self, title: &str) {
        self.line("");
        self.line(&title.bold().to_string());
    }

    pub(super) fn row(&self, key: &str, value: &str) {
        self.line(&format!(
            "  {} {value}",
            format!("{key:<KEY_WIDTH$}").dimmed()
        ));
    }

    /// A row whose value wraps under itself.
    pub(super) fn row_wrapped(&self, key: &str, value: &str) {
        let indent = 2 + KEY_WIDTH + 1;
        let value = self.redactor.apply(value);
        let mut lines = wrap(&value, width().saturating_sub(indent)).into_iter();
        self.row(key, &lines.next().unwrap_or_default());
        for line in lines {
            self.line(&format!("{:indent$}{line}", ""));
        }
    }

    /// Prints `columns`, then `text` wrapped to the space left after its
    /// `column_width` characters, with continuation lines aligned under the
    /// text. The text is redacted before it wraps, since a secret split
    /// across lines would no longer match.
    pub(super) fn print_in_column(
        &self,
        columns: &str,
        column_width: usize,
        text: &str,
        style: impl Fn(&str) -> ColoredString,
    ) {
        let max = width().saturating_sub(column_width).max(MIN_TEXT_WIDTH);
        let text = self.redactor.apply(text);
        let mut lines = wrap(&text, max).into_iter();
        self.line(&format!(
            "{columns}{}",
            style(&lines.next().unwrap_or_default())
        ));
        for line in lines {
            self.line(&format!("{:column_width$}{}", "", style(&line)));
        }
    }

    /// Prints name/value pairs as an aligned grid starting at column
    /// `indent`, as many pairs per row as fit.
    pub(super) fn print_fields(&self, indent: usize, fields: &[(String, String)]) {
        if fields.is_empty() {
            return;
        }
        let fields: Vec<(String, String)> = fields
            .iter()
            .map(|(name, value)| (self.redactor.apply(name), self.redactor.apply(value)))
            .collect();
        let name_width = fields
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .unwrap_or_default();
        let value_width = fields
            .iter()
            .map(|(_, value)| value.chars().count())
            .max()
            .unwrap_or_default();
        let available = width().saturating_sub(indent).max(MIN_TEXT_WIDTH);
        let cell = name_width + FIELD_GAP + value_width;
        let per_row = ((available + FIELD_GUTTER) / (cell + FIELD_GUTTER)).max(1);
        if per_row == 1 {
            for (name, value) in &fields {
                let columns = format!(
                    "{:indent$}{}{:FIELD_GAP$}",
                    "",
                    format!("{name:<name_width$}").dimmed(),
                    ""
                );
                self.print_in_column(&columns, indent + name_width + FIELD_GAP, value, |line| {
                    line.normal()
                });
            }
            return;
        }
        for row in fields.chunks(per_row) {
            let cells: Vec<String> = row
                .iter()
                .map(|(name, value)| {
                    format!(
                        "{}{:FIELD_GAP$}{value:<value_width$}",
                        format!("{name:<name_width$}").dimmed(),
                        ""
                    )
                })
                .collect();
            self.line(&format!(
                "{:indent$}{}",
                "",
                cells.join(&" ".repeat(FIELD_GUTTER)).trim_end()
            ));
        }
    }
}

/// The terminal's width, read afresh so a resize applies; `COLUMNS` or
/// [`DEFAULT_WIDTH`] when stdout is not a terminal.
pub(super) fn width() -> usize {
    terminal_width()
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse().ok())
        })
        .unwrap_or(DEFAULT_WIDTH)
}

fn terminal_width() -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ only writes a `winsize` into the pointer it is
    // given, which points at a live local for the whole call.
    let status = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (status == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// Breaks `text` at spaces into lines of at most `max` characters, splitting
/// any word longer than a line.
pub(super) fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        for piece in chars.chunks(max) {
            let piece: String = piece.iter().collect();
            if !line.is_empty() && line.chars().count() + 1 + piece.chars().count() > max {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&piece);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
