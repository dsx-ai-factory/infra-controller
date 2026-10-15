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

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::watch;
use tokio::time::Instant;

const OUTPUT_INTERVAL: Duration = Duration::from_millis(100);

const BOOT_TRANSCRIPT: &[&str] = &[
    "UEFI firmware initialization complete",
    "Loading Linux kernel and initial ramdisk",
    "Linux version 6.12.0-machine-a-tron",
    "Mounting root filesystem",
    "systemd[1]: Starting Network Configuration",
    "systemd[1]: Started Network Configuration",
    "systemd[1]: Starting Carbide machine agent",
    "systemd[1]: Started Carbide machine agent",
];

#[derive(Clone, Copy, Debug)]
struct OutputState {
    generation: u64,
    active_since: Option<Instant>,
}

/// Controls deterministic boot output for one machine simulated by machine-a-tron.
///
/// Starting an already-active controller begins a new generation. Subscribers see the current
/// output slot without replaying earlier lines, and slow subscribers skip missed slots.
/// Each start or stop updates the state atomically across controller clones.
#[derive(Clone, Debug)]
pub struct ConsoleOutputController {
    stable_id: Arc<str>,
    state: watch::Sender<OutputState>,
}

impl ConsoleOutputController {
    /// Creates an inactive boot-output controller for the supplied stable machine identity.
    pub fn new(stable_id: impl Into<String>) -> Self {
        Self {
            stable_id: Arc::from(stable_id.into()),
            state: watch::channel(OutputState {
                generation: 0,
                active_since: None,
            })
            .0,
        }
    }

    /// Starts a new boot-output generation at slot zero.
    pub fn start(&self) {
        self.state.send_modify(|state| {
            state.generation = state.generation.saturating_add(1);
            state.active_since = Some(Instant::now());
        });
    }

    /// Stops boot output while retaining the current generation number.
    pub fn stop(&self) {
        self.state.send_modify(|state| {
            state.active_since = None;
        });
    }

    /// Returns a lazy byte stream for one console connection.
    ///
    /// No output task is created. Each poll derives the newest slot from the controller clock.
    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = Bytes> + Send + 'static>> {
        let subscription = ConsoleOutputSubscription {
            stable_id: self.stable_id.clone(),
            state: self.state.subscribe(),
            last_output: None,
        };
        Box::pin(futures::stream::unfold(
            subscription,
            |mut subscription| async move {
                subscription
                    .next_line()
                    .await
                    .map(|line| (line, subscription))
            },
        ))
    }
}

struct ConsoleOutputSubscription {
    stable_id: Arc<str>,
    state: watch::Receiver<OutputState>,
    last_output: Option<(u64, u64)>,
}

#[derive(Debug, PartialEq, Eq)]
enum NextLineState {
    Inactive,
    Ready { generation: u64, slot: u64 },
    Waiting { deadline: Instant },
}

impl ConsoleOutputSubscription {
    async fn next_line(&mut self) -> Option<Bytes> {
        loop {
            let state = *self.state.borrow_and_update();
            match next_line_state(state, self.last_output, Instant::now()) {
                NextLineState::Inactive => self.state.changed().await.ok()?,
                NextLineState::Ready { generation, slot } => {
                    self.last_output = Some((generation, slot));
                    return Some(render_line(&self.stable_id, generation, slot));
                }
                NextLineState::Waiting { deadline } => {
                    tokio::select! {
                        changed = self.state.changed() => changed.ok()?,
                        () = tokio::time::sleep_until(deadline) => {}
                    }
                }
            }
        }
    }
}

fn next_line_state(
    state: OutputState,
    last_output: Option<(u64, u64)>,
    now: Instant,
) -> NextLineState {
    let Some(active_since) = state.active_since else {
        return NextLineState::Inactive;
    };

    let slot = slot_at(active_since, now);
    if last_output != Some((state.generation, slot)) {
        return NextLineState::Ready {
            generation: state.generation,
            slot,
        };
    }

    let deadline = active_since
        .checked_add(duration_for_slot(slot.saturating_add(1)))
        .unwrap_or(now);
    NextLineState::Waiting { deadline }
}

fn slot_at(active_since: Instant, now: Instant) -> u64 {
    let elapsed = now.saturating_duration_since(active_since).as_millis();
    u64::try_from(elapsed / OUTPUT_INTERVAL.as_millis()).unwrap_or(u64::MAX)
}

fn duration_for_slot(slot: u64) -> Duration {
    let milliseconds = u128::from(slot).saturating_mul(OUTPUT_INTERVAL.as_millis());
    Duration::from_millis(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}

fn render_line(stable_id: &str, generation: u64, slot: u64) -> Bytes {
    let transcript_index = usize::try_from(slot % BOOT_TRANSCRIPT.len() as u64)
        .expect("boot transcript index is bounded by its length");
    Bytes::from(format!(
        "[machine-a-tron machine={stable_id} generation={generation} slot={slot}] {}\r\n",
        BOOT_TRANSCRIPT[transcript_index]
    ))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use futures::{FutureExt, StreamExt};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn output_starts_stops_and_restarts_with_a_new_generation() {
        let controller = ConsoleOutputController::new("machine-1");
        let mut stream = controller.stream();

        assert!(stream.next().now_or_never().is_none());

        controller.start();
        assert_eq!(stream.next().await.unwrap(), render_line("machine-1", 1, 0));
        tokio::time::advance(OUTPUT_INTERVAL).await;
        assert_eq!(stream.next().await.unwrap(), render_line("machine-1", 1, 1));

        controller.stop();
        assert!(stream.next().now_or_never().is_none());

        controller.start();
        assert_eq!(stream.next().await.unwrap(), render_line("machine-1", 2, 0));
    }

    #[test]
    fn late_and_slow_subscribers_skip_old_slots() {
        let started_at = Instant::now();
        let state = OutputState {
            generation: 1,
            active_since: Some(started_at),
        };

        value_scenarios!(
            run = |(last_output, elapsed_ms)| next_line_state(
                state, last_output, started_at + Duration::from_millis(elapsed_ms)
            );
            "late subscriber" {
                (None, 250) => NextLineState::Ready { generation: 1, slot: 2 },
            }
            "slow subscriber" {
                (Some((1, 2)), 800) => NextLineState::Ready { generation: 1, slot: 8 },
            }
        );
    }

    #[test]
    fn rendered_lines_are_deterministic_crlf_records() {
        let line = render_line("machine-3", 4, 9);
        assert_eq!(
            line,
            "[machine-a-tron machine=machine-3 generation=4 slot=9] Loading Linux kernel and initial ramdisk\r\n"
        );
    }
}
