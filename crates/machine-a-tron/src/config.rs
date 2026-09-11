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
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bmc_mock::mac_address_pool::MacAddressPool;
use bmc_mock::{
    DpuMachineInfo, DpuSettings, HardwareType, HostFirmwareVersions, RackInfo, RackPlacement,
    RackType,
};
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackId, RackProfileId};
use clap::Parser;
use duration_str::deserialize_duration;
use eyre::Context;
use mac_address::MacAddress;
use rpc::forge::DesiredFirmwareVersionEntry;
use rpc::forge_tls_client::ForgeClientConfig;
use rpc::protos::forge_api_client::ForgeApiClient;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ufm_mock::UfmMockConfig;
use uuid::Uuid;

use crate::BmcMockRegistry;
use crate::api_client::ApiClient;
use crate::api_throttler::ApiThrottler;
use crate::lifecycle_timings::LifecycleTimingOverrides;
use crate::machine_state_machine::OsImage;
use crate::rack::{RackMemberRegistration, RackRegistration};

#[derive(Parser, Debug, Serialize, Deserialize)]
#[clap(name = "machine-sim")]
pub struct MachineATronArgs {
    #[clap(long, env = "FORGE_ROOT_CA_PATH")]
    #[clap(
        help = "Default to FORGE_ROOT_CA_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub forge_root_ca_path: Option<String>,

    #[clap(long, env = "CLIENT_CERT_PATH")]
    #[clap(
        help = "Default to CLIENT_CERT_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub client_cert_path: Option<String>,

    #[clap(long, env = "CLIENT_KEY_PATH")]
    #[clap(
        help = "Default to CLIENT_KEY_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub client_key_path: Option<String>,

    #[clap(
        help = "Machine-A-Tron config file",
        env = "MACHINE_A_TRON_CONFIG_PATH"
    )]
    pub config_file: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MachineConfig {
    #[serde(default)]
    pub rack_id: Option<RackId>,
    #[serde(skip)]
    pub rack_placement: Option<RackPlacement>,
    #[serde(default = "default_hardware_type")]
    pub hw_type: HardwareType,
    pub host_count: u32,
    pub dpu_per_host_count: u32,
    /// Deprecated: superseded by platform-specific defaults in `PlatformTimingProfile`.
    /// Still parsed so existing configs remain valid; no longer used by the lifecycle FSM.
    #[serde(default = "default_dpu_reboot_delay")]
    pub dpu_reboot_delay: u64,
    /// Deprecated: superseded by platform-specific defaults in `PlatformTimingProfile`.
    /// Still parsed so existing configs remain valid; no longer used by the lifecycle FSM.
    #[serde(default = "default_host_reboot_delay")]
    pub host_reboot_delay: u64,
    /// Per-field timing overrides applied on top of the platform defaults before
    /// `acceleration_factor` is applied.  Absent fields keep the platform default.
    #[serde(default)]
    pub timing_overrides: Option<LifecycleTimingOverrides>,
    /// Multiplier applied to all resolved lifecycle durations after overrides.
    /// Default `1.0` = real-time platform values.  Set to e.g. `0.05` for 20× faster CI.
    #[serde(default = "default_acceleration_factor")]
    pub acceleration_factor: f64,
    #[serde(
        default = "default_scout_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub scout_run_interval: Duration,
    /// Delay before retrying a failed DiscoverMachine request. The default matches the
    /// production DPU agent; local development can override it independently of the MAT work loop.
    #[serde(
        default = "default_discovery_retry_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub discovery_retry_interval: Duration,
    /// Relay address for BMC DHCP traffic.
    #[serde(alias = "oob_dhcp_relay_address")]
    pub bmc_dhcp_relay_address: Ipv4Addr,
    /// Shared Underlay relay for DPU OOB boot and switch NVOS DHCP traffic.
    #[serde(alias = "admin_dhcp_relay_address")]
    pub underlay_dhcp_relay_address: Ipv4Addr,
    /// Relay address used when a host DHCPs directly through a plain NIC rather than a managed DPU.
    /// If omitted, direct host DHCP falls back to `underlay_dhcp_relay_address` for compatibility.
    #[serde(default)]
    pub host_inband_dhcp_relay_address: Option<Ipv4Addr>,

    #[serde(
        default = "default_run_interval_working",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_working: Duration,
    #[serde(
        default = "default_run_interval_idle",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_idle: Duration,
    #[serde(
        default = "default_network_status_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub network_status_run_interval: Duration,
    /// If true, DPUs will run in "nic mode" and will not PXE boot, and their BMC JSON will reflect as such
    #[serde(default)]
    pub dpus_in_nic_mode: bool,

    /// What firmware versions to report for DPUs in this host
    #[serde(default)]
    pub dpu_firmware_versions: Option<DpuFirmwareVersions>,

    /// Initial host BMC / UEFI firmware versions to report in FirmwareInventory.
    /// carbide will detect that these are older than the desired versions and
    /// trigger an upgrade.  After the simulated power-cycle bmc-mock applies
    /// the staged (desired) versions so site-explorer observes the upgrade.
    /// When omitted, the hardware-type default versions are used.
    #[serde(default)]
    pub host_firmware_versions: Option<HostFirmwareVersions>,

    #[serde(default)]
    pub dpu_agent_version: Option<String>,
}

