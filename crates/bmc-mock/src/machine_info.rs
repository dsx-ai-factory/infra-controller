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
use std::sync::Arc;

use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::infiniband::Guid;
use crate::mac_address_pool::{MacAddressPool, PoolConfig as MacAddressPoolConfig};
use crate::redfish::update_service::UpdateServiceConfig;
use crate::{
    DUMMY_FACTORY_PASSWORD, DUMMY_FACTORY_USERNAME, HardwareType, RackPlacement, hw, redfish,
};

/// Represents static information we know ahead of time about a host or DPU (independent of any
/// state we get from carbide like IP addresses or machine ID's.) Intended to be immutable and
/// easily cloneable.
#[derive(Debug, Clone)]
pub enum MachineInfo {
    Host(HostMachineInfo),
    Dpu(DpuMachineInfo),
}

#[derive(Debug, Clone)]
pub struct HostMachineInfo {
    pub hw_type: HardwareType,
    pub rack_placement: Option<RackPlacement>,
    pub bmc_mac_address: MacAddress,
    pub serial: String,
    pub dpus: Vec<DpuMachineInfo>,
    pub non_dpu_mac_address: Option<MacAddress>,
    pub nvos_mac_addresses: Vec<MacAddress>,
    pub switch_serial_number: Option<String>,
    pub hw_mac_addr_pool: MacAddressPoolConfig,
    /// Per-PSU commanded on/off states for a Delta power shelf, reported under
    /// `Oem.deltaenergysystems.Power`. `None` uses the default all-on shelf;
    /// [`crate::test_support::delta_powershelf_bmc_with_psu_power`] sets it to
    /// model off/mixed shelves. Ignored for non-Delta hardware.
    pub delta_psu_power: Option<Vec<bool>>,
    /// Initial host firmware versions for the simulated BMC firmware inventory.
    /// When `None` the hardware-type default is used.  machine-a-tron sets this
    /// from the operator-provided `host_firmware` config so that the starting
    /// inventory reflects a version carbide will want to upgrade.
    pub initial_host_firmware: Option<HostFirmwareVersions>,
    /// Target host firmware versions to apply after an upload + power-cycle.
    /// machine-a-tron sets this from `desired_firmware_versions` so the mock
    /// knows what version to stage when carbide submits any firmware upload —
    /// without parsing the binary.  Separate from `initial_host_firmware` so
    /// the two roles (current vs target) are explicit.
    pub desired_host_firmware: Option<HostFirmwareVersions>,
}

/// Initial firmware versions for host BMC and UEFI components, used to
/// populate `UpdateService/FirmwareInventory` when the mock starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct HostFirmwareVersions {
    pub bmc: Option<String>,
    pub uefi: Option<String>,
}

trait HardwareTypeExt {
    fn infiniband_port_count(&self) -> usize;
}

impl HardwareTypeExt for HardwareType {
    fn infiniband_port_count(&self) -> usize {
        match self {
            HardwareType::WiwynnGB200Nvl => 4,
            HardwareType::NvidiaDgxH100 => 8,
            HardwareType::DellPowerEdgeR750
            | HardwareType::DellPowerEdgeR760Bf4
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl
            | HardwareType::NvidiaDgxVr
            | HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DpuMachineInfo {
    pub hw_type: HardwareType,
    pub bmc_mac_address: MacAddress,
    pub host_mac_address: MacAddress,
    pub oob_mac_address: MacAddress,
    pub serial: String,
    pub settings: DpuSettings,
}

/// Optional firmware inventory overrides for a generated BlueField DPU.
///
/// Redfish inventory IDs are selected separately for each modeled DPU generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct DpuFirmwareVersions {
    /// DPU BMC firmware version.
    pub bmc: Option<String>,
    /// DPU UEFI firmware version.
    pub uefi: Option<String>,
    /// Optional opaque DPU BSP firmware version.
    ///
    /// Any string is accepted verbatim, including an empty string. The default,
    /// `None`, omits `DPU_BSP` from generated firmware inventory; `Some("")`
    /// explicitly configures that inventory entry with an empty version. This is
    /// supported for generated BlueField-3 and BlueField-4 DPU profiles; profiles
    /// without a generated DPU do not expose the entry.
    #[serde(default)]
    pub bsp: Option<String>,
    /// DPU CEC firmware version.
    pub cec: Option<String>,
    /// DPU NIC firmware version.
    pub nic: Option<String>,
}

#[derive(Clone, Copy)]
enum DpuFirmwareComponent {
    Bmc,
    Uefi,
    Bsp,
    Cec,
    Nic,
}

impl DpuFirmwareVersions {
    fn configured(&self) -> impl Iterator<Item = (DpuFirmwareComponent, &str)> {
        [
            (DpuFirmwareComponent::Bmc, self.bmc.as_deref()),
            (DpuFirmwareComponent::Uefi, self.uefi.as_deref()),
            (DpuFirmwareComponent::Bsp, self.bsp.as_deref()),
            (DpuFirmwareComponent::Cec, self.cec.as_deref()),
            (DpuFirmwareComponent::Nic, self.nic.as_deref()),
        ]
        .into_iter()
        .filter_map(|(component, version)| version.map(|version| (component, version)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DpuSettings {
    pub nic_mode: bool,
    pub firmware_versions: DpuFirmwareVersions,
    #[serde(default = "default_true")]
    pub exposes_oob_eth: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy)]
enum DpuType {
    Bluefield3,
    Bluefield4,
}

impl DpuType {
    fn firmware_inventory_id(self, component: DpuFirmwareComponent) -> Option<&'static str> {
        match self {
            Self::Bluefield3 => Some(match component {
                DpuFirmwareComponent::Bmc => "BMC_Firmware",
                DpuFirmwareComponent::Uefi => "DPU_UEFI",
                DpuFirmwareComponent::Bsp => "DPU_BSP",
                DpuFirmwareComponent::Cec => "Bluefield_FW_ERoT",
                DpuFirmwareComponent::Nic => "DPU_NIC",
            }),
            Self::Bluefield4 => Some(match component {
                DpuFirmwareComponent::Bmc => "BlueField_FW_BMC_0",
                DpuFirmwareComponent::Uefi => "BlueField_FW_CPU_0",
                // NVIDIA's Redfish client uses this ID for both generations.
                DpuFirmwareComponent::Bsp => "DPU_BSP",
                DpuFirmwareComponent::Cec => "BlueField_FW_ERoT_BMC_0",
                DpuFirmwareComponent::Nic => "BlueField_FW_NIC_0",
            }),
        }
    }
}

impl Default for DpuSettings {
    fn default() -> Self {
        Self {
            nic_mode: false,
            firmware_versions: Default::default(),
            exposes_oob_eth: true,
        }
    }
}

impl DpuMachineInfo {
    pub fn new(hw_type: HardwareType, pool: &mut MacAddressPool, settings: DpuSettings) -> Self {
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        let bmc_mac_address = next_mac();
        let host_mac_address = next_mac();
        let oob_mac_address = next_mac();
        Self {
            hw_type,
            bmc_mac_address,
            host_mac_address,
            oob_mac_address,
            settings,
            serial: format!("MT{}", oob_mac_address.to_string().replace(':', "")),
        }
    }

    fn bluefield3(&self) -> hw::bluefield3::Bluefield3<'_> {
        let mode = match self.hw_type {
            HardwareType::DellPowerEdgeR750
            | HardwareType::NvidiaDgxH100
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro => hw::bluefield3::Mode::SuperNIC {
                nic_mode: self.settings.nic_mode,
            },
            // GB-class cold-aisle DPU mode. Confirmed for GB200; for DGX/SMC GB300 the BF3
            // chassis is in the scrape but the mode is not separately confirmed (synthetic).
            HardwareType::WiwynnGB200Nvl
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl => hw::bluefield3::Mode::B3240ColdAisle,
            HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld
            | HardwareType::NvidiaDgxVr
            | HardwareType::DellPowerEdgeR760Bf4 => {
                panic!("Bluefield3 DPU is defined for {}", self.hw_type)
            }
        };
        let settings = &self.settings;
        hw::bluefield3::Bluefield3 {
            host_mac_address: self.host_mac_address,
            bmc_mac_address: self.bmc_mac_address,
            oob_mac_address: settings.exposes_oob_eth.then_some(self.oob_mac_address),
            mode,
            product_serial_number: Cow::Borrowed(&self.serial),
            firmware_versions: hw::bluefield3::FirmwareVersions {
                bmc: settings.firmware_versions.bmc.clone().unwrap_or_default(),
                uefi: settings.firmware_versions.uefi.clone().unwrap_or_default(),
                erot: settings.firmware_versions.cec.clone().unwrap_or_default(),
                dpu_nic: settings.firmware_versions.nic.clone().unwrap_or_default(),
            },
        }
    }

    fn bluefield4(&self) -> hw::bluefield4::Bluefield4<'_> {
        let mode = match self.hw_type {
            HardwareType::DellPowerEdgeR750
            | HardwareType::NvidiaDgxH100
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro
            | HardwareType::WiwynnGB200Nvl
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl
            | HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld => {
                panic!("Bluefield4 DPU is defined for {}", self.hw_type)
            }
            HardwareType::NvidiaDgxVr => hw::bluefield4::Mode::B4240V,
            HardwareType::DellPowerEdgeR760Bf4 => hw::bluefield4::Mode::B4240,
        };
        hw::bluefield4::Bluefield4 {
            host_mac_address: self.host_mac_address,
            oob_mac_address: self.oob_mac_address,
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
            mode,
        }
    }

    fn dpu_type(&self) -> DpuType {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750
            | HardwareType::NvidiaDgxH100
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro
            | HardwareType::WiwynnGB200Nvl
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl
            | HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld => DpuType::Bluefield3,
            HardwareType::DellPowerEdgeR760Bf4 | HardwareType::NvidiaDgxVr => DpuType::Bluefield4,
        }
    }

    /// The [`bmc_vendor::DpuModel`] this DPU emulates, so the mock can share
    /// per-model logic (e.g. factory-default credentials) with the rest of the
    /// stack rather than duplicating it against the local [`DpuType`].
    fn dpu_model(&self) -> bmc_vendor::DpuModel {
        match self.dpu_type() {
            DpuType::Bluefield3 => bmc_vendor::DpuModel::BlueField3,
            DpuType::Bluefield4 => bmc_vendor::DpuModel::BlueField4,
        }
    }

    fn bmc_product(&self) -> Option<&'static str> {
        match self.dpu_type() {
            DpuType::Bluefield3 => Some("BlueField-3 DPU"),
            DpuType::Bluefield4 => Some("BlueField-4"),
        }
    }

