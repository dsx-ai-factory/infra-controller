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
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bmc_mock::injection::InjectionStore;
use bmc_mock::{
    BmcCommand, BmcEvent, BmcState, Callbacks, HostnameQuerying, MachineInfo, MockPowerState,
    SetSystemPowerError, SetSystemPowerResult, SystemPowerControl,
};
use carbide_network::virtualization::build_dual_stack_list;
use carbide_uuid::machine::{DpuMachineId, InvalidMachineType, MachineId, MachineInterfaceId};
use rpc::forge::{MachineArchitecture, MachineDiscoveryResult, ManagedHostNetworkConfigResponse};
use rpc::forge_agent_control_response::Action;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use uuid::Uuid;

use crate::api_client::{ClientApiError, DpuNetworkStatusArgs, MockDiscoveryData};
use crate::bmc_mock_wrapper::{BmcMockWrapper, BmcMockWrapperHandle};
use crate::config::{MachineATronContext, MachineConfig};
use crate::dhcp_wrapper::{
    DhcpRelayError, DhcpRelayResult, DhcpRequestInfo, DhcpRequester, DhcpResponseInfo,
    DpuDhcpRelay, vendor_class,
};
use crate::lifecycle_timings::{LifecycleTimings, PlatformTimingProfile};
use crate::machine_fsm::{Action as FsmAction, DhcpType, Event, MachineFsm, Timer};
use crate::machine_state_machine::MachineStateError::MissingMachineId;
use crate::machine_utils::{
    PxeBootTarget, PxeError, PxeResponse, forge_agent_control, get_validation_id,
    send_pxe_boot_request,
};
use crate::{Guid, InfinibandPortState, PersistedDevice, PersistedDpuMachine};

type DpuDhcpRelayHandle = oneshot::Sender<()>;

/// Abandon queued work for the current boot when the machine powers off or
/// cycles. A retrying in-band action would otherwise block the power-change
/// cleanup and timer queued behind it.
///
/// BMC initialization remains valid because the BMC stays powered independently
/// of the machine. Preserve an existing power-off cleanup in case another power
/// change arrives before that action runs.
fn abandon_machine_actions_on_power_change(
    actions: &mut VecDeque<FsmAction>,
    preserve_dhcp_retry: bool,
) {
    actions.retain(|action| match action {
        FsmAction::SetupBmc | FsmAction::Dhcp(DhcpType::Bmc) | FsmAction::CleanupOnPowerOff => true,
        FsmAction::ScheduleDhcpRetry { .. } | FsmAction::CancelDhcpRetry => preserve_dhcp_retry,
        FsmAction::SetTimer(
            Timer::PowerCycle
            | Timer::MachineOn
            | Timer::ScoutAgentControlPoll
            | Timer::DpuAgentControlPoll,
        )
        | FsmAction::Dhcp(DhcpType::Machine)
        | FsmAction::PxeBootRequest
        | FsmAction::InitialDiscoveryRequest(_)
        | FsmAction::AgentControlRequest(_)
        | FsmAction::DpuAgentNetworkObservation
        | FsmAction::BmcEvent(BmcEvent::PowerOn | BmcEvent::BootCompleted) => false,
    });
}

fn direct_dhcp_relay_address(
    is_host: bool,
    underlay_relay_address: Ipv4Addr,
    host_inband_relay_address: Option<Ipv4Addr>,
) -> Ipv4Addr {
    if is_host {
        host_inband_relay_address.unwrap_or(underlay_relay_address)
    } else {
        underlay_relay_address
    }
}

fn resolve_pxe_boot(
    pxe_response: PxeResponse,
    installed_os: OsImage,
) -> Result<(OsImage, Option<MachineInterfaceId>), MachineStateError> {
    let os_image = match pxe_response.boot_target {
        PxeBootTarget::Exit => installed_os,
        PxeBootTarget::Scout => OsImage::Scout,
        PxeBootTarget::DpuAgent => OsImage::DpuAgent,
    };

    match pxe_response.machine_interface_id {
        None if os_image != OsImage::None => Err(MachineStateError::MissingInterfaceId),
        machine_interface_id => Ok((os_image, machine_interface_id)),
    }
}

/// MachineStateMachine (yo dawg) models the state machine of a machine endpoint
///
/// This code is in common between DPUs and Hosts.(ie. anything that has a BMC, boots via DHCP, can
/// receive PXE instructions, etc.)
pub(super) struct MachineStateMachine {
    pub(super) live_state: Arc<RwLock<LiveState>>,
    pub(super) mat_host_id: Uuid,
    pub(super) installed_os: OsImage,

    fsm: MachineFsm,
    bmc_mock: Option<Arc<BmcMockWrapperHandle>>,
    bmc_state: Option<BmcState>,
    bmc_injection: Arc<InjectionStore>,
    power_cycle_deadline: Option<Instant>,
    machine_on_deadline: Option<Instant>,
    agent_polling_deadline: Option<(Instant, Timer)>,
    bmc_dhcp_info: Option<DhcpResponseInfo>,
    machine_dhcp_info: Option<DhcpResponseInfo>,
    machine_interface_id: Option<MachineInterfaceId>,
    dhcp_retry_deadline: Option<Instant>,
    machine_discovery_result: Option<MachineDiscoveryResult>,

    actions: VecDeque<FsmAction>,
    machine_info: MachineInfo,
    bmc_command_channel: mpsc::UnboundedSender<BmcCommand>,
    config: Arc<MachineConfig>,
    resolved_timings: LifecycleTimings,
    app_context: Arc<MachineATronContext>,
    dpu_dhcp_relay: Option<DpuDhcpRelay>,
    dpu_dhcp_relay_handle: Option<DpuDhcpRelayHandle>,
}

#[derive(Debug, Clone)]
struct LiveStateCallbacks {
    state: Arc<RwLock<LiveState>>,
    command_channel: mpsc::UnboundedSender<BmcCommand>,
}

impl LiveStateCallbacks {
    fn new(
        state: Arc<RwLock<LiveState>>,
        command_channel: mpsc::UnboundedSender<BmcCommand>,
    ) -> Self {
        Self {
            state,
            command_channel,
        }
    }
}

impl Callbacks for LiveStateCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        self.state.read().unwrap().power_state
    }

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        self.command_channel
            .send(BmcCommand::SetSystemPower {
                request: reset_type,
                reply: None,
            })
            .map_err(|err| SetSystemPowerError::CommandSendError(err.to_string()))
    }

    fn state_refresh_indication(&self) {
        let _ = self
            .command_channel
            .send(BmcCommand::StateRefreshIndication);
    }
}

#[derive(Debug, Clone)]
struct LiveStateHostnameQuery(Arc<RwLock<LiveState>>);

