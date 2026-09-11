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
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bmc_mock::injection::InjectionStore;
use bmc_mock::mac_address_pool::{MacAddressPool, PoolConfig as MacAddressPoolConfig};
use bmc_mock::{
    BmcCommand, Callbacks, HostMachineInfo, HostnameQuerying, MachineInfo, MockPowerState,
    POWER_CYCLE_DELAY, SetSystemPowerError, SetSystemPowerResult, SystemPowerControl,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult, AlarmId};
use crate::bmc_mock_wrapper::{BmcMockWrapper, BmcMockWrapperHandle, ConsoleSimulators};
use crate::config::{self, MachineATronContext, MachineConfig, PersistedDevice};
use crate::dhcp_wrapper::{DhcpRequestInfo, DhcpRequester, DhcpResponseInfo, vendor_class};
use crate::machine_state_machine::{MachineStateError, OsImage};
use crate::saturating_add_duration_to_instant;
use crate::status::{BmcStatus, DeviceKind, DeviceStatus, DeviceStatusConfig, EndpointStatus};
use crate::switch_fsm::{Action, DhcpEndpoint, Event, SwitchFsm, Timer};

fn abandon_nvos_dhcp_on_power_change(actions: &mut VecDeque<Action>) {
    actions.retain(|action| !matches!(action, Action::Dhcp(DhcpEndpoint::Nvos)));
}

#[derive(Debug)]
struct SwitchLiveState {
    power_state: MockPowerState,
    bmc_ip: Option<Ipv4Addr>,
    nvos_ip: Option<Ipv4Addr>,
    ipmi_port: Option<u16>,
    ssh_endpoint_port: Option<u16>,
    ssh_host_key: Option<String>,
    state: &'static str,
}

impl SwitchLiveState {
    fn new(fsm: &SwitchFsm) -> Self {
        Self {
            power_state: fsm.power_state(),
            bmc_ip: None,
            nvos_ip: None,
            ipmi_port: None,
            ssh_endpoint_port: None,
            ssh_host_key: None,
            state: fsm.state_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct SwitchCallbacks {
    state: Arc<RwLock<SwitchLiveState>>,
    mailbox: ActorMailbox<SwitchMessage>,
}

impl Callbacks for SwitchCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        self.state.read().unwrap().power_state
    }

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        self.mailbox
            .send(SwitchMessage::Bmc(BmcCommand::SetSystemPower {
                request: reset_type,
                reply: None,
            }))
            .map_err(|error| SetSystemPowerError::CommandSendError(error.to_string()))
    }

    fn state_refresh_indication(&self) {
        let _ = self
            .mailbox
            .send(SwitchMessage::Bmc(BmcCommand::StateRefreshIndication));
    }
}

#[derive(Debug)]
struct SwitchHostname;

impl HostnameQuerying for SwitchHostname {
    fn get_hostname(&'_ self) -> Cow<'_, str> {
        Cow::Borrowed("localhost")
    }
}

pub(crate) struct SwitchActor {
    mat_id: Uuid,
    machine_config_section: String,
    host_info: HostMachineInfo,
    app_context: Arc<MachineATronContext>,
    config: Arc<MachineConfig>,
    live_state: Arc<RwLock<SwitchLiveState>>,
    bmc_injection: Arc<InjectionStore>,
    /// Built on the first `SetupBmc` attempt and re-published, not rebuilt, if that action is
    /// retried after a failed start.
    bmc_mock_wrapper: Option<BmcMockWrapper>,
    _bmc_mock: Option<Arc<BmcMockWrapperHandle>>,
    bmc_dhcp_info: Option<DhcpResponseInfo>,
    fsm: SwitchFsm,
    actions: VecDeque<Action>,
    run_alarm: Option<AlarmId>,
    power_cycle_alarm: Option<AlarmId>,
    dhcp_retry_alarm: Option<AlarmId>,
}

impl SwitchActor {
    pub(crate) fn new(
        app_context: Arc<MachineATronContext>,
        machine_config_section: String,
        config: Arc<MachineConfig>,
        mac_pool: &mut MacAddressPool,
        hw_mac_addr_pool: MacAddressPoolConfig,
    ) -> Self {
        // Simulated switches start powered so NVOS can boot and expose its management interface.
        let (fsm, actions) = SwitchFsm::init(true);
        Self {
            mat_id: Uuid::new_v4(),
            machine_config_section,
            host_info: HostMachineInfo {
                rack_placement: config.rack_placement,
                ..HostMachineInfo::new(config.hw_type, Vec::new(), mac_pool, hw_mac_addr_pool)
            },
            app_context,
            config,
            live_state: Arc::new(RwLock::new(SwitchLiveState::new(&fsm))),
            bmc_injection: Arc::new(InjectionStore::new()),
            bmc_mock_wrapper: None,
            _bmc_mock: None,
            bmc_dhcp_info: None,
            fsm,
            actions: actions.into_iter().collect(),
            run_alarm: None,
            power_cycle_alarm: None,
            dhcp_retry_alarm: None,
        }
    }