    fn manager_config(&self) -> redfish::manager::Config {
        match self.dpu_type() {
            DpuType::Bluefield3 => self.bluefield3().manager_config(),
            DpuType::Bluefield4 => self.bluefield4().manager_config(),
        }
    }

    fn system_config(
        &self,
        callbacks: Arc<dyn crate::Callbacks>,
    ) -> redfish::computer_system::Config {
        match self.dpu_type() {
            DpuType::Bluefield3 => self.bluefield3().system_config(callbacks),
            DpuType::Bluefield4 => self.bluefield4().system_config(callbacks),
        }
    }

    fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        match self.dpu_type() {
            DpuType::Bluefield3 => self.bluefield3().chassis_config(),
            DpuType::Bluefield4 => self.bluefield4().chassis_config(),
        }
    }

    fn update_service_config(&self) -> UpdateServiceConfig {
        let mut config = match self.dpu_type() {
            DpuType::Bluefield3 => self.bluefield3().update_service_config(),
            DpuType::Bluefield4 => self.bluefield4().update_service_config(),
        };
        self.append_configured_firmware_inventory(&mut config);
        config
    }

    fn append_configured_firmware_inventory(&self, config: &mut UpdateServiceConfig) {
        let dpu_type = self.dpu_type();
        for (component, version) in self.settings.firmware_versions.configured() {
            let Some(id) = dpu_type.firmware_inventory_id(component) else {
                continue;
            };
            if config
                .firmware_inventory
                .iter()
                .any(|existing| existing.id == id)
            {
                continue;
            }
            config.firmware_inventory.push(
                redfish::software_inventory::builder(
                    &redfish::software_inventory::firmware_inventory_resource(id),
                )
                .version(version)
                .build(),
            );
        }
    }

    fn oem_state(&self) -> redfish::oem::State {
        match self.dpu_type() {
            DpuType::Bluefield3 => redfish::oem::State::NvidiaBluefield(
                redfish::oem::nvidia::bluefield::BluefieldState::new_bf3(
                    self.settings.nic_mode,
                    self.host_mac_address,
                ),
            ),
            DpuType::Bluefield4 => redfish::oem::State::NvidiaBluefield(
                redfish::oem::nvidia::bluefield::BluefieldState::new_bf4(),
            ),
        }
    }
}

impl HostMachineInfo {
    pub fn new(
        hw_type: HardwareType,
        dpus: Vec<DpuMachineInfo>,
        pool: &mut MacAddressPool,
        hw_mac_addr_pool: MacAddressPoolConfig,
    ) -> Self {
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        let bmc_mac_address = next_mac();
        let nvos_mac_addresses = if matches!(
            hw_type,
            HardwareType::NvidiaSwitchNd5200Ld | HardwareType::NvidiaSwitchN5700Ld
        ) {
            vec![next_mac()]
        } else {
            vec![]
        };
        let switch_serial_number = nvos_mac_addresses
            .first()
            .map(|mac| format!("MT{}", mac.to_string().replace(':', "")));
        Self {
            hw_type,
            rack_placement: None,
            bmc_mac_address,
            serial: bmc_mac_address.to_string().replace(':', ""),
            non_dpu_mac_address: if dpus.is_empty()
                && !matches!(
                    hw_type,
                    HardwareType::LiteOnPowerShelf
                        | HardwareType::DeltaPowerShelf
                        | HardwareType::NvidiaSwitchNd5200Ld
                        | HardwareType::NvidiaSwitchN5700Ld
                ) {
                Some(next_mac())
            } else {
                None
            },
            nvos_mac_addresses,
            switch_serial_number,
            dpus,
            hw_mac_addr_pool,
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        }
    }

