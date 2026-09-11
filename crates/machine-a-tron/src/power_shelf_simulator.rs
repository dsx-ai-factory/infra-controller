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
    BmcCommand, Callbacks, HardwareType, HostMachineInfo, HostnameQuerying, MachineInfo,
    MockPowerState, POWER_CYCLE_DELAY, SetSystemPowerError, SetSystemPowerResult,
    SystemPowerControl,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::actor::{Actor, ActorCallbacks, ActorMailbox, ActorResult, AlarmId};
use crate::bmc_mock_wrapper::{BmcMockWrapper, BmcMockWrapperHandle};
use crate::config::{self, MachineATronContext, MachineConfig, PersistedDevice};
use crate::dhcp_wrapper::{DhcpRequestInfo, DhcpRequester, DhcpResponseInfo, vendor_class};
use crate::machine_state_machine::{MachineStateError, OsImage};
use crate::power_shelf_fsm::{Action, Event, PowerShelfFsm, Timer};
use crate::saturating_add_duration_to_instant;
use crate::status::{BmcStatus, DeviceKind, DeviceStatus, DeviceStatusConfig, EndpointStatus};

#[derive(Debug)]
struct PowerShelfLiveState {
    power_state: MockPowerState,
    bmc_ip: Option<Ipv4Addr>,
    ipmi_port: Option<u16>,
    ssh_endpoint_port: Option<u16>,
    ssh_host_key: Option<String>,
    state: &'static str,
}

impl PowerShelfLiveState {
    fn new(fsm: &PowerShelfFsm) -> Self {
        Self {
            power_state: fsm.power_state(),
            bmc_ip: None,
            ipmi_port: None,
            ssh_endpoint_port: None,
            ssh_host_key: None,
            state: fsm.state_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct PowerShelfCallbacks {
    state: Arc<RwLock<PowerShelfLiveState>>,
    mailbox: ActorMailbox<PowerShelfMessage>,
}

impl Callbacks for PowerShelfCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        self.state.read().unwrap().power_state
    }

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        self.mailbox
            .send(PowerShelfMessage::Bmc(BmcCommand::SetSystemPower {
                request: reset_type,
                reply: None,
            }))
            .map_err(|error| SetSystemPowerError::CommandSendError(error.to_string()))
    }

    fn state_refresh_indication(&self) {
        let _ = self
            .mailbox
            .send(PowerShelfMessage::Bmc(BmcCommand::StateRefreshIndication));
    }
}

#[derive(Debug)]
struct PowerShelfHostname;

