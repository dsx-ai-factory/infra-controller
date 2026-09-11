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
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bmc_mock::injection::InjectionStore;
use bmc_mock::mac_address_pool::{MacAddressPool, PoolConfig as MacAddressPoolConfig};
use bmc_mock::{
    BmcCommand, HostFirmwareVersions, HostMachineInfo, MachineInfo, SetSystemPowerResult,
    SystemPowerControl,
};
use carbide_utils::test_support::certs::create_random_self_signed_cert;
use carbide_uuid::machine::MachineId;
use eyre::Context;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Interval;
use tracing::instrument;
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::config::{self, MachineATronContext, MachineConfig, PersistedDevice};
use crate::dhcp_wrapper::{DhcpRelayResult, DhcpResponseInfo, DpuDhcpRelay};
use crate::dpu_machine::{DpuMachine, DpuMachineHandle};
use crate::machine_state_machine::{LiveState, MachineStateMachine, PersistedMachine};
use crate::status::{
    BmcStatus, DeviceKind, DeviceStatus, DeviceStatusConfig, EndpointStatus, InfinibandPortStatus,
};
use crate::{Guid, InfinibandPortState, saturating_add_duration_to_instant};

pub(super) struct HostMachine {
    mat_id: Uuid,
    machine_config_section: String,
    host_info: HostMachineInfo,
    app_context: Arc<MachineATronContext>,
    live_state: Arc<RwLock<LiveState>>,
    state_machine: MachineStateMachine,
    api_state: String,

    dpus: Vec<DpuMachineHandle>,

    bmc_control_rx: mpsc::UnboundedReceiver<BmcCommand>,
    // This will be populated with callers waiting for the host to be MachineUp/Ready
    state_waiters: HashMap<String, Vec<oneshot::Sender<()>>>,
    paused: bool,
    api_refresh_interval: Interval,
    sleep_until: Instant,
}

/// Return true when `entry` carries firmware versions for hosts of the given
/// hardware type.  Vendor and model are compared case-insensitively against
/// the Redfish-reported values stored in the API's desired-firmware table.
fn firmware_entry_matches_host_hw_type(
    hw_type: bmc_mock::HardwareType,
    entry: &rpc::forge::DesiredFirmwareVersionEntry,
) -> bool {
    use bmc_mock::HardwareType::*;
    let vendor = entry.vendor.to_lowercase();
    let model = entry.model.to_lowercase().replace('-', " ");
    match hw_type {
        DellPowerEdgeR750 => vendor.contains("dell") && model.contains("r750"),
        DellPowerEdgeR760Bf4 => vendor.contains("dell") && model.contains("r760"),
        WiwynnGB200Nvl => vendor.contains("wiwynn") && model.contains("gb200"),
        LenovoGB300Nvl => vendor.contains("lenovo") && model.contains("gb300"),
        NvidiaDgxGb300 => vendor.contains("nvidia") && model.contains("gb300"),
        NvidiaDgxVr => vendor.contains("nvidia") && model.contains("dgx vr"),
        SupermicroGb300Nvl => vendor.contains("supermicro") && model.contains("gb300"),
        NvidiaDgxH100 => vendor.contains("nvidia") && model.contains("h100"),
        HpeProliantDl380aGen11 => {
            (vendor.contains("hpe") || vendor.contains("hewlett")) && model.contains("proliant")
        }
        // All other types (generic, power shelves, switches) do not simulate host
        // firmware upgrades — no matching entry exists for them.
        _ => false,
    }
}

/// Find the desired host firmware for `hw_type` from the API-configured entries.
/// Both bmc and uefi versions are read from the **same** entry so they always
/// come from a consistent hardware-specific record.
///
/// Uses `hw_type.host_bmc_version_key()` to look up the BMC component key,
/// since DGX H100 uses `"combinedbmcuefi"` rather than `"bmc"`.
fn desired_host_firmware(
    hw_type: bmc_mock::HardwareType,
    app_context: &MachineATronContext,
) -> Option<HostFirmwareVersions> {
    let versions = app_context.desired_firmware_versions.read().unwrap();
    let entry = versions
        .iter()
        .find(|e| firmware_entry_matches_host_hw_type(hw_type, e))?;
    let bmc = entry
        .component_versions
        .get(hw_type.host_bmc_version_key())
        .cloned();
    let uefi = entry.component_versions.get("uefi").cloned();
    if bmc.is_some() || uefi.is_some() {
        Some(HostFirmwareVersions { bmc, uefi })
    } else {
        None
    }
}