    /// Set the initial host firmware versions used to populate the Redfish
    /// FirmwareInventory when this machine's mock BMC starts.  machine-a-tron
    /// calls this from the operator-provided `host_firmware_versions` config.
    #[must_use]
    pub fn with_initial_host_firmware(mut self, fw: HostFirmwareVersions) -> Self {
        self.initial_host_firmware = Some(fw);
        self
    }

    /// Override the Delta power shelf's per-PSU on/off states (one entry per
    /// PSU bay). Used by tests to model off/mixed shelves; the default is an
    /// all-on six-bay shelf.
    #[must_use]
    pub(super) fn with_delta_psu_power(mut self, states: Vec<bool>) -> Self {
        self.delta_psu_power = Some(states);
        self
    }

    fn primary_dpu(&self) -> Option<&DpuMachineInfo> {
        self.dpus.first()
    }

    pub fn system_mac_address(&self) -> Option<MacAddress> {
        self.primary_dpu()
            .map(|d| d.host_mac_address)
            .or(self.non_dpu_mac_address)
    }

    pub fn infiniband_port_guids(&self) -> Vec<Guid> {
        let [b0, b1, b2, b3, b4, b5] = self.hw_mac_addr_pool.base().bytes();
        (0..self.hw_type.infiniband_port_count())
            .map(|interface_index| {
                let interface_index = u16::try_from(interface_index)
                    .expect("mock hardware models have fewer than 65536 InfiniBand interfaces");
                let [b6, b7] = interface_index.to_be_bytes();
                Guid::from([b0, b1, b2, b3, b4, b5, b6, b7])
            })
            .collect()
    }