    pub(crate) fn from_persisted(
        persisted: PersistedDevice,
        machine_config_section: String,
        app_context: Arc<MachineATronContext>,
        config: Arc<MachineConfig>,
        hw_mac_addr_pool: MacAddressPoolConfig,
    ) -> Self {
        let host_info = HostMachineInfo {
            hw_type: persisted.hw_type,
            rack_placement: config.rack_placement,
            bmc_mac_address: persisted.bmc_mac_address,
            serial: persisted.serial.clone(),
            dpus: Vec::new(),
            non_dpu_mac_address: persisted.non_dpu_mac_address,
            nvos_mac_addresses: persisted.nvos_mac_addresses.clone(),
            switch_serial_number: persisted.switch_serial_number.clone(),
            hw_mac_addr_pool,
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        };
        let (fsm, actions) = SwitchFsm::init(true);
        Self {
            mat_id: persisted.mat_id,
            machine_config_section,
            host_info,
            app_context,
            config,
            live_state: Arc::new(RwLock::new(SwitchLiveState::new(&fsm))),
            bmc_injection: Arc::new(InjectionStore::new()),
            bmc_mock_wrapper: None,
            _bmc_mock: None,
            bmc_dhcp_info: None,
            fsm,
            actions: actions.into_iter().collect(),
            run_alarm: None,
            power_cycle_alarm: None,
            dhcp_retry_alarm: None,
        }
    }

    pub(crate) fn start(mut self, paused: bool) -> SwitchHandle {
        self.fsm_event(if paused { Event::Pause } else { Event::Resume });
        let mat_id = self.mat_id;
        let live_state = self.live_state.clone();
        let host_info = self.host_info.clone();
        let machine_config_section = self.machine_config_section.clone();
        let bmc_injection = self.bmc_injection.clone();
        let (actor, mailbox) = Actor::new(self, SwitchMessage::Run);

        let join_handle = tokio::task::Builder::new()
            .name(&format!("Switch {mat_id}"))
            .spawn(actor.run())
            .unwrap();

        SwitchHandle(Arc::new(SwitchActorHandle {
            mailbox,
            join_handle: Mutex::new(Some(join_handle)),
            mat_id,
            live_state,
            host_info,
            machine_config_section,
            bmc_injection,
        }))
    }

    async fn process_state(&mut self, mailbox: &ActorMailbox<SwitchMessage>) -> ActorResult {
        if self.fsm.is_paused() {
            if let Some(alarm_id) = self.run_alarm.take() {
                mailbox.cancel(alarm_id);
            }
            return ActorResult::Noop;
        }

        let sleep_duration = if let Some(duration) = self.process_actions(mailbox).await {
            duration
        } else {
            self.config.run_interval_idle
        };
        self.schedule_run(mailbox, sleep_duration);
        ActorResult::Noop
    }