impl HostnameQuerying for LiveStateHostnameQuery {
    fn get_hostname(&'_ self) -> Cow<'_, str> {
        self.0
            .read()
            .unwrap()
            .observed_machine_id
            .as_ref()
            .map(|id| Cow::Owned(id.to_string()))
            .unwrap_or(Cow::Borrowed("localhost"))
    }
}

/// Represents state which changes over time with this machine. This is kept in an `Arc<RwLock>` so
/// that callers can query it at any time. It is updated after every state transition.
#[derive(Debug)]
pub(super) struct LiveState {
    pub(super) is_up: bool,
    pub(super) power_state: MockPowerState, // reflects the "desired" power state of the machine. Affects whether next_state will boot the machine or not.
    pub(super) observed_machine_id: Option<MachineId>,
    pub(super) machine_ip: Option<Ipv4Addr>,
    pub(super) bmc_ip: Option<Ipv4Addr>,
    pub(super) ipmi_port: Option<u16>,
    pub(super) ssh_endpoint_port: Option<u16>,
    pub(super) booted_os: MaybeOsImage,
    pub(super) installed_os: OsImage,
    pub(super) state_string: Option<&'static str>,
    pub(super) api_state: String,
    pub(super) tpm_ek_certificate: Option<Vec<u8>>,
    pub(super) ssh_host_key: Option<String>,
    pub(super) infiniband_port_states: HashMap<Guid, InfinibandPortState>,
    /// For a DPU machine, whether its BlueField has flipped to NIC mode. Lets the
    /// owning host observe the flip through the DPU handle and converge (detach
    /// its DPU DHCP relay). Always false for a host machine.
    pub(super) dpu_flipped_to_nic_mode: bool,
    /// Current host firmware inventory, updated on each power-on after staged
    /// firmware is applied.  Used by `persisted()` so restarts resume from the
    /// last observed versions rather than the operator-configured starting point.
    pub(super) active_host_firmware: Option<bmc_mock::HostFirmwareVersions>,
}

impl Default for LiveState {
    fn default() -> Self {
        let power_state = MockPowerState::default();
        LiveState {
            is_up: matches!(power_state, MockPowerState::On),
            power_state: MockPowerState::default(),
            observed_machine_id: None,
            machine_ip: None,
            bmc_ip: None,
            ipmi_port: None,
            ssh_endpoint_port: None,
            booted_os: Default::default(),
            installed_os: Default::default(),
            state_string: None,
            api_state: "Unknown".to_string(),
            tpm_ek_certificate: None,
            ssh_host_key: None,
            infiniband_port_states: HashMap::new(),
            dpu_flipped_to_nic_mode: false,
            active_host_firmware: None,
        }
    }
}

impl LiveState {
    fn for_machine(
        machine_info: &MachineInfo,
        power_state: MockPowerState,
        tpm_ek_certificate: Option<Vec<u8>>,
    ) -> Self {
        let infiniband_port_states = match machine_info {
            MachineInfo::Host(host) => host
                .infiniband_port_guids()
                .into_iter()
                .map(|guid| {
                    let bytes: [u8; 8] = guid.into();
                    (Guid::from(bytes), InfinibandPortState::Active)
                })
                .collect(),
            MachineInfo::Dpu(_) => HashMap::new(),
        };
        Self {
            power_state,
            tpm_ek_certificate,
            infiniband_port_states,
            ..Default::default()
        }
    }
}

pub(super) enum PersistedMachine {
    Host(PersistedDevice),
    Dpu(PersistedDpuMachine),
}

impl MachineStateMachine {
    /// Resolve the per-role [`LifecycleTimings`] for this machine from its config.
    ///
    /// Selects host or DPU timings from the [`PlatformTimingProfile`] based on
    /// `machine_info`. Called once at construction time; the result is stored on
    /// the state machine so timer arms read a single field rather than branching
    /// on `machine_info` and reaching into the config.
    fn resolve_timings(machine_info: &MachineInfo, config: &MachineConfig) -> LifecycleTimings {
        let profile = PlatformTimingProfile::for_hardware_type(&config.hw_type);
        let base = match machine_info {
            MachineInfo::Dpu(_) => profile.dpu,
            MachineInfo::Host(_) => profile.host,
        };
        let overrides_for_role = config
            .timing_overrides
            .as_ref()
            .map(|o| match machine_info {
                MachineInfo::Dpu(_) => &o.dpu,
                MachineInfo::Host(_) => &o.host,
            });
        let after_overrides = match overrides_for_role {
            Some(o) => base.with_overrides(o),
            None => base,
        };
        let resolved = after_overrides.scale(config.acceleration_factor);
        tracing::info!(
            hw_type = ?config.hw_type,
            role = match machine_info {
                MachineInfo::Dpu(_) => "dpu",
                MachineInfo::Host(_) => "host",
            },
            acceleration_factor = config.acceleration_factor,
            has_overrides = overrides_for_role.is_some(),
            reboot = ?resolved.reboot,
            power_on_os_ready = ?resolved.power_on_os_ready,
            power_off_force = ?resolved.power_off_force,
            bmc_reset = ?resolved.bmc_reset,
            "Resolved lifecycle timings"
        );
        resolved
    }

    pub(super) fn from_persisted(
        persisted_machine: PersistedMachine,
        machine_info: MachineInfo,
        config: Arc<MachineConfig>,
        app_context: Arc<MachineATronContext>,
        bmc_command_channel: mpsc::UnboundedSender<BmcCommand>,
        dpu_dhcp_relay: Option<DpuDhcpRelay>,
        mat_host_id: Uuid,
    ) -> MachineStateMachine {
        let (initial_os_image, tpm_ek_certificate) = match persisted_machine {
            PersistedMachine::Host(h) => (h.installed_os, h.tpm_ek_certificate),
            PersistedMachine::Dpu(d) => (d.installed_os, None),
        };
        let (fsm, actions) = MachineFsm::init(true, Self::is_bmc_only(&machine_info, &config));
        let resolved_timings = Self::resolve_timings(&machine_info, &config);
        MachineStateMachine {
            fsm,
            actions: actions.into_iter().collect(),
            bmc_mock: None,
            bmc_state: None,
            bmc_injection: Arc::new(InjectionStore::new()),
            power_cycle_deadline: None,
            machine_on_deadline: None,
            agent_polling_deadline: None,
            bmc_dhcp_info: None,
            machine_dhcp_info: None,
            machine_interface_id: None,
            dhcp_retry_deadline: None,
            machine_discovery_result: None,
            installed_os: initial_os_image,
            live_state: Arc::new(RwLock::new(LiveState::for_machine(
                &machine_info,
                MockPowerState::On,
                tpm_ek_certificate,
            ))),
            machine_info,
            bmc_command_channel,
            config,
            resolved_timings,
            app_context,
            dpu_dhcp_relay,
            dpu_dhcp_relay_handle: None,
            mat_host_id,
        }
    }

    pub(super) fn new(
        machine_info: MachineInfo,
        config: Arc<MachineConfig>,
        app_context: Arc<MachineATronContext>,
        bmc_command_channel: mpsc::UnboundedSender<BmcCommand>,
        tpm_ek_certificate: Option<Vec<u8>>,
        dpu_dhcp_relay: Option<DpuDhcpRelay>,
        mat_host_id: Uuid,
    ) -> MachineStateMachine {
        let (fsm, actions) = MachineFsm::init(false, Self::is_bmc_only(&machine_info, &config));
        let resolved_timings = Self::resolve_timings(&machine_info, &config);
        MachineStateMachine {
            live_state: Arc::new(RwLock::new(LiveState::for_machine(
                &machine_info,
                MockPowerState::Off,
                tpm_ek_certificate,
            ))),
            fsm,
            actions: actions.into_iter().collect(),
            bmc_dhcp_info: None,
            bmc_mock: None,
            bmc_state: None,
            bmc_injection: Arc::new(InjectionStore::new()),
            machine_dhcp_info: None,
            machine_interface_id: None,
            dhcp_retry_deadline: None,
            machine_discovery_result: None,
            machine_on_deadline: None,
            agent_polling_deadline: None,
            power_cycle_deadline: None,
            installed_os: OsImage::default(),
            machine_info,
            bmc_command_channel,
            config,
            resolved_timings,
            app_context,
            dpu_dhcp_relay,
            dpu_dhcp_relay_handle: None,
            mat_host_id,
        }
    }

