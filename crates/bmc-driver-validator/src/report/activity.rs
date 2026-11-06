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

//! The line under the results naming the checks running now.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use colored::Colorize;
use tokio::task::JoinHandle;

use super::terminal::{width, wrap};
use super::{CHECK_WIDTH, DETAIL_COLUMN, MIN_TEXT_WIDTH, seconds};

/// Most rows the activity line takes for one step.
const MAX_ACTIVITY_ROWS: usize = 6;

/// The running checks with how long they have run and their latest step,
/// redrawn in place. Results are printed through [`Activity::above`] so they
/// never mix with it.
pub(super) struct Activity {
    state: Arc<Mutex<ActivityState>>,
    ticker: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct ActivityState {
    running: Vec<Running>,
    frame: usize,
    /// Rows the last frame drew, which the next clear erases.
    rows: usize,
}

struct Running {
    id: &'static str,
    started: Instant,
    step: String,
}

impl Activity {
    /// Draws nothing unless `enabled`.
    pub(super) fn new(enabled: bool) -> Self {
        const FRAME_INTERVAL: Duration = Duration::from_millis(100);
        let state = Arc::new(Mutex::new(ActivityState::default()));
        let ticker = enabled.then(|| {
            let state = state.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(FRAME_INTERVAL);
                loop {
                    interval.tick().await;
                    state.lock().expect("activity lock").draw();
                }
            })
        });
        Self { state, ticker }
    }

    pub(super) fn start(&self, id: &'static str, started: Instant) {
        self.state
            .lock()
            .expect("activity lock")
            .running
            .push(Running {
                id,
                started,
                step: String::new(),
            });
    }

    /// Records `step` as what check `id` is doing now.
    pub(super) fn step(&self, id: &str, step: String) {
        if let Some(running) = self
            .state
            .lock()
            .expect("activity lock")
            .running
            .iter_mut()
            .find(|running| running.id == id)
        {
            running.step = step;
        }
    }

    pub(super) fn finish(&self, id: &str) {
        self.state
            .lock()
            .expect("activity lock")
            .running
            .retain(|running| running.id != id);
    }

    /// Runs `print` with the line cleared; the next frame redraws it below.
    pub(super) fn above(&self, print: impl FnOnce()) {
        let mut state = self.state.lock().expect("activity lock");
        state.clear();
        print();
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        if let Some(ticker) = self.ticker.take() {
            ticker.abort();
        }
        self.state.lock().expect("activity lock").clear();
    }
}

impl ActivityState {
    fn draw(&mut self) {
        const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let Some(first) = self.running.first() else {
            self.clear();
            return;
        };
        self.frame = (self.frame + 1) % FRAMES.len();
        let mut lines = self.step_lines(width()).into_iter();
        let mut frame = format!(
            "  {} {:<CHECK_WIDTH$} {:>7}  {}",
            FRAMES[self.frame].to_string().cyan(),
            first.id,
            seconds(first.started.elapsed()).dimmed(),
            lines.next().unwrap_or_default().dimmed()
        );
        let mut rows = 1;
        for line in lines {
            frame.push_str(&format!("\n{:DETAIL_COLUMN$}{}", "", line.dimmed()));
            rows += 1;
        }
        self.clear();
        // Autowrap is off while drawing: a row the terminal wrapped itself
        // would not be counted, and would survive the next clear.
        print!("\x1b[?7l{frame}\x1b[?7h");
        let _ = std::io::stdout().flush();
        self.rows = rows;
    }

    /// What the running checks are doing, wrapped to the detail column of a
    /// `width`-column terminal. A message carrying a whole error page would
    /// fill the screen, so it is cut at [`MAX_ACTIVITY_ROWS`]; the check's
    /// result prints it in full.
    fn step_lines(&self, width: usize) -> Vec<String> {
        let step = match self.running.as_slice() {
            [] => return Vec::new(),
            [only] => only.step.clone(),
            [_, others @ ..] => format!("and {} more running", others.len()),
        };
        let mut lines = wrap(
            &step,
            width.saturating_sub(DETAIL_COLUMN).max(MIN_TEXT_WIDTH),
        );
        if lines.len() > MAX_ACTIVITY_ROWS {
            lines.truncate(MAX_ACTIVITY_ROWS);
            if let Some(last) = lines.last_mut() {
                last.push_str(" …");
            }
        }
        lines
    }

    /// Erases the rows the last frame drew, leaving the cursor where it began.
    fn clear(&mut self) {
        if self.rows == 0 {
            return;
        }
        print!("\r\x1b[2K{}", "\x1b[1A\x1b[2K".repeat(self.rows - 1));
        let _ = std::io::stdout().flush();
        self.rows = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(steps: &[&str]) -> ActivityState {
        ActivityState {
            running: steps
                .iter()
                .map(|step| Running {
                    id: "check",
                    started: Instant::now(),
                    step: step.to_string(),
                })
                .collect(),
            ..ActivityState::default()
        }
    }

    /// Steps wrap within the detail column instead of being cut off, up to a
    /// cap that keeps an error page from filling the screen.
    #[test]
    fn steps_wrap_within_the_detail_column() {
        let width = DETAIL_COLUMN + 40;
        let words = |count: usize| vec!["word"; count].join(" ");
        let cases = [
            (
                "fits on one row",
                running(&["restarting the host"]),
                1,
                false,
            ),
            ("wraps onto more rows", running(&[&words(20)]), 3, false),
            (
                "cut at the cap",
                running(&[&words(200)]),
                MAX_ACTIVITY_ROWS,
                true,
            ),
            ("several running", running(&["a", "b", "c"]), 1, false),
        ];
        for (scenario, state, rows, cut) in cases {
            let lines = state.step_lines(width);
            assert_eq!(lines.len(), rows, "{scenario}: {lines:?}");
            assert!(
                lines
                    .iter()
                    .all(|line| line.chars().count() <= 40 + " …".len()),
                "{scenario}: a row is wider than the column: {lines:?}"
            );
            assert_eq!(
                lines.last().is_some_and(|line| line.ends_with(" …")),
                cut,
                "{scenario}"
            );
        }
        assert_eq!(
            running(&["a", "b", "c"]).step_lines(width),
            ["and 2 more running"]
        );
    }
}
