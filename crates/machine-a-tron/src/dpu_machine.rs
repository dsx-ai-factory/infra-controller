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
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bmc_mock::injection::InjectionStore;
use bmc_mock::mac_address_pool::MacAddressPool;
use bmc_mock::{
    ActionError, DeviceEffect, DpuMachineInfo, DpuSettings, HardwareType, MachineInfo,
    ResourceResetType,
};
use carbide_uuid::machine::MachineId;
use eyre::Context;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::instrument;
use uuid::Uuid;

use crate::bmc_mock_wrapper::BmcCommand;
use crate::config::{MachineATronContext, PersistedDpuMachine};
use crate::dhcp_wrapper::{DhcpRelayResult, DhcpResponseInfo, DpuDhcpRelay, DpuDhcpRelayServer};
use crate::machine_state_machine::{LiveState, MachineStateMachine, OsImage, PersistedMachine};
use crate::status::{BmcStatus, DeviceKind, DeviceStatus, DeviceStatusConfig, EndpointStatus};
use crate::{MachineConfig, saturating_add_duration_to_instant};

pub(super) struct DpuMachine {
    mat_id: Uuid,
    // The mat_id of the host that owns this DPU
    host_id: Uuid,
    // Our index within this host
    dpu_index: u8,
    state_machine: MachineStateMachine,

    dpu_info: DpuMachineInfo,
    bmc_control_rx: mpsc::UnboundedReceiver<BmcCommand>,
    paused: bool,
    sleep_until: Instant,
}

impl DpuMachine {
    pub(super) fn from_persisted(
        persisted_dpu_machine: PersistedDpuMachine,
        mat_host: Uuid,
        app_context: Arc<MachineATronContext>,
        config: Arc<MachineConfig>,
        host_dhcp_request_rx: Option<
            mpsc::UnboundedReceiver<oneshot::Sender<DhcpRelayResult<DhcpResponseInfo>>>,
        >,
    ) -> Self {
        let mat_id = persisted_dpu_machine.mat_id;
        let dpu_index = persisted_dpu_machine.dpu_index;
        let (bmc_control_tx, bmc_control_rx) = mpsc::unbounded_channel();

        let dpu_info = DpuMachineInfo {
            hw_type: persisted_dpu_machine.hw_type,
            bmc_mac_address: persisted_dpu_machine.bmc_mac_address,
            host_mac_address: persisted_dpu_machine.host_mac_address,
            oob_mac_address: persisted_dpu_machine.oob_mac_address,
            serial: persisted_dpu_machine.serial.clone(),
            settings: persisted_dpu_machine.settings.clone(),
        };
        let state_machine = MachineStateMachine::from_persisted(
            PersistedMachine::Dpu(persisted_dpu_machine),
            MachineInfo::Dpu(dpu_info.clone()),
            config,
            app_context,
            bmc_control_tx,
            host_dhcp_request_rx.map(|rx| DpuDhcpRelay::DpuEnd(DpuDhcpRelayServer::new(rx))),
            mat_host,
        );
        DpuMachine {
            mat_id,
            dpu_index,
            host_id: mat_host,
            dpu_info,
            state_machine,

            bmc_control_rx,
            paused: true,
            sleep_until: Instant::now(),
        }
    }

    pub(super) fn new(
        hw_type: HardwareType,
        mat_host: Uuid,
        dpu_index: u8,
        app_context: Arc<MachineATronContext>,
        config: Arc<MachineConfig>,
        mac_addr_pool: &mut MacAddressPool,
        host_dhcp_request_rx: Option<
            mpsc::UnboundedReceiver<oneshot::Sender<DhcpRelayResult<DhcpResponseInfo>>>,
        >,
    ) -> Self {
        let mat_id = Uuid::new_v4();
        let (bmc_control_tx, bmc_control_rx) = mpsc::unbounded_channel();

        // Prefer the configured firmware versions, but if any of the version fields are None, use
        // the ones the server wants.
        let firmware_versions = config
            .dpu_firmware_versions
            .clone()
            .unwrap_or_default()
            .fill_missing_from_desired_firmware(
                &app_context.desired_firmware_versions.read().unwrap(),
            );

        let dpu_info = DpuMachineInfo::new(
            hw_type,
            mac_addr_pool,
            DpuSettings {
                nic_mode: config.dpus_in_nic_mode,
                firmware_versions: firmware_versions.into(),
                ..Default::default()
            },
        );
        let state_machine = MachineStateMachine::new(
            MachineInfo::Dpu(dpu_info.clone()),
            config,
            app_context,
            bmc_control_tx,
            None,
            host_dhcp_request_rx.map(|rx| DpuDhcpRelay::DpuEnd(DpuDhcpRelayServer::new(rx))),
            mat_id,
        );
        DpuMachine {
            mat_id,
            dpu_index,
            host_id: mat_host,
            dpu_info,
            state_machine,

            bmc_control_rx,
            paused: true,
            sleep_until: Instant::now(),
        }
    }