    pub(super) async fn advance(&mut self) -> Duration {
        if let Some(duration) = self.process_actions().await {
            duration
        } else {
            let now = Instant::now();
            if let Some(power_cycle_deadline) = self.power_cycle_deadline
                && now > power_cycle_deadline
            {
                self.power_cycle_deadline = None;
                self.fsm_event(Event::TimerAlert(Timer::PowerCycle));
            }
            if let Some(machine_on_deadline) = self.machine_on_deadline
                && now > machine_on_deadline
            {
                self.machine_on_deadline = None;
                self.fsm_event(Event::TimerAlert(Timer::MachineOn));
            }
            if let Some((agent_polling_deadline, timer)) = self.agent_polling_deadline
                && now > agent_polling_deadline
            {
                self.agent_polling_deadline = None;
                self.fsm_event(Event::TimerAlert(timer));
            }
            if let Some(dhcp_retry_deadline) = self.dhcp_retry_deadline
                && now > dhcp_retry_deadline
            {
                self.dhcp_retry_deadline = None;
                self.fsm_event(Event::DhcpRetryExpired);
            }

            if let Some(duration) = self.process_actions().await {
                duration
            } else {
                [
                    self.machine_on_deadline,
                    self.power_cycle_deadline,
                    self.agent_polling_deadline.map(|v| v.0),
                    self.dhcp_retry_deadline,
                ]
                .iter()
                .flatten()
                .min()
                .map(|nearest| nearest.saturating_duration_since(now))
                .unwrap_or(self.config.run_interval_idle)
            }
        }
    }

    async fn process_actions(&mut self) -> Option<Duration> {
        while let Some(action) = self.actions.front() {
            self.update_live_state();
            match action {
                FsmAction::SetupBmc => match self.setup_bmc().await {
                    Ok((bmc_mock, bmc_state)) => {
                        self.bmc_mock = bmc_mock;
                        self.bmc_state = Some(bmc_state);
                        self.actions.pop_front();
                    }
                    Err(_) => return Some(self.config.run_interval_working),
                },
                FsmAction::SetTimer(Timer::PowerCycle) => {
                    tracing::info!(
                        duration = ?self.resolved_timings.power_off_force,
                        "Timer armed: PowerCycle (power_off_force)"
                    );
                    self.power_cycle_deadline =
                        Some(Instant::now() + self.resolved_timings.power_off_force);
                    self.actions.pop_front();
                }
                FsmAction::SetTimer(Timer::MachineOn) => {
                    tracing::info!(
                        duration = ?self.resolved_timings.reboot,
                        "Timer armed: MachineOn (reboot)"
                    );
                    self.machine_on_deadline = Some(Instant::now() + self.resolved_timings.reboot);
                    self.actions.pop_front();
                }
                FsmAction::SetTimer(Timer::ScoutAgentControlPoll) => {
                    self.agent_polling_deadline = Some((
                        Instant::now() + self.config.scout_run_interval,
                        Timer::ScoutAgentControlPoll,
                    ));
                    self.actions.pop_front();
                }
                FsmAction::SetTimer(Timer::DpuAgentControlPoll) => {
                    self.agent_polling_deadline = Some((
                        Instant::now() + self.config.network_status_run_interval,
                        Timer::DpuAgentControlPoll,
                    ));
                    self.actions.pop_front();
                }
                FsmAction::Dhcp(DhcpType::Bmc) => match self.bmc_dhcp_discovery().await {
                    Ok(bmc_dhcp_info) => {
                        self.bmc_dhcp_info = Some(bmc_dhcp_info);
                        self.actions.pop_front();
                        self.fsm_event(Event::DhcpComplete)
                    }
                    Err(_) => {
                        self.actions.pop_front();
                        self.fsm_event(Event::dhcp_failed());
                    }
                },
                FsmAction::Dhcp(DhcpType::Machine) => match self.machine_dhcp_discovery().await {
                    Ok(machine_dhcp_info) => {
                        self.machine_interface_id = None;
                        self.machine_dhcp_info = Some(machine_dhcp_info);
                        self.actions.pop_front();
                        self.fsm_event(Event::DhcpComplete)
                    }
                    Err(_) => {
                        self.actions.pop_front();
                        self.fsm_event(Event::dhcp_failed());
                    }
                },
                FsmAction::ScheduleDhcpRetry { delay } => {
                    tracing::debug!(
                        retry_delay_milliseconds = delay.as_millis(),
                        "scheduled DHCP retry"
                    );
                    self.dhcp_retry_deadline = Some(Instant::now() + *delay);
                    self.actions.pop_front();
                }
                FsmAction::CancelDhcpRetry => {
                    self.dhcp_retry_deadline = None;
                    self.actions.pop_front();
                }
                FsmAction::PxeBootRequest => match self.pxe_boot_request().await {
                    Ok((os_image, machine_interface_id)) => {
                        self.machine_interface_id = machine_interface_id;
                        // A netbooted DPU-agent image is this simulation's stand-in
                        // for the DPF BFB install writing the OS to the DPU's disk,
                        // so record it as installed at boot. NICo's PXE serves a DPU
                        // EXIT in every DPUInit sub-state after Init; without a disk
                        // OS to fall back on, any mid-walk reboot (BIOS setup, the
                        // DPF reboot handshake, an external power-cycle) strands the
                        // DPU OS-less and dpuinit never completes. Waiting for
                        // initial discovery to succeed (which also sets this) is not
                        // enough: at cold start discovery can fail before the record
                        // exists, and the recovery reboot then EXITs into nothing.
                        if matches!(self.machine_info, MachineInfo::Dpu(_))
                            && matches!(os_image, OsImage::DpuAgent)
                        {
                            self.installed_os = OsImage::DpuAgent;
                        }
                        self.actions.pop_front();
                        self.fsm_event(Event::PxeComplete(os_image))
                    }
                    Err(_) => return Some(self.config.run_interval_working),
                },
                FsmAction::BmcEvent(event) => {
                    if let Some(bmc_state) = &self.bmc_state {
                        bmc_state.on_event(event)
                    }
                    self.actions.pop_front();
                    // A BlueField that just applied a staged NIC-mode flip on this
                    // power-on is now a plain NIC, not a managed DPU. Converge to the
                    // dormant BMC-only track: drop the queued boot actions and stop,
                    // so it no longer PXE-boots or is re-discovered as a DPU, matching
                    // a host configured `dpus_in_nic_mode` from the start.
                    let flipped_to_nic = matches!(&self.machine_info, MachineInfo::Dpu(_))
                        && self
                            .bmc_state
                            .as_ref()
                            .and_then(|bmc_state| bmc_state.bluefield_nic_mode())
                            .unwrap_or(false);
                    let already_dormant = self.fsm.is_bmc_only();
                    if flipped_to_nic && !already_dormant {
                        // Stop the converged DPU completely: drop queued boot actions
                        // and the pending timers, so `advance()` can't re-enqueue work
                        // after the FSM converges to the dormant BMC-only track.
                        self.actions.clear();
                        self.machine_on_deadline = None;
                        self.power_cycle_deadline = None;
                        self.agent_polling_deadline = None;
                        // Let the FSM own the transition: it is returned by `event()`,
                        // not assigned here.
                        self.fsm_event(Event::DpuFlippedToNicMode);
                    }
                }
                FsmAction::InitialDiscoveryRequest(os_image) => {
                    match self.initial_discovery_request(*os_image).await {
                        Ok(None) => {
                            self.actions.pop_front();
                            self.fsm_event(Event::MachineNotFound)
                        }
                        Ok(Some(machine_discovery_result)) => {
                            self.installed_os = *os_image;
                            self.machine_discovery_result = Some(machine_discovery_result);
                            self.actions.pop_front();
                            self.fsm_event(Event::InitialDiscoveryCompleted)
                        }
                        Err(_) => return Some(self.config.discovery_retry_interval),
                    }
                }
                FsmAction::AgentControlRequest(os_image) => {
                    match self.agent_control_request(*os_image).await {
                        Ok(_) => {
                            self.actions.pop_front();
                            self.fsm_event(Event::AgentControlCompleted)
                        }
                        Err(MachineStateError::MachineNotFound(machine_id)) => {
                            tracing::warn!(%machine_id, "Machine not found during agent control, likely force deleted");
                            self.actions.pop_front();
                            self.fsm_event(Event::MachineNotFound)
                        }
                        Err(_) => return Some(self.config.run_interval_working),
                    }
                }
                FsmAction::DpuAgentNetworkObservation => {
                    match self.dpu_agent_network_observation().await {
                        Ok(maybe_dhcp_relay_handle) => {
                            if let Some(dhcp_relay_handle) = maybe_dhcp_relay_handle {
                                self.dpu_dhcp_relay_handle = Some(dhcp_relay_handle);
                            }
                            self.actions.pop_front();
                            self.fsm_event(Event::NetworkObservationCompleted)
                        }
                        Err(MachineStateError::MachineNotFound(machine_id)) => {
                            tracing::warn!(%machine_id, "Machine not found during network observation, likely force deleted");
                            self.actions.pop_front();
                            self.fsm_event(Event::MachineNotFound)
                        }
                        Err(_) => return Some(self.config.run_interval_working),
                    }
                }
                FsmAction::CleanupOnPowerOff => {
                    self.actions.pop_front();
                    self.machine_interface_id = None;
                    self.machine_discovery_result = None;
                    self.dpu_dhcp_relay_handle = None;
                }
            }
        }
        self.update_live_state();
        None
    }

