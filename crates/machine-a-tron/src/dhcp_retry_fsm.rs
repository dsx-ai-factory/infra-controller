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

use std::time::Duration;

// RFC 2131 section 4.1's Ethernet example starts at four seconds, doubles to a
// 64-second base, and adds uniform jitter from -1 through +1 second.
pub(super) const DHCP_RETRY_MAX_JITTER: Milliseconds = Milliseconds::new(1_000);
const DHCP_RETRY_MIN_JITTER: Milliseconds = Milliseconds::new(-1_000);
const DHCP_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(4);
const DHCP_RETRY_MAX_DELAY: Duration = Duration::from_secs(64);

/// Signed millisecond offset used for DHCP retry jitter.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Milliseconds(i64);

impl Milliseconds {
    pub(super) const fn new(value: i64) -> Self {
        Self(value)
    }

    pub(super) const fn value(self) -> i64 {
        self.0
    }

    fn clamp(self, min: Self, max: Self) -> Self {
        Self(self.0.clamp(min.0, max.0))
    }

    fn apply_to(self, duration: Duration) -> Duration {
        let magnitude = Duration::from_millis(self.0.unsigned_abs());
        if self.0.is_negative() {
            duration.saturating_sub(magnitude)
        } else {
            duration.saturating_add(magnitude)
        }
    }
}

/// Pure single-flight state machine for DHCP retry policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DhcpRetryFsm {
    state: State,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Waiting { attempt: u32 },
    Retrying { attempt: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Event {
    Failed(Milliseconds),
    TimerExpired,
    Completed,
    Abandon,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Schedule { delay: Duration },
    Run,
    Cancel,
}

impl DhcpRetryFsm {
    pub(super) const fn new() -> Self {
        Self { state: State::Idle }
    }

    pub(super) fn event(self, event: Event) -> (Self, Vec<Action>) {
        match self.state {
            State::Idle => self.fsm_idle(event),
            State::Waiting { attempt } => self.fsm_waiting(event, attempt),
            State::Retrying { attempt } => self.fsm_retrying(event, attempt),
        }
    }

    fn fsm_idle(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::Failed(jitter) => self.schedule_retry(0, jitter),
            Event::TimerExpired | Event::Completed | Event::Abandon => (self, vec![]),
        }
    }

    fn fsm_waiting(self, event: Event, attempt: u32) -> (Self, Vec<Action>) {
        match event {
            Event::TimerExpired => (
                Self {
                    state: State::Retrying { attempt },
                },
                vec![Action::Run],
            ),
            Event::Completed | Event::Abandon => {
                (Self { state: State::Idle }, vec![Action::Cancel])
            }
            Event::Failed(_) => (self, vec![]),
        }
    }

    fn fsm_retrying(self, event: Event, attempt: u32) -> (Self, Vec<Action>) {
        match event {
            Event::Failed(jitter) => self.schedule_retry(attempt, jitter),
            Event::Completed | Event::Abandon => (Self { state: State::Idle }, vec![]),
            Event::TimerExpired => (self, vec![]),
        }
    }

    fn schedule_retry(self, retry_attempt: u32, jitter: Milliseconds) -> (Self, Vec<Action>) {
        let delay = dhcp_retry_delay(retry_attempt, jitter);
        let attempt = retry_attempt.saturating_add(1);
        (
            Self {
                state: State::Waiting { attempt },
            },
            vec![Action::Schedule { delay }],
        )
    }
}