    #[instrument(skip_all, fields(mat_host_id = %self.host_id, dpu_index = self.dpu_index))]
    pub(super) fn start(mut self, paused: bool) -> DpuMachineHandle {
        self.paused = paused;
        let (message_tx, mut message_rx) = mpsc::unbounded_channel();
        let mat_id = self.mat_id;
        let dpu_info = self.dpu_info.clone();
        let dpu_index = self.dpu_index;
        let live_state = self.state_machine.live_state.clone();
        let bmc_injection = self.state_machine.bmc_injection_store();
        let join_handle = tokio::task::Builder::new()
            .name(&format!("DPU {}", self.mat_id))
            .spawn(async move {
                loop {
                    // Run the actual iterations in a separate function so that #[instrument] can
                    // create spans with the current values for all fields.
                    if !self.run_iteration(&mut message_rx).await {
                        break;
                    }
                }
            })
            .unwrap();

        DpuMachineHandle(Arc::new(DpuMachineActor {
            message_tx,
            live_state,
            mat_id,
            dpu_info,
            dpu_index,
            bmc_injection,
            join_handle: Mutex::new(Some(join_handle)),
        }))
    }

    #[instrument(skip_all, fields(mat_host_id = %self.host_id, dpu_index = self.dpu_index, state = %self.state_machine, booted_os = %self.state_machine.booted_os()))]
    async fn run_iteration(
        &mut self,
        actor_message_rx: &mut mpsc::UnboundedReceiver<DpuMachineMessage>,
    ) -> bool {
        tokio::select! {
            _ = tokio::time::sleep_until(self.sleep_until.into()) => {},
            Some(cmd) = self.bmc_control_rx.recv() => {
                match cmd {
                    BmcCommand::SetSystemPower {request, reply} => {
                        let response = self.state_machine.set_system_power(request);
                        if let Some(reply) = reply {
                            _ = reply.send(response)
                        }
                    }
                    BmcCommand::DeviceEffect { effect, reply } => {
                        let effect_debug = format!("{effect:?}");
                        let response = match effect {
                            // The DPU's own power drops with the tray's.
                            DeviceEffect::AuxPowerCycle => self.state_machine.apply_aux_power_cycle(),
                            DeviceEffect::ResetBmcToDefaults(update) => {
                                self.state_machine.reset_bmc(update).await
                            },
                        };
                        if let Some(reply) = reply {
                            _ = reply.send(response);
                        } else if let Err(error) = response {
                            tracing::error!(
                                effect = %effect_debug,
                                %error,
                                "Could not apply device effect",
                            );
                        }
                    }
                    BmcCommand::StateRefreshIndication => {
                        self.state_machine.update_live_state();
                    }
                }
            }
            result = actor_message_rx.recv() => {
                let Some(cmd) = result else {
                    tracing::info!("Command channel gone, stopping DPU");
                    return false;
                };
                self.handle_actor_message(cmd);
            }
        }

        let sleep_duration = self.process_state().await;

        self.sleep_until = saturating_add_duration_to_instant(Instant::now(), sleep_duration);
        true
    }

    fn handle_actor_message(&mut self, message: DpuMachineMessage) {
        match message {
            DpuMachineMessage::SetSystemPower { request, reply } => {
                let response = self.state_machine.set_system_power(request);
                if let Some(reply) = reply {
                    _ = reply.send(response);
                }
            }
            DpuMachineMessage::AuxPowerCycle => {
                _ = self
                    .state_machine
                    .apply_aux_power_cycle()
                    .inspect_err(|error| {
                        tracing::error!(%error, "Could not apply DPU AC power cycle");
                    });
            }
            DpuMachineMessage::SetPaused { paused, reply } => {
                if paused {
                    tracing::info!("Pausing state operations");
                    self.paused = true;
                } else {
                    tracing::info!("Resuming state operations");
                    self.paused = false;
                }
                if let Some(reply) = reply {
                    reply.send(()).ok();
                }
            }
        }
    }