impl HostnameQuerying for PowerShelfHostname {
    fn get_hostname(&'_ self) -> Cow<'_, str> {
        Cow::Borrowed("localhost")
    }
}

pub(crate) struct PowerShelfActor {
    mat_id: Uuid,
    machine_config_section: String,
    host_info: HostMachineInfo,
    app_context: Arc<MachineATronContext>,
    config: Arc<MachineConfig>,
    live_state: Arc<RwLock<PowerShelfLiveState>>,
    bmc_injection: Arc<InjectionStore>,
    _bmc_mock: Option<Arc<BmcMockWrapperHandle>>,
    bmc_dhcp_info: Option<DhcpResponseInfo>,
    fsm: PowerShelfFsm,
    actions: VecDeque<Action>,
    run_alarm: Option<AlarmId>,
    power_cycle_alarm: Option<AlarmId>,
    dhcp_retry_alarm: Option<AlarmId>,
}

impl PowerShelfActor {
    pub(crate) fn new(
        app_context: Arc<MachineATronContext>,
        machine_config_section: String,
        config: Arc<MachineConfig>,
        mac_pool: &mut MacAddressPool,
        hw_mac_addr_pool: MacAddressPoolConfig,
    ) -> Self {
        let (fsm, actions) = PowerShelfFsm::init(false);
        Self {
            mat_id: Uuid::new_v4(),
            machine_config_section,
            host_info: HostMachineInfo {
                rack_placement: config.rack_placement,
                ..HostMachineInfo::new(config.hw_type, Vec::new(), mac_pool, hw_mac_addr_pool)
            },
            app_context,
            config,
            live_state: Arc::new(RwLock::new(PowerShelfLiveState::new(&fsm))),
            bmc_injection: Arc::new(InjectionStore::new()),
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
        let (fsm, actions) = PowerShelfFsm::init(true);
        Self {
            mat_id: persisted.mat_id,
            machine_config_section,
            host_info,
            app_context,
            config,
            live_state: Arc::new(RwLock::new(PowerShelfLiveState::new(&fsm))),
            bmc_injection: Arc::new(InjectionStore::new()),
            _bmc_mock: None,
            bmc_dhcp_info: None,
            fsm,
            actions: actions.into_iter().collect(),
            run_alarm: None,
            power_cycle_alarm: None,
            dhcp_retry_alarm: None,
        }
    }

    pub(crate) fn start(mut self, paused: bool) -> PowerShelfHandle {
        self.fsm_event(if paused { Event::Pause } else { Event::Resume });
        let mat_id = self.mat_id;
        let live_state = self.live_state.clone();
        let host_info = self.host_info.clone();
        let machine_config_section = self.machine_config_section.clone();
        let bmc_injection = self.bmc_injection.clone();
        let (actor, mailbox) = Actor::new(self, PowerShelfMessage::Run);

        let join_handle = tokio::task::Builder::new()
            .name(&format!("Power shelf {mat_id}"))
            .spawn(actor.run())
            .unwrap();

        PowerShelfHandle(Arc::new(PowerShelfActorHandle {
            mailbox,
            join_handle: Mutex::new(Some(join_handle)),
            mat_id,
            live_state,
            host_info,
            machine_config_section,
            bmc_injection,
        }))
    }

    async fn process_state(&mut self, mailbox: &ActorMailbox<PowerShelfMessage>) -> ActorResult {
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

    async fn process_actions(
        &mut self,
        mailbox: &ActorMailbox<PowerShelfMessage>,
    ) -> Option<Duration> {
        while let Some(action) = self.actions.front().copied() {
            self.update_live_state();
            match action {
                Action::Dhcp => match self.bmc_dhcp_discovery().await {
                    Ok(dhcp_info) => {
                        self.bmc_dhcp_info = Some(dhcp_info);
                        self.actions.pop_front();
                        self.fsm_event(Event::DhcpComplete);
                    }
                    Err(error) => {
                        tracing::warn!(
                            device_id = %self.mat_id,
                            error = %error,
                            "Power-shelf BMC DHCP failed",
                        );
                        self.actions.pop_front();
                        self.fsm_event(Event::dhcp_failed());
                    }
                },
                Action::ScheduleDhcpRetry { delay } => {
                    tracing::debug!(
                        retry_delay_milliseconds = delay.as_millis(),
                        "scheduled power-shelf DHCP retry"
                    );
                    self.dhcp_retry_alarm = Some(
                        mailbox
                            .replace_alarm(
                                self.dhcp_retry_alarm.take(),
                                saturating_add_duration_to_instant(Instant::now(), delay),
                                PowerShelfMessage::DhcpRetryExpired,
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
                            "Power-shelf BMC startup failed",
                        );
                        return Some(self.config.run_interval_working);
                    }
                },
                Action::SetTimer(Timer::PowerCycle) => {
                    self.power_cycle_alarm = Some(
                        mailbox
                            .replace_alarm(
                                self.power_cycle_alarm.take(),
                                Instant::now() + POWER_CYCLE_DELAY,
                                PowerShelfMessage::PowerCycleExpired,
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

    fn schedule_run(&mut self, mailbox: &ActorMailbox<PowerShelfMessage>, duration: Duration) {
        self.run_alarm = Some(
            mailbox
                .replace_alarm(
                    self.run_alarm.take(),
                    saturating_add_duration_to_instant(Instant::now(), duration),
                    PowerShelfMessage::Run,
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

    async fn setup_bmc(
        &mut self,
        mailbox: &ActorMailbox<PowerShelfMessage>,
    ) -> Result<(), MachineStateError> {
        let dhcp_info = self
            .bmc_dhcp_info
            .as_ref()
            .ok_or(MachineStateError::NoBmcDhcpInfo)?;
        let machine_info = MachineInfo::Host(self.host_info.clone());
        let bmc_mock = BmcMockWrapper::new(
            &machine_info,
            self.app_context.clone(),
            Arc::new(PowerShelfCallbacks {
                state: self.live_state.clone(),
                mailbox: mailbox.clone(),
            }),
            Arc::new(PowerShelfHostname),
            self.mat_id,
            self.bmc_injection.clone(),
            // no lifecycle timing profile for this device kind yet
            None,
        );
        if self.host_info.hw_type != HardwareType::LiteOnPowerShelf
            && let Some(password) = self.app_context.app_config.host_bmc_password.as_deref()
        {
            bmc_mock
                .state()
                .account_service_state
                .change_factory_default_password(password);
        }

        let bmc_handle = {
            self.app_context
                .bmc_registry
                .write()
                .await
                .insert(dhcp_info.ip_address.to_string(), bmc_mock.router().clone());
            bmc_mock.start().await?.map(Arc::new)
        };

        if let Some(ssh_host_key) = bmc_handle
            .as_ref()
            .and_then(|handle| handle.ssh_handle.as_ref())
            .map(|handle| handle.host_pubkey.clone())
        {
            self.live_state.write().unwrap().ssh_host_key = Some(ssh_host_key);
        }

        {
            let mut state = self.live_state.write().unwrap();
            state.bmc_ip = Some(dhcp_info.ip_address);
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
        let previous_state = self.fsm;
        let (next_state, actions) = self.fsm.event(event);
        tracing::info!(
            ?previous_state,
            ?event,
            ?next_state,
            ?actions,
            "Power-shelf FSM step",
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
enum PowerShelfMessage {
    Run,
    PowerCycleExpired,
    DhcpRetryExpired,
    SetPaused(bool),
    Bmc(BmcCommand),
    Stop,
}

impl ActorCallbacks<PowerShelfMessage> for PowerShelfActor {
    async fn message(
        &mut self,
        mailbox: &ActorMailbox<PowerShelfMessage>,
        message: PowerShelfMessage,
    ) -> ActorResult {
        match message {
            PowerShelfMessage::Run => self.run_alarm = None,
            PowerShelfMessage::PowerCycleExpired => {
                self.power_cycle_alarm = None;
                self.fsm_event(Event::TimerAlert(Timer::PowerCycle));
            }
            PowerShelfMessage::DhcpRetryExpired => {
                self.dhcp_retry_alarm = None;
                self.fsm_event(Event::DhcpRetryExpired);
            }
            PowerShelfMessage::Stop => return ActorResult::Stop,
            PowerShelfMessage::SetPaused(paused) => {
                self.fsm_event(if paused { Event::Pause } else { Event::Resume })
            }
            PowerShelfMessage::Bmc(BmcCommand::SetSystemPower { request, reply }) => {
                let result = self.set_system_power(request);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            PowerShelfMessage::Bmc(BmcCommand::StateRefreshIndication) => {
                self.update_live_state();
            }
        }
        PowerShelfActor::process_state(self, mailbox).await
    }
}

#[derive(Debug)]
struct PowerShelfActorHandle {
    mailbox: ActorMailbox<PowerShelfMessage>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    mat_id: Uuid,
    live_state: Arc<RwLock<PowerShelfLiveState>>,
    host_info: HostMachineInfo,
    machine_config_section: String,
    bmc_injection: Arc<InjectionStore>,
}

#[derive(Debug, Clone)]
pub(crate) struct PowerShelfHandle(Arc<PowerShelfActorHandle>);

impl PowerShelfHandle {
    pub(crate) fn mat_id(&self) -> Uuid {
        self.0.mat_id
    }

    pub(crate) fn pause(&self) -> eyre::Result<()> {
        self.0.mailbox.send(PowerShelfMessage::SetPaused(true))?;
        Ok(())
    }

    pub(crate) fn resume(&self) -> eyre::Result<()> {
        self.0.mailbox.send(PowerShelfMessage::SetPaused(false))?;
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
            device_kind: DeviceKind::PowerShelf,
            device_id: self.0.mat_id.to_string(),
            machine_id: None,
            hardware_type: Some(self.0.host_info.hw_type),
            mat_state: Some(state.state.to_string()),
            api_state: "Unknown".to_string(),
            power_state: state.power_state.to_string(),
            machine_ip: None,
            nvos_ip: None,
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
            // Power shelves always accept factory-default logins, so there is
            // no rotated credential to persist (issue #5966).
            bmc_accounts: None,
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
        let _ = self.0.mailbox.send(PowerShelfMessage::Stop);
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