impl MachineConfig {
    pub(crate) fn missing_host_inband_relay_for_direct_host_dhcp(&self) -> bool {
        self.host_inband_dhcp_relay_address.is_none()
            && (self.dpu_per_host_count == 0 || self.dpus_in_nic_mode)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WiwynnGb200RackConfig {
    /// Deprecated: see `MachineConfig::dpu_reboot_delay`.
    #[serde(default = "default_dpu_reboot_delay")]
    pub dpu_reboot_delay: u64,
    /// Deprecated: see `MachineConfig::host_reboot_delay`.
    #[serde(default = "default_host_reboot_delay")]
    pub host_reboot_delay: u64,
    #[serde(default)]
    pub timing_overrides: Option<LifecycleTimingOverrides>,
    #[serde(default = "default_acceleration_factor")]
    pub acceleration_factor: f64,
    #[serde(
        default = "default_scout_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub scout_run_interval: Duration,
    #[serde(
        default = "default_discovery_retry_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub discovery_retry_interval: Duration,
    /// Relay address for BMC DHCP traffic.
    #[serde(alias = "oob_dhcp_relay_address")]
    pub bmc_dhcp_relay_address: Ipv4Addr,
    /// Shared Underlay relay for DPU OOB boot and switch NVOS DHCP traffic.
    #[serde(alias = "admin_dhcp_relay_address")]
    pub underlay_dhcp_relay_address: Ipv4Addr,
    /// Relay address used when a rack host DHCPs directly through a plain NIC.
    /// If omitted, direct host DHCP falls back to `underlay_dhcp_relay_address` for compatibility.
    #[serde(default)]
    pub host_inband_dhcp_relay_address: Option<Ipv4Addr>,
    #[serde(
        default = "default_run_interval_working",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_working: Duration,
    #[serde(
        default = "default_run_interval_idle",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_idle: Duration,
    #[serde(
        default = "default_network_status_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub network_status_run_interval: Duration,
    #[serde(default)]
    pub dpus_in_nic_mode: bool,
    #[serde(default)]
    pub dpu_firmware_versions: Option<DpuFirmwareVersions>,
    #[serde(default)]
    pub dpu_agent_version: Option<String>,
}

impl WiwynnGb200RackConfig {
    fn component_machine_config(
        &self,
        rack_id: RackId,
        rack_placement: RackPlacement,
        hw_type: HardwareType,
        dpu_per_host_count: u32,
    ) -> MachineConfig {
        MachineConfig {
            rack_id: Some(rack_id),
            rack_placement: Some(rack_placement),
            hw_type,
            host_count: 1,
            dpu_per_host_count,
            dpu_reboot_delay: self.dpu_reboot_delay,
            host_reboot_delay: self.host_reboot_delay,
            timing_overrides: self.timing_overrides.clone(),
            acceleration_factor: self.acceleration_factor,
            scout_run_interval: self.scout_run_interval,
            discovery_retry_interval: self.discovery_retry_interval,
            bmc_dhcp_relay_address: self.bmc_dhcp_relay_address,
            underlay_dhcp_relay_address: self.underlay_dhcp_relay_address,
            host_inband_dhcp_relay_address: self.host_inband_dhcp_relay_address,
            run_interval_working: self.run_interval_working,
            run_interval_idle: self.run_interval_idle,
            network_status_run_interval: self.network_status_run_interval,
            dpus_in_nic_mode: self.dpus_in_nic_mode,
            dpu_firmware_versions: self.dpu_firmware_versions.clone(),
            host_firmware_versions: None,
            dpu_agent_version: self.dpu_agent_version.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LenovoGb300RackConfig {
    /// Deprecated: see `MachineConfig::dpu_reboot_delay`.
    #[serde(default = "default_dpu_reboot_delay")]
    pub dpu_reboot_delay: u64,
    /// Deprecated: see `MachineConfig::host_reboot_delay`.
    #[serde(default = "default_host_reboot_delay")]
    pub host_reboot_delay: u64,
    #[serde(default)]
    pub timing_overrides: Option<LifecycleTimingOverrides>,
    #[serde(default = "default_acceleration_factor")]
    pub acceleration_factor: f64,
    #[serde(
        default = "default_scout_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub scout_run_interval: Duration,
    #[serde(
        default = "default_discovery_retry_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub discovery_retry_interval: Duration,
    /// Relay address for BMC DHCP traffic.
    #[serde(alias = "oob_dhcp_relay_address")]
    pub bmc_dhcp_relay_address: Ipv4Addr,
    /// Shared Underlay relay for DPU OOB boot and switch NVOS DHCP traffic.
    #[serde(alias = "admin_dhcp_relay_address")]
    pub underlay_dhcp_relay_address: Ipv4Addr,
    /// Relay address used when a rack host DHCPs directly through a plain NIC.
    /// If omitted, direct host DHCP falls back to `underlay_dhcp_relay_address` for compatibility.
    #[serde(default)]
    pub host_inband_dhcp_relay_address: Option<Ipv4Addr>,
    #[serde(
        default = "default_run_interval_working",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_working: Duration,
    #[serde(
        default = "default_run_interval_idle",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval_idle: Duration,
    #[serde(
        default = "default_network_status_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub network_status_run_interval: Duration,
    #[serde(default)]
    pub dpus_in_nic_mode: bool,
    #[serde(default)]
    pub dpu_firmware_versions: Option<DpuFirmwareVersions>,
    #[serde(default)]
    pub dpu_agent_version: Option<String>,
}

impl LenovoGb300RackConfig {
    fn component_machine_config(
        &self,
        rack_id: RackId,
        rack_placement: RackPlacement,
        hw_type: HardwareType,
        dpu_per_host_count: u32,
    ) -> MachineConfig {
        MachineConfig {
            rack_id: Some(rack_id),
            rack_placement: Some(rack_placement),
            hw_type,
            host_count: 1,
            dpu_per_host_count,
            dpu_reboot_delay: self.dpu_reboot_delay,
            host_reboot_delay: self.host_reboot_delay,
            timing_overrides: self.timing_overrides.clone(),
            acceleration_factor: self.acceleration_factor,
            scout_run_interval: self.scout_run_interval,
            discovery_retry_interval: self.discovery_retry_interval,
            bmc_dhcp_relay_address: self.bmc_dhcp_relay_address,
            underlay_dhcp_relay_address: self.underlay_dhcp_relay_address,
            host_inband_dhcp_relay_address: self.host_inband_dhcp_relay_address,
            run_interval_working: self.run_interval_working,
            run_interval_idle: self.run_interval_idle,
            network_status_run_interval: self.network_status_run_interval,
            dpus_in_nic_mode: self.dpus_in_nic_mode,
            dpu_firmware_versions: self.dpu_firmware_versions.clone(),
            host_firmware_versions: None,
            dpu_agent_version: self.dpu_agent_version.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RackConfig {
    pub rack_profile_id: RackProfileId,
    pub ids: Vec<RackId>,
    #[serde(flatten)]
    pub model: RackModelConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RackModelConfig {
    WiwynnGb200Nvl72 {
        #[serde(flatten)]
        simulation: WiwynnGb200RackConfig,
    },
    LenovoGb300Nvl72 {
        #[serde(flatten)]
        simulation: LenovoGb300RackConfig,
    },
}

impl RackModelConfig {
    fn rack_type(&self) -> RackType {
        match self {
            Self::WiwynnGb200Nvl72 { .. } => RackType::WiwynnGb200Nvl72,
            Self::LenovoGb300Nvl72 { .. } => RackType::LenovoGb300Nvl72,
        }
    }

    fn component_machine_config(
        &self,
        rack_id: RackId,
        rack_placement: RackPlacement,
        hardware_type: HardwareType,
        dpu_per_host_count: u32,
    ) -> MachineConfig {
        match self {
            Self::WiwynnGb200Nvl72 { simulation } => simulation.component_machine_config(
                rack_id,
                rack_placement,
                hardware_type,
                dpu_per_host_count,
            ),
            Self::LenovoGb300Nvl72 { simulation } => simulation.component_machine_config(
                rack_id,
                rack_placement,
                hardware_type,
                dpu_per_host_count,
            ),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct DpuFirmwareVersions {
    pub bmc: Option<String>,
    pub cec: Option<String>,
    pub uefi: Option<String>,
    pub nic: Option<String>,
    /// Optional opaque DPU BSP firmware version.
    ///
    /// Any string is accepted verbatim, including an empty string. The default,
    /// `None`, omits `DPU_BSP` from generated firmware inventory; `Some("")`
    /// explicitly configures that inventory entry with an empty version. This is
    /// supported for generated BlueField-3 and BlueField-4 DPU profiles; profiles
    /// without a generated DPU do not expose the entry.
    #[serde(default)]
    pub bsp: Option<String>,
}

/// BMC-mock has its own version of this data structure to avoid cyclic dependencies
impl From<DpuFirmwareVersions> for bmc_mock::DpuFirmwareVersions {
    fn from(value: DpuFirmwareVersions) -> Self {
        Self {
            bmc: value.bmc,
            cec: value.cec,
            uefi: value.uefi,
            nic: value.nic,
            bsp: value.bsp,
        }
    }
}

impl DpuFirmwareVersions {
    pub fn fill_missing_from_desired_firmware(
        self,
        desired_firmware: &[DesiredFirmwareVersionEntry],
    ) -> Self {
        // TODO: Pass the emulated DPU generation into this lookup and select the
        // matching desired-firmware model. This currently always uses BlueField-3,
        // so missing BF4 values can be filled from the BF3 entry; explicit overrides
        // still take precedence. BF4 BMC version strings use the `BF4-` convention:
        // https://github.com/NVIDIA/infra-controller/pull/3477
        let Some(bf3_firmware_map) = desired_firmware
            .iter()
            .find(|entry| {
                if entry.vendor != "nvidia" {
                    return false;
                }
                let normalized = entry.model.as_str().to_lowercase().replace("-", " ");
                normalized.contains("bluefield 3")
            })
            .map(|entry| &entry.component_versions)
        else {
            return self;
        };

        // Prefer onese we already have set, falling back on the server-wanted ones.
        Self {
            bmc: self.bmc.or_else(|| bf3_firmware_map.get("bmc").cloned()),
            cec: self.cec.or_else(|| bf3_firmware_map.get("cec").cloned()),
            uefi: self.uefi.or_else(|| bf3_firmware_map.get("uefi").cloned()),
            nic: self.nic.or_else(|| bf3_firmware_map.get("nic").cloned()),
            bsp: self.bsp,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Compact,
    Logfmt,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MachineATronConfig {
    #[serde(default)]
    pub racks: BTreeMap<String, RackConfig>,
    // note that order is important in machines so that mac addresses are assigned the same way between runs
    #[serde(
        deserialize_with = "deserialize_machine_config",
        serialize_with = "serialize_machine_config"
    )]
    pub machines: BTreeMap<String, Arc<MachineConfig>>,
    pub carbide_api_url: String,
    pub log_file: Option<String>,
    /// Format used for logs written to stdout or `log_file`.
    #[serde(default)]
    pub log_format: LogFormat,
    /// How machine-a-tron obtains DHCP leases for BMCs and directly attached hosts.
    #[serde(default)]
    pub dhcp: DhcpType,

    #[serde(default = "default_bmc_mock_port")]
    pub bmc_mock_port: u16,

    #[serde(default)]
    pub bmc_mock_certs_dir: Option<PathBuf>,

    /// Set this to true if you want each mock machine to run a mock BMC ssh server. This is useful
    /// for testing things like ssh-console.
    #[serde(default = "default_false")]
    pub mock_bmc_ssh_server: bool,

    /// Opt in to an independent IPMI/SOL simulator for each IPMI-capable host BMC.
    #[serde(default = "default_false")]
    pub enable_ipmi_simulation: bool,

    /// Set this to a hostname or IP If you want machine-a-tron to register its BMC-mock as the
    /// bmc_proxy host (this will be combined with bmc_mock_port.)
    pub configure_carbide_bmc_proxy_host: Option<String>,

    #[serde(default)]
    /// Set this to the path of a directory that can be used to persist machine info between runs
    pub persist_dir: Option<PathBuf>,

    #[serde(default)]
    /// Set this to true to delete created machines from the API on quit
    pub cleanup_on_quit: bool,

    /// When true (default), machine-a-tron auto-registers each mock host as an
    /// expected machine on startup. Set to false to skip auto-registration so
    /// expected machines can be added manually (e.g. via the admin CLI's
    /// `expected-machine add`) for testing alternative registration flows.
    #[serde(default = "default_true")]
    pub register_expected_machines: bool,

    /// If set, host BMC mocks start with this password instead of the factory default
    /// (`DUMMY_FACTORY_PASSWORD`). Emulates a BMC that was already rotated by an operator.
    #[serde(default)]
    pub host_bmc_password: Option<String>,

    /// Same as `host_bmc_password`, for DPU BMC mocks. When unset, each DPU
    /// model uses its factory-default password.
    #[serde(default)]
    pub dpu_bmc_password: Option<String>,

    /// How often to refresh the API state from the server. Longer durations are appropriate for
    /// mocking lots of hosts, shorter durations are appropriate for integration tests where the
    /// interval should be shorter than the state controller update interval
    #[serde(
        default = "default_api_refresh_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub api_refresh_interval: Duration,

    /// Pool to allocate regular MAC addresses for the machines.
    #[serde(default)]
    pub mac_address_pool: Option<MacAddressPoolConfig>,
    /// Pool to allocate ranges of HW MAC addresses for the machines.
    /// Ranges are needed for deterministic and unique addresses but
    /// that do not participate in any associations (allocated using
    /// just "next_mac()" manner). The normalized base also identifies
    /// the inventory exposed by `/machines/status`, so deployments whose
    /// inventories are aggregated must use non-overlapping ranges.
    #[serde(default)]
    pub hw_mac_address_ranges: Option<MacAddressRangesConfig>,

    /// Optional UFM API hosted on the machine-a-tron control listener.
    ///
    /// Unlike standalone execution, the hosted mock may consume machine-a-tron's control state
    /// directly when `include_local_inventory` is enabled. Configured static sources are still
    /// polled and can be combined with that local inventory. A present section is activated only
    /// when its explicit `enabled` flag is set.
    #[serde(default)]
    pub ufm_mock: Option<UfmMockConfig>,
}

impl MachineATronConfig {
    pub fn validate(&self) -> eyre::Result<()> {
        if let Some(ufm_mock) = self.ufm_mock.as_ref() {
            ufm_mock.validate()?;
        }

        if let DhcpType::UdpRelay {
            server_address,
            listen_address,
            advertise_address,
        } = self.dhcp
        {
            eyre::ensure!(
                server_address.port() != 0,
                "DHCP server address must use a nonzero UDP port"
            );
            eyre::ensure!(
                !server_address.ip().is_unspecified()
                    && !server_address.ip().is_broadcast()
                    && !server_address.ip().is_multicast(),
                "DHCP server address must use a concrete unicast IPv4 address"
            );
            eyre::ensure!(
                listen_address.port() != 0,
                "DHCP relay listen address must use a nonzero UDP port"
            );
            eyre::ensure!(
                !listen_address.ip().is_broadcast() && !listen_address.ip().is_multicast(),
                "DHCP relay listen address must use an unspecified or unicast IPv4 address"
            );
            eyre::ensure!(
                !advertise_address.is_unspecified()
                    && !advertise_address.is_broadcast()
                    && !advertise_address.is_multicast(),
                "DHCP relay advertise address must be a concrete unicast IPv4 address"
            );
        }

        if self.enable_ipmi_simulation {
            bmc_mock::ipmi_sim::validate_executable()?;
        }

        let mut simulated_rack_ids = BTreeSet::new();
        for (rack_group, rack) in &self.racks {
            eyre::ensure!(!rack_group.is_empty(), "rack group name cannot be empty");
            eyre::ensure!(
                !rack.rack_profile_id.as_str().is_empty(),
                "racks.{rack_group}.rack_profile_id cannot be empty"
            );
            eyre::ensure!(
                !rack.ids.is_empty(),
                "racks.{rack_group}.ids must contain at least one rack ID"
            );
            for rack_id in &rack.ids {
                eyre::ensure!(
                    !rack_id.as_str().is_empty(),
                    "racks.{rack_group}.ids cannot contain an empty rack ID"
                );
                eyre::ensure!(
                    simulated_rack_ids.insert(rack_id.clone()),
                    "rack ID {rack_id} is configured more than once"
                );
            }
        }

        for (machine_group, machine) in &self.machines {
            if let Some(rack_id) = machine.rack_id.as_ref() {
                eyre::ensure!(
                    !simulated_rack_ids.contains(rack_id),
                    "machines.{machine_group} cannot add a standalone device to atomic rack {rack_id}"
                );
            }
        }

        Ok(())
    }

    pub(crate) fn resolved_device_configs(&self) -> eyre::Result<ResolvedDeviceConfigs> {
        self.validate()?;

        let mut machines = self.machines.clone();
        let mut racks = Vec::new();
        let mut configured_racks = self
            .racks
            .values()
            .flat_map(|rack| rack.ids.iter().cloned().map(move |rack_id| (rack_id, rack)))
            .collect::<Vec<_>>();
        configured_racks.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));

        for (rack_id, rack) in configured_racks {
            let rack_type = rack.model.rack_type();
            let rack_info = RackInfo { rack_type };
            let elevation = rack_info.rack_elevation().wrap_err_with(|| {
                format!("rack {rack_id} uses an invalid {rack_type} rack design")
            })?;
            let rack_key = encoded_rack_key(&rack_id);
            let mut members = Vec::with_capacity(elevation.units.len());

            for unit in elevation.units {
                let placement = rack_info.placement(unit.position);
                let machine_config_section = format!("rack-{rack_key}--unit-{:02}", unit.position);
                let dpu_per_host_count = unit
                    .hardware_type
                    .fixed_number_of_dpu()
                    .expect("rack hardware must have a fixed number of DPUs")
                    .into();
                eyre::ensure!(
                    machines
                        .insert(
                            machine_config_section.clone(),
                            Arc::new(rack.model.component_machine_config(
                                rack_id.clone(),
                                placement,
                                unit.hardware_type,
                                dpu_per_host_count,
                            )),
                        )
                        .is_none(),
                    "rack {rack_id} generated duplicate device identity {machine_config_section}"
                );
                members.push(RackMemberRegistration {
                    placement,
                    hardware_type: unit.hardware_type,
                    machine_config_section,
                });
            }

            racks.push(RackRegistration {
                rack_id,
                rack_profile_id: rack.rack_profile_id.clone(),
                rack_type,
                version: elevation.version,
                members,
            });
        }

        Ok(ResolvedDeviceConfigs { machines, racks })
    }

    pub fn read_persisted_devices(
        &self,
    ) -> eyre::Result<Option<HashMap<String, Vec<PersistedDevice>>>> {
        let Some(devices_persist_dir) = &self.devices_persist_dir() else {
            return Ok(None);
        };

        let devices_by_config_section: HashMap<String, Vec<PersistedDevice>> =
            std::fs::read_dir(devices_persist_dir)?
                .map(|f| {
                    let f = f?;
                    let filename = f.file_name().to_string_lossy().into_owned();
                    let Some(config_section) = filename.strip_suffix(".json") else {
                        return Ok(None);
                    };
                    Ok(Some((
                        config_section.to_string(),
                        serde_json::from_reader(std::fs::File::open(f.path())?)?,
                    )))
                })
                // Ensure no errors
                .collect::<eyre::Result<Vec<_>>>()?
                // Drop None's
                .into_iter()
                .flatten()
                // Build the HashMap
                .collect();
        Ok(Some(devices_by_config_section))
    }

    pub fn write_persisted_devices(&self, devices: &[PersistedDevice]) -> eyre::Result<()> {
        let Some(devices_persist_dir) = &self.devices_persist_dir() else {
            return Ok(());
        };

        std::fs::create_dir_all(devices_persist_dir)?;

        let mut persisted_devices_by_section: HashMap<String, Vec<&PersistedDevice>> =
            HashMap::new();
        for device in devices {
            if let Some(devices) =
                persisted_devices_by_section.get_mut(&device.machine_config_section)
            {
                devices.push(device);
            } else {
                persisted_devices_by_section
                    .insert(device.machine_config_section.clone(), vec![device]);
            }
        }

        for (config_section, persisted_devices) in persisted_devices_by_section {
            std::fs::write(
                devices_persist_dir.join(format!("{config_section}.json")),
                serde_json::to_vec(&persisted_devices)?,
            )?;
        }

        Ok(())
    }

    fn devices_persist_dir(&self) -> Option<PathBuf> {
        self.persist_dir.as_ref().map(|d| d.join("machines"))
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedDeviceConfigs {
    pub machines: BTreeMap<String, Arc<MachineConfig>>,
    pub racks: Vec<RackRegistration>,
}

fn encoded_rack_key(rack_id: &RackId) -> String {
    rack_id
        .as_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DhcpType {
    Api {},
    UdpRelay {
        /// Destination for relayed DHCP packets.
        server_address: SocketAddrV4,
        /// Local UDP socket used to receive relayed DHCP replies.
        listen_address: SocketAddrV4,
        /// Reachable IPv4 address placed in DHCP `giaddr` for server replies.
        advertise_address: Ipv4Addr,
    },
}

impl Default for DhcpType {
    fn default() -> Self {
        Self::Api {}
    }
}

/// Device information persisted to JSON for recovery by subsequent machine-a-tron runs.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PersistedDevice {
    pub mat_id: Uuid,
    pub machine_config_section: String,
    /// Modern machine-a-tron versions always persist this field. Keeping it mandatory prevents
    /// an incomplete snapshot from silently restoring a device as the default hardware type.
    pub hw_type: HardwareType,
    pub bmc_mac_address: MacAddress,
    pub serial: String,
    pub dpus: Vec<PersistedDpuMachine>,
    pub non_dpu_mac_address: Option<MacAddress>,
    #[serde(default)]
    pub nvos_mac_addresses: Vec<MacAddress>,
    #[serde(default)]
    pub switch_serial_number: Option<String>,
    pub observed_machine_id: Option<MachineId>,
    pub installed_os: OsImage,
    pub tpm_ek_certificate: Option<Vec<u8>>,
    #[serde(default)]
    pub hw_mac_addr_pool: Option<MacAddressPoolConfig>,
    /// Active host firmware inventory at the time this snapshot was taken.
    /// Restored as `initial_host_firmware` on restart so the mock starts with
    /// the versions last observed, not the operator-configured starting point.
    #[serde(default)]
    pub active_host_firmware: Option<HostFirmwareVersions>,
}

impl PersistedDevice {
    pub fn mac_addresses(&self) -> impl Iterator<Item = MacAddress> {
        std::iter::once(self.bmc_mac_address)
            .chain(self.dpus.iter().flat_map(|d| d.mac_addresses()))
            .chain(self.non_dpu_mac_address.iter().copied())
            .chain(self.nvos_mac_addresses.iter().copied())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDpuMachine {
    pub mat_id: Uuid,
    /// Modern machine-a-tron versions always persist this field. Keeping it mandatory prevents
    /// an incomplete snapshot from silently restoring a DPU as the default hardware type.
    pub hw_type: HardwareType,
    pub bmc_mac_address: MacAddress,
    pub host_mac_address: MacAddress,
    pub oob_mac_address: MacAddress,
    pub serial: String,
    pub installed_os: OsImage,
    pub dpu_index: u8,
    #[serde(flatten)]
    pub settings: DpuSettings,
}

impl PersistedDpuMachine {
    pub fn mac_addresses(&self) -> impl Iterator<Item = MacAddress> {
        [
            self.bmc_mac_address,
            self.host_mac_address,
            self.oob_mac_address,
        ]
        .into_iter()
    }
}

impl From<PersistedDpuMachine> for DpuMachineInfo {
    fn from(value: PersistedDpuMachine) -> Self {
        Self {
            hw_type: value.hw_type,
            bmc_mac_address: value.bmc_mac_address,
            host_mac_address: value.host_mac_address,
            oob_mac_address: value.oob_mac_address,
            serial: value.serial,
            settings: value.settings,
        }
    }
}

fn default_bmc_mock_port() -> u16 {
    2000
}

fn default_run_interval_working() -> Duration {
    Duration::from_secs(5)
}

fn default_discovery_retry_interval() -> Duration {
    Duration::from_secs(60)
}

fn default_run_interval_idle() -> Duration {
    Duration::from_secs(30)
}

fn default_api_refresh_interval() -> Duration {
    Duration::from_secs(2)
}

fn default_network_status_run_interval() -> Duration {
    Duration::from_secs(20)
}

fn default_hardware_type() -> HardwareType {
    HardwareType::default()
}

fn default_host_reboot_delay() -> u64 {
    300
}

fn default_dpu_reboot_delay() -> u64 {
    120
}

fn default_acceleration_factor() -> f64 {
    1.0
}

fn default_scout_run_interval() -> Duration {
    Duration::from_secs(60)
}

fn default_false() -> bool {
    false
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MacAddressPoolConfig {
    pub base: MacAddress,
    pub host_bits: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MacAddressRangesConfig {
    pub base: MacAddress,
    pub host_bits: usize,
    pub range_host_bits: usize,
}

// Lots of types keep an owned reference to MachineATronContext, making an Arc keeps this cheap.

pub struct MachineATronContext {
    pub app_config: MachineATronConfig,
    pub forge_client_config: ForgeClientConfig,
    pub bmc_mock_certs_dir: Option<PathBuf>,
    pub bmc_registry: BmcMockRegistry,
    pub api_throttler: ApiThrottler,
    /// These are the firmware versions the server wants us to be on. If not configured for other
    /// firmware, DPU's can mock that they already have this installed.
    pub desired_firmware_versions: Vec<DesiredFirmwareVersionEntry>,
    pub forge_api_client: ForgeApiClient,
    pub dhcp_client: crate::dhcp_wrapper::DhcpClient,
    pub mac_address_pool: Arc<Mutex<MacAddressPool>>,
}

impl MachineATronContext {
    pub fn api_client(&self) -> ApiClient {
        self.forge_api_client.clone().into()
    }
}

fn as_std_duration<S>(d: &std::time::Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if d.lt(&Duration::from_secs(1)) {
        serializer.serialize_str(&format!("{}ms", d.as_millis()))
    } else {
        serializer.serialize_str(&format!("{}s", d.as_secs()))
    }
}

fn deserialize_machine_config<'a, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Arc<MachineConfig>>, D::Error>
where
    D: Deserializer<'a>,
{
    let result: BTreeMap<String, MachineConfig> = Deserialize::deserialize(deserializer)?;
    Ok(result.into_iter().map(|(k, v)| (k, v.into())).collect())
}

fn serialize_machine_config<S>(
    d: &BTreeMap<String, Arc<MachineConfig>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut map = serializer.serialize_map(Some(d.len()))?;
    for (k, v) in d {
        map.serialize_entry(k, v.as_ref())?;
    }
    map.end()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use serde::de::DeserializeOwned;

    use super::*;

    fn rack_config() -> MachineATronConfig {
        toml::from_str(
            r#"
carbide_api_url = "https://carbide-api.forge:443"
log_file = "mat.log"
pxe_server_host = "192.168.176.7"
pxe_server_port = "8080"
bmc_mock_port = 1266
mat_api_server_enabled = true
mat_api_server_listen_port = 2112
configure_carbide_bmc_proxy_host = "192.168.1.20"

[machines.config]
rack_id = "rack-001"
host_count = 10
dpu_per_host_count = 2
dpu_reboot_delay = 1 # in units of seconds
host_reboot_delay = 1 # in units of seconds
underlay_dhcp_relay_address = "192.168.176.1"
host_inband_dhcp_relay_address = "192.168.177.1"
bmc_dhcp_relay_address = "192.168.192.1"
run_interval_working = "100ms"
run_interval_idle = "1s"
network_status_run_interval = "5s"
scout_run_interval = "5s"
    "#,
        )
        .expect("Could not parse config")
    }

    fn wiwynn_gb200_rack_from_machine(machine: &MachineConfig) -> WiwynnGb200RackConfig {
        WiwynnGb200RackConfig {
            dpu_reboot_delay: machine.dpu_reboot_delay,
            host_reboot_delay: machine.host_reboot_delay,
            timing_overrides: machine.timing_overrides.clone(),
            acceleration_factor: machine.acceleration_factor,
            scout_run_interval: machine.scout_run_interval,
            discovery_retry_interval: machine.discovery_retry_interval,
            bmc_dhcp_relay_address: machine.bmc_dhcp_relay_address,
            underlay_dhcp_relay_address: machine.underlay_dhcp_relay_address,
            host_inband_dhcp_relay_address: machine.host_inband_dhcp_relay_address,
            run_interval_working: machine.run_interval_working,
            run_interval_idle: machine.run_interval_idle,
            network_status_run_interval: machine.network_status_run_interval,
            dpus_in_nic_mode: machine.dpus_in_nic_mode,
            dpu_firmware_versions: machine.dpu_firmware_versions.clone(),
            dpu_agent_version: machine.dpu_agent_version.clone(),
        }
    }

    fn lenovo_gb300_rack_from_machine(machine: &MachineConfig) -> LenovoGb300RackConfig {
        LenovoGb300RackConfig {
            dpu_reboot_delay: machine.dpu_reboot_delay,
            host_reboot_delay: machine.host_reboot_delay,
            timing_overrides: machine.timing_overrides.clone(),
            acceleration_factor: machine.acceleration_factor,
            scout_run_interval: machine.scout_run_interval,
            discovery_retry_interval: machine.discovery_retry_interval,
            bmc_dhcp_relay_address: machine.bmc_dhcp_relay_address,
            underlay_dhcp_relay_address: machine.underlay_dhcp_relay_address,
            host_inband_dhcp_relay_address: machine.host_inband_dhcp_relay_address,
            run_interval_working: machine.run_interval_working,
            run_interval_idle: machine.run_interval_idle,
            network_status_run_interval: machine.network_status_run_interval,
            dpus_in_nic_mode: machine.dpus_in_nic_mode,
            dpu_firmware_versions: machine.dpu_firmware_versions.clone(),
            dpu_agent_version: machine.dpu_agent_version.clone(),
        }
    }

    fn gb200_rack_config() -> MachineATronConfig {
        let mut config = rack_config();
        let template = config.machines["config"].clone();
        config.machines.clear();
        config.racks.insert(
            "default".to_string(),
            RackConfig {
                ids: vec![RackId::new("rack-002"), RackId::new("rack-001")],
                rack_profile_id: RackProfileId::new("NVL72"),
                model: RackModelConfig::WiwynnGb200Nvl72 {
                    simulation: wiwynn_gb200_rack_from_machine(&template),
                },
            },
        );
        config
    }

    fn gb300_rack_config() -> MachineATronConfig {
        let mut config = rack_config();
        let template = config.machines["config"].clone();
        config.machines.clear();
        config.racks.insert(
            "default".to_string(),
            RackConfig {
                ids: vec![RackId::new("rack-002"), RackId::new("rack-001")],
                rack_profile_id: RackProfileId::new("NVL72_GB300"),
                model: RackModelConfig::LenovoGb300Nvl72 {
                    simulation: lenovo_gb300_rack_from_machine(&template),
                },
            },
        );
        config
    }

    #[test]
    fn test_serialize_config() {
        let cfg = rack_config();
        assert_eq!(
            cfg.machines["config"].discovery_retry_interval,
            Duration::from_secs(60)
        );
        cfg.validate().expect("Could not validate config");
        let serialized = toml::to_string(&cfg).expect("Could not serialize config");
        let round_tripped = toml::from_str::<MachineATronConfig>(&serialized)
            .expect("Could not deserialize serialized config");
        assert_eq!(round_tripped, cfg);
    }

    #[test]
    fn rack_configs_round_trip() {
        check_cases(
            [
                Case {
                    scenario: "WIWYNN GB200 rack",
                    input: (
                        gb200_rack_config(),
                        "type = \"wiwynn_gb200_nvl72\"",
                        "rack_profile_id = \"NVL72\"",
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "Lenovo GB300 rack",
                    input: (
                        gb300_rack_config(),
                        "type = \"lenovo_gb300_nvl72\"",
                        "rack_profile_id = \"NVL72_GB300\"",
                    ),
                    expect: Yields(()),
                },
            ],
            |(config, expected_type, expected_profile)| {
                (|| -> eyre::Result<()> {
                    config.validate()?;
                    let serialized = toml::to_string(&config)?;
                    eyre::ensure!(serialized.contains("[racks.default]"));
                    eyre::ensure!(serialized.contains(expected_type));
                    eyre::ensure!(serialized.contains(expected_profile));
                    eyre::ensure!(serialized.contains("ids = [\"rack-002\", \"rack-001\"]"));
                    let round_tripped = toml::from_str::<MachineATronConfig>(&serialized)?;
                    eyre::ensure!(round_tripped == config);
                    Ok(())
                })()
                .map_err(drop)
            },
        );
    }

    #[test]
    fn rack_models_expand_their_managed_hardware() {
        #[derive(Debug)]
        struct ExpectedExpansion {
            config: MachineATronConfig,
            rack_profile_id: &'static str,
            rack_type: RackType,
            member_count: usize,
            compute_type: HardwareType,
            compute_count: usize,
            switch_type: HardwareType,
            switch_count: usize,
            power_shelf_type: HardwareType,
            power_shelf_count: usize,
        }

        check_cases(
            [
                Case {
                    scenario: "WIWYNN GB200 rack",
                    input: ExpectedExpansion {
                        config: gb200_rack_config(),
                        rack_profile_id: "NVL72",
                        rack_type: RackType::WiwynnGb200Nvl72,
                        member_count: 35,
                        compute_type: HardwareType::WiwynnGB200Nvl,
                        compute_count: 36,
                        switch_type: HardwareType::NvidiaSwitchNd5200Ld,
                        switch_count: 18,
                        power_shelf_type: HardwareType::LiteOnPowerShelf,
                        power_shelf_count: 16,
                    },
                    expect: Yields(()),
                },
                Case {
                    scenario: "Lenovo GB300 rack",
                    input: ExpectedExpansion {
                        config: gb300_rack_config(),
                        rack_profile_id: "NVL72_GB300",
                        rack_type: RackType::LenovoGb300Nvl72,
                        member_count: 33,
                        compute_type: HardwareType::LenovoGB300Nvl,
                        compute_count: 36,
                        switch_type: HardwareType::NvidiaSwitchN5700Ld,
                        switch_count: 18,
                        power_shelf_type: HardwareType::LiteOnPowerShelf,
                        power_shelf_count: 12,
                    },
                    expect: Yields(()),
                },
            ],
            |expected| {
                (|| -> eyre::Result<()> {
                    let first = expected.config.resolved_device_configs()?;
                    let second = expected.config.resolved_device_configs()?;

                    eyre::ensure!(first.machines.len() == expected.member_count * 2);
                    eyre::ensure!(
                        first.machines.keys().collect::<Vec<_>>()
                            == second.machines.keys().collect::<Vec<_>>()
                    );
                    eyre::ensure!(first.racks.len() == 2);
                    eyre::ensure!(first.racks[0].rack_id == RackId::new("rack-001"));
                    eyre::ensure!(first.racks[1].rack_id == RackId::new("rack-002"));
                    for rack in &first.racks {
                        eyre::ensure!(
                            rack.rack_profile_id == RackProfileId::new(expected.rack_profile_id)
                        );
                        eyre::ensure!(rack.rack_type == expected.rack_type);
                        eyre::ensure!(rack.members.len() == expected.member_count);
                        eyre::ensure!(
                            rack.members
                                .iter()
                                .map(|member| member.placement.position())
                                .collect::<BTreeSet<_>>()
                                .len()
                                == expected.member_count
                        );
                        for member in &rack.members {
                            let machine = first
                                .machines
                                .get(&member.machine_config_section)
                                .expect("rack member must reference a generated machine");
                            eyre::ensure!(machine.rack_id.as_ref() == Some(&rack.rack_id));
                            eyre::ensure!(machine.rack_placement == Some(member.placement));
                        }
                    }

                    for machine in first.machines.values() {
                        eyre::ensure!(machine.host_count == 1);
                        eyre::ensure!(
                            machine.rack_id == Some(RackId::new("rack-001"))
                                || machine.rack_id == Some(RackId::new("rack-002"))
                        );
                    }
                    for (hardware_type, count) in [
                        (expected.compute_type, expected.compute_count),
                        (expected.switch_type, expected.switch_count),
                        (expected.power_shelf_type, expected.power_shelf_count),
                    ] {
                        eyre::ensure!(
                            first
                                .machines
                                .values()
                                .filter(|machine| machine.hw_type == hardware_type)
                                .count()
                                == count
                        );
                    }

                    Ok(())
                })()
                .map_err(drop)
            },
        );
    }

    #[test]
    fn simulated_rack_rejects_manual_members() {
        let valid = gb200_rack_config();

        let mut manual_member = valid.clone();
        let machine = valid
            .resolved_device_configs()
            .unwrap()
            .machines
            .values()
            .next()
            .unwrap()
            .clone();
        manual_member.machines.insert("manual".to_string(), machine);

        check_cases(
            [
                Case {
                    scenario: "valid concrete WIWYNN GB200 rack",
                    input: valid,
                    expect: Yields(()),
                },
                Case {
                    scenario: "manual machine in simulated rack",
                    input: manual_member,
                    expect: Fails,
                },
            ],
            |config| config.validate().map_err(drop),
        );
    }

    #[test]
    fn ipmi_simulation_is_disabled_by_default() {
        assert!(!rack_config().enable_ipmi_simulation);
    }

    #[test]
    fn dhcp_uses_api_by_default() {
        assert_eq!(rack_config().dhcp, DhcpType::Api {});
    }

    #[test]
    fn log_format_configuration() {
        #[derive(Deserialize)]
        struct LoggingConfig {
            #[serde(default)]
            log_format: LogFormat,
        }

        check_values(
            [
                Check {
                    scenario: "format omitted",
                    input: "",
                    expect: Some(LogFormat::Compact),
                },
                Check {
                    scenario: "compact format",
                    input: r#"log_format = "compact""#,
                    expect: Some(LogFormat::Compact),
                },
                Check {
                    scenario: "logfmt format",
                    input: r#"log_format = "logfmt""#,
                    expect: Some(LogFormat::Logfmt),
                },
                Check {
                    scenario: "unknown format",
                    input: r#"log_format = "json""#,
                    expect: None,
                },
            ],
            |serialized| {
                toml::from_str::<LoggingConfig>(serialized)
                    .ok()
                    .map(|config| config.log_format)
            },
        );
    }

    #[test]
    fn udp_relay_configuration_requires_all_addresses() {
        check_values(
            [
                Check {
                    scenario: "complete UDP relay configuration",
                    input: r#"type = "udp_relay"
server_address = "127.0.0.1:6767"
listen_address = "0.0.0.0:6768"
advertise_address = "127.0.0.1""#,
                    expect: false,
                },
                Check {
                    scenario: "missing DHCP server address",
                    input: r#"type = "udp_relay"
listen_address = "0.0.0.0:6768"
advertise_address = "127.0.0.1""#,
                    expect: true,
                },
                Check {
                    scenario: "missing relay listen address",
                    input: r#"type = "udp_relay"
server_address = "127.0.0.1:6767"
advertise_address = "127.0.0.1""#,
                    expect: true,
                },
                Check {
                    scenario: "missing relay advertise address",
                    input: r#"type = "udp_relay"
server_address = "127.0.0.1:6767"
listen_address = "0.0.0.0:6768""#,
                    expect: true,
                },
                Check {
                    scenario: "relay address on API mode",
                    input: r#"type = "api"
server_address = "127.0.0.1:6767""#,
                    expect: true,
                },
            ],
            |serialized| toml::from_str::<DhcpType>(serialized).is_err(),
        );
    }

    #[test]
    fn udp_relay_configuration_is_validated() {
        fn udp_relay(
            server_address: &str,
            listen_address: &str,
            advertise_address: Ipv4Addr,
        ) -> DhcpType {
            DhcpType::UdpRelay {
                server_address: server_address.parse().unwrap(),
                listen_address: listen_address.parse().unwrap(),
                advertise_address,
            }
        }

        let mut complete = rack_config();
        complete.dhcp = udp_relay("127.0.0.1:6767", "0.0.0.0:6768", Ipv4Addr::LOCALHOST);

        let mut unspecified_advertise_address = complete.clone();
        unspecified_advertise_address.dhcp =
            udp_relay("127.0.0.1:6767", "0.0.0.0:6768", Ipv4Addr::UNSPECIFIED);

        let mut zero_server_port = complete.clone();
        zero_server_port.dhcp = udp_relay("127.0.0.1:0", "0.0.0.0:6768", Ipv4Addr::LOCALHOST);

        let mut unspecified_server_address = complete.clone();
        unspecified_server_address.dhcp =
            udp_relay("0.0.0.0:6767", "0.0.0.0:6768", Ipv4Addr::LOCALHOST);

        let mut multicast_server_address = complete.clone();
        multicast_server_address.dhcp =
            udp_relay("224.0.0.1:6767", "0.0.0.0:6768", Ipv4Addr::LOCALHOST);

        let mut broadcast_server_address = complete.clone();
        broadcast_server_address.dhcp =
            udp_relay("255.255.255.255:6767", "0.0.0.0:6768", Ipv4Addr::LOCALHOST);

        let mut zero_listen_port = complete.clone();
        zero_listen_port.dhcp = udp_relay("127.0.0.1:6767", "0.0.0.0:0", Ipv4Addr::LOCALHOST);

        let mut multicast_listen_address = complete.clone();
        multicast_listen_address.dhcp =
            udp_relay("127.0.0.1:6767", "224.0.0.1:6768", Ipv4Addr::LOCALHOST);

        let mut broadcast_listen_address = complete.clone();
        broadcast_listen_address.dhcp = udp_relay(
            "127.0.0.1:6767",
            "255.255.255.255:6768",
            Ipv4Addr::LOCALHOST,
        );

        let mut multicast_advertise_address = complete.clone();
        multicast_advertise_address.dhcp = udp_relay(
            "127.0.0.1:6767",
            "0.0.0.0:6768",
            Ipv4Addr::new(224, 0, 0, 1),
        );

        let mut broadcast_advertise_address = complete.clone();
        broadcast_advertise_address.dhcp =
            udp_relay("127.0.0.1:6767", "0.0.0.0:6768", Ipv4Addr::BROADCAST);

        check_cases(
            [
                Case {
                    scenario: "complete UDP relay configuration",
                    input: complete,
                    expect: Yields(()),
                },
                Case {
                    scenario: "zero DHCP server port",
                    input: zero_server_port,
                    expect: Fails,
                },
                Case {
                    scenario: "unspecified DHCP server address",
                    input: unspecified_server_address,
                    expect: Fails,
                },
                Case {
                    scenario: "multicast DHCP server address",
                    input: multicast_server_address,
                    expect: Fails,
                },
                Case {
                    scenario: "broadcast DHCP server address",
                    input: broadcast_server_address,
                    expect: Fails,
                },
                Case {
                    scenario: "zero relay listen port",
                    input: zero_listen_port,
                    expect: Fails,
                },
                Case {
                    scenario: "multicast relay listen address",
                    input: multicast_listen_address,
                    expect: Fails,
                },
                Case {
                    scenario: "broadcast relay listen address",
                    input: broadcast_listen_address,
                    expect: Fails,
                },
                Case {
                    scenario: "unspecified advertised address",
                    input: unspecified_advertise_address,
                    expect: Fails,
                },
                Case {
                    scenario: "multicast advertised address",
                    input: multicast_advertise_address,
                    expect: Fails,
                },
                Case {
                    scenario: "broadcast advertised address",
                    input: broadcast_advertise_address,
                    expect: Fails,
                },
            ],
            |config| config.validate().map_err(drop),
        );
    }

    #[test]
    fn host_inband_dhcp_relay_is_optional() {
        let mut serialized =
            toml::Value::try_from(rack_config()).expect("Could not serialize config");
        serialized["machines"]["config"]
            .as_table_mut()
            .expect("machine config should be a TOML table")
            .remove("host_inband_dhcp_relay_address");

        let cfg: MachineATronConfig = serialized
            .try_into()
            .expect("legacy config without host_inband_dhcp_relay_address should deserialize");
        assert_eq!(cfg.machines["config"].host_inband_dhcp_relay_address, None);
    }

    fn assert_relay_name_compatibility<T>(config: T)
    where
        T: Clone + Debug + DeserializeOwned + PartialEq + Serialize,
    {
        let canonical = toml::Value::try_from(config.clone())
            .expect("relay configuration should serialize to TOML");
        let canonical_table = canonical
            .as_table()
            .expect("relay configuration should serialize as a TOML table");
        assert!(canonical_table.contains_key("bmc_dhcp_relay_address"));
        assert!(canonical_table.contains_key("underlay_dhcp_relay_address"));
        assert!(!canonical_table.contains_key("oob_dhcp_relay_address"));
        assert!(!canonical_table.contains_key("admin_dhcp_relay_address"));

        let mut legacy = canonical.clone();
        let legacy_table = legacy
            .as_table_mut()
            .expect("relay configuration should serialize as a TOML table");
        let bmc_relay = legacy_table
            .remove("bmc_dhcp_relay_address")
            .expect("canonical BMC relay should be present");
        legacy_table.insert("oob_dhcp_relay_address".to_string(), bmc_relay);
        let underlay_relay = legacy_table
            .remove("underlay_dhcp_relay_address")
            .expect("canonical Underlay relay should be present");
        legacy_table.insert("admin_dhcp_relay_address".to_string(), underlay_relay);
        let deserialized: T = legacy
            .try_into()
            .expect("legacy relay names should deserialize");
        assert_eq!(deserialized, config);

        let mut duplicate = canonical.clone();
        let duplicate_table = duplicate
            .as_table_mut()
            .expect("relay configuration should serialize as a TOML table");
        let bmc_relay = duplicate_table["bmc_dhcp_relay_address"].clone();
        duplicate_table.insert("oob_dhcp_relay_address".to_string(), bmc_relay);
        let underlay_relay = duplicate_table["underlay_dhcp_relay_address"].clone();
        duplicate_table.insert("admin_dhcp_relay_address".to_string(), underlay_relay);
        assert!(
            duplicate.try_into::<T>().is_err(),
            "canonical and legacy relay names must not be accepted together"
        );

        for missing_key in ["bmc_dhcp_relay_address", "underlay_dhcp_relay_address"] {
            let mut missing = canonical.clone();
            missing
                .as_table_mut()
                .expect("relay configuration should serialize as a TOML table")
                .remove(missing_key);
            assert!(
                missing.try_into::<T>().is_err(),
                "{missing_key} or its legacy alias must be configured"
            );
        }
    }

    #[test]
    fn relay_names_are_backward_compatible() {
        let machine = rack_config().machines["config"].as_ref().clone();

        assert_relay_name_compatibility(machine.clone());
        assert_relay_name_compatibility(wiwynn_gb200_rack_from_machine(&machine));
        assert_relay_name_compatibility(lenovo_gb300_rack_from_machine(&machine));
    }

    #[test]
    fn rack_references_are_validated() {
        let standalone = rack_config();
        let valid = gb200_rack_config();

        let mut empty_group_name = valid.clone();
        let rack = empty_group_name.racks.remove("default").unwrap();
        empty_group_name.racks.insert(String::new(), rack);

        let mut no_ids = valid.clone();
        no_ids.racks.get_mut("default").unwrap().ids.clear();

        let mut empty_profile_id = valid.clone();
        empty_profile_id
            .racks
            .get_mut("default")
            .unwrap()
            .rack_profile_id = RackProfileId::new("");

        let mut empty_rack_id = valid.clone();
        empty_rack_id.racks.get_mut("default").unwrap().ids[0] = RackId::new("");

        let mut duplicate_rack_id = valid.clone();
        duplicate_rack_id
            .racks
            .get_mut("default")
            .unwrap()
            .ids
            .push(RackId::new("rack-001"));

        check_cases(
            [
                Case {
                    scenario: "atomic rack group",
                    input: valid,
                    expect: Yields(()),
                },
                Case {
                    scenario: "standalone machine with arbitrary rack ID",
                    input: standalone,
                    expect: Yields(()),
                },
                Case {
                    scenario: "empty rack group name",
                    input: empty_group_name,
                    expect: Fails,
                },
                Case {
                    scenario: "rack group without IDs",
                    input: no_ids,
                    expect: Fails,
                },
                Case {
                    scenario: "rack group without profile ID",
                    input: empty_profile_id,
                    expect: Fails,
                },
                Case {
                    scenario: "empty rack ID",
                    input: empty_rack_id,
                    expect: Fails,
                },
                Case {
                    scenario: "duplicate rack ID",
                    input: duplicate_rack_id,
                    expect: Fails,
                },
            ],
            |config| config.validate().map_err(drop),
        );
    }

    #[test]
    fn missing_host_inband_relay_warning_selection() {
        let host_inband = Ipv4Addr::new(192, 168, 177, 1);

        check_values(
            [
                Check {
                    scenario: "zero-DPU host without HostInband",
                    input: (0, false, None),
                    expect: true,
                },
                Check {
                    scenario: "NIC-mode host without HostInband",
                    input: (1, true, None),
                    expect: true,
                },
                Check {
                    scenario: "managed-DPU host without HostInband",
                    input: (1, false, None),
                    expect: false,
                },
                Check {
                    scenario: "zero-DPU host with HostInband",
                    input: (0, false, Some(host_inband)),
                    expect: false,
                },
            ],
            |(dpu_per_host_count, dpus_in_nic_mode, host_inband_dhcp_relay_address)| {
                let mut config = rack_config();
                let machine = Arc::make_mut(config.machines.get_mut("config").unwrap());
                machine.dpu_per_host_count = dpu_per_host_count;
                machine.dpus_in_nic_mode = dpus_in_nic_mode;
                machine.host_inband_dhcp_relay_address = host_inband_dhcp_relay_address;
                machine.missing_host_inband_relay_for_direct_host_dhcp()
            },
        );
    }
}