    async fn process_state(&mut self) -> Duration {
        if self.paused {
            return Duration::MAX;
        }

        tracing::trace!("state_machine.advance start");
        let result = self.state_machine.advance().await;
        tracing::trace!("state_machine.advance end");

        result
    }

    pub(super) fn dpu_info(&self) -> &DpuMachineInfo {
        &self.dpu_info
    }
}

enum DpuMachineMessage {
    AuxPowerCycle,
    SetSystemPower {
        request: ResourceResetType,
        reply: Option<oneshot::Sender<Result<(), ActionError>>>,
    },
    SetPaused {
        paused: bool,
        reply: Option<oneshot::Sender<()>>,
    },
}

/// DpuMachineActor presents a friendly, actor-style interface for various methods a HostMachine
/// needs to call while a DpuMachine is running.
///
/// This is needed because DpuMachine runs its own control loop inside a Tokio Task, which consumes
/// self, and it is not thread-safe to query the DPU while it's running. Instead, the control loop
/// will poll for any DpuMachineCommands sent to it (in addition to periodically running
/// process_state) and reply to them. DpuMachineHandle abstracts these commands/replies into simple
/// async methods.
#[derive(Debug)]
struct DpuMachineActor {
    message_tx: mpsc::UnboundedSender<DpuMachineMessage>,
    live_state: Arc<RwLock<LiveState>>,
    mat_id: Uuid,
    dpu_info: DpuMachineInfo,
    dpu_index: u8,
    bmc_injection: Arc<InjectionStore>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Debug, Clone)]
pub struct DpuMachineHandle(Arc<DpuMachineActor>);

impl DpuMachineHandle {
    /// Observes live BMC effects without contacting its leased address.
    /// Returns None when the live BMC resource state is unavailable.
    pub fn bmc_effect_snapshot(&self) -> Option<bmc_mock::persistence::BmcEffectSnapshot> {
        self.0
            .live_state
            .read()
            .unwrap()
            .bmc_persistence
            .effect_snapshot()
    }

    pub fn mat_id(&self) -> Uuid {
        self.0.mat_id
    }

    pub fn bmc_ip(&self) -> Option<Ipv4Addr> {
        self.0.live_state.read().unwrap().bmc_ip
    }

    pub fn machine_ip(&self) -> Option<Ipv4Addr> {
        self.0.live_state.read().unwrap().machine_ip
    }

    pub(super) fn tenant_network_config(
        &self,
    ) -> Option<crate::tenant_network::TenantNetworkConfig> {
        self.0
            .live_state
            .read()
            .unwrap()
            .tenant_network_config
            .clone()
    }

    #[cfg(test)]
    pub(super) fn set_control_test_tenant_network(
        &self,
        config: Option<crate::tenant_network::TenantNetworkConfig>,
    ) {
        self.0.live_state.write().unwrap().tenant_network_config = config;
    }

    pub fn observed_machine_id(&self) -> Option<MachineId> {
        self.0
            .live_state
            .read()
            .unwrap()
            .observed_machine_id
            .as_ref()
            .map(ToOwned::to_owned)
    }