    fn fsm_event(&mut self, event: Event) {
        if matches!(event, Event::PowerCycle | Event::PowerOff) {
            abandon_machine_actions_on_power_change(
                &mut self.actions,
                self.fsm.is_bmc_initializing(),
            );

            self.machine_on_deadline = None;
            self.power_cycle_deadline = None;
            self.agent_polling_deadline = None;
        }

        let old_state = self.fsm;
        let (new_state, actions) = self.fsm.event(event);
        tracing::info!(previous_state = ?old_state, ?event, next_state = ?new_state, ?actions, "machine FSM step");
        actions
            .into_iter()
            .for_each(|action| self.actions.push_back(action));
        self.fsm = new_state;
    }

    async fn setup_bmc(
        &self,
    ) -> Result<(Option<Arc<BmcMockWrapperHandle>>, BmcState), MachineStateError> {
        let Some(dhcp_info) = &self.bmc_dhcp_info else {
            return Err(MachineStateError::NoBmcDhcpInfo);
        };
        self.run_bmc_mock(dhcp_info.ip_address).await
    }

    async fn bmc_dhcp_discovery(&self) -> DhcpRelayResult<DhcpResponseInfo> {
        let start = Instant::now();
        self.app_context
            .dhcp_client
            .request_ip(DhcpRequestInfo {
                mac_address: self.machine_info.bmc_mac_address(),
                relay_address: self.config.bmc_dhcp_relay_address,
                vendor_class: vendor_class(&self.machine_info, DhcpRequester::Bmc),
            })
            .await
            .inspect(|_| {
                tracing::debug!(
                    bmc_mac_address = %self.machine_info.bmc_mac_address(),
                    elapsed_milliseconds = start.elapsed().as_millis(),
                    "BMC DHCP request completed",
                );
            })
            .inspect_err(|err| {
                tracing::warn!(
                    elapsed_milliseconds = start.elapsed().as_millis(),
                    error = %err,
                    "BMC DHCP request failed",
                );
            })
    }

    async fn machine_dhcp_discovery(&self) -> Result<DhcpResponseInfo, MachineStateError> {
        let Some(primary_mac) = self.machine_info.dhcp_mac_addresses().first().copied() else {
            return Err(MachineStateError::NoMachineMacAddress);
        };

        let start = Instant::now();
        // Bound the relay wait so an unavailable DPU cannot block the host actor
        // indefinitely. Returning to the actor lets it confirm a NIC-mode flip
        // before detaching the relay, or retry the managed relay otherwise.
        const DPU_DHCP_RELAY_TIMEOUT: Duration = Duration::from_secs(10);
        let machine_dhcp_info_result = if let Some(DpuDhcpRelay::HostEnd(relay_tx)) =
            &self.dpu_dhcp_relay
        {
            tracing::debug!(primary_mac_address = %primary_mac, "requesting machine DHCP through DPU relay");
            let (reply_tx, reply_rx) = oneshot::channel();
            if relay_tx.send(reply_tx).is_err() {
                tracing::warn!(
                    primary_mac_address = %primary_mac,
                    "DPU DHCP relay request channel is closed; retrying after relay state reconciliation"
                );
                return Err(MachineStateError::DpuDhcpRelayUnavailable);
            }
            match tokio::time::timeout(DPU_DHCP_RELAY_TIMEOUT, reply_rx).await {
                // The relay answered. Preserve either its successful response or
                // the actual DHCP/API error it returned.
                Ok(Ok(result)) => result,
                Ok(Err(_)) => {
                    tracing::warn!(
                        primary_mac_address = %primary_mac,
                        "DPU DHCP relay response was canceled; retrying after relay state reconciliation"
                    );
                    return Err(MachineStateError::DpuDhcpRelayUnavailable);
                }
                Err(_) => {
                    tracing::warn!(
                        primary_mac_address = %primary_mac,
                        timeout_milliseconds = DPU_DHCP_RELAY_TIMEOUT.as_millis(),
                        "DPU DHCP relay response timed out; retrying after relay state reconciliation"
                    );
                    return Err(MachineStateError::DpuDhcpRelayUnavailable);
                }
            }
        } else {
            let direct_relay_address = direct_dhcp_relay_address(
                matches!(&self.machine_info, MachineInfo::Host(_)),
                self.config.underlay_dhcp_relay_address,
                self.config.host_inband_dhcp_relay_address,
            );
            tracing::debug!(
                primary_mac_address = %primary_mac,
                %direct_relay_address,
                "requesting machine DHCP directly"
            );
            self.app_context
                .dhcp_client
                .request_ip(DhcpRequestInfo {
                    mac_address: primary_mac,
                    relay_address: direct_relay_address,
                    vendor_class: vendor_class(&self.machine_info, DhcpRequester::System),
                })
                .await
        };
        machine_dhcp_info_result
            .inspect(|_| {
                tracing::debug!(
                    primary_mac_address = %primary_mac,
                    elapsed_milliseconds = start.elapsed().as_millis(),
                    "machine DHCP request completed"
                );
            })
            .map_err(|err| {
                tracing::debug!(
                    primary_mac_address = %primary_mac,
                    elapsed_milliseconds = start.elapsed().as_millis(),
                    error = %err,
                    "machine DHCP request failed"
                );
                err.into()
            })
    }