    fn oem_state(&self) -> redfish::oem::State {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 | HardwareType::DellPowerEdgeR760Bf4 => {
                redfish::oem::State::DellIdrac(redfish::oem::dell::idrac::IdracState::default())
            }
            HardwareType::WiwynnGB200Nvl
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::NvidiaDgxVr
            | HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaDgxH100
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld
            | HardwareType::GenericAmi
            | HardwareType::HpeProliantDl380aGen11
            | HardwareType::GenericSupermicro => redfish::oem::State::Other,
            HardwareType::SupermicroGb300Nvl => redfish::oem::State::Supermicro(
                redfish::oem::supermicro::manager::SupermicroState::default(),
            ),
        }
    }

    fn bmc_vendor(&self) -> redfish::oem::BmcVendor {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 | HardwareType::DellPowerEdgeR760Bf4 => {
                redfish::oem::BmcVendor::Dell
            }
            HardwareType::WiwynnGB200Nvl => redfish::oem::BmcVendor::Wiwynn,
            HardwareType::LenovoGB300Nvl => redfish::oem::BmcVendor::Ami,
            HardwareType::NvidiaDgxGb300 => {
                redfish::oem::BmcVendor::Nvidia(redfish::oem::NvidiaNamestyle::Uppercase)
            }
            HardwareType::SupermicroGb300Nvl => redfish::oem::BmcVendor::Supermicro,
            HardwareType::NvidiaDgxVr => {
                redfish::oem::BmcVendor::Nvidia(redfish::oem::NvidiaNamestyle::Uppercase)
            }
            HardwareType::LiteOnPowerShelf => redfish::oem::BmcVendor::LiteOn,
            HardwareType::DeltaPowerShelf => redfish::oem::BmcVendor::Delta,
            HardwareType::NvidiaSwitchNd5200Ld => {
                redfish::oem::BmcVendor::Nvidia(redfish::oem::NvidiaNamestyle::Uppercase)
            }
            HardwareType::NvidiaSwitchN5700Ld => {
                redfish::oem::BmcVendor::Nvidia(redfish::oem::NvidiaNamestyle::Uppercase)
            }
            HardwareType::NvidiaDgxH100 => redfish::oem::BmcVendor::Ami,
            HardwareType::GenericAmi => redfish::oem::BmcVendor::Ami,
            HardwareType::HpeProliantDl380aGen11 => redfish::oem::BmcVendor::Hpe,
            HardwareType::GenericSupermicro => redfish::oem::BmcVendor::Supermicro,
        }
    }

    fn bmc_product(&self) -> Option<&'static str> {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 => None,
            HardwareType::DellPowerEdgeR760Bf4 => Some("Integrated Dell Remote Access Controller"),
            HardwareType::WiwynnGB200Nvl => Some("GB200 NVL"),
            HardwareType::LenovoGB300Nvl => Some("AMI Redfish Server"),
            HardwareType::NvidiaDgxGb300 => Some("GB BMC"),
            HardwareType::SupermicroGb300Nvl => Some("GB NVL"),
            HardwareType::NvidiaDgxVr => Some("VR NVL72"),
            HardwareType::LiteOnPowerShelf => None,
            HardwareType::DeltaPowerShelf => None,
            HardwareType::NvidiaSwitchNd5200Ld => Some("P3809"),
            HardwareType::NvidiaSwitchN5700Ld => Some("P3809"),
            HardwareType::NvidiaDgxH100 => Some("AMI Redfish Server"),
            HardwareType::GenericAmi => Some("AMI Redfish Server"),
            HardwareType::HpeProliantDl380aGen11 => Some("ProLiant DL380a Gen11"),
            HardwareType::GenericSupermicro => Some("Super Server"),
        }
    }

    fn bmc_redfish_version(&self) -> &'static str {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 | HardwareType::DellPowerEdgeR760Bf4 => "1.18.0",
            HardwareType::WiwynnGB200Nvl => "1.17.0",
            HardwareType::LenovoGB300Nvl => "1.21.1",
            HardwareType::NvidiaDgxGb300 => "1.17.0",
            HardwareType::SupermicroGb300Nvl => "1.17.0",
            HardwareType::NvidiaDgxVr => "1.17.0",
            HardwareType::LiteOnPowerShelf => "1.9.0",
            HardwareType::DeltaPowerShelf => "1.9.0",
            HardwareType::NvidiaSwitchNd5200Ld => "1.17.0",
            HardwareType::NvidiaSwitchN5700Ld => "1.17.0",
            HardwareType::NvidiaDgxH100 => "1.11.0",
            HardwareType::GenericAmi => "1.17.0",
            HardwareType::HpeProliantDl380aGen11 => "1.13.0",
            HardwareType::GenericSupermicro => "1.17.0",
        }
    }

    fn manager_config(&self) -> redfish::manager::Config {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 => self.dell_poweredge_r750().manager_config(),
            HardwareType::DellPowerEdgeR760Bf4 => self.dell_poweredge_r760_bf4().manager_config(),
            HardwareType::WiwynnGB200Nvl => self.wiwynn_gb200_nvl().manager_config(),
            HardwareType::LenovoGB300Nvl => self.lenovo_gb300_nvl().manager_config(),
            HardwareType::NvidiaDgxGb300 => self.dgx_gb300_nvl().manager_config(),
            HardwareType::SupermicroGb300Nvl => self.supermicro_gb300_nvl().manager_config(),
            HardwareType::NvidiaDgxVr => self.dgx_vr_nvl().manager_config(),
            HardwareType::LiteOnPowerShelf => self.liteon_power_shelf().manager_config(),
            HardwareType::DeltaPowerShelf => self.delta_power_shelf().manager_config(),
            HardwareType::NvidiaSwitchNd5200Ld => self.nvidia_switch_nd5200_ld().manager_config(),
            HardwareType::NvidiaSwitchN5700Ld => self.nvidia_switch_n5700_ld().manager_config(),
            HardwareType::NvidiaDgxH100 => self.nvidia_dgx_h100().manager_config(),
            HardwareType::HpeProliantDl380aGen11 => {
                self.hpe_proliant_dl380a_gen11().manager_config()
            }
            HardwareType::GenericAmi | HardwareType::GenericSupermicro => {
                self.generic_server().manager_config()
            }
        }
    }

    fn system_config(
        &self,
        callbacks: Arc<dyn crate::Callbacks>,
    ) -> redfish::computer_system::Config {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 => self.dell_poweredge_r750().system_config(callbacks),
            HardwareType::DellPowerEdgeR760Bf4 => {
                self.dell_poweredge_r760_bf4().system_config(callbacks)
            }
            HardwareType::WiwynnGB200Nvl => self.wiwynn_gb200_nvl().system_config(callbacks),
            HardwareType::LenovoGB300Nvl => self.lenovo_gb300_nvl().system_config(callbacks),
            HardwareType::NvidiaDgxGb300 => self.dgx_gb300_nvl().system_config(callbacks),
            HardwareType::NvidiaDgxVr => self.dgx_vr_nvl().system_config(callbacks),
            HardwareType::SupermicroGb300Nvl => {
                self.supermicro_gb300_nvl().system_config(callbacks)
            }
            HardwareType::LiteOnPowerShelf => self.liteon_power_shelf().system_config(),
            HardwareType::DeltaPowerShelf => self.delta_power_shelf().system_config(),
            HardwareType::NvidiaSwitchNd5200Ld => self.nvidia_switch_nd5200_ld().system_config(),
            HardwareType::NvidiaSwitchN5700Ld => self.nvidia_switch_n5700_ld().system_config(),
            HardwareType::NvidiaDgxH100 => self.nvidia_dgx_h100().system_config(callbacks),
            HardwareType::HpeProliantDl380aGen11 => {
                self.hpe_proliant_dl380a_gen11().system_config(callbacks)
            }
            HardwareType::GenericAmi | HardwareType::GenericSupermicro => {
                self.generic_server().system_config(callbacks)
            }
        }
    }

    fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        match self.hw_type {
            HardwareType::DellPowerEdgeR750 => self.dell_poweredge_r750().chassis_config(),
            HardwareType::DellPowerEdgeR760Bf4 => self.dell_poweredge_r760_bf4().chassis_config(),
            HardwareType::WiwynnGB200Nvl => self.wiwynn_gb200_nvl().chassis_config(),
            HardwareType::LenovoGB300Nvl => self.lenovo_gb300_nvl().chassis_config(),
            HardwareType::NvidiaDgxGb300 => self.dgx_gb300_nvl().chassis_config(),
            HardwareType::SupermicroGb300Nvl => self.supermicro_gb300_nvl().chassis_config(),
            HardwareType::NvidiaDgxVr => self.dgx_vr_nvl().chassis_config(),
            HardwareType::LiteOnPowerShelf => self.liteon_power_shelf().chassis_config(),
            HardwareType::DeltaPowerShelf => self.delta_power_shelf().chassis_config(),
            HardwareType::NvidiaSwitchNd5200Ld => self.nvidia_switch_nd5200_ld().chassis_config(),
            HardwareType::NvidiaSwitchN5700Ld => self.nvidia_switch_n5700_ld().chassis_config(),
            HardwareType::NvidiaDgxH100 => self.nvidia_dgx_h100().chassis_config(),
            HardwareType::HpeProliantDl380aGen11 => {
                self.hpe_proliant_dl380a_gen11().chassis_config()
            }
            HardwareType::GenericAmi | HardwareType::GenericSupermicro => {
                self.generic_server().chassis_config()
            }
        }
    }

    fn update_service_config(&self) -> UpdateServiceConfig {
        let mut config = match self.hw_type {
            HardwareType::DellPowerEdgeR750 => self.dell_poweredge_r750().update_service_config(),
            HardwareType::DellPowerEdgeR760Bf4 => {
                self.dell_poweredge_r760_bf4().update_service_config()
            }
            HardwareType::WiwynnGB200Nvl => self.wiwynn_gb200_nvl().update_service_config(),
            HardwareType::LenovoGB300Nvl => self.lenovo_gb300_nvl().update_service_config(),
            HardwareType::NvidiaDgxGb300 => self.dgx_gb300_nvl().update_service_config(),
            HardwareType::SupermicroGb300Nvl => self.supermicro_gb300_nvl().update_service_config(),
            HardwareType::NvidiaDgxVr => self.dgx_vr_nvl().update_service_config(),
            HardwareType::LiteOnPowerShelf => self.liteon_power_shelf().update_service_config(),
            HardwareType::DeltaPowerShelf => self.delta_power_shelf().update_service_config(),
            HardwareType::NvidiaSwitchNd5200Ld => {
                self.nvidia_switch_nd5200_ld().update_service_config()
            }
            HardwareType::NvidiaSwitchN5700Ld => {
                self.nvidia_switch_n5700_ld().update_service_config()
            }
            HardwareType::NvidiaDgxH100 => self.nvidia_dgx_h100().update_service_config(),
            HardwareType::HpeProliantDl380aGen11 => {
                self.hpe_proliant_dl380a_gen11().update_service_config()
            }
            HardwareType::GenericAmi | HardwareType::GenericSupermicro => {
                self.generic_server().update_service_config()
            }
        };

        // Apply operator-supplied initial host firmware versions on top of the
        // hardware-type defaults.  machine-a-tron sets these from the
        // `host_firmware` config block so the inventory starts at the version
        // carbide needs to upgrade from.
        if let Some(ref fw) = self.initial_host_firmware {
            config.apply_host_firmware_versions(fw);
        }

        // Host inventory owns any colliding ID; only explicitly configured DPU
        // entries are appended, rather than the DPU's complete baseline inventory.
        if let Some(dpu) = self.primary_dpu() {
            dpu.append_configured_firmware_inventory(&mut config);
        }

        // Populate the ordered pending_upgrades map so UpdateServiceState knows
        // what version to stage for each component when an upload arrives.
        // machine-a-tron sets desired_host_firmware from desired_firmware_versions
        // (the API-configured target); bmc-mock peeks from this map in record_upload()
        // when the upload request carries no explicit Targets — making component
        // identification deterministic even for multipart uploads.
        // IDs come from the platform-specific UpdateServiceConfig set above.
        if let Some(ref fw) = self.desired_host_firmware {
            if let (Some(id), Some(bmc)) = (&config.host_bmc_inventory_id, &fw.bmc) {
                config.pending_upgrades.insert(id.clone(), bmc.clone());
            }
            if let (Some(id), Some(uefi)) = (&config.host_uefi_inventory_id, &fw.uefi) {
                config.pending_upgrades.insert(id.clone(), uefi.clone());
            }
        }

        config
    }

    fn factory_default_account(&self) -> redfish::account_service::Account {
        // TODO: need to be updated for each individual system.
        let id = match self.hw_type {
            HardwareType::NvidiaDgxH100 | HardwareType::GenericAmi => "2",
            _ => DUMMY_FACTORY_USERNAME,
        };
        redfish::account_service::Account::administrator(
            id,
            DUMMY_FACTORY_USERNAME,
            DUMMY_FACTORY_PASSWORD,
        )
    }

    fn dell_poweredge_r750(&self) -> hw::dell_poweredge_r750::DellPowerEdgeR750<'_> {
        let nics = if self.dpus.is_empty() {
            self.non_dpu_mac_address
                .iter()
                .enumerate()
                .map(|(index, mac_address)| (index + 1, hw::nic::Nic::rooftop(*mac_address)))
                .collect()
        } else {
            self.dpus
                .iter()
                .enumerate()
                .map(|(index, dpu)| (index + 1, dpu.bluefield3().host_nic()))
                .collect()
        };
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        hw::dell_poweredge_r750::DellPowerEdgeR750 {
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
            nics,
            embedded_nic: hw::dell_poweredge_r750::EmbeddedNic {
                port_1: next_mac(),
                port_2: next_mac(),
            },
        }
    }

    fn dell_poweredge_r760_bf4(&self) -> hw::dell_poweredge_r760_bf4::DellPowerEdgeR760Bf4<'_> {
        let mut dpus = self.dpus.iter();
        hw::dell_poweredge_r760_bf4::DellPowerEdgeR760Bf4 {
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
            bf4: dpus
                .next()
                .expect("BF4 dpu must present")
                .bluefield4()
                .host_nic(),
        }
    }

    fn wiwynn_gb200_nvl(&self) -> hw::wiwynn_gb200_nvl::WiwynnGB200Nvl<'_> {
        let mut dpus = self.dpus.iter();
        hw::wiwynn_gb200_nvl::WiwynnGB200Nvl {
            system_serial_number: Cow::Borrowed(&self.serial),
            chassis_serial_number: Cow::Borrowed(&self.serial),
            compute_board: [
                hw::nvidia_gb200::BiancaBoard {
                    index: hw::nvidia_gb200::BoardIndex::Board0,
                    cpu_serial_number: "0x000000017FFFFFFFFF00000000000001".into(),
                    gpu_serial_number: "165300000001".into(),
                },
                hw::nvidia_gb200::BiancaBoard {
                    index: hw::nvidia_gb200::BoardIndex::Board1,
                    cpu_serial_number: "0x000000017FFFFFFFFF00000000000002".into(),
                    gpu_serial_number: "165300000002".into(),
                },
            ],
            dpu1: dpus
                .next()
                .expect("Two DPUs must present for GB200 NVL")
                .bluefield3(),
            dpu2: dpus
                .next()
                .expect("Two DPUs must present for GB200 NVL")
                .bluefield3(),
            io_board: [
                hw::nvidia_gb200::IoBoard {
                    serial_number: "MT0000000001".into(),
                },
                hw::nvidia_gb200::IoBoard {
                    serial_number: "MT0000000002".into(),
                },
            ],
            topology: self
                .rack_placement
                .and_then(hw::nvidia_gbx00::Topology::from_rack_placement),
        }
    }

    fn dgx_gb300_nvl(&self) -> hw::dgx_gb300_nvl::DgxGB300Nvl<'_> {
        let mut dpus = self.dpus.iter();
        // Serials are from the DGX GB300 scrape.
        // GPU_0/1 and GPU_2/3 share a superchip serial; the HGX baseboard
        // (Systems/HGX_Baseboard_0) reports the same serial as the first GPU superchip.
        // The DGX scrape has a single IO board (IO_Board_0); the mock's second slot is
        // a synthetic placeholder.
        let superchip_a_sn = "1642225000100";
        let boards = gb300_boards(
            [
                "0x000000017831E0C9100000000F018200",
                "0x000000017831E0C91000000018018240",
            ],
            [superchip_a_sn, "1642225000086"],
            ["MT2521XZ0GJM", "MT2521XZ0GJM-SYNTH"],
        );
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        hw::dgx_gb300_nvl::DgxGB300Nvl {
            system_0_serial_number: Cow::Borrowed(&self.serial),
            chassis_0_serial_number: Cow::Borrowed(&self.serial),
            dpu: dpus
                .next()
                .expect("One DPU must present for DGX GB300 NVL")
                .bluefield3(),
            embedded_1g_nic: hw::nic_intel_i210::NicIntelI210 {
                mac_address: next_mac(),
            },
            bmc_mac_address_eth0: next_mac(),
            bmc_mac_address_eth1: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            hgx_bmc_mac_address_usb0: next_mac(),
            hgx_serial_number: superchip_a_sn.into(),
            topology: self
                .rack_placement
                .and_then(hw::nvidia_gbx00::Topology::from_rack_placement),
            cpu: boards.cpu,
            gpu: boards.gpu,
            io_board: boards.io_board,
        }
    }

    fn supermicro_gb300_nvl(&self) -> hw::supermicro_gb300_nvl::SupermicroGB300Nvl<'_> {
        let mut dpus = self.dpus.iter();
        // Serials are from the SMC GB300 tray scrape.
        // GPU_0/1 and GPU_2/3 share a superchip serial; the HGX baseboard
        // (Systems/HGX_Baseboard_0) reports the same serial as the first GPU superchip.
        let superchip_a_sn = "1764625801410";
        let boards = gb300_boards(
            [
                "0x000000017844A04120000000120081C0",
                "0x00000001784191C11000000008018040",
            ],
            [superchip_a_sn, "1764625800673"],
            ["MT2609603LCN", "MT2609603LQ2"],
        );
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        hw::supermicro_gb300_nvl::SupermicroGB300Nvl {
            system_0_serial_number: Cow::Borrowed(&self.serial),
            chassis_0_serial_number: Cow::Borrowed(&self.serial),
            dpu: dpus
                .next()
                .expect("One DPU must present for SMC GB300 NVL")
                .bluefield3(),
            embedded_1g_nic: hw::nic_intel_i210::NicIntelI210 {
                mac_address: next_mac(),
            },
            bmc_mac_address_eth0: next_mac(),
            bmc_mac_address_eth1: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            hgx_bmc_mac_address_usb0: next_mac(),
            hgx_serial_number: superchip_a_sn.into(),
            topology: self
                .rack_placement
                .and_then(hw::nvidia_gbx00::Topology::from_rack_placement),
            cpu: boards.cpu,
            gpu: boards.gpu,
            io_board: boards.io_board,
        }
    }

    fn lenovo_gb300_nvl(&self) -> hw::lenovo_gb300_nvl::LenovoGB300Nvl<'_> {
        let mut dpus = self.dpus.iter();
        let cpu0_sn = "0x000000017FFFFFFFFF00000000000001";
        let cpu1_sn = "0x000000017FFFFFFFFF00000000000002";
        let superchip_a_sn = "165300000001";
        let superchip_b_sn = "165300000002";
        let io_board0_sn = "MT2524000001";
        let io_board1_sn = "MT2524000002";
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        // Machine-a-tron's `lenovo_network_interfaces` assigns the first ten
        // addresses to the CX-8s. Reserve the same slots here so the embedded
        // NIC and BMC interfaces report matching addresses.
        for _ in 0..10 {
            next_mac();
        }
        hw::lenovo_gb300_nvl::LenovoGB300Nvl {
            system_0_serial_number: Cow::Borrowed(&self.serial),
            chassis_0_serial_number: Cow::Borrowed(&self.serial),
            dpu: dpus
                .next()
                .expect("One DPU must present for GB300 NVL")
                .bluefield3(),
            embedded_1g_nic: hw::nic_intel_i210::NicIntelI210 {
                mac_address: next_mac(),
            },
            bmc_mac_address_eth0: next_mac(),
            bmc_mac_address_eth1: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            hgx_bmc_mac_address_usb0: next_mac(),
            hgx_serial_number: "012345678901234567890123".into(),
            topology: self
                .rack_placement
                .and_then(hw::nvidia_gbx00::Topology::from_rack_placement),
            cpu: [
                hw::nvidia_gb300::NvidiaGB300Cpu {
                    serial_number: cpu0_sn.into(),
                },
                hw::nvidia_gb300::NvidiaGB300Cpu {
                    serial_number: cpu1_sn.into(),
                },
            ],
            gpu: [
                hw::nvidia_gb300::NvidiaGB300Gpu {
                    serial_number: superchip_a_sn.into(),
                },
                hw::nvidia_gb300::NvidiaGB300Gpu {
                    serial_number: superchip_a_sn.into(),
                },
                hw::nvidia_gb300::NvidiaGB300Gpu {
                    serial_number: superchip_b_sn.into(),
                },
                hw::nvidia_gb300::NvidiaGB300Gpu {
                    serial_number: superchip_b_sn.into(),
                },
            ],
            io_board: [
                hw::nvidia_gb300::NvidiaGB300IoBoard {
                    serial_number: io_board0_sn.into(),
                },
                hw::nvidia_gb300::NvidiaGB300IoBoard {
                    serial_number: io_board1_sn.into(),
                },
            ],
        }
    }

    fn dgx_vr_nvl(&self) -> hw::dgx_vr_nvl::DgxVrNvl<'_> {
        let mut dpus = self.dpus.iter();
        hw::dgx_vr_nvl::DgxVrNvl {
            system_0_serial_number: "012345678901234567890123".into(),
            chassis_0_serial_number: Cow::Borrowed(&self.serial),
            dpu: dpus
                .next()
                .expect("One DPU must present for VR NVL")
                .bluefield4(),
            bmc_mac_address_eth0: self.bmc_mac_address,
        }
    }

    fn liteon_power_shelf(&self) -> hw::liteon_power_shelf::LiteOnPowerShelf<'_> {
        hw::liteon_power_shelf::LiteOnPowerShelf {
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
        }
    }

    fn delta_power_shelf(&self) -> hw::delta_power_shelf::DeltaPowerShelf<'_> {
        hw::delta_power_shelf::DeltaPowerShelf {
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
            psu_power: self.delta_psu_power.as_deref().map_or(
                Cow::Borrowed(hw::delta_power_shelf::DEFAULT_PSU_POWER),
                Cow::Borrowed,
            ),
        }
    }

    /// Whether this host advertises and serves a `/redfish/v1/Systems`
    /// collection. Delta power shelves do not.
    fn exposes_computer_systems(&self) -> bool {
        !matches!(self.hw_type, HardwareType::DeltaPowerShelf)
    }

    fn nvidia_switch_nd5200_ld(&self) -> hw::nvidia_switch_nd5200_ld::NvidiaSwitchNd5200Ld<'_> {
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        hw::nvidia_switch_nd5200_ld::NvidiaSwitchNd5200Ld {
            bmc_mac_address_eth0: self.bmc_mac_address,
            bmc_mac_address_eth1: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            bmc_serial_number: Cow::Borrowed(&self.serial),
            switch_serial_number: self
                .switch_serial_number
                .as_deref()
                .unwrap_or(&self.serial)
                .into(),
        }
    }

    fn nvidia_switch_n5700_ld(&self) -> hw::nvidia_switch_n5700_ld::NvidiaSwitchN5700Ld<'_> {
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        hw::nvidia_switch_n5700_ld::NvidiaSwitchN5700Ld {
            bmc_mac_address_eth0: self.bmc_mac_address,
            bmc_mac_address_eth1: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            bmc_serial_number: Cow::Borrowed(&self.serial),
            switch_serial_number: self
                .switch_serial_number
                .as_deref()
                .unwrap_or(&self.serial)
                .into(),
        }
    }

    fn nvidia_dgx_h100(&self) -> hw::nvidia_dgx_h100::NvidiaDgxH100<'_> {
        let mut pool = MacAddressPool::new_pool(self.hw_mac_addr_pool);
        let mut next_mac = || pool.allocate().expect("MAC address must be allocated");
        let storage_nic0_p0_mac = next_mac();
        let storage_nic0_serial = format!("MT{}", storage_nic0_p0_mac.to_string().replace(":", ""));
        hw::nvidia_dgx_h100::NvidiaDgxH100 {
            dgx_system_serial_number: Cow::Borrowed(&self.serial),
            dgx_chassis_serial_number: Cow::Borrowed("1663223000002"),
            ib_nics: [
                hw::nic_nvidia_cx7::NicNvidiaCx7B {
                    serial_number: "MT2307X00001".into(),
                    mac_addresses: [(); _].map(|_| next_mac()),
                },
                hw::nic_nvidia_cx7::NicNvidiaCx7B {
                    serial_number: "MT2307X00002".into(),
                    mac_addresses: [(); _].map(|_| next_mac()),
                },
            ],
            mgmt_nic: hw::nic_intel_x550::NicIntelX550 {
                mac_address: next_mac(),
            },
            storage_nic0: hw::nic_nvidia_cx7::NicNvidiaCx7A {
                serial_number: storage_nic0_serial.into(),
                mac_addresses: [(); _].map(|_| next_mac()),
            },
            storage_nic1: hw::nic_intel_e810::NicIntelE810 {
                mac_addresses: [(); _].map(|_| next_mac()),
            },
            dpu: self
                .dpus
                .first()
                .expect("Single DPUs must present for H100")
                .bluefield3(),
            gpu_serial: [
                "1652900000001".into(),
                "1652900000002".into(),
                "1652900000003".into(),
                "1652900000004".into(),
                "1652900000005".into(),
                "1652900000006".into(),
                "1652900000007".into(),
                "1652900000008".into(),
            ],
            bmc_mac_address_eth0: next_mac(),
            bmc_mac_address_usb0: next_mac(),
            hgx_bmc_mac_address_usb0: next_mac(),
        }
    }

    fn hpe_proliant_dl380a_gen11(
        &self,
    ) -> hw::hpe_proliant_dl380a_gen11::HpeProliantDl380aGen11<'_> {
        let nics = if self.dpus.is_empty() {
            self.non_dpu_mac_address
                .iter()
                .enumerate()
                .map(|(index, mac_address)| (index + 1, hw::nic::Nic::rooftop(*mac_address)))
                .collect()
        } else {
            self.dpus
                .iter()
                .enumerate()
                .map(|(index, dpu)| (index + 1, dpu.bluefield3().host_nic()))
                .collect()
        };
        hw::hpe_proliant_dl380a_gen11::HpeProliantDl380aGen11 {
            bmc_mac_address: self.bmc_mac_address,
            product_serial_number: Cow::Borrowed(&self.serial),
            nics,
        }
    }

    fn generic_server(&self) -> hw::generic_ami::GenericAmi<'_> {
        let nics = self
            .dpus
            .iter()
            .enumerate()
            .map(|(index, dpu)| (index + 1, dpu.bluefield3().host_nic()))
            .collect();

        hw::generic_ami::GenericAmi {
            product_serial_number: Cow::Borrowed(&self.serial),
            nics,
        }
    }
}

