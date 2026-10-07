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

use bmc_mock::MockPowerState;

use crate::dhcp_retry_fsm::{
    Action as RetryAction, DhcpRetryFsm, Event as RetryEvent, Milliseconds,
};

type FsmReturn = (PowerShelfFsm, Vec<Action>);
type StateReturn = (PowerShelfState, Vec<Action>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PowerShelfFsm {
    state: PowerShelfState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PowerShelfState {
    BmcInit {
        power_on: bool,
        power_cycle_pending: bool,
        paused: bool,
        dhcp_retry: DhcpRetryFsm,
    },
    DeviceUp {
        paused: bool,
    },
    DeviceDown {
        power_cycle_pending: bool,
        paused: bool,
    },
}

impl PowerShelfFsm {
    pub(super) fn init(power_on: bool) -> FsmReturn {
        (
            Self {
                state: PowerShelfState::BmcInit {
                    power_on,
                    power_cycle_pending: false,
                    paused: true,
                    dhcp_retry: DhcpRetryFsm::new(),
                },
            },
            vec![Action::Dhcp],
        )
    }

    pub(super) fn event(self, event: Event) -> FsmReturn {
        let (state, actions) = self.state.event(event);
        (Self { state }, actions)
    }

    pub(super) fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    pub(super) fn power_state(&self) -> MockPowerState {
        self.state.power_state()
    }

    pub(super) fn state_string(&self) -> &'static str {
        self.state.state_string()
    }
}

impl PowerShelfState {
    fn event(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::Pause => return (self.with_paused(true), vec![]),
            Event::Resume => return (self.with_paused(false), vec![]),
            _ => {}
        }

        match self {
            Self::BmcInit {
                power_on,
                power_cycle_pending,
                paused,
                dhcp_retry,
            } => self.fsm_bmc_init(event, power_on, power_cycle_pending, paused, dhcp_retry),
            Self::DeviceUp { paused } => self.fsm_device_up(event, paused),
            Self::DeviceDown {
                power_cycle_pending,
                paused,
            } => self.fsm_device_down(event, power_cycle_pending, paused),
        }
    }

    fn is_paused(&self) -> bool {
        match self {
            Self::BmcInit { paused, .. }
            | Self::DeviceUp { paused }
            | Self::DeviceDown { paused, .. } => *paused,
        }
    }

    fn power_state(&self) -> MockPowerState {
        match self {
            Self::BmcInit { power_on: true, .. } | Self::DeviceUp { .. } => MockPowerState::On,
            Self::BmcInit {
                power_on: false, ..
            }
            | Self::DeviceDown { .. } => MockPowerState::Off,
        }
    }

    fn state_string(&self) -> &'static str {
        match self {
            Self::BmcInit { .. } => "BmcInit",
            Self::DeviceUp { .. } => "BmcOnly/DeviceUp",
            Self::DeviceDown { .. } => "BmcOnly/DeviceDown",
        }
    }

    fn with_paused(self, paused: bool) -> Self {
        match self {
            Self::BmcInit {
                power_on,
                power_cycle_pending,
                dhcp_retry,
                ..
            } => Self::BmcInit {
                power_on,
                power_cycle_pending,
                paused,
                dhcp_retry,
            },
            Self::DeviceUp { .. } => Self::DeviceUp { paused },
            Self::DeviceDown {
                power_cycle_pending,
                ..
            } => Self::DeviceDown {
                power_cycle_pending,
                paused,
            },
        }
    }

    fn fsm_bmc_init(
        self,
        event: Event,
        power_on: bool,
        power_cycle_pending: bool,
        paused: bool,
        dhcp_retry: DhcpRetryFsm,
    ) -> StateReturn {
        match event {
            Event::DhcpFailed(jitter) => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::Failed(jitter));
                (
                    Self::BmcInit {
                        power_on,
                        power_cycle_pending,
                        paused,
                        dhcp_retry,
                    },
                    actions.into_iter().map(map_retry_action).collect(),
                )
            }
            Event::DhcpRetryExpired => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::TimerExpired);
                (
                    Self::BmcInit {
                        power_on,
                        power_cycle_pending,
                        paused,
                        dhcp_retry,
                    },
                    actions.into_iter().map(map_retry_action).collect(),
                )
            }
            Event::DhcpComplete => {
                let (_, retry_actions) = dhcp_retry.event(RetryEvent::Completed);
                let next_state = if power_on {
                    Self::DeviceUp { paused }
                } else {
                    Self::DeviceDown {
                        power_cycle_pending,
                        paused,
                    }
                };
                let mut actions: Vec<_> = retry_actions.into_iter().map(map_retry_action).collect();
                actions.push(Action::SetupBmc);
                (next_state, actions)
            }
            Event::PowerOn => (
                Self::BmcInit {
                    power_on: true,
                    power_cycle_pending: false,
                    paused,
                    dhcp_retry,
                },
                cancel_timer_action(power_cycle_pending),
            ),
            Event::PowerOff => (
                Self::BmcInit {
                    power_on: false,
                    power_cycle_pending: false,
                    paused,
                    dhcp_retry,
                },
                cancel_timer_action(power_cycle_pending),
            ),
            Event::PowerCycle => (
                Self::BmcInit {
                    power_on: false,
                    power_cycle_pending: true,
                    paused,
                    dhcp_retry,
                },
                vec![Action::SetTimer(Timer::PowerCycle)],
            ),
            Event::TimerAlert(Timer::PowerCycle) if power_cycle_pending => (
                Self::BmcInit {
                    power_on: true,
                    power_cycle_pending: false,
                    paused,
                    dhcp_retry,
                },
                vec![],
            ),
            Event::TimerAlert(Timer::PowerCycle) => (self, vec![]),
            Event::Pause | Event::Resume => unreachable!("handled before state dispatch"),
        }
    }

    fn fsm_device_up(self, event: Event, paused: bool) -> StateReturn {
        match event {
            Event::PowerOff => (
                Self::DeviceDown {
                    power_cycle_pending: false,
                    paused,
                },
                vec![],
            ),
            Event::PowerCycle => (
                Self::DeviceDown {
                    power_cycle_pending: true,
                    paused,
                },
                vec![Action::SetTimer(Timer::PowerCycle)],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_device_down(self, event: Event, power_cycle_pending: bool, paused: bool) -> StateReturn {
        match event {
            Event::PowerOn => (
                Self::DeviceUp { paused },
                cancel_timer_action(power_cycle_pending),
            ),
            Event::PowerOff => (
                Self::DeviceDown {
                    power_cycle_pending: false,
                    paused,
                },
                cancel_timer_action(power_cycle_pending),
            ),
            Event::TimerAlert(Timer::PowerCycle) if power_cycle_pending => {
                (Self::DeviceUp { paused }, vec![])
            }
            Event::TimerAlert(Timer::PowerCycle) => (self, vec![]),
            Event::PowerCycle => (
                Self::DeviceDown {
                    power_cycle_pending: true,
                    paused,
                },
                vec![Action::SetTimer(Timer::PowerCycle)],
            ),
            _ => (self, vec![]),
        }
    }
}

fn map_retry_action(action: RetryAction) -> Action {
    match action {
        RetryAction::Schedule { delay } => Action::ScheduleDhcpRetry { delay },
        RetryAction::Run => Action::Dhcp,
        RetryAction::Cancel => Action::CancelDhcpRetry,
    }
}

fn cancel_timer_action(power_cycle_pending: bool) -> Vec<Action> {
    if power_cycle_pending {
        vec![Action::CancelTimer(Timer::PowerCycle)]
    } else {
        vec![]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Event {
    DhcpComplete,
    DhcpFailed(Milliseconds),
    DhcpRetryExpired,
    PowerOn,
    PowerOff,
    PowerCycle,
    TimerAlert(Timer),
    Pause,
    Resume,
}

#[cfg(test)]
impl Event {
    fn dhcp_failed_with_jitter(jitter: Milliseconds) -> Self {
        Self::DhcpFailed(jitter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Dhcp,
    ScheduleDhcpRetry { delay: Duration },
    CancelDhcpRetry,
    SetupBmc,
    SetTimer(Timer),
    CancelTimer(Timer),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Timer {
    PowerCycle,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    type TestFsm = PowerShelfFsm;

    fn with_state(state: PowerShelfState) -> TestFsm {
        PowerShelfFsm { state }
    }

    fn bmc_init(power_on: bool, power_cycle_pending: bool, paused: bool) -> PowerShelfState {
        PowerShelfState::BmcInit {
            power_on,
            power_cycle_pending,
            paused,
            dhcp_retry: DhcpRetryFsm::new(),
        }
    }

    #[test]
    fn power_shelf_transitions() {
        check_values(
            [
                Check {
                    scenario: "new power shelf completes BMC DHCP powered off",
                    input: (
                        with_state(bmc_init(false, false, false)),
                        Event::DhcpComplete,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: false,
                            paused: false,
                        }),
                        vec![Action::SetupBmc],
                    ),
                },
                Check {
                    scenario: "persisted power shelf completes BMC DHCP powered on",
                    input: (
                        with_state(bmc_init(true, false, false)),
                        Event::DhcpComplete,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceUp { paused: false }),
                        vec![Action::SetupBmc],
                    ),
                },
                Check {
                    scenario: "powered shelf begins a power cycle",
                    input: (
                        with_state(PowerShelfState::DeviceUp { paused: false }),
                        Event::PowerCycle,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: true,
                            paused: false,
                        }),
                        vec![Action::SetTimer(Timer::PowerCycle)],
                    ),
                },
                Check {
                    scenario: "power-cycle timer restores power",
                    input: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: true,
                            paused: false,
                        }),
                        Event::TimerAlert(Timer::PowerCycle),
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceUp { paused: false }),
                        vec![],
                    ),
                },
                Check {
                    scenario: "explicit power-on cancels a pending power cycle",
                    input: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: true,
                            paused: false,
                        }),
                        Event::PowerOn,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceUp { paused: false }),
                        vec![Action::CancelTimer(Timer::PowerCycle)],
                    ),
                },
                Check {
                    scenario: "a new power cycle replaces the pending timer",
                    input: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: true,
                            paused: false,
                        }),
                        Event::PowerCycle,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: true,
                            paused: false,
                        }),
                        vec![Action::SetTimer(Timer::PowerCycle)],
                    ),
                },
                Check {
                    scenario: "power-shelf processing can be paused",
                    input: (
                        with_state(PowerShelfState::DeviceUp { paused: false }),
                        Event::Pause,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceUp { paused: true }),
                        vec![],
                    ),
                },
                Check {
                    scenario: "power-shelf processing can be resumed",
                    input: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: false,
                            paused: true,
                        }),
                        Event::Resume,
                    ),
                    expect: (
                        with_state(PowerShelfState::DeviceDown {
                            power_cycle_pending: false,
                            paused: false,
                        }),
                        vec![],
                    ),
                },
            ],
            |(fsm, event)| fsm.event(event),
        );
    }

    #[test]
    fn power_off_supersedes_power_cycle() {
        let (fsm, _) =
            with_state(PowerShelfState::DeviceUp { paused: false }).event(Event::PowerCycle);
        let (fsm, actions) = fsm.event(Event::PowerOff);
        assert_eq!(
            (fsm, actions,),
            (
                with_state(PowerShelfState::DeviceDown {
                    power_cycle_pending: false,
                    paused: false,
                }),
                vec![Action::CancelTimer(Timer::PowerCycle)],
            )
        );

        let (fsm, actions) = fsm.event(Event::TimerAlert(Timer::PowerCycle));
        assert_eq!(
            (fsm, actions,),
            (
                with_state(PowerShelfState::DeviceDown {
                    power_cycle_pending: false,
                    paused: false,
                }),
                vec![],
            )
        );
    }

    #[test]
    fn bmc_retry_survives_pause_and_power_changes() {
        let fsm = with_state(bmc_init(false, false, false));
        let (fsm, actions) = fsm.event(Event::dhcp_failed_with_jitter(Milliseconds::new(0)));
        assert_eq!(
            actions,
            vec![Action::ScheduleDhcpRetry {
                delay: Duration::from_secs(4),
            }]
        );

        let (fsm, actions) = fsm.event(Event::Pause);
        assert!(actions.is_empty());
        let (fsm, actions) = fsm.event(Event::PowerCycle);
        assert_eq!(actions, vec![Action::SetTimer(Timer::PowerCycle)]);
        let (_, actions) = fsm.event(Event::DhcpRetryExpired);
        assert_eq!(actions, vec![Action::Dhcp]);
    }
}