    pub(crate) fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        self.0.bmc_injection.clone()
    }

    #[cfg(test)]
    pub(crate) fn for_control_test(mat_id: Uuid, observed_machine_id: Option<MachineId>) -> Self {
        let (message_tx, _message_rx) = mpsc::unbounded_channel();
        let mac = mac_address::MacAddress::new([2, 0, 0, 0, 0, 1]);
        let live_state = LiveState {
            observed_machine_id,
            ..LiveState::default()
        };
        Self(Arc::new(DpuMachineActor {
            message_tx,
            live_state: Arc::new(RwLock::new(live_state)),
            mat_id,
            dpu_info: DpuMachineInfo {
                hw_type: HardwareType::default(),
                bmc_mac_address: mac,
                host_mac_address: mac,
                oob_mac_address: mac,
                serial: "test-dpu".to_string(),
                settings: DpuSettings::default(),
            },
            dpu_index: 0,
            bmc_injection: Arc::new(InjectionStore::new()),
            join_handle: Mutex::new(None),
        }))
    }

    pub fn set_system_power(&self, request: ResourceResetType) -> eyre::Result<()> {
        Ok(self.0.message_tx.send(DpuMachineMessage::SetSystemPower {
            request,
            reply: None,
        })?)
    }

    pub(super) fn aux_power_cycle(&self) -> eyre::Result<()> {
        Ok(self.0.message_tx.send(DpuMachineMessage::AuxPowerCycle)?)
    }

    /// AC power drops applied by this actor, independent of ordinary ForceOff.
    pub fn aux_power_cycle_generation(&self) -> u64 {
        self.0.live_state.read().unwrap().aux_power_cycle_generation
    }

    #[cfg(test)]
    pub(crate) fn power_state(&self) -> nv_redfish::schema::resource::PowerState {
        self.0.live_state.read().unwrap().power_state
    }

    pub fn is_ready(&self) -> bool {
        let live_state = self.0.live_state.read().unwrap();
        // Whether we are up and booted to the agent OS (or if we're nic mode, we don't have to be
        // booted to any OS.)
        live_state.is_up
            && (self.0.dpu_info.settings.nic_mode
                || matches!(live_state.booted_os.0, Some(OsImage::DpuAgent)))
    }

    /// Whether this DPU's BlueField has flipped to NIC mode (a `Mode.Set` applied
    /// on a power cycle). The owning host polls this to converge -- detaching its
    /// DPU DHCP relay -- once a managed DPU becomes a plain NIC.
    pub fn flipped_to_nic_mode(&self) -> bool {
        self.0.live_state.read().unwrap().dpu_flipped_to_nic_mode
    }

    /// The DPU's disk image and BFB install generation as its BMC reports
    /// them, or `None` before the BMC starts. Read-only.
    pub fn dpu_os_status(&self) -> Option<bmc_mock::DpuOsStatus> {
        self.0.live_state.read().unwrap().dpu_os_status()
    }

    /// `is_up` reads the DPU's local up flag from its latest snapshot, regardless of
    /// which OS is booted. Returns an error if the actor has stopped.
    /// It does not read Core's lifecycle state.
    pub fn is_up(&self) -> eyre::Result<bool> {
        eyre::ensure!(!self.0.message_tx.is_closed(), "DPU actor stopped");
        Ok(self.0.live_state.read().unwrap().is_up)
    }

    pub fn status(&self, config: &DeviceStatusConfig) -> DeviceStatus {
        let live_state = self.0.live_state.read().unwrap();
        DeviceStatus {
            mat_id: self.0.mat_id.to_string(),
            device_kind: DeviceKind::Dpu,
            device_id: live_state
                .observed_machine_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| self.0.mat_id.to_string()),
            machine_id: live_state
                .observed_machine_id
                .as_ref()
                .map(ToString::to_string),
            hardware_type: None,
            mat_state: live_state.state_string.map(ToOwned::to_owned),
            power_state: live_state.power_state,
            machine_ip: live_state.machine_ip.map(|ip| ip.to_string()),
            nvos_ip: None,
            infiniband_ports: None,
            bmc: BmcStatus {
                ip: live_state.bmc_ip.map(|ip| ip.to_string()),
                redfish: EndpointStatus::redfish(config),
                ipmi: live_state.ipmi_port.map(EndpointStatus::same_port),
                ssh: live_state.ssh_endpoint_port.map(EndpointStatus::same_port),
            },
            dpus: Vec::new(),
        }
    }

    pub fn pause(&self) -> eyre::Result<()> {
        self.0.message_tx.send(DpuMachineMessage::SetPaused {
            paused: true,
            reply: None,
        })?;
        Ok(())
    }

    /// `pause_and_wait` pauses this DPU and waits for its actor to process the
    /// request. It returns an error if the actor stops or does not acknowledge
    /// within `timeout`; timing out does not cancel the queued pause.
    pub async fn pause_and_wait(&self, timeout: Duration) -> eyre::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.0.message_tx.send(DpuMachineMessage::SetPaused {
            paused: true,
            reply: Some(tx),
        })?;
        tokio::time::timeout(timeout, rx)
            .await
            .wrap_err("timed out waiting for DPU pause")?
            .wrap_err("DPU stopped before acknowledging pause")?;
        Ok(())
    }

    pub fn resume(&self) -> eyre::Result<()> {
        self.0.message_tx.send(DpuMachineMessage::SetPaused {
            paused: false,
            reply: None,
        })?;
        Ok(())
    }

    pub fn persisted(&self) -> PersistedDpuMachine {
        let live_state = self.0.live_state.read().unwrap();
        let installed_os = live_state.installed_os;
        let bmc_state = live_state.bmc_persistence.persisted();
        drop(live_state);
        PersistedDpuMachine {
            mat_id: self.0.mat_id,
            hw_type: self.0.dpu_info.hw_type,
            bmc_mac_address: self.0.dpu_info.bmc_mac_address,
            host_mac_address: self.0.dpu_info.host_mac_address,
            oob_mac_address: self.0.dpu_info.oob_mac_address,
            serial: self.0.dpu_info.serial.clone(),
            settings: self.0.dpu_info.settings.clone(),
            installed_os,
            dpu_index: self.0.dpu_index,
            bmc_state,
        }
    }

    pub fn abort(&self) {
        _ = self.abort_task();
    }

    pub(crate) fn abort_task(&self) -> Option<JoinHandle<()>> {
        let join_handle = self.0.join_handle.lock().unwrap().take()?;
        join_handle.abort();
        Some(join_handle)
    }

    pub async fn abort_and_wait(&self) -> eyre::Result<()> {
        if let Some(join_handle) = self.abort_task() {
            match join_handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod actor_handle_tests {
    use bmc_mock::mac_address_pool::PoolConfig;
    use futures::poll;

    use super::*;

    fn dpu() -> DpuMachine {
        let context = MachineATronContext::for_test();
        let config = context.app_config.machines["config"].clone();
        let mac = mac_address::MacAddress::new([2, 0, 0, 0, 0, 0]);
        let pool_config = PoolConfig::new(mac, 24).unwrap();
        DpuMachine::new(
            HardwareType::default(),
            Uuid::new_v4(),
            1,
            context,
            config,
            &mut MacAddressPool::new_pool(pool_config),
            None,
        )
    }

    fn handle_with_messages() -> (DpuMachineHandle, mpsc::UnboundedReceiver<DpuMachineMessage>) {
        let mut handle = DpuMachineHandle::for_control_test(Uuid::new_v4(), None);
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        Arc::get_mut(&mut handle.0).unwrap().message_tx = message_tx;
        (handle, message_rx)
    }

    #[tokio::test]
    async fn stopped_dpu_rejects_retained_up_snapshot() {
        let handle = dpu().start(true);
        handle.0.live_state.write().unwrap().is_up = true;
        assert!(handle.is_up().unwrap());

        handle.abort_and_wait().await.unwrap();

        assert!(handle.0.live_state.read().unwrap().is_up);
        assert!(
            handle
                .is_up()
                .unwrap_err()
                .to_string()
                .contains("DPU actor stopped")
        );
    }

    #[tokio::test]
    async fn dpu_pause_acknowledges_applied_pause() {
        let mut dpu = dpu();
        dpu.paused = false;
        let (handle, mut messages) = handle_with_messages();
        let pause = handle.pause_and_wait(Duration::from_secs(5));
        tokio::pin!(pause);

        assert!(poll!(&mut pause).is_pending());
        let message = messages.try_recv().expect("DPU pause was not queued");
        assert!(!dpu.paused);
        assert!(poll!(&mut pause).is_pending());

        dpu.handle_actor_message(message);

        assert!(dpu.paused);
        pause.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn dpu_pause_timeout_preserves_queued_pause() {
        let mut dpu = dpu();
        dpu.paused = false;
        let (handle, mut messages) = handle_with_messages();
        let timeout = Duration::from_secs(5);
        let pause = handle.pause_and_wait(timeout);
        tokio::pin!(pause);
        assert!(poll!(&mut pause).is_pending());

        tokio::time::advance(timeout).await;
        let error = pause.await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("timed out waiting for DPU pause")
        );
        assert!(!dpu.paused);

        let message = messages.try_recv().expect("timed-out DPU pause was lost");
        dpu.handle_actor_message(message);
        assert!(dpu.paused);
    }

    #[tokio::test]
    async fn dpu_pause_fails_when_actor_stops() {
        let (handle, messages) = handle_with_messages();
        let pause = handle.pause_and_wait(Duration::from_secs(5));
        tokio::pin!(pause);
        assert!(poll!(&mut pause).is_pending());

        drop(messages);

        let error = pause.await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DPU stopped before acknowledging pause")
        );
        assert!(handle.pause_and_wait(Duration::from_secs(5)).await.is_err());
    }
}