impl MachineInfo {
    /// Returns whether machine-a-tron should start an IPMI console simulator for this profile.
    pub fn needs_ipmi_console(&self) -> bool {
        match self {
            MachineInfo::Host(host) => match host.hw_type {
                HardwareType::WiwynnGB200Nvl
                | HardwareType::NvidiaDgxGb300
                | HardwareType::SupermicroGb300Nvl
                | HardwareType::NvidiaDgxVr
                | HardwareType::NvidiaSwitchNd5200Ld
                | HardwareType::NvidiaSwitchN5700Ld
                | HardwareType::GenericSupermicro => true,
                HardwareType::DellPowerEdgeR750
                | HardwareType::DellPowerEdgeR760Bf4
                | HardwareType::LenovoGB300Nvl
                | HardwareType::LiteOnPowerShelf
                | HardwareType::DeltaPowerShelf
                | HardwareType::NvidiaDgxH100
                | HardwareType::GenericAmi
                | HardwareType::HpeProliantDl380aGen11 => false,
            },
            MachineInfo::Dpu(_) => false,
        }
    }

    pub(super) fn oem_state(&self) -> redfish::oem::State {
        match self {
            MachineInfo::Host(host) => host.oem_state(),
            MachineInfo::Dpu(dpu) => dpu.oem_state(),
        }
    }