    async fn pxe_boot_request(
        &self,
    ) -> Result<(OsImage, Option<MachineInterfaceId>), MachineStateError> {
        let Some(dhcp_info) = self.machine_dhcp_info.as_ref() else {
            return Err(MachineStateError::NoMachineDhcpInfo);
        };

        let (architecture, product) = match self.machine_info {
            MachineInfo::Dpu(_) => (
                MachineArchitecture::Arm,
                "Machine-A-Tron Bluefield".to_string(),
            ),
            MachineInfo::Host(_) => (
                MachineArchitecture::X86,
                "Machine-A-Tron X86 Host".to_string(),
            ),
        };

        let pxe_response = send_pxe_boot_request(
            &self.app_context,
            architecture,
            dhcp_info.ip_address.into(),
            Some(product),
        )
        .await?;

        let (os, machine_interface_id) = resolve_pxe_boot(pxe_response, self.installed_os)?;

        match (&self.machine_info, os) {
            (MachineInfo::Host(_), OsImage::DpuAgent) => Err(MachineStateError::WrongOsForMachine(
                "ERROR: Running DpuAgent OS on a host machine, this should not happen.".to_string(),
            )),
            (MachineInfo::Dpu(_), OsImage::Scout) => {
                tracing::warn!("ERROR: Running Scout OS on a DPU machine, this should not happen.");
                Err(MachineStateError::WrongOsForMachine(
                    "ERROR: Running Scout OS on a DPU machine, this should not happen.".to_string(),
                ))
            }
            _ => Ok((os, machine_interface_id)),
        }
    }