    async fn process_actions(&mut self, mailbox: &ActorMailbox<SwitchMessage>) -> Option<Duration> {
        while let Some(action) = self.actions.front().copied() {
            self.update_live_state();
            match action {
                Action::Dhcp(DhcpEndpoint::Bmc) => match self.bmc_dhcp_discovery().await {
                    Ok(dhcp_info) => {
                        self.bmc_dhcp_info = Some(dhcp_info);
                        self.actions.pop_front();
                        self.fsm_event(Event::DhcpComplete);
                    }
                    Err(error) => {
                        tracing::warn!(
                            device_id = %self.mat_id,
                            error = %error,
                            "Switch BMC DHCP failed",
                        );
                        self.actions.pop_front();
                        self.fsm_event(Event::dhcp_failed());
                    }
                },
                Action::Dhcp(DhcpEndpoint::Nvos) => match self.nvos_dhcp_discovery().await {
                    Ok(dhcp_info) => {
                        self.live_state.write().unwrap().nvos_ip = Some(dhcp_info.ip_address);
                        self.actions.pop_front();
                        self.fsm_event(Event::DhcpComplete);
                    }
                    Err(error) => {
                        tracing::warn!(
                            device_id = %self.mat_id,
                            error = %error,
                            "Switch NVOS DHCP failed",
                        );
                        self.actions.pop_front();
                        self.fsm_event(Event::dhcp_failed());
                    }
                },
                Action::ScheduleDhcpRetry { delay } => {
                    tracing::debug!(
                        device_id = %self.mat_id,
                        retry_delay_milliseconds = delay.as_millis(),
                        "scheduled switch DHCP retry"
                    );
                    self.dhcp_retry_alarm = Some(
                        mailbox
                            .replace_alarm(
                                self.dhcp_retry_alarm.take(),
                                saturating_add_duration_to_instant(Instant::now(), delay),
                                SwitchMessage::DhcpRetryExpired,
                            )
                            .expect("running actor mailbox must be open"),
                    );
                    self.actions.pop_front();
                }
                Action::CancelDhcpRetry => {
                    if let Some(alarm_id) = self.dhcp_retry_alarm.take() {
                        mailbox.cancel(alarm_id);
                    }
                    self.actions.pop_front();
                }
                Action::SetupBmc => match self.setup_bmc(mailbox).await {
                    Ok(()) => {
                        self.actions.pop_front();
                    }
                    Err(error) => {
                        tracing::warn!(
                            device_id = %self.mat_id,
                            error = %error,
                            "Switch BMC startup failed",
                        );
                        return Some(self.config.run_interval_working);
                    }
                },
                Action::StopNvos => {
                    self.live_state.write().unwrap().nvos_ip = None;
                    self.actions.pop_front();
                }
                Action::SetTimer(Timer::PowerCycle) => {
                    self.power_cycle_alarm = Some(
                        mailbox
                            .replace_alarm(
                                self.power_cycle_alarm.take(),
                                Instant::now() + POWER_CYCLE_DELAY,
                                SwitchMessage::PowerCycleExpired,
                            )
                            .expect("running actor mailbox must be open"),
                    );
                    self.actions.pop_front();
                }
                Action::CancelTimer(Timer::PowerCycle) => {
                    if let Some(alarm_id) = self.power_cycle_alarm.take() {
                        mailbox.cancel(alarm_id);
                    }
                    self.actions.pop_front();
                }
            }
        }
        self.update_live_state();
        None
    }

    fn schedule_run(&mut self, mailbox: &ActorMailbox<SwitchMessage>, duration: Duration) {
        self.run_alarm = Some(
            mailbox
                .replace_alarm(
                    self.run_alarm.take(),
                    saturating_add_duration_to_instant(Instant::now(), duration),
                    SwitchMessage::Run,
                )
                .expect("running actor mailbox must be open"),
        );
    }

    async fn bmc_dhcp_discovery(&self) -> crate::dhcp_wrapper::DhcpRelayResult<DhcpResponseInfo> {
        let machine_info = MachineInfo::Host(self.host_info.clone());
        self.app_context
            .dhcp_client
            .request_ip(DhcpRequestInfo {
                mac_address: self.host_info.bmc_mac_address,
                relay_address: self.config.bmc_dhcp_relay_address,
                vendor_class: vendor_class(&machine_info, DhcpRequester::Bmc),
            })
            .await
    }

    async fn nvos_dhcp_discovery(&self) -> eyre::Result<DhcpResponseInfo> {
        let [nvos_mac_address] = self.host_info.nvos_mac_addresses.as_slice() else {
            eyre::bail!(
                "switch must have exactly one NVOS MAC address, found {}",
                self.host_info.nvos_mac_addresses.len(),
            );
        };

        Ok(self
            .app_context
            .dhcp_client
            .request_ip(DhcpRequestInfo {
                mac_address: *nvos_mac_address,
                relay_address: self.config.underlay_dhcp_relay_address,
                // No DHCP option 60 value has been verified for NVOS.
                vendor_class: None,
            })
            .await?)
    }

    fn build_bmc_mock(&self, mailbox: &ActorMailbox<SwitchMessage>) -> BmcMockWrapper {
        let machine_info = MachineInfo::Host(self.host_info.clone());
        let bmc_mock = BmcMockWrapper::new(
            &machine_info,
            ConsoleSimulators::from(&self.app_context.app_config),
            Arc::new(SwitchCallbacks {
                state: self.live_state.clone(),
                mailbox: mailbox.clone(),
            }),
            Arc::new(SwitchHostname),
            self.mat_id,
            self.bmc_injection.clone(),
            // no lifecycle timing profile for this device kind yet
            None,
        );
        if let Some(password) = self.app_context.app_config.host_bmc_password.as_deref() {
            bmc_mock
                .state()
                .account_service_state
                .change_factory_default_password(password);
        }
        bmc_mock
    }

