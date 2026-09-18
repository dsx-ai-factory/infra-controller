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

use bmc_mock::{BmcEvent, MockPowerState};

use crate::dhcp_retry_fsm::{
    Action as RetryAction, DhcpRetryFsm, Event as RetryEvent, Milliseconds,
};
use crate::machine_state_machine::OsImage;

type FsmReturn<Fsm> = (Fsm, Vec<Action>);

#[derive(Clone, Copy, Debug)]
pub(super) struct MachineFsm {
    state: MachineState,
}

#[derive(Clone, Copy, Debug)]
enum MachineState {
    BmcInit {
        power_on: bool,
        bmc_only: bool,
        dhcp_retry: DhcpRetryFsm,
    },
    Init {
        dhcp_retry: DhcpRetryFsm,
    },
    MachineDown,
    DhcpComplete,
    MachineUp {
        os_fsm: OsFsm,
    },
    BmcOnlyMachineUp,
    BmcOnlyMachineDown,
}

impl MachineFsm {
    pub(super) fn init(power_on: bool, bmc_only: bool) -> FsmReturn<Self> {
        (
            Self {
                state: MachineState::BmcInit {
                    power_on,
                    bmc_only,
                    dhcp_retry: DhcpRetryFsm::new(),
                },
            },
            vec![Action::Dhcp(DhcpType::Bmc)],
        )
    }

    pub(super) fn event(self, event: Event) -> FsmReturn<Self> {
        let (state, actions) = self.state.event(event);
        (Self { state }, actions)
    }

    pub(super) fn is_up(&self) -> bool {
        self.state.is_up()
    }

    pub(super) fn is_bmc_only(&self) -> bool {
        matches!(
            self.state,
            MachineState::BmcOnlyMachineUp | MachineState::BmcOnlyMachineDown
        )
    }

    pub(super) fn is_bmc_initializing(&self) -> bool {
        matches!(self.state, MachineState::BmcInit { .. })
    }

    pub(super) fn power_state(&self) -> MockPowerState {
        self.state.power_state()
    }

    pub(super) fn state_string(&self) -> &'static str {
        self.state.state_string()
    }

    pub(super) fn booted_os(&self) -> Option<OsImage> {
        self.state.booted_os()
    }
}

impl MachineState {
    fn event(self, event: Event) -> FsmReturn<Self> {
        // A managed DPU that applied a staged NIC-mode flip is now a plain NIC,
        // not a DPU: converge it to the dormant BMC-only track from any active
        // state. Producing this transition here (rather than assigning the state
        // in the driver) keeps every FSM transition flowing through `event()`.
        if matches!(event, Event::DpuFlippedToNicMode) {
            return match self {
                Self::BmcOnlyMachineUp | Self::BmcOnlyMachineDown => (self, vec![]),
                // Clean up as the DPU parks: drop its relay handle and cached
                // discovery state so the flipped NIC stops serving host DHCP.
                _ => (
                    Self::BmcOnlyMachineUp,
                    self.abandon_dhcp_retry()
                        .into_iter()
                        .chain([Action::ConsoleOutputStop, Action::CleanupOnPowerOff])
                        .collect(),
                ),
            };
        }
        match self {
            Self::BmcInit {
                power_on,
                bmc_only,
                dhcp_retry,
            } => self.fsm_bmc_init(event, power_on, bmc_only, dhcp_retry),
            Self::Init { dhcp_retry } => self.fsm_init(event, dhcp_retry),
            Self::MachineDown => self.fsm_machine_down(event),
            Self::DhcpComplete => self.fsm_dhcp_complete(event),
            Self::MachineUp { os_fsm } => self.fsm_machine_up(event, os_fsm),

            Self::BmcOnlyMachineUp => self.fsm_bmc_only_machine_up(event),
            Self::BmcOnlyMachineDown => self.fsm_bmc_only_machine_down(event),
        }
    }

    fn is_up(&self) -> bool {
        matches!(self, Self::MachineUp { .. } | Self::BmcOnlyMachineUp)
    }

    fn power_state(&self) -> MockPowerState {
        match self {
            Self::BmcInit { power_on: true, .. } => MockPowerState::On,
            Self::BmcInit {
                power_on: false, ..
            } => MockPowerState::Off,
            Self::Init { .. } => MockPowerState::On,
            Self::MachineDown => MockPowerState::Off,
            Self::DhcpComplete => MockPowerState::On,
            Self::MachineUp { .. } => MockPowerState::On,
            Self::BmcOnlyMachineUp => MockPowerState::On,
            Self::BmcOnlyMachineDown => MockPowerState::Off,
        }
    }