fn dhcp_retry_delay(retry_attempt: u32, jitter: Milliseconds) -> Duration {
    let jitter = jitter.clamp(DHCP_RETRY_MIN_JITTER, DHCP_RETRY_MAX_JITTER);
    let multiplier = 1_u32 << retry_attempt.min(4);
    let base_delay = DHCP_RETRY_INITIAL_DELAY
        .saturating_mul(multiplier)
        .min(DHCP_RETRY_MAX_DELAY);
    jitter.apply_to(base_delay)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    fn fsm() -> DhcpRetryFsm {
        DhcpRetryFsm::new()
    }

    fn failed() -> Event {
        Event::Failed(Milliseconds::new(0))
    }

    #[test]
    fn delay_uses_rfc_2131_backoff() {
        check_values(
            [
                Check {
                    scenario: "first retry with minimum jitter",
                    input: (0, Milliseconds::new(-1_000)),
                    expect: Duration::from_secs(3),
                },
                Check {
                    scenario: "jitter below the minimum is clamped",
                    input: (0, Milliseconds::new(i64::MIN)),
                    expect: Duration::from_secs(3),
                },
                Check {
                    scenario: "first retry without jitter",
                    input: (0, Milliseconds::new(0)),
                    expect: Duration::from_secs(4),
                },
                Check {
                    scenario: "first retry with maximum jitter",
                    input: (0, Milliseconds::new(1_000)),
                    expect: Duration::from_secs(5),
                },
                Check {
                    scenario: "jitter above the maximum is clamped",
                    input: (0, Milliseconds::new(i64::MAX)),
                    expect: Duration::from_secs(5),
                },
                Check {
                    scenario: "second retry doubles the base",
                    input: (1, Milliseconds::new(0)),
                    expect: Duration::from_secs(8),
                },
                Check {
                    scenario: "third retry doubles the base",
                    input: (2, Milliseconds::new(0)),
                    expect: Duration::from_secs(16),
                },
                Check {
                    scenario: "fourth retry doubles the base",
                    input: (3, Milliseconds::new(0)),
                    expect: Duration::from_secs(32),
                },
                Check {
                    scenario: "fifth retry reaches the cap",
                    input: (4, Milliseconds::new(0)),
                    expect: Duration::from_secs(64),
                },
                Check {
                    scenario: "later retry remains capped with minimum jitter",
                    input: (u32::MAX, Milliseconds::new(-1_000)),
                    expect: Duration::from_secs(63),
                },
                Check {
                    scenario: "later retry remains capped with maximum jitter",
                    input: (u32::MAX, Milliseconds::new(1_000)),
                    expect: Duration::from_secs(65),
                },
            ],
            |(attempt, jitter)| dhcp_retry_delay(attempt, jitter),
        );
    }

    #[test]
    fn retry_lifecycle_is_single_flight() {
        let fsm = fsm();
        let (fsm, actions) = fsm.event(failed());
        assert_eq!(
            actions,
            vec![Action::Schedule {
                delay: Duration::from_secs(4),
            }]
        );

        let (fsm, actions) = fsm.event(Event::TimerExpired);
        assert_eq!(actions, vec![Action::Run]);
        let (fsm, actions) = fsm.event(failed());
        assert_eq!(
            actions,
            vec![Action::Schedule {
                delay: Duration::from_secs(8),
            }]
        );

        let (fsm, actions) = fsm.event(Event::Abandon);
        assert_eq!(fsm.state, State::Idle);
        assert_eq!(actions, vec![Action::Cancel]);
    }

    #[test]
    fn failure_uses_supplied_jitter() {
        let (_, actions) = fsm().event(Event::Failed(DHCP_RETRY_MAX_JITTER));
        assert_eq!(
            actions,
            vec![Action::Schedule {
                delay: Duration::from_secs(5),
            }]
        );
    }

    #[test]
    fn completion_resets_the_retry_ladder() {
        let (fsm, _) = fsm().event(failed());
        let (fsm, _) = fsm.event(Event::TimerExpired);
        let (fsm, _) = fsm.event(failed());
        let (fsm, actions) = fsm.event(Event::TimerExpired);
        assert_eq!(actions, vec![Action::Run]);
        let (fsm, actions) = fsm.event(Event::Completed);
        assert_eq!(actions, vec![]);

        let (_, actions) = fsm.event(failed());
        assert_eq!(
            actions,
            vec![Action::Schedule {
                delay: Duration::from_secs(4),
            }]
        );
    }
}