    async fn setup_bmc(
        &mut self,
        mailbox: &ActorMailbox<SwitchMessage>,
    ) -> Result<(), MachineStateError> {
        let ip_address = self
            .bmc_dhcp_info
            .as_ref()
            .ok_or(MachineStateError::NoBmcDhcpInfo)?
            .ip_address;
        let bmc_mock = match &mut self.bmc_mock_wrapper {
            Some(bmc_mock) => bmc_mock,
            None => {
                let bmc_mock = self.build_bmc_mock(mailbox);
                self.bmc_mock_wrapper.insert(bmc_mock)
            }
        };
        bmc_mock
            .register(&self.app_context.bmc_registry, ip_address)
            .await;
        let bmc_handle = bmc_mock.start().await?.map(Arc::new);

        if let Some(ssh_host_key) = bmc_handle
            .as_ref()
            .and_then(|handle| handle.ssh_handle.as_ref())
            .map(|handle| handle.host_pubkey.clone())
        {
            self.live_state.write().unwrap().ssh_host_key = Some(ssh_host_key);
        }

        {
            let mut state = self.live_state.write().unwrap();
            state.bmc_ip = Some(ip_address);
            state.ipmi_port = bmc_handle.as_ref().and_then(|handle| handle.ipmi_port());
            state.ssh_endpoint_port = bmc_handle
                .as_ref()
                .and_then(|handle| handle.ssh_endpoint_port());
        }
        self._bmc_mock = bmc_handle;
        Ok(())
    }

    fn set_system_power(&mut self, request: SystemPowerControl) -> SetSystemPowerResult {
        use SystemPowerControl::*;

        match request {
            On | ForceOn => self.fsm_event(Event::PowerOn),
            GracefulShutdown | ForceOff => self.fsm_event(Event::PowerOff),
            GracefulRestart | ForceRestart | PowerCycle => self.fsm_event(Event::PowerCycle),
            PushPowerButton | Nmi | Suspend | Pause | Resume => {
                return Err(SetSystemPowerError::BadRequest(format!(
                    "Machine-a-tron mock: unsupported power request {request:?}"
                )));
            }
        }
        self.update_live_state();
        Ok(())
    }

    fn fsm_event(&mut self, event: Event) {
        if matches!(event, Event::PowerOff | Event::PowerCycle) {
            abandon_nvos_dhcp_on_power_change(&mut self.actions);
        }

        let previous_state = self.fsm;
        let (next_state, actions) = self.fsm.event(event);
        tracing::info!(
            ?previous_state,
            ?event,
            ?next_state,
            ?actions,
            "Switch FSM step",
        );
        self.actions.extend(actions);
        self.fsm = next_state;
    }

    fn update_live_state(&self) {
        let mut state = self.live_state.write().unwrap();
        state.power_state = self.fsm.power_state();
        state.state = self.fsm.state_string();
    }
}

#[derive(Debug)]
enum SwitchMessage {
    Run,
    PowerCycleExpired,
    DhcpRetryExpired,
    SetPaused(bool),
    Bmc(BmcCommand),
    Stop,
}

impl ActorCallbacks<SwitchMessage> for SwitchActor {
    async fn message(
        &mut self,
        mailbox: &ActorMailbox<SwitchMessage>,
        message: SwitchMessage,
    ) -> ActorResult {
        match message {
            SwitchMessage::Run => self.run_alarm = None,
            SwitchMessage::PowerCycleExpired => {
                self.power_cycle_alarm = None;
                self.fsm_event(Event::TimerAlert(Timer::PowerCycle));
            }
            SwitchMessage::DhcpRetryExpired => {
                self.dhcp_retry_alarm = None;
                self.fsm_event(Event::DhcpRetryExpired);
            }
            SwitchMessage::Stop => return ActorResult::Stop,
            SwitchMessage::SetPaused(paused) => {
                self.fsm_event(if paused { Event::Pause } else { Event::Resume })
            }
            SwitchMessage::Bmc(BmcCommand::SetSystemPower { request, reply }) => {
                let result = self.set_system_power(request);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            SwitchMessage::Bmc(BmcCommand::StateRefreshIndication) => {
                self.update_live_state();
            }
        }
        SwitchActor::process_state(self, mailbox).await
    }
}

#[derive(Debug)]
struct SwitchActorHandle {
    mailbox: ActorMailbox<SwitchMessage>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    mat_id: Uuid,
    live_state: Arc<RwLock<SwitchLiveState>>,
    host_info: HostMachineInfo,
    machine_config_section: String,
    bmc_injection: Arc<InjectionStore>,
}

#[derive(Debug, Clone)]
pub(crate) struct SwitchHandle(Arc<SwitchActorHandle>);

impl SwitchHandle {
    pub(crate) fn mat_id(&self) -> Uuid {
        self.0.mat_id
    }