    pub(super) fn manager_config(&self) -> redfish::manager::Config {
        match self {
            MachineInfo::Host(host) => host.manager_config(),
            MachineInfo::Dpu(dpu) => dpu.manager_config(),
        }
    }

    pub(super) fn bmc_vendor(&self) -> redfish::oem::BmcVendor {
        match self {
            MachineInfo::Host(h) => h.bmc_vendor(),
            MachineInfo::Dpu(_) => {
                redfish::oem::BmcVendor::Nvidia(redfish::oem::NvidiaNamestyle::Capitalized)
            }
        }
    }

    pub(super) fn bmc_redfish_version(&self) -> &'static str {
        match self {
            MachineInfo::Host(h) => h.bmc_redfish_version(),
            MachineInfo::Dpu(_) => "1.17.0",
        }
    }

    pub(super) fn bmc_product(&self) -> Option<&'static str> {
        match self {
            MachineInfo::Host(h) => h.bmc_product(),
            MachineInfo::Dpu(d) => d.bmc_product(),
        }
    }

    pub(super) fn system_config(
        &self,
        callbacks: Arc<dyn crate::Callbacks>,
    ) -> redfish::computer_system::Config {
        match self {
            MachineInfo::Host(host) => host.system_config(callbacks),
            MachineInfo::Dpu(dpu) => dpu.system_config(callbacks),
        }
    }

    pub(super) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        match self {
            Self::Host(h) => h.chassis_config(),
            Self::Dpu(dpu) => dpu.chassis_config(),
        }
    }

    pub(super) fn update_service_config(&self) -> UpdateServiceConfig {
        match self {
            Self::Host(h) => h.update_service_config(),
            Self::Dpu(dpu) => dpu.update_service_config(),
        }
    }

    /// Whether this machine advertises and serves a `/redfish/v1/Systems`
    /// collection. Only Delta power shelves omit it.
    pub(super) fn exposes_computer_systems(&self) -> bool {
        match self {
            Self::Host(h) => h.exposes_computer_systems(),
            Self::Dpu(_) => true,
        }
    }

    pub fn bmc_mac_address(&self) -> MacAddress {
        match self {
            Self::Host(h) => h.bmc_mac_address,
            Self::Dpu(d) => d.bmc_mac_address,
        }
    }

    /// Returns the mac addresses this system would use to request DHCP on boot
    pub fn dhcp_mac_addresses(&self) -> Vec<MacAddress> {
        match self {
            Self::Host(h) => {
                if h.dpus.is_empty() {
                    h.non_dpu_mac_address.map(|m| vec![m]).unwrap_or_default()
                } else {
                    h.dpus.iter().map(|d| d.host_mac_address).collect()
                }
            }
            Self::Dpu(d) => vec![d.oob_mac_address],
        }
    }

    // If this is a DPU, return its host mac address
    pub fn host_mac_address(&self) -> Option<MacAddress> {
        if let Self::Dpu(d) = self {
            Some(d.host_mac_address)
        } else {
            None
        }
    }

    pub(super) fn factory_default_account(&self) -> redfish::account_service::Account {
        match self {
            MachineInfo::Host(h) => h.factory_default_account(),
            MachineInfo::Dpu(d) => {
                // Read the per-model default from the shared source of truth so the
                // mock and site-explorer's fallback cannot drift.
                let (username, password) = d.dpu_model().default_factory_credentials();
                redfish::account_service::Account::administrator(username, username, password)
            }
        }
    }
}