    async fn initial_discovery_request(
        &self,
        os_image: OsImage,
    ) -> Result<Option<MachineDiscoveryResult>, MachineStateError> {
        if self.machine_dhcp_info.is_none() {
            return Err(MachineStateError::NoMachineDhcpInfo);
        }
        // No machine_discovery_result means we just booted. Run discovery now.
        tracing::trace!("Running initial discovery after boot");
        match self.run_machine_discovery().await {
            Ok(result) => {
                if os_image == OsImage::Scout {
                    let machine_id = result.machine_id.as_ref().ok_or(MissingMachineId)?;
                    // Inform the API that we have finished our reboot (ie. scout is now running)
                    self.app_context
                        .forge_api_client
                        .reboot_completed(*machine_id)
                        .await?;
                }
                Ok(Some(result))
            }
            Err(MachineStateError::ClientApi(ClientApiError::InvocationError(status))) => {
                match status.code() {
                    tonic::Code::InvalidArgument => {
                        // Not ingested yet: at MAT cold start the machines power on
                        // and PXE the agent image BEFORE site-explorer has created
                        // their machine records (on real hardware NICo powers hosts
                        // on only after creation, so this window does not exist).
                        // Treat it as retryable so the queued discovery action runs
                        // again next iteration; giving up here (MachineNotFound)
                        // permanently loses the installed OS — later reboots PXE
                        // EXIT and boot OsImage::None, deadlocking dpuinit.
                        tracing::warn!(error=%status, "Machine not ingested yet; retrying discovery until site-explorer creates it.");
                        Err(MachineStateError::ClientApi(
                            ClientApiError::InvocationError(status),
                        ))
                    }
                    tonic::Code::NotFound => {
                        tracing::warn!(error=%status, "Machine not found in discovery, likely force deleted.");
                        Ok(None)
                    }
                    _ => Err(MachineStateError::ClientApi(
                        ClientApiError::InvocationError(status),
                    )),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn agent_control_request(&self, os_image: OsImage) -> Result<(), MachineStateError> {
        let machine_id = self
            .machine_discovery_result
            .as_ref()
            .and_then(|result| result.machine_id)
            .ok_or(MissingMachineId)?;

        // Ask the API server what to do next
        let start = Instant::now();
        let Some(control_response) = forge_agent_control(&self.app_context, machine_id).await
        else {
            return Err(MachineStateError::MachineNotFound(machine_id));
        };
        tracing::trace!(
            elapsed_milliseconds = start.elapsed().as_millis(),
            action = ?control_response.action,
            "forge_agent_control action received",
        );

        match &control_response.action {
            Some(Action::Discovery(_)) => self.send_discovery_complete(&machine_id).await?,
            Some(Action::MachineValidation(_)) if os_image == OsImage::Scout => {
                if let Some(validation_id) = get_validation_id(&control_response) {
                    self.app_context
                        .api_client()
                        .machine_validation_complete(&machine_id, &validation_id)
                        .await?;
                }
            }
            Some(Action::Reset(_)) if os_image == OsImage::Scout => {
                tracing::debug!("Got Reset action in scout image, sending cleanup_complete");
                // Wait a bit before confirming the cleanup in order to mimic real
                // cleanup and give the tests a higher chance to observe teh cleanup state
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                self.app_context
                    .api_client()
                    .cleanup_complete(&machine_id)
                    .await?;
            }
            Some(Action::Noop(_)) => {}
            _ => {
                tracing::warn!(
                    action = ?control_response.action,
                    os_image = %os_image,
                    "Unknown forge_agent_control action for OS image",
                );
            }
        }
        Ok(())
    }
    async fn dpu_agent_network_observation(
        &self,
    ) -> Result<Option<DpuDhcpRelayHandle>, MachineStateError> {
        let machine_id = self
            .machine_discovery_result
            .as_ref()
            .and_then(|result| result.machine_id)
            .ok_or(MissingMachineId)?;
        let network_config = match self
            .app_context
            .forge_api_client
            .get_managed_host_network_config(DpuMachineId::try_from(machine_id)?)
            .await
        {
            Ok(config) => config,
            Err(status) if status.code() == tonic::Code::NotFound => {
                return Err(MachineStateError::MachineNotFound(machine_id));
            }
            Err(status) => return Err(status.into()),
        };

        // DPUs send network status periodically
        self.send_network_status_observation(machine_id.to_owned(), &network_config)
            .await?;

        // Re-spawn the host-facing DHCP relay with the freshly-fetched
        // network_config. We do this on every network observation (not
        // just the first one) because the relay captures the config it
        // was spawned with -- if we never re-spawn, the relay keeps
        // returning stale tenant IPs after an instance is released and
        // managed_host_network_config has switched back to admin-only.
        // The caller assigns the returned handle into
        // self.dpu_dhcp_relay_handle, which drops the prior handle and
        // signals the old relay task to exit.
        if let Some(DpuDhcpRelay::DpuEnd(dhcp_relay)) = self.dpu_dhcp_relay.clone() {
            Ok(Some(dhcp_relay.spawn(network_config.clone())))
        } else {
            Ok(None)
        }
    }

    pub(super) fn update_live_state(&self) {
        let mut live_state = self.live_state.write().unwrap();
        live_state.is_up = self.fsm.is_up();
        live_state.machine_ip = self.machine_ip();
        live_state.bmc_ip = self.bmc_ip();
        live_state.ipmi_port = self
            .bmc_mock
            .as_ref()
            .and_then(|bmc_mock| bmc_mock.ipmi_port());
        live_state.ssh_endpoint_port = self
            .bmc_mock
            .as_ref()
            .and_then(|bmc_mock| bmc_mock.ssh_endpoint_port());
        live_state.installed_os = self.installed_os;
        if let Some(machine_id) = self.machine_id()
            && live_state.observed_machine_id != Some(machine_id)
        {
            live_state.observed_machine_id = Some(machine_id)
        }
        live_state.state_string = Some(self.fsm.state_string());
        live_state.power_state = self.fsm.power_state();
        live_state.booted_os = self.booted_os();
        live_state.dpu_flipped_to_nic_mode = matches!(&self.machine_info, MachineInfo::Dpu(_))
            && self
                .bmc_state
                .as_ref()
                .and_then(|state| state.bluefield_nic_mode())
                .unwrap_or(false);
        live_state.active_host_firmware = self.current_host_firmware();
    }

    /// Whether this machine still relays its data-plane DHCP through a managed
    /// DPU. False once the relay is detached (see `detach_dpu_dhcp_relay`) or for
    /// a host that never had a managed DPU.
    pub(super) fn has_dpu_dhcp_relay(&self) -> bool {
        self.dpu_dhcp_relay.is_some()
    }

    /// Return the active host firmware versions from the live BMC mock inventory.
    /// Returns `None` when the BMC mock has not started yet, or when this
    /// platform has no host firmware simulation (inventory IDs are `None`).
    pub(super) fn current_host_firmware(&self) -> Option<bmc_mock::HostFirmwareVersions> {
        let bmc_state = self.bmc_state.as_ref()?;
        // host_bmc_inventory_id is None for platforms without host firmware simulation
        // (switches, power shelves, Dell R760+BF4, etc.).  Return None early so
        // live_state.active_host_firmware stays None for those machines.
        let bmc_id = bmc_state
            .update_service_state
            .host_bmc_inventory_id
            .as_deref()?;
        let uefi_id = bmc_state
            .update_service_state
            .host_uefi_inventory_id
            .as_deref();
        let bmc = bmc_state
            .update_service_state
            .find_firmware_inventory(bmc_id)
            .and_then(|v| v["Version"].as_str().map(str::to_owned));
        let uefi = uefi_id.and_then(|id| {
            bmc_state
                .update_service_state
                .find_firmware_inventory(id)
                .and_then(|v| v["Version"].as_str().map(str::to_owned))
        });
        if bmc.is_some() || uefi.is_some() {
            Some(bmc_mock::HostFirmwareVersions { bmc, uefi })
        } else {
            None
        }
    }

    /// Stop relaying data-plane DHCP through the DPU. Once a DPU flips to NIC
    /// mode it is a plain NIC, so the host DHCPs directly on its own (former-DPU
    /// host) MAC -- the same MAC, so a retained boot interface still matches on
    /// re-ingestion. Idempotent.
    pub(super) fn detach_dpu_dhcp_relay(&mut self) {
        self.dpu_dhcp_relay = None;
        self.dpu_dhcp_relay_handle = None;
    }

    /// Drop this host's managed DPUs from its reported inventory once they have
    /// flipped to NIC mode, so the host BMC enumerates zero managed DPUs and the
    /// host re-ingests (and rediscovers) as a zero-DPU NIC-mode machine -- the
    /// PCIe/identity signal a real flipped host would present. The flipped DPU's
    /// host-facing MAC becomes the host's own plain-NIC MAC, so the host keeps
    /// DHCPing on the same MAC and a retained boot interface still matches.
    /// No-op for a non-host machine or a host that already has no DPUs.
    pub(super) fn drop_managed_dpus(&mut self) {
        if let MachineInfo::Host(host) = &mut self.machine_info {
            if host.dpus.is_empty() {
                return;
            }
            // `non_dpu_mac_address` holds a single MAC, so this faithfully models a
            // single-DPU flip (the case this harness exercises): the former-DPU
            // host-facing MAC becomes the host's plain-NIC MAC. Preserving every NIC
            // of a multi-DPU host would need the host info to carry multiple NIC MACs
            // -- a follow-up.
            debug_assert!(
                host.dpus.len() == 1,
                "drop_managed_dpus preserves only the first former-DPU MAC; \
                 multi-DPU flip preservation is a follow-up",
            );
            if host.non_dpu_mac_address.is_none() {
                host.non_dpu_mac_address = host.dpus.first().map(|dpu| dpu.host_mac_address);
            }
            host.dpus.clear();
        }
    }

    async fn run_machine_discovery(&self) -> Result<MachineDiscoveryResult, MachineStateError> {
        let Some(machine_interface_id) = self.machine_interface_id else {
            return Err(MachineStateError::MissingInterfaceId);
        };

        let start = Instant::now();
        let tpm_ek_certificate = self.live_state.read().unwrap().tpm_ek_certificate.clone();
        let machine_discovery_result = self
            .app_context
            .api_client()
            .discover_machine(
                &self.machine_info,
                MockDiscoveryData {
                    machine_interface_id,
                    tpm_ek_certificate,
                },
            )
            .await?;

        tracing::trace!(
            elapsed_milliseconds = start.elapsed().as_millis(),
            "discover_machine completed",
        );
        Ok(machine_discovery_result)
    }

    // Machine-a-tron receives the compatibility fields from the agent-facing response.
    #[allow(deprecated)]
    async fn send_network_status_observation(
        &self,
        machine_id: MachineId,
        network_config: &ManagedHostNetworkConfigResponse,
    ) -> Result<(), MachineStateError> {
        let mut instance_network_config_version: Option<String> = None;
        let instance_config_version: Option<String> = None;
        let mut interfaces = vec![];

        if network_config.use_admin_network {
            let iface = network_config
                .admin_interface
                .as_ref()
                .expect("use_admin_network true so admin_interface should be Some");
            let addresses = build_dual_stack_list(
                iface.ip.clone(),
                iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
            );
            let prefixes = build_dual_stack_list(
                iface.interface_prefix.clone(),
                iface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
            );
            interfaces = vec![rpc::forge::InstanceInterfaceStatusObservation {
                function_type: iface.function_type,
                virtual_function_id: None,
                mac_address: self.machine_info.host_mac_address().map(|a| a.to_string()),
                addresses,
                prefixes,
                gateways: build_dual_stack_list(iface.gateway.clone(), None),
                network_security_group: None,
                internal_uuid: None,
            }]
        } else {
            instance_network_config_version =
                Some(network_config.instance_network_config_version.clone());

            for iface in network_config.tenant_interfaces.iter() {
                let addresses = build_dual_stack_list(
                    iface.ip.clone(),
                    iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
                );
                let prefixes = build_dual_stack_list(
                    iface.interface_prefix.clone(),
                    iface
                        .ipv6_interface_config
                        .as_ref()
                        .map(|v6| v6.interface_prefix.clone()),
                );
                interfaces.push(rpc::forge::InstanceInterfaceStatusObservation {
                    function_type: iface.function_type,
                    virtual_function_id: iface.virtual_function_id,
                    mac_address: self.machine_info.host_mac_address().map(|a| a.to_string()),
                    addresses,
                    prefixes,
                    gateways: build_dual_stack_list(iface.gateway.clone(), None),
                    network_security_group: iface.network_security_group.as_ref().map(|s| {
                        rpc::forge::NetworkSecurityGroupStatus {
                            source: s.source,
                            id: s.id.clone(),
                            version: s.version.clone(),
                        }
                    }),
                    internal_uuid: None,
                });
            }
        };

        self.app_context
            .api_client()
            .record_dpu_network_status(DpuNetworkStatusArgs {
                dpu_machine_id: DpuMachineId::try_from(machine_id)?,
                network_config_version: network_config.managed_host_config_version.clone(),
                instance_network_config_version,
                instance_config_version,
                instance_id: network_config.instance_id,
                interfaces,
                machine_config: &self.config,
            })
            .await?;
        Ok(())
    }

    pub(super) fn set_system_power(&mut self, request: SystemPowerControl) -> SetSystemPowerResult {
        use SystemPowerControl::*;
        match request {
            On | ForceOn => self.fsm_event(Event::PowerOn),
            GracefulRestart | ForceRestart | PowerCycle => self.fsm_event(Event::PowerCycle),
            GracefulShutdown | ForceOff => self.fsm_event(Event::PowerOff),
            PushPowerButton | Nmi | Suspend | Pause | Resume => {
                let msg = format!("Machine-a-tron mock: unsupported power request {request:?}",);
                tracing::warn!(?request, "unsupported machine-a-tron mock power request",);
                return Err(SetSystemPowerError::BadRequest(msg));
            }
        };
        self.update_live_state();
        Ok(())
    }

    fn machine_id(&self) -> Option<MachineId> {
        self.machine_discovery_result
            .as_ref()
            .and_then(|result| result.machine_id)
    }

    fn machine_ip(&self) -> Option<Ipv4Addr> {
        self.machine_dhcp_info.as_ref().map(|v| v.ip_address)
    }

    fn bmc_ip(&self) -> Option<Ipv4Addr> {
        self.bmc_dhcp_info.as_ref().map(|v| v.ip_address)
    }

    pub(crate) fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        self.bmc_injection.clone()
    }

    pub(super) fn booted_os(&self) -> MaybeOsImage {
        MaybeOsImage(self.fsm.booted_os())
    }

    async fn run_bmc_mock(
        &self,
        ip_address: Ipv4Addr,
    ) -> Result<(Option<Arc<BmcMockWrapperHandle>>, BmcState), MachineStateError> {
        let bmc_mock = BmcMockWrapper::new(
            &self.machine_info,
            self.app_context.clone(),
            Arc::new(LiveStateCallbacks::new(
                self.live_state.clone(),
                self.bmc_command_channel.clone(),
            )),
            Arc::new(LiveStateHostnameQuery(self.live_state.clone())),
            self.mat_host_id,
            self.bmc_injection.clone(),
            // wires LifecycleTimings::bmc_reset (epic #3796 issue 4)
            Some(self.resolved_timings.bmc_reset),
        );

        let pw_override = match &self.machine_info {
            MachineInfo::Host(_) => self.app_context.app_config.host_bmc_password.as_deref(),
            MachineInfo::Dpu(_) => self.app_context.app_config.dpu_bmc_password.as_deref(),
        };
        if let Some(pw) = pw_override {
            bmc_mock
                .state()
                .account_service_state
                .change_factory_default_password(pw);
        }

        let maybe_bmc_mock_handle = {
            self.app_context
                .bmc_registry
                .write()
                .await
                .insert(ip_address.to_string(), bmc_mock.router().clone());
            bmc_mock.start().await?.map(Arc::new)
        };
        if let Some(ssh_host_key) = maybe_bmc_mock_handle
            .as_ref()
            .and_then(|handle| handle.ssh_handle.as_ref())
            .map(|handle| handle.host_pubkey.clone())
        {
            self.live_state.write().unwrap().ssh_host_key = Some(ssh_host_key);
        }
        Ok((maybe_bmc_mock_handle, bmc_mock.state().clone()))
    }

    async fn send_discovery_complete(&self, machine_id: &MachineId) -> Result<(), ClientApiError> {
        let start = Instant::now();
        self.app_context
            .forge_api_client
            .discovery_completed(*machine_id)
            .await
            .map_err(ClientApiError::InvocationError)?;
        tracing::trace!(
            elapsed_milliseconds = start.elapsed().as_millis(),
            "discovery_complete completed",
        );
        Ok(())
    }

    fn is_bmc_only(info: &MachineInfo, config: &MachineConfig) -> bool {
        match info {
            MachineInfo::Dpu(_) => config.dpus_in_nic_mode,
            MachineInfo::Host(_) => false,
        }
    }
}

impl Display for MachineStateMachine {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.fsm.state_string().fmt(f)
    }
}

/// Represents the image that can be booted to via PXE or installed on-device
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OsImage {
    /// Default installed OS, will sleep forever when booted to.
    #[default]
    None,
    /// This is the carbide.efi image and should only run on DPUs. It can be run via PXE or installed.
    DpuAgent,
    /// This is the scout image and can be run on hosts via PXE but should not be installed
    Scout,
}

impl Display for OsImage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            OsImage::DpuAgent => f.write_str("Dpu Agent"),
            OsImage::Scout => f.write_str("Scout"),
            OsImage::None => f.write_str("No OS"),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct MaybeOsImage(pub(super) Option<OsImage>);

impl Display for MaybeOsImage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str("<None>"),
            Some(os_image) => write!(f, "{os_image}"),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub(super) enum MachineStateError {
    #[error(
        "invalid machine state: missing interface_id for this machine in machine discovery results"
    )]
    MissingInterfaceId,
    #[error(
        "invalid machine state: missing machine_id for this machine in machine discovery results"
    )]
    MissingMachineId,
    #[error("invalid machine ID subtype: {0}")]
    InvalidMachineIdSubtype(#[from] InvalidMachineType),
    #[error("no mac addresses specified for machine")]
    NoMachineMacAddress,
    #[error("no DHCP info for BMC. this is bug")]
    NoBmcDhcpInfo,
    #[error("no DHCP info for machine. this is bug")]
    NoMachineDhcpInfo,
    #[error("error configuring listening address: {0}")]
    ListenAddressConfigError(#[from] AddressConfigError),
    #[error("error calling forge API: {0}")]
    ClientApi(#[from] ClientApiError),
    #[error("failed to get DHCP address: {0:?}")]
    DhcpError(#[from] DhcpRelayError),
    #[error("DPU DHCP relay is unavailable")]
    DpuDhcpRelayUnavailable,
    #[error("failed to get PXE response: {0}")]
    PxeError(#[from] PxeError),
    #[error("BMC mock TLS error: {0}")]
    BmcMockTls(#[from] bmc_mock::tls::Error),
    #[error("failed to start IPMI simulator: {0}")]
    IpmiSim(#[from] bmc_mock::ipmi_sim::Error),
    #[error("mock SSH server error: {0}")]
    MockSshServer(String),
    #[error("{0}")]
    WrongOsForMachine(String),
    #[error("machine not found: {0}")]
    MachineNotFound(MachineId),
}
impl From<tonic::Status> for MachineStateError {
    fn from(err: tonic::Status) -> Self {
        MachineStateError::ClientApi(ClientApiError::InvocationError(err))
    }
}
#[derive(thiserror::Error, Debug)]
pub(super) enum AddressConfigError {
    #[error("error running ip command: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;

    #[test]
    fn resolves_pxe_boot_identity_requirements() {
        let machine_interface_id: MachineInterfaceId =
            "0fd6e9a3-06fc-4a22-ad29-aca299677b00".parse().unwrap();

        check_cases(
            [
                Case {
                    scenario: "Scout netboot",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::Scout,
                            machine_interface_id: Some(machine_interface_id),
                        },
                        OsImage::None,
                    ),
                    expect: Yields((OsImage::Scout, Some(machine_interface_id))),
                },
                Case {
                    scenario: "DPU agent netboot",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::DpuAgent,
                            machine_interface_id: Some(machine_interface_id),
                        },
                        OsImage::None,
                    ),
                    expect: Yields((OsImage::DpuAgent, Some(machine_interface_id))),
                },
                Case {
                    scenario: "exit to installed Scout",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::Exit,
                            machine_interface_id: Some(machine_interface_id),
                        },
                        OsImage::Scout,
                    ),
                    expect: Yields((OsImage::Scout, Some(machine_interface_id))),
                },
                Case {
                    scenario: "unknown machine exits without an installed OS",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::Exit,
                            machine_interface_id: None,
                        },
                        OsImage::None,
                    ),
                    expect: Yields((OsImage::None, None)),
                },
                Case {
                    scenario: "Scout netboot without an interface ID",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::Scout,
                            machine_interface_id: None,
                        },
                        OsImage::None,
                    ),
                    expect: Fails,
                },
                Case {
                    scenario: "exit to installed Scout without an interface ID",
                    input: (
                        PxeResponse {
                            boot_target: PxeBootTarget::Exit,
                            machine_interface_id: None,
                        },
                        OsImage::Scout,
                    ),
                    expect: Fails,
                },
            ],
            |(pxe_response, installed_os)| {
                resolve_pxe_boot(pxe_response, installed_os).map_err(drop)
            },
        );
    }

    #[test]
    fn direct_dhcp_relay_selection() {
        let underlay = Ipv4Addr::new(172, 21, 0, 1);
        let host_inband = Ipv4Addr::new(172, 22, 0, 1);

        check_values(
            [
                Check {
                    scenario: "NIC-mode or zero-DPU host",
                    input: (true, Some(host_inband)),
                    expect: host_inband,
                },
                Check {
                    scenario: "legacy host without HostInband configuration",
                    input: (true, None),
                    expect: underlay,
                },
                Check {
                    scenario: "DPU ignores HostInband configuration",
                    input: (false, Some(host_inband)),
                    expect: underlay,
                },
            ],
            |(is_host, host_inband)| direct_dhcp_relay_address(is_host, underlay, host_inband),
        );
    }

    #[test]
    fn power_change_abandons_queued_machine_actions() {
        let queued = |actions: &[FsmAction]| VecDeque::from(actions.to_vec());

        check_values(
            [
                Check {
                    scenario: "failing machine DHCP no longer blocks power-off cleanup",
                    input: (queued(&[FsmAction::Dhcp(DhcpType::Machine)]), false),
                    expect: vec![],
                },
                Check {
                    scenario: "in-band work from the previous boot is abandoned",
                    input: (
                        queued(&[
                            FsmAction::SetupBmc,
                            FsmAction::Dhcp(DhcpType::Machine),
                            FsmAction::SetTimer(Timer::PowerCycle),
                            FsmAction::SetTimer(Timer::MachineOn),
                            FsmAction::SetTimer(Timer::ScoutAgentControlPoll),
                            FsmAction::SetTimer(Timer::DpuAgentControlPoll),
                            FsmAction::PxeBootRequest,
                            FsmAction::InitialDiscoveryRequest(OsImage::Scout),
                            FsmAction::AgentControlRequest(OsImage::Scout),
                            FsmAction::DpuAgentNetworkObservation,
                            FsmAction::BmcEvent(BmcEvent::PowerOn),
                            FsmAction::BmcEvent(BmcEvent::BootCompleted),
                            FsmAction::CleanupOnPowerOff,
                        ]),
                        false,
                    ),
                    expect: vec![
                        format!("{:?}", FsmAction::SetupBmc),
                        format!("{:?}", FsmAction::CleanupOnPowerOff),
                    ],
                },
                Check {
                    scenario: "out-of-band BMC DHCP stays queued across a power change",
                    input: (queued(&[FsmAction::Dhcp(DhcpType::Bmc)]), true),
                    expect: vec![format!("{:?}", FsmAction::Dhcp(DhcpType::Bmc))],
                },
                Check {
                    scenario: "BMC retry work stays queued during BMC initialization",
                    input: (
                        queued(&[
                            FsmAction::ScheduleDhcpRetry {
                                delay: Duration::from_secs(4),
                            },
                            FsmAction::CancelDhcpRetry,
                        ]),
                        true,
                    ),
                    expect: vec![
                        format!(
                            "{:?}",
                            FsmAction::ScheduleDhcpRetry {
                                delay: Duration::from_secs(4),
                            }
                        ),
                        format!("{:?}", FsmAction::CancelDhcpRetry),
                    ],
                },
                Check {
                    scenario: "machine retry work is abandoned after BMC initialization",
                    input: (
                        queued(&[
                            FsmAction::ScheduleDhcpRetry {
                                delay: Duration::from_secs(4),
                            },
                            FsmAction::CancelDhcpRetry,
                        ]),
                        false,
                    ),
                    expect: vec![],
                },
                Check {
                    scenario: "power change without queued in-band work changes nothing",
                    input: (
                        queued(&[FsmAction::SetupBmc, FsmAction::CleanupOnPowerOff]),
                        false,
                    ),
                    expect: vec![
                        format!("{:?}", FsmAction::SetupBmc),
                        format!("{:?}", FsmAction::CleanupOnPowerOff),
                    ],
                },
            ],
            |(mut actions, preserve_dhcp_retry)| {
                abandon_machine_actions_on_power_change(&mut actions, preserve_dhcp_retry);
                actions.iter().map(|action| format!("{action:?}")).collect()
            },
        );
    }
}