    pub(crate) fn pause(&self) -> eyre::Result<()> {
        self.0.mailbox.send(SwitchMessage::SetPaused(true))?;
        Ok(())
    }

    pub(crate) fn resume(&self) -> eyre::Result<()> {
        self.0.mailbox.send(SwitchMessage::SetPaused(false))?;
        Ok(())
    }

    pub(crate) fn host_info(&self) -> &HostMachineInfo {
        &self.0.host_info
    }

    pub(crate) fn machine_config_section(&self) -> &str {
        &self.0.machine_config_section
    }

    pub(crate) fn status(&self, config: &DeviceStatusConfig) -> DeviceStatus {
        let state = self.0.live_state.read().unwrap();
        DeviceStatus {
            mat_id: self.0.mat_id.to_string(),
            device_kind: DeviceKind::Switch,
            device_id: self.0.mat_id.to_string(),
            machine_id: None,
            hardware_type: Some(self.0.host_info.hw_type),
            mat_state: Some(state.state.to_string()),
            api_state: "Unknown".to_string(),
            power_state: state.power_state.to_string(),
            machine_ip: None,
            nvos_ip: state.nvos_ip.map(|ip| ip.to_string()),
            infiniband_ports: None,
            bmc: BmcStatus {
                ip: state.bmc_ip.map(|ip| ip.to_string()),
                redfish: EndpointStatus::redfish(config),
                ipmi: state.ipmi_port.map(EndpointStatus::same_port),
                ssh: state.ssh_endpoint_port.map(EndpointStatus::same_port),
            },
            dpus: Vec::new(),
        }
    }

    pub(crate) fn persisted(&self) -> PersistedDevice {
        PersistedDevice {
            hw_type: self.0.host_info.hw_type,
            mat_id: self.0.mat_id,
            machine_config_section: self.0.machine_config_section.clone(),
            bmc_mac_address: self.0.host_info.bmc_mac_address,
            serial: self.0.host_info.serial.clone(),
            dpus: Vec::new(),
            non_dpu_mac_address: self.0.host_info.non_dpu_mac_address,
            nvos_mac_addresses: self.0.host_info.nvos_mac_addresses.clone(),
            switch_serial_number: self.0.host_info.switch_serial_number.clone(),
            observed_machine_id: None,
            installed_os: OsImage::None,
            tpm_ek_certificate: None,
            hw_mac_addr_pool: Some(config::MacAddressPoolConfig {
                base: self.0.host_info.hw_mac_addr_pool.base(),
                host_bits: self.0.host_info.hw_mac_addr_pool.host_bits(),
            }),
            active_host_firmware: None,
        }
    }

    pub(crate) fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        self.0.bmc_injection.clone()
    }

    pub(crate) fn abort(&self) {
        if let Some(join_handle) = self.0.join_handle.lock().unwrap().take() {
            join_handle.abort();
        }
    }

    pub(crate) async fn abort_and_wait(&self) -> eyre::Result<()> {
        let _ = self.0.mailbox.send(SwitchMessage::Stop);
        let join_handle = self.0.join_handle.lock().unwrap().take();
        if let Some(join_handle) = join_handle {
            match join_handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn bmc_ssh_host_pubkey(&self) -> Option<String> {
        self.0.live_state.read().unwrap().ssh_host_key.clone()
    }

    pub(crate) fn bmc_ip(&self) -> Option<Ipv4Addr> {
        self.0.live_state.read().unwrap().bmc_ip
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn power_change_abandons_queued_nvos_dhcp() {
        check_values(
            [
                Check {
                    scenario: "queued NVOS DHCP is abandoned",
                    input: vec![Action::Dhcp(DhcpEndpoint::Nvos)],
                    expect: vec![],
                },
                Check {
                    scenario: "unrelated actions remain queued",
                    input: vec![Action::Dhcp(DhcpEndpoint::Bmc), Action::SetupBmc],
                    expect: vec![Action::Dhcp(DhcpEndpoint::Bmc), Action::SetupBmc],
                },
            ],
            |actions| {
                let mut actions = VecDeque::from(actions);
                abandon_nvos_dhcp_on_power_change(&mut actions);
                Vec::from(actions)
            },
        );
    }
}