    fn state_string(&self) -> &'static str {
        match self {
            Self::BmcInit { .. } => "BmcInit",
            Self::Init { .. } => "Init",
            Self::MachineDown => "MachineDown",
            Self::DhcpComplete => "DhcpComplete",
            Self::MachineUp { .. } => "MachineUp",
            Self::BmcOnlyMachineUp => "BmcOnly/MachineUp",
            Self::BmcOnlyMachineDown => "BmcOnly/MachineDown",
        }
    }

    fn booted_os(&self) -> Option<OsImage> {
        match self {
            Self::MachineUp {
                os_fsm: OsFsm::Scout { .. },
            } => Some(OsImage::Scout),
            Self::MachineUp {
                os_fsm: OsFsm::DpuAgent { .. },
            } => Some(OsImage::DpuAgent),
            Self::MachineUp {
                os_fsm: OsFsm::None,
            } => Some(OsImage::None),
            _ => None,
        }
    }

    fn fsm_bmc_init(
        self,
        event: Event,
        power_on: bool,
        bmc_only: bool,
        dhcp_retry: DhcpRetryFsm,
    ) -> (Self, Vec<Action>) {
        match event {
            Event::DhcpFailed(jitter) => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::Failed(jitter));
                (
                    Self::BmcInit {
                        power_on,
                        bmc_only,
                        dhcp_retry,
                    },
                    map_retry_actions(actions, DhcpType::Bmc),
                )
            }
            Event::DhcpRetryExpired => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::TimerExpired);
                (
                    Self::BmcInit {
                        power_on,
                        bmc_only,
                        dhcp_retry,
                    },
                    map_retry_actions(actions, DhcpType::Bmc),
                )
            }
            Event::DhcpComplete => {
                let (_, retry_actions) = dhcp_retry.event(RetryEvent::Completed);
                let next_state = if bmc_only {
                    if power_on {
                        Self::BmcOnlyMachineUp
                    } else {
                        Self::BmcOnlyMachineDown
                    }
                } else if power_on {
                    Self::Init {
                        dhcp_retry: DhcpRetryFsm::new(),
                    }
                } else {
                    Self::MachineDown
                };
                let mut actions = map_retry_actions(retry_actions, DhcpType::Bmc);
                actions.extend(if power_on && !bmc_only {
                    vec![
                        Action::SetupBmc,
                        Action::ConsoleOutputStart,
                        Action::SetTimer(Timer::MachineOn),
                    ]
                } else {
                    vec![Action::SetupBmc]
                });
                (next_state, actions)
            }
            Event::PowerOn => (
                Self::BmcInit {
                    bmc_only,
                    power_on: true,
                    dhcp_retry,
                },
                if power_on || bmc_only {
                    vec![]
                } else {
                    vec![
                        Action::ConsoleOutputStart,
                        Action::SetTimer(Timer::MachineOn),
                    ]
                },
            ),
            Event::PowerOff => (
                Self::BmcInit {
                    bmc_only,
                    power_on: false,
                    dhcp_retry,
                },
                vec![Action::ConsoleOutputStop],
            ),
            Event::PowerCycle => (
                Self::BmcInit {
                    bmc_only,
                    power_on: false,
                    dhcp_retry,
                },
                vec![
                    Action::ConsoleOutputStop,
                    Action::SetTimer(Timer::PowerCycle),
                ],
            ),
            Event::TimerAlert(Timer::PowerCycle) => (
                Self::BmcInit {
                    bmc_only,
                    power_on: true,
                    dhcp_retry,
                },
                vec![],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_init(self, event: Event, dhcp_retry: DhcpRetryFsm) -> (Self, Vec<Action>) {
        match event {
            Event::DhcpFailed(jitter) => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::Failed(jitter));
                (
                    Self::Init { dhcp_retry },
                    map_retry_actions(actions, DhcpType::Machine),
                )
            }
            Event::DhcpRetryExpired => {
                let (dhcp_retry, actions) = dhcp_retry.event(RetryEvent::TimerExpired);
                (
                    Self::Init { dhcp_retry },
                    map_retry_actions(actions, DhcpType::Machine),
                )
            }
            Event::TimerAlert(Timer::MachineOn) => (
                self,
                vec![
                    Action::BmcEvent(BmcEvent::PowerOn),
                    Action::Dhcp(DhcpType::Machine),
                ],
            ),
            Event::DhcpComplete => {
                let (_, retry_actions) = dhcp_retry.event(RetryEvent::Completed);
                let mut actions = map_retry_actions(retry_actions, DhcpType::Machine);
                actions.push(Action::PxeBootRequest);
                (Self::DhcpComplete, actions)
            }
            Event::PowerCycle => {
                let (_, retry_actions) = dhcp_retry.event(RetryEvent::Abandon);
                let mut actions = map_retry_actions(retry_actions, DhcpType::Machine);
                actions.extend([
                    Action::ConsoleOutputStop,
                    Action::CleanupOnPowerOff,
                    Action::SetTimer(Timer::PowerCycle),
                ]);
                (Self::MachineDown, actions)
            }
            Event::PowerOff => {
                let (_, retry_actions) = dhcp_retry.event(RetryEvent::Abandon);
                (
                    Self::MachineDown,
                    map_retry_actions(retry_actions, DhcpType::Machine)
                        .into_iter()
                        .chain([Action::ConsoleOutputStop, Action::CleanupOnPowerOff])
                        .collect(),
                )
            }
            _ => (self, vec![]),
        }
    }

    fn fsm_machine_down(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::PowerCycle => (
                self,
                vec![
                    Action::ConsoleOutputStop,
                    Action::SetTimer(Timer::PowerCycle),
                ],
            ),
            Event::PowerOn | Event::TimerAlert(Timer::PowerCycle) => (
                Self::Init {
                    dhcp_retry: DhcpRetryFsm::new(),
                },
                vec![
                    Action::ConsoleOutputStart,
                    Action::SetTimer(Timer::MachineOn),
                ],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_dhcp_complete(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::PowerCycle => self.machine_down_on_power_cycle(),
            Event::PowerOff => self.machine_down_on_power_off(),
            Event::PxeComplete(os_image) => {
                let os_fsm = match os_image {
                    OsImage::None => OsFsm::None,
                    OsImage::DpuAgent => OsFsm::DpuAgent(DpuAgentFsm::Discovery),
                    OsImage::Scout => OsFsm::Scout(ScoutFsm::Discovery),
                };
                let actions = match os_fsm {
                    OsFsm::None => vec![
                        Action::ConsoleOutputStop,
                        Action::BmcEvent(BmcEvent::BootCompleted),
                    ],
                    _ => os_fsm.init_actions(),
                };
                (Self::MachineUp { os_fsm }, actions)
            }
            _ => (self, vec![]),
        }
    }

    fn fsm_machine_up(self, event: Event, os_fsm: OsFsm) -> (Self, Vec<Action>) {
        match event {
            Event::PowerCycle => self.machine_down_on_power_cycle(),
            Event::PowerOff => self.machine_down_on_power_off(),
            // A host whose OS failed and is parked waiting for a reboot -- e.g.
            // its machine was force-deleted, so its agent hit `MachineNotFound`
            // -- reboots when the controller powers it back on, so it re-PXEs and
            // re-ingests. A real host always boots on power-on; a normally serving
            // host treats a redundant `PowerOn` as a no-op (the `os_fsm`
            // fall-through below), so scope the reboot to the failed-waiting case.
            Event::PowerOn if os_fsm.is_awaiting_reboot() => self.machine_down_on_power_cycle(),
            _ => {
                let (os_fsm, actions) = os_fsm.event(event);
                (Self::MachineUp { os_fsm }, actions)
            }
        }
    }

    fn fsm_bmc_only_machine_up(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::PowerOff => (Self::BmcOnlyMachineDown, vec![Action::ConsoleOutputStop]),
            Event::PowerCycle => (
                Self::BmcOnlyMachineDown,
                vec![
                    Action::ConsoleOutputStop,
                    Action::CleanupOnPowerOff,
                    Action::SetTimer(Timer::PowerCycle),
                ],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_bmc_only_machine_down(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::PowerCycle => (
                Self::BmcOnlyMachineDown,
                vec![
                    Action::ConsoleOutputStop,
                    Action::CleanupOnPowerOff,
                    Action::SetTimer(Timer::PowerCycle),
                ],
            ),
            Event::PowerOn | Event::TimerAlert(Timer::PowerCycle) => {
                (Self::BmcOnlyMachineUp, vec![])
            }
            _ => (self, vec![]),
        }
    }

    fn machine_down_on_power_off(self) -> (Self, Vec<Action>) {
        (
            Self::MachineDown,
            vec![Action::ConsoleOutputStop, Action::CleanupOnPowerOff],
        )
    }

    fn machine_down_on_power_cycle(self) -> (Self, Vec<Action>) {
        (
            Self::MachineDown,
            vec![
                Action::ConsoleOutputStop,
                Action::CleanupOnPowerOff,
                Action::SetTimer(Timer::PowerCycle),
            ],
        )
    }

    fn abandon_dhcp_retry(self) -> Vec<Action> {
        let (dhcp_retry, dhcp_type) = match self {
            Self::BmcInit { dhcp_retry, .. } => (dhcp_retry, DhcpType::Bmc),
            Self::Init { dhcp_retry } => (dhcp_retry, DhcpType::Machine),
            _ => return vec![],
        };
        let (_, actions) = dhcp_retry.event(RetryEvent::Abandon);
        map_retry_actions(actions, dhcp_type)
    }
}

fn map_retry_actions(actions: Vec<RetryAction>, dhcp_type: DhcpType) -> Vec<Action> {
    actions
        .into_iter()
        .map(|action| map_retry_action(action, dhcp_type))
        .collect()
}

fn map_retry_action(action: RetryAction, dhcp_type: DhcpType) -> Action {
    match action {
        RetryAction::Schedule { delay } => Action::ScheduleDhcpRetry { delay },
        RetryAction::Run => Action::Dhcp(dhcp_type),
        RetryAction::Cancel => Action::CancelDhcpRetry,
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) enum Event {
    DhcpComplete,
    DhcpFailed(Milliseconds),
    DhcpRetryExpired,
    PowerOn,
    PowerOff,
    PowerCycle,
    TimerAlert(Timer),
    PxeComplete(OsImage),
    InitialDiscoveryCompleted,
    AgentControlCompleted,
    MachineNotFound,
    NetworkObservationCompleted,
    DpuFlippedToNicMode,
}

#[cfg(test)]
impl Event {
    fn dhcp_failed_with_jitter(jitter: Milliseconds) -> Self {
        Self::DhcpFailed(jitter)
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) enum Action {
    SetupBmc,
    ConsoleOutputStart,
    ConsoleOutputStop,
    SetTimer(Timer),
    Dhcp(DhcpType),
    ScheduleDhcpRetry { delay: Duration },
    CancelDhcpRetry,
    PxeBootRequest,
    InitialDiscoveryRequest(OsImage),
    AgentControlRequest(OsImage),
    DpuAgentNetworkObservation,
    CleanupOnPowerOff,
    BmcEvent(BmcEvent),
}

#[derive(Copy, Clone, Debug)]
pub(super) enum Timer {
    PowerCycle,
    MachineOn,
    ScoutAgentControlPoll,
    DpuAgentControlPoll,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum DhcpType {
    Bmc,
    Machine,
}

#[derive(Copy, Clone, Debug)]
pub(super) enum OsFsm {
    None,
    Scout(ScoutFsm),
    DpuAgent(DpuAgentFsm),
}

impl OsFsm {
    fn init_actions(&self) -> Vec<Action> {
        match self {
            Self::None => vec![],
            Self::Scout(_) => vec![Action::InitialDiscoveryRequest(OsImage::Scout)],
            Self::DpuAgent(_) => vec![Action::InitialDiscoveryRequest(OsImage::DpuAgent)],
        }
    }

    fn event(self, event: Event) -> (Self, Vec<Action>) {
        match self {
            Self::None => (self, vec![]),
            Self::Scout(scout_fsm) => {
                let (scout_fsm, actions) = scout_fsm.event(event);
                (Self::Scout(scout_fsm), actions)
            }
            Self::DpuAgent(dpu_agent_fsm) => {
                let (dpu_agent_fsm, actions) = dpu_agent_fsm.event(event);
                (Self::DpuAgent(dpu_agent_fsm), actions)
            }
        }
    }

    /// A failed OS parked waiting to be rebooted -- e.g. its machine was
    /// force-deleted and its agent hit `MachineNotFound`. The host must reboot
    /// on the next power-on to re-PXE and re-ingest.
    fn is_awaiting_reboot(&self) -> bool {
        matches!(
            self,
            Self::Scout(ScoutFsm::FailedAndWaitForReboot)
                | Self::DpuAgent(DpuAgentFsm::FailedAndWaitForReboot)
        )
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) enum ScoutFsm {
    Discovery,
    PollingLoop,
    FailedAndWaitForReboot,
}

impl ScoutFsm {
    fn event(self, event: Event) -> (Self, Vec<Action>) {
        match self {
            Self::Discovery => self.fsm_discovery(event),
            Self::PollingLoop => self.fsm_polling_loop(event),
            Self::FailedAndWaitForReboot => (self, vec![]),
        }
    }

    fn fsm_discovery(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::InitialDiscoveryCompleted => (
                Self::PollingLoop,
                vec![
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted),
                    Action::AgentControlRequest(OsImage::Scout),
                ],
            ),
            Event::MachineNotFound => (
                Self::FailedAndWaitForReboot,
                vec![
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted),
                ],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_polling_loop(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::AgentControlCompleted => {
                (self, vec![Action::SetTimer(Timer::ScoutAgentControlPoll)])
            }
            Event::TimerAlert(Timer::ScoutAgentControlPoll) => {
                (self, vec![Action::AgentControlRequest(OsImage::Scout)])
            }
            Event::MachineNotFound => (Self::FailedAndWaitForReboot, vec![]),
            _ => (self, vec![]),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) enum DpuAgentFsm {
    Discovery,
    AgentControl,
    NetworkObservation,
    FailedAndWaitForReboot,
}

impl DpuAgentFsm {
    fn event(self, event: Event) -> (Self, Vec<Action>) {
        match self {
            Self::Discovery => self.fsm_discovery(event),
            Self::AgentControl => self.fsm_agent_control(event),
            Self::NetworkObservation => self.fsm_network_observation(event),
            Self::FailedAndWaitForReboot => (self, vec![]),
        }
    }

    fn fsm_discovery(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::InitialDiscoveryCompleted => (
                Self::AgentControl,
                vec![
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted),
                    Action::AgentControlRequest(OsImage::DpuAgent),
                ],
            ),
            Event::MachineNotFound => (
                Self::FailedAndWaitForReboot,
                vec![
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted),
                ],
            ),
            _ => (self, vec![]),
        }
    }

    fn fsm_agent_control(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::TimerAlert(Timer::DpuAgentControlPoll) => {
                (self, vec![Action::AgentControlRequest(OsImage::DpuAgent)])
            }
            Event::AgentControlCompleted => (
                Self::NetworkObservation,
                vec![Action::DpuAgentNetworkObservation],
            ),
            Event::MachineNotFound => (Self::FailedAndWaitForReboot, vec![]),
            _ => (self, vec![]),
        }
    }

    fn fsm_network_observation(self, event: Event) -> (Self, Vec<Action>) {
        match event {
            Event::NetworkObservationCompleted => (
                Self::AgentControl,
                vec![Action::SetTimer(Timer::DpuAgentControlPoll)],
            ),
            Event::MachineNotFound => (Self::FailedAndWaitForReboot, vec![]),
            _ => (self, vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmc_dhcp_completion_selects_state_and_actions() {
        enum ExpectedState {
            Init,
            MachineDown,
            BmcOnlyMachineUp,
            BmcOnlyMachineDown,
        }

        for (power_on, bmc_only, expected_state, starts_boot) in [
            (true, false, ExpectedState::Init, true),
            (false, false, ExpectedState::MachineDown, false),
            (true, true, ExpectedState::BmcOnlyMachineUp, false),
            (false, true, ExpectedState::BmcOnlyMachineDown, false),
        ] {
            let (fsm, _) = MachineFsm::init(power_on, bmc_only);
            let (fsm, actions) = fsm.event(Event::DhcpComplete);

            assert!(
                match expected_state {
                    ExpectedState::Init => matches!(fsm.state, MachineState::Init { .. }),
                    ExpectedState::MachineDown => matches!(fsm.state, MachineState::MachineDown),
                    ExpectedState::BmcOnlyMachineUp => {
                        matches!(fsm.state, MachineState::BmcOnlyMachineUp)
                    }
                    ExpectedState::BmcOnlyMachineDown => {
                        matches!(fsm.state, MachineState::BmcOnlyMachineDown)
                    }
                },
                "unexpected state for power_on={power_on}, bmc_only={bmc_only}"
            );
            assert!(
                if starts_boot {
                    matches!(
                        actions.as_slice(),
                        [
                            Action::SetupBmc,
                            Action::ConsoleOutputStart,
                            Action::SetTimer(Timer::MachineOn)
                        ]
                    )
                } else {
                    matches!(actions.as_slice(), [Action::SetupBmc])
                },
                "unexpected actions for power_on={power_on}, bmc_only={bmc_only}"
            );
        }
    }

    #[test]
    fn bmc_only_power_on_during_initialization_does_not_start_boot() {
        let (fsm, _) = MachineFsm::init(false, true);
        let (fsm, actions) = fsm.event(Event::PowerOn);

        assert!(matches!(
            fsm.state,
            MachineState::BmcInit {
                power_on: true,
                bmc_only: true,
                ..
            }
        ));
        assert!(actions.is_empty());

        let (fsm, actions) = fsm.event(Event::DhcpComplete);
        assert!(matches!(fsm.state, MachineState::BmcOnlyMachineUp));
        assert!(matches!(actions.as_slice(), [Action::SetupBmc]));
    }

    #[test]
    fn bmc_retry_survives_power_changes() {
        let (fsm, _) = MachineFsm::init(true, false);
        let (fsm, actions) = fsm.event(Event::dhcp_failed_with_jitter(Milliseconds::new(0)));
        assert!(matches!(
            actions.as_slice(),
            [Action::ScheduleDhcpRetry { delay }] if *delay == Duration::from_secs(4)
        ));

        let (fsm, actions) = fsm.event(Event::PowerOff);
        assert!(matches!(actions.as_slice(), [Action::ConsoleOutputStop]));
        let (_, actions) = fsm.event(Event::DhcpRetryExpired);
        assert!(matches!(actions.as_slice(), [Action::Dhcp(DhcpType::Bmc)]));
    }

    #[test]
    fn power_off_abandons_machine_dhcp_retry() {
        let (fsm, _) = MachineFsm::init(true, false);
        let (fsm, _) = fsm.event(Event::DhcpComplete);
        let (fsm, _) = fsm.event(Event::dhcp_failed_with_jitter(Milliseconds::new(0)));

        let (fsm, actions) = fsm.event(Event::PowerOff);
        assert!(matches!(fsm.state, MachineState::MachineDown));
        assert!(matches!(
            actions.as_slice(),
            [
                Action::CancelDhcpRetry,
                Action::ConsoleOutputStop,
                Action::CleanupOnPowerOff
            ]
        ));

        let (_, actions) = fsm.event(Event::DhcpRetryExpired);
        assert!(actions.is_empty());
    }

    #[test]
    fn output_stops_before_boot_completion_and_steady_agent_work() {
        for os_image in [OsImage::Scout, OsImage::DpuAgent] {
            let (fsm, _) = MachineFsm::init(true, false);
            let (fsm, _) = fsm.event(Event::DhcpComplete);
            let (fsm, _) = fsm.event(Event::DhcpComplete);
            let (fsm, actions) = fsm.event(Event::PxeComplete(os_image));
            assert!(matches!(
                actions.as_slice(),
                [Action::InitialDiscoveryRequest(discovery_os)] if *discovery_os == os_image
            ));

            let (_, actions) = fsm.event(Event::InitialDiscoveryCompleted);
            assert!(matches!(
                actions.as_slice(),
                [
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted),
                    Action::AgentControlRequest(agent_os),
                ] if *agent_os == os_image
            ));
        }
    }

    #[test]
    fn disk_boot_stops_output_before_boot_completion() {
        let (fsm, _) = MachineFsm::init(true, false);
        let (fsm, _) = fsm.event(Event::DhcpComplete);
        let (fsm, _) = fsm.event(Event::DhcpComplete);
        let (_, actions) = fsm.event(Event::PxeComplete(OsImage::None));

        assert!(matches!(
            actions.as_slice(),
            [
                Action::ConsoleOutputStop,
                Action::BmcEvent(BmcEvent::BootCompleted)
            ]
        ));
    }

    #[test]
    fn initial_discovery_failure_stops_output_and_completes_boot() {
        for os_fsm in [
            OsFsm::Scout(ScoutFsm::Discovery),
            OsFsm::DpuAgent(DpuAgentFsm::Discovery),
        ] {
            let (os_fsm, actions) = os_fsm.event(Event::MachineNotFound);
            assert!(os_fsm.is_awaiting_reboot());
            // The OS has booted even if discovery cannot find its machine; the BMC must
            // still consume the one-time boot override and record boot completion.
            assert!(matches!(
                actions.as_slice(),
                [
                    Action::ConsoleOutputStop,
                    Action::BmcEvent(BmcEvent::BootCompleted)
                ]
            ));
        }
    }
}