impl HostMachine {
    pub(super) fn from_persisted(
        persisted_device: PersistedDevice,
        machine_config_section: String,
        app_context: Arc<MachineATronContext>,
        config: Arc<MachineConfig>,
        hw_mac_addr_pool: MacAddressPoolConfig,
    ) -> Self {
        let mat_id = persisted_device.mat_id;
        let (bmc_control_tx, bmc_control_rx) = mpsc::unbounded_channel();
        let (dpu_dhcp_tx, dpu_dhcp_rx) =
            mpsc::unbounded_channel::<oneshot::Sender<DhcpRelayResult<DhcpResponseInfo>>>();
        let mut dpu_dhcp_rx = Some(dpu_dhcp_rx);
        let dpus_in_nic_mode = config.dpus_in_nic_mode;

        let dpu_machines = persisted_device
            .dpus
            .iter()
            .map(|dpu| {
                DpuMachine::from_persisted(
                    dpu.clone(),
                    persisted_device.mat_id,
                    app_context.clone(),
                    config.clone(),
                    if dpus_in_nic_mode {
                        None
                    } else {
                        dpu_dhcp_rx.take()
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut host_info = HostMachineInfo {
            hw_type: persisted_device.hw_type,
            rack_placement: config.rack_placement,
            bmc_mac_address: persisted_device.bmc_mac_address,
            serial: persisted_device.serial.clone(),
            dpus: persisted_device
                .dpus
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            non_dpu_mac_address: persisted_device.non_dpu_mac_address,
            nvos_mac_addresses: persisted_device.nvos_mac_addresses.clone(),
            switch_serial_number: persisted_device.switch_serial_number.clone(),
            hw_mac_addr_pool,
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        };
        // Restore the active firmware inventory persisted from the previous run so
        // the mock starts at the versions last observed by carbide rather than the
        // operator-configured starting point.  Falls back to the config's initial
        // versions when no persisted inventory is available (first run after upgrade).
        host_info.initial_host_firmware = persisted_device
            .active_host_firmware
            .clone()
            .or_else(|| config.host_firmware_versions.clone());
        host_info.desired_host_firmware =
            desired_host_firmware(persisted_device.hw_type, &app_context);
        let dpus = dpu_machines
            .into_iter()
            .map(|d| d.start(true))
            .collect::<Vec<_>>();
        let state_machine = MachineStateMachine::from_persisted(
            PersistedMachine::Host(persisted_device),
            MachineInfo::Host(host_info.clone()),
            config,
            app_context.clone(),
            bmc_control_tx,
            if !dpus.is_empty() && !dpus_in_nic_mode {
                Some(DpuDhcpRelay::HostEnd(dpu_dhcp_tx))
            } else {
                None
            },
            mat_id,
        );

        HostMachine {
            mat_id,
            machine_config_section,
            host_info,
            live_state: state_machine.live_state.clone(),
            state_machine,
            dpus,
            api_state: "Unknown".to_owned(),

            bmc_control_rx,
            state_waiters: HashMap::new(),
            paused: true,
            sleep_until: Instant::now(),
            api_refresh_interval: tokio::time::interval(
                app_context.app_config.api_refresh_interval,
            ),
            app_context,
        }
    }

    pub(super) fn new(
        app_context: Arc<MachineATronContext>,
        machine_config_section: String,
        config: Arc<MachineConfig>,
        mac_pool: &mut MacAddressPool,
        hw_pool_config: MacAddressPoolConfig,
    ) -> Self {
        let mat_id = Uuid::new_v4();
        let (bmc_control_tx, bmc_control_rx) = mpsc::unbounded_channel();
        let (dpu_dhcp_tx, dpu_dhcp_rx) =
            mpsc::unbounded_channel::<oneshot::Sender<DhcpRelayResult<DhcpResponseInfo>>>();
        let mut dpu_dhcp_rx = Some(dpu_dhcp_rx);
        let dpus_in_nic_mode = config.dpus_in_nic_mode;

        let num_dpu = config
            .hw_type
            .fixed_number_of_dpu()
            .unwrap_or(config.dpu_per_host_count as u8);
        let dpu_machines = (1..=num_dpu)
            .map(|index| {
                DpuMachine::new(
                    config.hw_type,
                    mat_id,
                    index,
                    app_context.clone(),
                    config.clone(),
                    mac_pool,
                    if dpus_in_nic_mode {
                        None
                    } else {
                        dpu_dhcp_rx.take()
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut host_info = HostMachineInfo::new(
            config.hw_type,
            dpu_machines.iter().map(|d| d.dpu_info().clone()).collect(),
            mac_pool,
            hw_pool_config,
        );
        host_info.rack_placement = config.rack_placement;
        host_info.initial_host_firmware = config.host_firmware_versions.clone();
        host_info.desired_host_firmware = desired_host_firmware(config.hw_type, &app_context);
        let dpus = dpu_machines
            .into_iter()
            .map(|d| d.start(true))
            .collect::<Vec<_>>();
        let state_machine = MachineStateMachine::new(
            MachineInfo::Host(host_info.clone()),
            config,
            app_context.clone(),
            bmc_control_tx,
            Some(create_random_self_signed_cert()),
            if !dpus.is_empty() && !dpus_in_nic_mode {
                Some(DpuDhcpRelay::HostEnd(dpu_dhcp_tx))
            } else {
                None
            },
            mat_id,
        );

        HostMachine {
            mat_id,
            machine_config_section,
            host_info,
            live_state: state_machine.live_state.clone(),
            state_machine,
            dpus,
            api_state: "Unknown".to_owned(),

            bmc_control_rx,
            state_waiters: HashMap::new(),
            paused: true,
            sleep_until: Instant::now(),
            api_refresh_interval: tokio::time::interval(
                app_context.app_config.api_refresh_interval,
            ),
            app_context,
        }
    }

    #[instrument(skip_all, fields(mat_host_id = %self.mat_id))]
    pub(crate) fn start(mut self, paused: bool) -> MachineHandle {
        self.paused = paused;
        let (message_tx, mut message_rx) = mpsc::unbounded_channel();
        let live_state = self.live_state.clone();
        let mat_id = self.mat_id;
        let host_info = self.host_info.clone();
        let dpus = self.dpus.clone();
        let machine_config_section = self.machine_config_section.clone();
        let bmc_injection = self.state_machine.bmc_injection_store();

        if !paused {
            self.resume_dpus();
        }

        let join_handle = tokio::task::Builder::new()
            .name(&format!("Host {}", self.mat_id))
            .spawn({
                let message_tx = message_tx.clone();
                async move {
                    loop {
                        if !self.run_iteration(&mut message_rx, &message_tx).await {
                            break;
                        }
                    }
                }
            })
            .unwrap();

        MachineHandle(Arc::new(HostMachineActor {
            message_tx,
            live_state,
            mat_id,
            host_info,
            dpus,
            machine_config_section,
            bmc_injection,

            join_handle: Mutex::new(Some(join_handle)),
        }))
    }

    #[instrument(skip_all, fields(mat_host_id = %self.mat_id, api_state = %self.api_state, state = %self.state_machine, booted_os = %self.state_machine.booted_os()))]
    async fn run_iteration(
        &mut self,
        actor_message_rx: &mut mpsc::UnboundedReceiver<HostMachineMessage>,
        actor_message_tx: &mpsc::UnboundedSender<HostMachineMessage>,
    ) -> bool {
        // If the host is up, and if anyone is waiting for the current state to be
        // reached, notify them.
        if self.live_state.read().unwrap().is_up
            && let Some(waiters) = self.state_waiters.remove(&self.api_state)
        {
            for waiter in waiters.into_iter() {
                _ = waiter.send(());
            }
        }

        tokio::select! {
            _ = tokio::time::sleep_until(self.sleep_until.into()) => {}
            _ = self.api_refresh_interval.tick() => {
                // Pick up any change to the API-configured firmware targets
                // before refreshing the API state (issue #4688).
                self.refresh_desired_host_firmware();
                // Wake up to refresh the API state
                if DeviceKind::from(self.host_info.hw_type) == DeviceKind::Machine
                    && let Some(machine_id) = self.live_state.read().unwrap().observed_machine_id
                {
                    let actor_message_tx = actor_message_tx.clone();
                    self.app_context.api_throttler.get_machine(machine_id, move |machine| {
                        if let Some(machine) = machine {
                            // Write the API state back using the actor channel, since we can't just write to self
                            _ = actor_message_tx.send(HostMachineMessage::SetApiState(machine.state));
                        }
                    })
                }
                return true; // go back to sleeping
            }
            result = actor_message_rx.recv() => {
                let Some(cmd) = result else {
                    tracing::info!("Command channel gone, stopping Host");
                    return false;
                };
                match self.handle_actor_message(cmd) {
                    HandleMessageResult::ContinuePolling => return true,
                    HandleMessageResult::ProcessStateNow => {},
                }
            }
            Some(cmd) = self.bmc_control_rx.recv() => {
                tracing::debug!(
                    command = ?cmd,
                    "Received host power command",
                );
                match cmd {
                    BmcCommand::SetSystemPower { request, reply } => {
                        let response = self.set_system_power(request);
                        if let Some(reply) = reply {
                            _ = reply.send(response);
                        }
                    }
                    BmcCommand::StateRefreshIndication => {
                        self.state_machine.update_live_state();
                    }
                }
                // continue to process_state
            }
        }

        let sleep_duration = self.process_state().await;

        self.sleep_until = saturating_add_duration_to_instant(Instant::now(), sleep_duration);
        true
    }

    async fn process_state(&mut self) -> Duration {
        if self.paused {
            return Duration::MAX;
        }

        self.maybe_converge_after_dpu_flip();

        let sleep_duration = self.state_machine.advance().await;
        tracing::trace!("state_machine.advance end");
        sleep_duration
    }

    /// When a managed DPU flips to NIC mode it becomes a plain NIC, so this host
    /// must stop relaying its data-plane DHCP through the DPU and instead DHCP
    /// directly (on the same former-DPU host MAC, so a retained boot interface
    /// still matches). Detect the flip through the DPU handle and detach the
    /// relay once; the host then re-ingests as a zero-managed-DPU NIC-mode
    /// machine on its next power cycle.
    fn maybe_converge_after_dpu_flip(&mut self) {
        if self.state_machine.has_dpu_dhcp_relay()
            && self.dpus.iter().any(|dpu| dpu.flipped_to_nic_mode())
        {
            tracing::info!(
                "a managed DPU flipped to NIC mode; converging the host to zero managed DPUs (detaching its DPU DHCP relay and dropping the DPU from its reported inventory) so it re-ingests as a NIC-mode host"
            );
            self.state_machine.detach_dpu_dhcp_relay();
            self.state_machine.drop_managed_dpus();
        }
    }

    /// Re-derive this host's firmware targets from the (periodically
    /// refreshed) API-wide desired versions and, when they changed, re-stage
    /// the live BMC mock's pending upgrades. No-op while the targets are
    /// unchanged, so the per-tick cost is one map lookup.
    fn refresh_desired_host_firmware(&mut self) {
        let desired = desired_host_firmware(self.host_info.hw_type, &self.app_context);
        if desired == self.host_info.desired_host_firmware {
            return;
        }
        tracing::info!(
            machine = %self.machine_config_section,
            previous = ?self.host_info.desired_host_firmware,
            new = ?desired,
            "Desired host firmware changed; re-staging pending upgrades",
        );
        self.host_info.desired_host_firmware = desired.clone();
        self.state_machine.set_desired_host_firmware(desired);
    }

    fn handle_actor_message(&mut self, message: HostMachineMessage) -> HandleMessageResult {
        match message {
            HostMachineMessage::WaitUntilMachineUpWithApiState(state, reply) => {
                if let Some(state_waiters) = self.state_waiters.get_mut(&state) {
                    state_waiters.push(reply);
                } else {
                    self.state_waiters.insert(state, vec![reply]);
                }
                HandleMessageResult::ContinuePolling
            }
            HostMachineMessage::SetPaused(value) => {
                if value {
                    self.pause()
                } else {
                    self.resume()
                }
                HandleMessageResult::ProcessStateNow
            }
            HostMachineMessage::GetApiState(reply) => {
                _ = reply.send(self.api_state.clone());
                HandleMessageResult::ContinuePolling
            }
            HostMachineMessage::SetApiState(api_state) => {
                self.api_state = api_state.clone();
                self.live_state.write().unwrap().api_state = api_state;
                HandleMessageResult::ContinuePolling
            }
        }
    }

    fn set_system_power(&mut self, request: SystemPowerControl) -> SetSystemPowerResult {
        tracing::debug!(?request, "Received host system-power request",);

        match request {
            // Force-restart does not restart DPUs
            SystemPowerControl::ForceRestart => {}
            // Other power actions happen on the DPUs too (power cycle, force-off, etc.)
            _ => {
                // Graceful restart might not restart DPUs if an OS is running (let's emulate that)
                if matches!(request, SystemPowerControl::GracefulRestart)
                    && self.live_state.read().unwrap().booted_os.0.is_some()
                {
                    tracing::debug!(
                        "Got graceful restart when host is booted to an OS, will not reboot DPUs"
                    );
                } else {
                    for (dpu_index, dpu) in self.dpus.iter_mut().enumerate() {
                        _ = dpu.set_system_power(request).inspect_err(|e| {
                            tracing::error!(
                                error = %e,
                                dpu_index,
                                "Could not send power request to DPU",
                            )
                        });
                    }
                }
            }
        }
        self.state_machine.set_system_power(request)
    }

    fn pause(&mut self) {
        let was_paused = self.paused;
        self.paused = true;
        if !was_paused {
            tracing::info!("Pausing state operations");
            for dpu in &self.dpus {
                _ = dpu.pause().inspect_err(|e| {
                    tracing::error!(error=%e, "Could not pause DPU when pausing host");
                });
            }
        }
    }

    fn resume(&mut self) {
        let was_paused = self.paused;
        self.paused = false;
        if was_paused {
            tracing::info!("Resuming state operations");
            self.resume_dpus();
        }
    }

    fn resume_dpus(&self) {
        for dpu in &self.dpus {
            _ = dpu.resume().inspect_err(
                |e| tracing::error!(error=%e, "Could not resume DPU when resuming Host"),
            );
        }
    }
}

// Shared with DpuMachine
pub(super) enum HandleMessageResult {
    ContinuePolling,
    ProcessStateNow,
}

enum HostMachineMessage {
    GetApiState(oneshot::Sender<String>),
    WaitUntilMachineUpWithApiState(String, oneshot::Sender<()>),
    SetPaused(bool),
    SetApiState(String),
}

#[derive(Debug)]
struct HostMachineActor {
    message_tx: mpsc::UnboundedSender<HostMachineMessage>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    live_state: Arc<RwLock<LiveState>>,
    mat_id: Uuid,
    host_info: HostMachineInfo,
    dpus: Vec<DpuMachineHandle>,
    machine_config_section: String,
    bmc_injection: Arc<InjectionStore>,
}

#[derive(Debug, Clone)]
pub(crate) struct MachineHandle(Arc<HostMachineActor>);

impl MachineHandle {
    #[cfg(test)]
    pub(crate) fn for_control_test(dpus: Vec<DpuMachineHandle>, ipmi_port: Option<u16>) -> Self {
        Self::for_control_test_in_section(dpus, ipmi_port, "test")
    }

    #[cfg(test)]
    pub(crate) fn for_control_test_in_section(
        dpus: Vec<DpuMachineHandle>,
        ipmi_port: Option<u16>,
        machine_config_section: &str,
    ) -> Self {
        let (message_tx, _message_rx) = mpsc::unbounded_channel();
        let mac = mac_address::MacAddress::new([2, 0, 0, 0, 0, 2]);
        let live_state = LiveState {
            ipmi_port,
            ..LiveState::default()
        };
        Self(Arc::new(HostMachineActor {
            message_tx,
            join_handle: Mutex::new(None),
            live_state: Arc::new(RwLock::new(live_state)),
            mat_id: Uuid::new_v4(),
            host_info: HostMachineInfo {
                hw_type: Default::default(),
                rack_placement: None,
                bmc_mac_address: mac,
                serial: "test-host".to_string(),
                dpus: Vec::new(),
                non_dpu_mac_address: None,
                nvos_mac_addresses: Vec::new(),
                switch_serial_number: None,
                hw_mac_addr_pool: MacAddressPoolConfig::new(mac, 24).unwrap(),
                delta_psu_power: None,
                initial_host_firmware: None,
                desired_host_firmware: None,
            },
            dpus,
            machine_config_section: machine_config_section.to_string(),
            bmc_injection: Arc::new(InjectionStore::new()),
        }))
    }

    #[cfg(test)]
    pub(crate) fn with_control_test_ssh_endpoint(self, port: u16) -> Self {
        self.0.live_state.write().unwrap().ssh_endpoint_port = Some(port);
        self
    }

    pub(super) fn mat_id(&self) -> Uuid {
        self.0.mat_id
    }

    pub(super) fn observed_machine_id(&self) -> Option<MachineId> {
        self.0
            .live_state
            .read()
            .unwrap()
            .observed_machine_id
            .as_ref()
            .map(|m| m.to_owned())
    }

    pub(super) async fn api_state(&self) -> eyre::Result<String> {
        let (tx, rx) = oneshot::channel();
        self.0
            .message_tx
            .send(HostMachineMessage::GetApiState(tx))?;
        Ok(rx.await?)
    }

    pub(crate) fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        self.0.bmc_injection.clone()
    }

    pub(super) async fn wait_until_machine_up_with_api_state(
        &self,
        state: &str,
        timeout: Duration,
    ) -> eyre::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.0
            .message_tx
            .send(HostMachineMessage::WaitUntilMachineUpWithApiState(
                state.to_owned(),
                tx,
            ))?;
        tokio::time::timeout(timeout, rx)
            .await
            .wrap_err_with(|| format!("timed out waiting for machine up with state {state}"))?
            .wrap_err_with(|| format!("machine stopped while waiting for state {state}"))?;
        Ok(())
    }

    pub(super) fn pause(&self) -> eyre::Result<()> {
        self.0
            .message_tx
            .send(HostMachineMessage::SetPaused(true))?;
        Ok(())
    }

    pub(super) fn resume(&self) -> eyre::Result<()> {
        self.0
            .message_tx
            .send(HostMachineMessage::SetPaused(false))?;
        Ok(())
    }

    pub(super) fn host_info(&self) -> &HostMachineInfo {
        &self.0.host_info
    }

    pub(super) fn machine_config_section(&self) -> &str {
        &self.0.machine_config_section
    }

    pub(super) fn set_infiniband_port_state(
        &self,
        guid: Guid,
        state: InfinibandPortState,
    ) -> eyre::Result<()> {
        let mut live_state = self.0.live_state.write().unwrap();
        let port_state = live_state
            .infiniband_port_states
            .get_mut(&guid)
            .ok_or_else(|| eyre::eyre!("infiniband port {guid} not found"))?;
        *port_state = state;
        Ok(())
    }

    pub(super) fn status(&self, config: &DeviceStatusConfig) -> DeviceStatus {
        let live_state = self.0.live_state.read().unwrap();
        let mut infiniband_ports = live_state
            .infiniband_port_states
            .iter()
            .map(|(&guid, &state)| InfinibandPortStatus { guid, state })
            .collect::<Vec<_>>();
        infiniband_ports.sort_by_key(|port| port.guid);
        DeviceStatus {
            mat_id: self.0.mat_id.to_string(),
            device_kind: DeviceKind::Machine,
            device_id: live_state
                .observed_machine_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| self.0.mat_id.to_string()),
            machine_id: live_state
                .observed_machine_id
                .as_ref()
                .map(ToString::to_string),
            hardware_type: Some(self.0.host_info.hw_type),
            mat_state: live_state.state_string.map(ToOwned::to_owned),
            api_state: live_state.api_state.clone(),
            power_state: live_state.power_state.to_string(),
            machine_ip: live_state.machine_ip.map(|ip| ip.to_string()),
            nvos_ip: None,
            infiniband_ports: (!infiniband_ports.is_empty()).then_some(infiniband_ports),
            bmc: BmcStatus {
                ip: live_state.bmc_ip.map(|ip| ip.to_string()),
                redfish: EndpointStatus::redfish(config),
                ipmi: live_state.ipmi_port.map(EndpointStatus::same_port),
                ssh: live_state.ssh_endpoint_port.map(EndpointStatus::same_port),
            },
            dpus: self.0.dpus.iter().map(|dpu| dpu.status(config)).collect(),
        }
    }

    pub(super) fn persisted(&self) -> PersistedDevice {
        let live_state = self.0.live_state.read().unwrap();
        PersistedDevice {
            hw_type: self.0.host_info.hw_type,
            mat_id: self.0.mat_id,
            machine_config_section: self.0.machine_config_section.clone(),
            bmc_mac_address: self.0.host_info.bmc_mac_address,
            serial: self.0.host_info.serial.clone(),
            dpus: self.0.dpus.iter().map(|d| d.persisted()).collect(),
            non_dpu_mac_address: self.0.host_info.non_dpu_mac_address,
            nvos_mac_addresses: self.0.host_info.nvos_mac_addresses.clone(),
            switch_serial_number: self.0.host_info.switch_serial_number.clone(),
            observed_machine_id: live_state.observed_machine_id,
            installed_os: live_state.installed_os,
            tpm_ek_certificate: live_state.tpm_ek_certificate.clone(),
            hw_mac_addr_pool: Some(config::MacAddressPoolConfig {
                base: self.0.host_info.hw_mac_addr_pool.base(),
                host_bits: self.0.host_info.hw_mac_addr_pool.host_bits(),
            }),
            active_host_firmware: live_state.active_host_firmware.clone(),
        }
    }

    pub(super) fn dpus(&self) -> &[DpuMachineHandle] {
        &self.0.dpus
    }

    pub(super) async fn delete_from_api(self, api_client: ApiClient) -> eyre::Result<()> {
        let delete_by = match self
            .0
            .live_state
            .read()
            .unwrap()
            .observed_machine_id
            .as_ref()
        {
            Some(machine_id) => {
                tracing::info!(
                    %machine_id,
                    "Attempting to delete machine from database",
                );
                machine_id.to_string()
            }
            None => {
                // force_delete_machine also supports sending MAC address (which could break if there is 0 DPUs on this host)
                match self.0.host_info.system_mac_address() {
                    Some(mac) => {
                        tracing::info!(
                            mac_address = %mac,
                            "Attempting to delete machine from database",
                        );
                        mac.to_string()
                    }
                    None => {
                        tracing::info!(
                            "Not deleting machine as we have not seen a machine ID for it, and it has no known MAC addresses (no DPUs)",
                        );
                        return Ok(());
                    }
                }
            }
        };

        api_client.force_delete_machine(delete_by).await?;
        Ok(())
    }

    pub(super) fn abort(&self) {
        for dpu in &self.0.dpus {
            dpu.abort();
        }
        if let Some(join_handle) = self.0.join_handle.lock().unwrap().take() {
            join_handle.abort();
        }
    }

    pub(super) async fn abort_and_wait(&self) -> eyre::Result<()> {
        let mut join_handles = self
            .0
            .dpus
            .iter()
            .filter_map(DpuMachineHandle::abort_task)
            .collect::<Vec<_>>();
        if let Some(join_handle) = self.0.join_handle.lock().unwrap().take() {
            join_handle.abort();
            join_handles.push(join_handle);
        }

        for join_handle in join_handles {
            match join_handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) fn bmc_ssh_host_pubkey(&self) -> Option<String> {
        self.0.live_state.read().unwrap().ssh_host_key.clone()
    }

    pub(super) fn bmc_ip(&self) -> Option<Ipv4Addr> {
        self.0.live_state.read().unwrap().bmc_ip
    }
}