/// CPU / GPU / IO-board chassis common to every GB300 tray: NVIDIA HGX reference
/// silicon, identical in shape across ODMs (only the serials differ per scrape).
/// GPU_0/1 and GPU_2/3 each share a superchip serial.
struct Gb300Boards<'a> {
    cpu: [hw::nvidia_gb300::NvidiaGB300Cpu<'a>; 2],
    gpu: [hw::nvidia_gb300::NvidiaGB300Gpu<'a>; 4],
    io_board: [hw::nvidia_gb300::NvidiaGB300IoBoard<'a>; 2],
}

fn gb300_boards<'a>(
    cpu_serials: [&'a str; 2],
    superchip_serials: [&'a str; 2],
    io_board_serials: [&'a str; 2],
) -> Gb300Boards<'a> {
    let [cpu0, cpu1] = cpu_serials;
    let [superchip_a, superchip_b] = superchip_serials;
    let [io0, io1] = io_board_serials;
    Gb300Boards {
        cpu: [
            hw::nvidia_gb300::NvidiaGB300Cpu {
                serial_number: cpu0.into(),
            },
            hw::nvidia_gb300::NvidiaGB300Cpu {
                serial_number: cpu1.into(),
            },
        ],
        gpu: [
            hw::nvidia_gb300::NvidiaGB300Gpu {
                serial_number: superchip_a.into(),
            },
            hw::nvidia_gb300::NvidiaGB300Gpu {
                serial_number: superchip_a.into(),
            },
            hw::nvidia_gb300::NvidiaGB300Gpu {
                serial_number: superchip_b.into(),
            },
            hw::nvidia_gb300::NvidiaGB300Gpu {
                serial_number: superchip_b.into(),
            },
        ],
        io_board: [
            hw::nvidia_gb300::NvidiaGB300IoBoard {
                serial_number: io0.into(),
            },
            hw::nvidia_gb300::NvidiaGB300IoBoard {
                serial_number: io1.into(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac_address_pool::{Config, PoolConfig};

    fn dpu(
        hw_type: HardwareType,
        pool: &mut MacAddressPool,
        firmware_versions: DpuFirmwareVersions,
    ) -> DpuMachineInfo {
        DpuMachineInfo::new(
            hw_type,
            pool,
            DpuSettings {
                firmware_versions,
                ..DpuSettings::default()
            },
        )
    }

    fn gb300_host_info(hw_type: HardwareType, pool: &mut MacAddressPool) -> HostMachineInfo {
        let hw_mac_addr_pool = PoolConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 16)
            .expect("valid hardware MAC pool");
        let dpu = DpuMachineInfo::new(hw_type, pool, DpuSettings::default());
        HostMachineInfo::new(hw_type, vec![dpu], pool, hw_mac_addr_pool)
    }

    #[test]
    fn gb300_primary_serials_match_machine_serial() {
        let pool_config =
            PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
        let mut pool = MacAddressPool::new(Config {
            ranges: None,
            pool: Some(pool_config),
        });

        let dgx = gb300_host_info(HardwareType::NvidiaDgxGb300, &mut pool);
        let supermicro = gb300_host_info(HardwareType::SupermicroGb300Nvl, &mut pool);

        let dgx_redfish = dgx.dgx_gb300_nvl();
        assert_eq!(dgx_redfish.system_0_serial_number, dgx.serial);
        assert_eq!(dgx_redfish.chassis_0_serial_number, dgx.serial);

        let supermicro_redfish = supermicro.supermicro_gb300_nvl();
        assert_eq!(supermicro_redfish.system_0_serial_number, supermicro.serial);
        assert_eq!(
            supermicro_redfish.chassis_0_serial_number,
            supermicro.serial
        );
        assert_ne!(dgx.serial, supermicro.serial);
    }

    #[test]
    fn switch_profiles_allocate_one_nvos_mac() {
        for hardware_type in [
            HardwareType::NvidiaSwitchNd5200Ld,
            HardwareType::NvidiaSwitchN5700Ld,
        ] {
            let pool_config =
                PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
            let mut pool = MacAddressPool::new(Config {
                ranges: None,
                pool: Some(pool_config),
            });
            let hw_mac_addr_pool = PoolConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 16)
                .expect("valid hardware MAC pool");

            let host = HostMachineInfo::new(hardware_type, vec![], &mut pool, hw_mac_addr_pool);

            assert_eq!(host.nvos_mac_addresses.len(), 1, "{hardware_type}");
            assert!(host.switch_serial_number.is_some(), "{hardware_type}");
            assert_eq!(host.non_dpu_mac_address, None, "{hardware_type}");
        }
    }

    #[test]
    fn configured_dpu_firmware_uses_generation_specific_inventory_ids() {
        for (hardware_type, expected_inventory) in [
            (
                HardwareType::DellPowerEdgeR750,
                [
                    ("BMC_Firmware", "bmc-version"),
                    ("DPU_UEFI", "uefi-version"),
                    ("DPU_BSP", "bsp-version"),
                    ("Bluefield_FW_ERoT", "cec-version"),
                    ("DPU_NIC", "nic-version"),
                ],
            ),
            (
                HardwareType::DellPowerEdgeR760Bf4,
                [
                    ("BlueField_FW_BMC_0", "bmc-version"),
                    ("BlueField_FW_CPU_0", "uefi-version"),
                    ("DPU_BSP", "bsp-version"),
                    ("BlueField_FW_ERoT_BMC_0", "cec-version"),
                    ("BlueField_FW_NIC_0", "nic-version"),
                ],
            ),
        ] {
            let pool_config =
                PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
            let mut pool = MacAddressPool::new(Config {
                ranges: None,
                pool: Some(pool_config),
            });
            let dpu = dpu(
                hardware_type,
                &mut pool,
                DpuFirmwareVersions {
                    bmc: Some("bmc-version".to_string()),
                    uefi: Some("uefi-version".to_string()),
                    bsp: Some("bsp-version".to_string()),
                    cec: Some("cec-version".to_string()),
                    nic: Some("nic-version".to_string()),
                },
            );
            let config = dpu.update_service_config();

            for (id, expected) in expected_inventory {
                let inventory = config
                    .firmware_inventory
                    .iter()
                    .find(|inventory| inventory.id == id)
                    .unwrap_or_else(|| panic!("missing {id} for {hardware_type}"));
                assert_eq!(
                    inventory.to_json()["Version"].as_str(),
                    Some(expected),
                    "{hardware_type}",
                );
            }

            if matches!(hardware_type, HardwareType::DellPowerEdgeR760Bf4) {
                for bf3_only_id in ["BMC_Firmware", "DPU_UEFI", "Bluefield_FW_ERoT", "DPU_NIC"] {
                    assert!(
                        config
                            .firmware_inventory
                            .iter()
                            .all(|inventory| inventory.id != bf3_only_id),
                        "unexpected BF3-only inventory ID {bf3_only_id}",
                    );
                }
            }
        }
    }

    #[test]
    fn host_adds_only_explicit_primary_dpu_firmware() {
        let pool_config =
            PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
        let mut pool = MacAddressPool::new(Config {
            ranges: None,
            pool: Some(pool_config),
        });
        let dpu = dpu(
            HardwareType::DellPowerEdgeR750,
            &mut pool,
            DpuFirmwareVersions {
                bsp: Some("bsp-version".to_string()),
                ..DpuFirmwareVersions::default()
            },
        );
        let hw_mac_addr_pool = PoolConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 16)
            .expect("valid hardware MAC pool");
        let host = HostMachineInfo::new(
            HardwareType::DellPowerEdgeR750,
            vec![dpu],
            &mut pool,
            hw_mac_addr_pool,
        );
        let config = host.update_service_config();
        let mut ids = config
            .firmware_inventory
            .iter()
            .map(|inventory| inventory.id.as_ref())
            .collect::<Vec<_>>();
        ids.sort_unstable();

        assert_eq!(ids, ["DPU_BSP", "HostBIOS_0", "HostBMC_0"]);
        assert_eq!(
            config
                .firmware_inventory
                .iter()
                .find(|inventory| inventory.id == "DPU_BSP")
                .expect("DPU_BSP must be present")
                .to_json()["Version"]
                .as_str(),
            Some("bsp-version"),
        );
    }
}
