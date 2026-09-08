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
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use carbide_network::BaseMac;
use carbide_utils::arch::CpuArchitecture;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineType};
use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use itertools::Itertools;
use mac_address::MacAddress;
#[cfg(test)]
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

use super::DpuModel;
use super::bmc_info::BmcInfo;
use super::hardware_info::DpuData;
use crate::errors::{ErrorCode, ErrorSubsystem, ModelError, ModelResult, OperatorError};
use crate::firmware::{Firmware, FirmwareComponentType};
use crate::hardware_info::{DmiData, HardwareInfo, HardwareInfoError};
use crate::machine::machine_id::{MissingHardwareInfo, from_hardware_info_with_type};
use crate::machine_boot_interface::{
    BootInterfaceSelectionSource, MachineBootInterface, MachineBootInterfaceTarget,
};
use crate::pci::{UefiPciOrderingKey, UefiPciOrderingKeyParseError, normalize_uefi_device_path};
use crate::power_shelf::power_shelf_id;
use crate::switch::switch_id;

#[derive(Clone, Debug, Default)]
pub struct ExploredEndpointSearchFilter {}

#[derive(Clone, Debug, Default)]
pub struct ExploredManagedHostSearchFilter {}

/// Data that we gathered about a particular endpoint during site exploration
/// This data is stored as JSON in the Database. Therefore the format can
/// only be adjusted in a backward compatible fashion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct EndpointExplorationReport {
    /// The type of the endpoint
    pub endpoint_type: EndpointType,
    /// If the endpoint could not be explored, this contains the last error
    pub last_exploration_error: Option<EndpointExplorationError>,
    /// The time it took to explore the endpoint in the last site explorer run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exploration_latency: Option<std::time::Duration>,
    /// Vendor as reported by Redfish
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<bmc_vendor::BMCVendor>,
    /// `Managers` reported by Redfish
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managers: Vec<Manager>,
    /// `Systems` reported by Redfish
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub systems: Vec<ComputerSystem>,
    /// `Chassis` reported by Redfish
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chassis: Vec<Chassis>,
    /// `Service` reported by Redfish
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service: Vec<Service>,
    /// If the endpoint is a BMC that belongs to a Machine and enough data is
    /// available to calculate the `MachineId`, this field contains the `MachineId`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    /// Parsed versions, serializtion override means it will always be sorted
    #[serde(
        default,
        serialize_with = "carbide_utils::ordered_map",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub versions: HashMap<FirmwareComponentType, String>,
    /// Model, parsed out of chassis and service
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "ForgeSetupStatus"
    )]
    pub machine_setup_status: Option<MachineSetupStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure_boot_status: Option<SecureBootStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockdown_status: Option<LockdownStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_shelf_id: Option<PowerShelfId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_id: Option<SwitchId>,
    // Merged from multiple chassis entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_slot_number: Option<i32>,
    // Merged from multiple chassis entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_tray_index: Option<i32>,
    // Merged from multiple chassis entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<i32>,
    // Merged from multiple chassis entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<i32>,
    /// Transient remediation error detected during an otherwise successful exploration.
    /// Not persisted; used to trigger Site Explorer auto-remediation in the same run.
    #[serde(skip, default)]
    pub remediation_error: Option<EndpointExplorationError>,
}

impl EndpointExplorationReport {
    /// model does a best effort to find a model name within the report
    pub fn model(&self) -> Option<String> {
        // Prefer Systems, not Chassis; at least for Lenovo, Chassis has what is more of a SKU instead of the actual model name.
        let system_with_model = self.systems.iter().find(|&x| x.model.is_some());
        Some(match system_with_model {
            Some(system) => match &system.model {
                Some(model) => model.to_owned(),
                None => {
                    return None;
                }
            },
            None if self.is_dpu() => self
                .identify_dpu()
                .map(|d| d.to_string())
                .unwrap_or("unknown model".to_string()),
            None => match self.chassis.iter().find(|&x| x.model.is_some()) {
                Some(chassis) => chassis.model.as_ref().unwrap().to_string(),
                None => {
                    return None;
                }
            },
        })
    }

    pub fn all_mac_addresses(&self) -> Vec<MacAddress> {
        self.systems
            .iter()
            .flat_map(|s| s.ethernet_interfaces.as_slice())
            .filter_map(|e| e.mac_address)
            .chain(
                self.chassis
                    .iter()
                    .flat_map(|chassis| &chassis.network_adapters)
                    .flat_map(|adapter| adapter.port_mac_addresses.iter().copied()),
            )
            .unique()
            .collect()
    }

    /// Finds the Redfish interface id of the host ethernet interface whose MAC
    /// matches `mac`, if any. An interface that reports an empty id is treated
    /// as having none, so callers never capture an empty string as the id (which
    /// would otherwise clobber a previously stored, valid one).
    ///
    /// Used to capture the boot interface's [stable] Redfish interface id
    /// alongside its MAC, giving setup calls a second, [stable] handle to target
    /// in addition to the MAC.
    pub fn find_interface_id_for_mac(&self, mac: MacAddress) -> Option<&str> {
        self.systems
            .iter()
            .flat_map(|s| s.ethernet_interfaces.iter())
            .find(|e| e.mac_address == Some(mac))
            .and_then(|e| e.id.as_deref().none_if_empty())
    }

    /// Yields a [`MachineBootInterface`] for every host ethernet interface that
    /// reports both a MAC and a non-empty Redfish interface id -- for any NIC
    /// type (integrated NICs, SuperNICs, DPUs in NIC mode, DPU host-PFs).
    /// Interfaces missing either half are skipped (via
    /// [`MachineBootInterface::from_parts`]).
    pub fn complete_boot_interfaces(&self) -> impl Iterator<Item = MachineBootInterface> + '_ {
        self.systems
            .iter()
            .flat_map(|s| s.ethernet_interfaces.iter())
            .filter_map(|e| MachineBootInterface::from_parts(e.mac_address, e.id.clone()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExploredEndpoint {
    /// The IP address of the endpoint we explored
    pub address: std::net::IpAddr,
    /// The data we gathered about the endpoint
    pub report: EndpointExplorationReport,
    /// The version of `report`.
    /// Will increase every time the report gets updated.
    pub report_version: ConfigVersion,
    /// State within preingestion state machine
    pub preingestion_state: PreingestionState,
    /// Indicates that preingestion is waiting for site explorer to refresh the state
    pub waiting_for_explorer_refresh: bool,
    /// Whether the endpoint will be explored in the next site-explorer run
    pub exploration_requested: bool,
    /// Last BMC Reset issued through redfish
    pub last_redfish_bmc_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// Last BMC Reset issued through ipmitool
    pub last_ipmitool_bmc_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// Last Reboot issued through redfish
    pub last_redfish_reboot: Option<chrono::DateTime<chrono::Utc>>,
    /// Last Powercycle issued through redfish
    pub last_redfish_powercycle: Option<chrono::DateTime<chrono::Utc>>,
    /// whether this host is allowed to power on
    pub pause_ingestion_and_poweron: bool,
    /// Flag to prevent site explorer from taking remediation actions on redfish errors
    pub pause_remediation: bool,
    /// The MAC address of the boot interface (primary interface) for this host endpoint
    pub boot_interface_mac: Option<MacAddress>,
    /// The vendor-native Redfish interface id of the boot interface, captured
    /// alongside `boot_interface_mac`. [`ExploredEndpoint::boot_interface`]
    /// returns the complete pair, while
    /// [`ExploredEndpoint::boot_interface_target`] preserves a MAC-only target.
    pub boot_interface_id: Option<String>,
}

impl Display for ExploredEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} / {}", self.address, self.report_version)
    }
}

impl ExploredEndpoint {
    /// Returns the fully-populated boot interface (MAC + Redfish interface id)
    /// for this endpoint, or `None` if either part is missing.
    ///
    /// `None` means we have no complete pair yet -- e.g. the endpoint predates
    /// interface-id capture, or has only ever been reported without a resolvable
    /// interface id.
    pub fn boot_interface(&self) -> Option<MachineBootInterface> {
        MachineBootInterface::from_parts(self.boot_interface_mac, self.boot_interface_id.clone())
    }

    /// Returns the boot interface selector Site Explorer should evaluate.
    ///
    /// A complete endpoint record yields [`MachineBootInterfaceTarget::Pair`].
    /// Older records with only `boot_interface_mac` remain usable as
    /// [`MachineBootInterfaceTarget::MacOnly`]. An interface id by itself
    /// cannot identify the target and yields `None`.
    pub fn boot_interface_target(&self) -> Option<MachineBootInterfaceTarget> {
        MachineBootInterfaceTarget::from_parts(
            self.boot_interface_mac,
            self.boot_interface_id.clone(),
        )
    }

    /// find_version will locate a version number within an ExploredEndpoint
    pub fn find_version(
        &self,
        fw_info: &Firmware,
        firmware_type: FirmwareComponentType,
    ) -> Option<&String> {
        for service in self.report.service.iter() {
            if let Some(matching_inventory) = service
                .inventories
                .iter()
                .find(|&x| fw_info.matching_version_id(&x.id, firmware_type))
            {
                tracing::debug!(
                    bmc_ip_address = %self.address,
                    firmware_type = ?firmware_type,
                    version = ?matching_inventory.version,
                    "Found matching firmware version",
                );
                return matching_inventory.version.as_ref();
            };
        }
        None
    }

    pub fn find_all_versions(
        &self,
        fw_info: &Firmware,
        firmware_type: FirmwareComponentType,
    ) -> Vec<&String> {
        let mut versions = Vec::new();

        // find all matching versions
        for service in self.report.service.iter() {
            for inventory in service.inventories.iter() {
                if fw_info.matching_version_id(&inventory.id, firmware_type)
                    && let Some(ref version) = inventory.version
                {
                    versions.push(version);
                };
            }
        }

        tracing::debug!(
            bmc_ip_address = %self.address,
            version_count = versions.len(),
            firmware_type = ?firmware_type,
            versions = ?versions,
            "Found firmware versions",
        );

        versions
    }

    pub fn has_bluefield_part_number(&self) -> bool {
        self.report.chassis.iter().any(|chassis| {
            chassis
                .part_number
                .as_ref()
                .is_some_and(|p| is_bluefield_part_number(p.trim()))
                || chassis.network_adapters.iter().any(|n| {
                    n.part_number
                        .as_ref()
                        .is_some_and(|p| is_bluefield_part_number(p.trim()))
                })
        })
    }
}

impl EndpointExplorationReport {
    /// The boot interface selection for this endpoint's explored default -- the
    /// selection Site Explorer records before any machine owns the endpoint.
    ///
    /// A declared `ExpectedInterface.primary` wins when this report has that NIC
    /// as a full pair -- its MAC present on a system ethernet interface with a
    /// non-empty Redfish interface id -- whatever its type (an integrated NIC as
    /// readily as a DPU host-PF), so the explored default agrees with the managed
    /// store's declared primary across the ownership handoff. A declared NIC
    /// whose id this report has not resolved yet falls back, alongside the
    /// no-declaration case, to the automatic pick: the lowest-PCI DPU host-PF
    /// interface.
    pub fn select_host_primary_interface(
        &self,
        explored_dpus: &[ExploredDpu],
        declared_primary: Option<MacAddress>,
    ) -> Option<HostPrimaryInterfaceSelection> {
        // A declared primary wins as long as the report has it as a full pair
        // (`find_interface_id_for_mac` scans every system ethernet interface,
        // integrated NICs included).
        if let Some(declared) = declared_primary
            && self.find_interface_id_for_mac(declared).is_some()
        {
            return Some(HostPrimaryInterfaceSelection {
                mac_address: declared,
                source: BootInterfaceSelectionSource::ExpectedMachine,
            });
        }

        let system = self.systems.first()?;

        // Gather explored DPUs mac.
        let explored_dpus_macs = explored_dpus
            .iter()
            .filter_map(|x| x.host_pf_mac_address)
            .collect::<Vec<MacAddress>>();

        // Filter PCI device names only for the interfaces which are mapped to DPU.
        // Host might have some integrated or embedded interfaces, which are not used by forge.
        // Need to ignore them.
        let interfaces = system
            .ethernet_interfaces
            .iter()
            .filter(|x| {
                if let Some(mac) = x.mac_address {
                    explored_dpus_macs.contains(&mac)
                } else {
                    false
                }
            })
            .collect::<Vec<&EthernetInterface>>();

        let interfaces = interfaces
            .into_iter()
            .map(|interface| {
                let ordering_key = interface.uefi_device_path.as_ref()?.ordering_key().ok()?;
                Some((interface, ordering_key))
            })
            .collect::<Option<Vec<_>>>()?;
        let (interface_with_min_pci, _) = interfaces
            .into_iter()
            .min_by(|(_, left), (_, right)| left.cmp(right))?;

        // If we know the bootable interface name, find the MAC address associated with it.
        interface_with_min_pci
            .mac_address
            .map(|mac_address| HostPrimaryInterfaceSelection {
                mac_address,
                source: BootInterfaceSelectionSource::RedfishUefiPci,
            })
    }
}

/// A host primary interface decision made from one Site Explorer report.
///
/// Keeping the selected MAC beside the exact input that selected it prevents a
/// later fallback or target resolution step from recording the wrong source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostPrimaryInterfaceSelection {
    /// MAC address selected as the host's primary boot interface.
    pub mac_address: MacAddress,
    /// Exact mechanism that selected `mac_address`.
    pub source: BootInterfaceSelectionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum PreingestionState {
    Initial,
    RecheckVersions,
    ScriptRunning,
    BfbRecoveryNeeded {
        reason: String,
        host_bmc_ip: IpAddr,
        #[serde(default)]
        pre_copy_powercycle: bool,
    },
    BfbPlatformPowercycle {
        host_bmc_ip: IpAddr,
        phase: BfbPlatformPowercyclePhase,
        #[serde(default)]
        post_install: bool,
    },
    BfbCopyInProgress {
        started_at: DateTime<Utc>,
        host_bmc_ip: IpAddr,
    },
    BfbInstallationWait {
        started_at: DateTime<Utc>,
        host_bmc_ip: IpAddr,
    },
    InitialReset {
        phase: InitialResetPhase,
        last_time: DateTime<Utc>,
    },
    /// One-shot BMC reset run immediately after `Initial` for every endpoint,
    /// so a freshly-booted BMC report is what pairing/ingestion reads. Notably
    /// refreshes GB200 host BMCs that intermittently drop a DPU from their
    /// PCIe inventory.
    InitialBMCReset {
        phase: InitialBmcResetPhase,
    },
    /// Configure site NTP servers on the BMC before checking whether its clock
    /// is synchronized. `set_at` records a successful Redfish update so the
    /// state machine can wait for the setting to take effect before checking.
    SetNtpServers {
        set_at: Option<DateTime<Utc>>,
        #[serde(default)]
        attempts: u32,
    },
    TimeSyncReset {
        phase: TimeSyncResetPhase,
        last_time: DateTime<Utc>,
        /// How many full reset cycles have already been attempted for this
        /// endpoint. Used to retry a transient clock failure a bounded number
        /// of times before giving up. Defaults to 0 so states serialized
        /// before this field existed still deserialize.
        #[serde(default)]
        attempt: u32,
    },
    UpgradeFirmwareWait {
        task_id: String,
        final_version: String,
        upgrade_type: FirmwareComponentType,
        power_drains_needed: Option<u32>,
        firmware_number: Option<u32>,
    },
    ResetForNewFirmware {
        final_version: String,
        upgrade_type: FirmwareComponentType,
        power_drains_needed: Option<u32>,
        delay_until: Option<i64>,
        last_power_drain_operation: Option<PowerDrainState>,
    },
    NewFirmwareReportedWait {
        final_version: String,
        upgrade_type: FirmwareComponentType,
        previous_reset_time: Option<i64>,
    },
    RecheckVersionsAfterFailure {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BfbPlatformPowercyclePhase {
    PowerOff,
    PowerOn,
    WaitingForDpuBmc,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InitialResetPhase {
    Start,
    BMCWasReset,
    WaitHostBoot,
}

/// Phases of the one-shot `InitialBMCReset` state. `Start { attempts }` issues
/// the BMC reset; if the BMC is reachable but the reset errors, it retries up
/// to a bound and then proceeds without the reset rather than blocking
/// ingestion. `WaitForBmc` polls until the BMC comes back; an unreachable BMC
/// keeps waiting (it is never a reason to move on). Once it returns, a fresh
/// exploration report is requested and `WaitForExplorerRefresh` waits for it so
/// the relocated checks (and downstream pairing) read the post-reset inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InitialBmcResetPhase {
    Start { attempts: u32 },
    WaitForBmc,
    WaitForExplorerRefresh,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TimeSyncResetPhase {
    Start,
    BMCWasReset,
    WaitHostBoot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PowerDrainState {
    Off,
    Powercycle,
    On,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct PCIeDevice {
    pub description: Option<String>,
    pub firmware_version: Option<String>,
    pub gpu_vendor: Option<String>,
    pub id: Option<String>,
    pub manufacturer: Option<String>,
    pub name: Option<String>,
    pub part_number: Option<String>,
    pub serial_number: Option<String>,
    pub status: Option<SystemStatus>,
}

impl PCIeDevice {
    // is_bluefield returns whether the device is a Bluefield
    pub fn is_bluefield(&self) -> bool {
        let Some(part_number) = &self.part_number else {
            // TODO: maybe model this as an enum that has "Indeterminable" if there's no part number
            // but for now it's 'technically' true
            return false;
        };

        is_bluefield_part_number(part_number)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct SystemStatus {
    pub health: Option<String>,
    pub health_rollup: Option<String>,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExploredDpu {
    /// The DPUs BMC IP
    pub bmc_ip: IpAddr,
    /// The MAC address that is visible to the host (provided by the DPU)
    #[serde(with = "serialize_option_display", default)]
    pub host_pf_mac_address: Option<MacAddress>,
    /// The trimmed, nonblank host BMC `Chassis.id` associated with this DPU's
    /// serial number. Conflicting IDs leave this unset so Site Explorer can
    /// fall back to ordering by DPU serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_chassis_id: Option<String>,

    #[serde(skip)]
    pub report: Arc<EndpointExplorationReport>,
}

impl ExploredDpu {
    pub fn machine_id_if_valid_report(&self) -> ModelResult<DpuMachineId> {
        let Some(machine_id) = self.report.machine_id else {
            return Err(ModelError::MissingArgument("Missing Machine ID"));
        };

        if self.report.systems.is_empty() {
            return Err(ModelError::MissingArgument("Missing Systems Info"));
        }

        if self.report.chassis.is_empty() {
            return Err(ModelError::MissingArgument("Missing Chassis Info"));
        }

        if self.report.service.is_empty() {
            return Err(ModelError::MissingArgument("Missing Service Info"));
        }

        Ok(machine_id.try_into()?)
    }

    pub fn bmc_firmware_version(&self) -> Option<String> {
        self.report
            .dpu_component_version(FirmwareComponentType::Bmc)
    }

    pub fn bmc_info(&self) -> BmcInfo {
        BmcInfo {
            ip: Some(self.bmc_ip),
            mac: self
                .report
                .managers
                .first()
                .and_then(|m| m.ethernet_interfaces.first().and_then(|e| e.mac_address)),
            firmware_version: self.bmc_firmware_version(),
            ..Default::default()
        }
    }

    pub fn hardware_info(&self) -> ModelResult<HardwareInfo> {
        let serial_number = self
            .report
            .dpu_pairing_serial_number()
            .ok_or(ModelError::MissingArgument("Missing DPU serial number"))?;
        let vendor = self
            .report
            .systems
            .first()
            .and_then(|system| system.manufacturer.as_ref());
        let model = self
            .report
            .systems
            .first()
            .and_then(|system| system.model.as_ref());
        let dmi_data = self
            .report
            .create_temporary_dmi_data(serial_number, vendor, model);

        let inventory_map = self.report.get_inventory_map();

        let dpu_data = DpuData {
            factory_mac_address: self
                .host_pf_mac_address
                .ok_or(ModelError::MissingArgument("Missing base mac"))?
                .to_string(),
            part_number: self
                .report
                .chassis
                .iter()
                .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                .find_map(chassis_part_number)
                .unwrap_or("")
                .to_string(),
            part_description: self
                .report
                .chassis
                .iter()
                .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                .find_map(chassis_model)
                .unwrap_or("")
                .to_string(),
            firmware_version: inventory_map
                .get("DPU_NIC")
                .and_then(|value| value.version.as_ref())
                .unwrap_or(&"".to_string())
                .to_string(),
            firmware_date: inventory_map
                .get("DPU_NIC")
                .and_then(|value| value.release_date.as_ref())
                .unwrap_or(&"".to_string())
                .to_string(),
            ..Default::default()
        };

        Ok(HardwareInfo {
            dmi_data: Some(dmi_data),
            dpu_info: Some(dpu_data),
            machine_type: CpuArchitecture::Aarch64,
            ..Default::default()
        })
    }
}

/// A combination of DPU and host that was discovered via Site Exploration
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExploredManagedHost {
    /// The Hosts BMC IP
    pub host_bmc_ip: IpAddr,
    /// Attached DPUs
    pub dpus: Vec<ExploredDpu>,
}

impl ExploredManagedHost {
    pub fn bmc_info(&self) -> BmcInfo {
        BmcInfo {
            ip: Some(self.host_bmc_ip),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExploredManagedSwitch {
    /// The Switch's BMC IP
    pub bmc_ip: IpAddr,
    // Host mac address
    pub nv_os_mac_addresses: Vec<MacAddress>,
    /// Exploration report for this switch endpoint
    pub report: EndpointExplorationReport,
}

impl ExploredManagedSwitch {
    pub fn bmc_info(&self) -> BmcInfo {
        BmcInfo {
            ip: Some(self.bmc_ip),
            ..Default::default()
        }
    }
}

/// Serialization methods for types which support FromStr/Display
mod serialize_option_display {
    use std::fmt::Display;
    use std::str::FromStr;

    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<T, S>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: Display,
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_str(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        let value: Option<String> = Option::deserialize(deserializer)?;
        match value {
            None => Ok(None),
            Some(value) => Ok(Some(T::from_str(&value).map_err(de::Error::custom)?)),
        }
    }
}

/// That that we gathered from exploring a site
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SiteExplorationReport {
    /// Metadata about the latest site explorer run, if site explorer has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<SiteExplorerLastRun>,
    /// The endpoints that had been explored
    pub endpoints: Vec<ExploredEndpoint>,
    /// The managed-hosts which have been explored
    pub managed_hosts: Vec<ExploredManagedHost>,
}

/// Operator-facing status for the latest site explorer run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SiteExplorerLastRun {
    /// When the run started.
    pub started_at: DateTime<Utc>,
    /// When the run finished.
    pub finished_at: DateTime<Utc>,
    /// Whether the run completed successfully.
    pub success: bool,
    /// Error string for a failed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Failure category for a failed run, suitable for metrics and alert routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<String>,
    /// Number of endpoint exploration attempts made during the run.
    pub endpoint_explorations: i64,
    /// Number of successful endpoint explorations during the run.
    pub endpoint_explorations_success: i64,
    /// Number of endpoint exploration errors during the run.
    pub endpoint_explorations_failed: i64,
    /// When the most recent successful run finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_finished_at: Option<DateTime<Utc>>,
    /// When the most recent failed run finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failed_finished_at: Option<DateTime<Utc>>,
}

impl EndpointExplorationReport {
    /// Returns a report for an endpoint that is not reachable and could therefore
    /// not be explored
    pub fn new_with_error(e: EndpointExplorationError) -> Self {
        Self {
            endpoint_type: EndpointType::Unknown,
            last_exploration_error: Some(e),
            last_exploration_latency: None,
            managers: Vec::new(),
            systems: Vec::new(),
            chassis: Vec::new(),
            service: Vec::new(),
            vendor: None,
            machine_id: None,
            versions: HashMap::default(),
            model: None,
            machine_setup_status: None,
            secure_boot_status: None,
            lockdown_status: None,
            power_shelf_id: None,
            switch_id: None,
            physical_slot_number: None,
            compute_tray_index: None,
            topology_id: None,
            revision_id: None,
            remediation_error: None,
        }
    }

    pub fn bluefield_operating_mode(&self) -> Option<BlueFieldOperatingMode> {
        if self.is_dpu() && !self.systems.is_empty() {
            self.systems[0].attributes.nic_mode
        } else {
            None
        }
    }

    pub fn dpu_part_number(&self) -> Option<&str> {
        if !self.is_dpu() {
            return None;
        }

        self.chassis
            .iter()
            .find(|chassis| chassis.id == "Card1")
            .and_then(chassis_part_number)
            .or_else(|| {
                // BF4 DPU BMC firmware often leaves Card1 empty and publishes the
                // product part on the integrated BMC chassis instead (POR id
                // `Bluefield_BMC` on some trays, `BlueField_BMC_0` on others).
                self.chassis
                    .iter()
                    .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                    .find_map(chassis_part_number)
            })
    }

    /// Return `true` if the explored endpoint is a DPU
    pub fn is_dpu(&self) -> bool {
        self.identify_dpu().is_some()
    }

    /// Return `true` if the explored endpoint is a PowerShelf.
    /// This checks if the chassis ID is /Chassis/powershelf, or,
    /// if that fails, checks to see if /Chassis/chassis has
    /// a manufacturer containing "lite-on" or "delta".
    ///
    /// TODO(chet): These are obviously workarounds for now while
    /// we work with vendors to update their BMC firmware.
    pub fn is_power_shelf(&self) -> bool {
        self.chassis.iter().any(|c| {
            c.id.to_lowercase().contains("powershelf")
                || (c.id == "chassis"
                    && c.manufacturer.as_ref().is_some_and(|m| {
                        let m = m.to_lowercase();
                        m.contains("lite-on") || m.contains("delta")
                    }))
        })
    }

    /// Return `true` if the explored endpoint is a Switch
    pub fn is_switch(&self) -> bool {
        self.chassis
            .iter()
            .any(|c| c.id.to_lowercase().contains("mgx_nvswitch_0"))
    }

    /// Return `DpuModel` if the explored endpoint is a DPU
    pub fn identify_dpu(&self) -> Option<DpuModel> {
        if !self
            .systems
            .first()
            .map(is_bluefield_system)
            .unwrap_or(false)
        {
            return None;
        }

        let chassis_map = self
            .chassis
            .iter()
            .map(|x| (x.id.as_str(), x))
            .collect::<HashMap<_, _>>();
        let model = chassis_map
            .get("Card1")
            .and_then(|value| chassis_model(value))
            .or_else(|| {
                self.chassis
                    .iter()
                    .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                    .find_map(chassis_model)
            })
            .unwrap_or("");
        Some(DpuModel::from(model))
    }

    pub fn create_temporary_dmi_data(
        &self,
        serial_number: &str,
        vendor: Option<&String>,
        model: Option<&String>,
    ) -> DmiData {
        let sys_vendor = if let Some(x) = vendor {
            x.to_string()
        } else {
            carbide_utils::DEFAULT_DMI_SYSTEM_MANUFACTURER.to_string()
        };
        let product_name = if let Some(x) = model {
            x.to_string()
        } else {
            carbide_utils::DEFAULT_DMI_SYSTEM_MODEL.to_string()
        };
        // For DPUs the discovered data contains enough information to
        // calculate a MachineId
        // The "Unspecified" strings are delivered as serial numbers when doing
        // inband discovery via libudev. For compatibility we have to use
        // the same values here.
        DmiData {
            product_serial: serial_number.trim().to_string(),
            chassis_serial: carbide_utils::DEFAULT_DPU_DMI_CHASSIS_SERIAL_NUMBER.to_string(),
            board_serial: carbide_utils::DEFAULT_DPU_DMI_BOARD_SERIAL_NUMBER.to_string(),
            bios_version: "".to_string(),
            sys_vendor,
            board_name: "BlueField SoC".to_string(),
            bios_date: "".to_string(),
            board_version: "".to_string(),
            product_name,
        }
    }

    fn machine_id_serial_number(&self) -> Option<&str> {
        self.systems
            .first()
            .and_then(|system| system.serial_number.as_deref().map(str::trim))
            .none_if_empty()
            .or_else(|| {
                self.is_dpu().then(|| {
                    // BF4 reports no system serial in Redfish. The stable product serial is
                    // on the product BMC chassis; use its known legacy/new IDs instead of
                    // depending on chassis collection order or unrelated component serials.
                    self.chassis
                        .iter()
                        .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                        .find_map(|chassis| {
                            chassis
                                .serial_number
                                .as_deref()
                                .map(str::trim)
                                .none_if_empty()
                        })
                })?
            })
    }

    /// Tries to generate and store a MachineId for the discovered endpoint if
    /// enough data for generation is available
    pub fn generate_machine_id(
        &mut self,
        force_predicted_host: bool,
    ) -> ModelResult<Option<&MachineId>> {
        if let Some(serial_number) = self.machine_id_serial_number() {
            let vendor = self
                .systems
                .first()
                .and_then(|system| system.manufacturer.as_ref());
            let model = self
                .systems
                .first()
                .and_then(|system| system.model.as_ref());

            let dmi_data = self.create_temporary_dmi_data(serial_number, vendor, model);

            // Construct a HardwareInfo object specifically so that we can mint a MachineId.
            let hardware_info = HardwareInfo {
                dmi_data: Some(dmi_data),
                // This field should not be read, machine_id::from_hardware_info_with_type should not
                // need this, only the dmi_data.
                machine_type: CpuArchitecture::Unknown,
                ..Default::default()
            };

            let machine_type = if self.is_dpu() {
                MachineType::Dpu
            } else if force_predicted_host {
                MachineType::PredictedHost
            } else {
                return Ok(None);
            };

            let machine_id = from_hardware_info_with_type(&hardware_info, machine_type)
                .map_err(|e| ModelError::HardwareInfo(HardwareInfoError::MissingHardwareInfo(e)))?;

            Ok(Some(self.machine_id.insert(machine_id)))
        } else {
            Err(ModelError::HardwareInfo(
                HardwareInfoError::MissingHardwareInfo(MissingHardwareInfo::Serial),
            ))
        }
    }

    /// Tries to generate and store a MachineId for the discovered endpoint if
    /// enough data for generation is available
    pub fn generate_power_shelf_id(&mut self) -> ModelResult<Option<&PowerShelfId>> {
        let chassis = self.chassis.first().unwrap();
        let serial_number = chassis.serial_number.clone().unwrap_or("".to_string());
        let manufacturer = chassis.manufacturer.clone().unwrap_or("".to_string());
        let model = chassis.model.clone().unwrap_or("".to_string());

        let power_shelf_type = PowerShelfType::Rack; //TODO Check later if we need to support other types
        let power_shelf_source = PowerShelfIdSource::ProductBoardChassisSerial;

        let power_shelf_id = power_shelf_id::from_hardware_info_with_type(
            serial_number.as_str(),
            manufacturer.as_str(),
            model.as_str(),
            power_shelf_source,
            power_shelf_type,
        )
        .map_err(|_e| {
            ModelError::HardwareInfo(HardwareInfoError::MissingHardwareInfo(
                MissingHardwareInfo::Serial,
            ))
        })?;

        Ok(Some(self.power_shelf_id.insert(power_shelf_id)))
    }

    //TODO: refactor for common code with generate_power_shelf_id
    /// Tries to generate and store a MachineId for the discovered endpoint if
    /// enough data for generation is available
    pub fn generate_switch_id(&mut self) -> ModelResult<Option<SwitchId>> {
        let chassis = self
            .chassis
            .iter()
            .find(|c| c.id.to_string().to_lowercase() == "mgx_nvswitch_0")
            .unwrap();
        let serial_number = chassis.serial_number.clone();
        let manufacturer = chassis.manufacturer.clone().unwrap_or("NVIDIA".to_string());
        let model = "Switch".to_string();

        if let Some(serial_number) = serial_number.as_ref() {
            let switch_type = SwitchType::NvLink;
            let switch_source = SwitchIdSource::ProductBoardChassisSerial;

            let switch_id = switch_id::from_hardware_info_with_type(
                serial_number.as_str(),
                manufacturer.as_str(),
                model.as_str(),
                switch_source,
                switch_type,
            )
            .map_err(|_e| {
                ModelError::HardwareInfo(HardwareInfoError::MissingHardwareInfo(
                    MissingHardwareInfo::Serial,
                ))
            })?;
            self.switch_id = Some(switch_id);
            Ok(self.switch_id)
        } else {
            Err(ModelError::HardwareInfo(
                HardwareInfoError::MissingHardwareInfo(MissingHardwareInfo::Serial),
            ))
        }
    }

    pub fn get_inventory_map(&self) -> HashMap<&str, &Inventory> {
        self.service
            .iter()
            .find(|s| s.id == *"FirmwareInventory")
            .map(|s| {
                s.inventories
                    .iter()
                    .map(|i| (i.id.as_str(), i))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default()
    }

    /// BMC firmware observed directly from the exact `BMC` inventory entry.
    pub fn observed_host_bmc_version(&self) -> Option<&str> {
        self.service
            .iter()
            .find(|service| service.id == "FirmwareInventory")
            .and_then(|service| {
                service
                    .inventories
                    .iter()
                    .find(|inventory| inventory.id == "BMC")
            })
            .and_then(|inventory| inventory.version.as_deref())
            .map(str::trim)
            .filter(|version| !version.is_empty())
    }

    /// Host BIOS/UEFI version observed on the `System_0` resource.
    pub fn system_bios_version(&self) -> Option<&str> {
        self.systems
            .iter()
            .find(|system| system.id == "System_0")
            .and_then(|system| system.bios_version.as_deref())
            .map(str::trim)
            .filter(|version| !version.is_empty())
    }

    pub fn dpu_component_version(&self, component: FirmwareComponentType) -> Option<String> {
        match component {
            FirmwareComponentType::Bmc => self.dpu_bmc_version(),
            FirmwareComponentType::Uefi => self.dpu_uefi_version(),
            _ => None,
        }
    }

    pub fn dpu_bmc_version(&self) -> Option<String> {
        Some(
            self.get_inventory_map()
                .iter()
                // BF3 exposes BMC firmware as inventory id "BMC_Firmware"; BF4
                // uses exactly "BlueField_FW_BMC_0". Matching the full BF4 id
                // (via `ends_with`) excludes unrelated components — including
                // "FW_BMC_0_x" / "FW_BMC_01" and any other id merely ending in
                // "FW_BMC_0". Both ids are unique per report, so `find` selects
                // the single BMC firmware entry unambiguously.
                .find(|s| s.0.contains("BMC_Firmware") || s.0.ends_with("BlueField_FW_BMC_0"))
                .and_then(|value| value.1.version.as_ref())
                .unwrap_or(&"0".to_string())
                .to_lowercase()
                .replace("bf-", "")
                .replace("bf4-", ""),
        )
    }

    pub fn dpu_uefi_version(&self) -> Option<String> {
        self.get_inventory_map()
            .get("DPU_UEFI")
            .and_then(|value| value.version.clone())
    }

    pub fn parse_versions(&mut self, fw_info: &Firmware) -> Vec<FirmwareComponentType> {
        let mut not_found = Vec::new();
        for fwtype in fw_info.components.keys() {
            if let Some(current) = fw_info.find_version(self, *fwtype) {
                self.versions.insert(*fwtype, current);
            } else {
                not_found.push(*fwtype)
            }
        }
        not_found
    }

    /// Extract position info from chassis entries into the report-level fields.
    ///
    /// Uses "first wins" strategy: takes the first non-None value found across
    /// all chassis entries. This is consistent with how `model()` extracts data
    /// from the chassis array.
    pub fn parse_position_info(&mut self) {
        for chassis in &self.chassis {
            self.physical_slot_number = self.physical_slot_number.or(chassis.physical_slot_number);
            self.compute_tray_index = self.compute_tray_index.or(chassis.compute_tray_index);
            self.topology_id = self.topology_id.or(chassis.topology_id);
            self.revision_id = self.revision_id.or(chassis.revision_id);
        }
    }
}

/// Describes errors that might have been encountered during exploring an endpoint
#[derive(thiserror::Error, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "Type", rename_all = "PascalCase")]
pub enum EndpointExplorationError {
    /// site-explorer timed out sending a request (or getting a response) from
    /// this endpoint, either due to connectivity issues to the destination IP,
    /// or the destination port [being up but] not responding in a timely
    /// matter. This is ultimately tripped by a reqwest is_timeout error in
    /// the current implementation. For cases where the destination IP *is*
    /// reachable, but the  port is not listening, see ConnectionRefused.
    #[error("site-explorer timed out communicating with the endpoint: {details:?}")]
    #[serde(rename_all = "PascalCase")]
    ConnectionTimeout { details: String },
    /// The connection to the configured endpoint was refused. This indicates
    /// that site-explorer probably has connectivity to the target IP (unless
    /// a network device in the path is sending an RST), and is able to positively
    /// confirm the endpoint is not listening on the target port (which probably
    /// means no Redfish API is being exposed), OR, can ALSO mean there was a TLS
    /// handshake failure (since reqwest is_connect errors capture TLS handshake
    /// errors as well). A more common example here is if site-explorer is
    /// [unknowingly] exploring a yet-unpaired DPU, and the IP it is attempting
    /// to explore happens to be the DPU admin IP. Since the admin/host side of
    /// a DPU doesn't expose a Redfish API, you will see ConnectionRefused. This
    /// is ultimately tripped by a reqwest is_connect error in the current
    /// implementation.
    #[error("the connection to the endpoint was refused: {details:?}")]
    #[serde(rename_all = "PascalCase")]
    ConnectionRefused { details: String },
    /// Some other generic error happened while attempting to connect
    /// and make a request (or receive a response) from the endpoint
    /// which was not otherwise handled by connection timeout or
    /// connection refused handlers.
    #[error("the endpoint was not reachable due to a generic network issue: {details:?}")]
    #[serde(rename_all = "PascalCase")]
    Unreachable { details: Option<String> },
    /// A Redfish variant we don't support, typically a new vendor
    #[error("redfish vendor '{vendor}' not supported")]
    UnsupportedVendor { vendor: String },
    /// A generic redfish error. No additional details are available
    #[error(
        "error while performing redfish request: {details}: {response_body:?} (response code: {response_code:?})"
    )]
    #[serde(rename_all = "PascalCase")]
    RedfishError {
        details: String,
        response_body: Option<String>,
        response_code: Option<u16>,
    },
    /// The endpoint returned a 401 Unauthorized or 403 Forbidden Status
    #[error("unauthorized: {details}")]
    #[serde(rename_all = "PascalCase")]
    Unauthorized {
        details: String,
        response_body: Option<String>,
        response_code: Option<u16>,
    },
    #[error("missing credential {key}")]
    MissingCredentials {
        #[serde(default)]
        key: String,
        cause: String,
    },
    #[error("secrets engine error occurred: {cause}")]
    SecretsEngineError {
        #[serde(default)]
        cause: String,
    },
    #[error("failed setting credential {key}: {cause}")]
    SetCredentials { key: String, cause: String },
    /// Deprecated. Replaced by `RedfishError`.
    /// This field just exists here until site-explorer updates existing records
    #[error("endpoint is not a BMC with redfish support at the specified URI")]
    MissingRedfish { uri: Option<String> },
    /// The BMC's Redfish ServiceRoot (`/redfish/v1`) did not yield a vendor we
    /// recognize. `observed` is the raw vendor string we read from the root —
    /// the `Vendor` field, falling back to the first `Oem` key. `None` means the
    /// BMC reported neither, which is commonly transient while the BMC is still
    /// initializing/syncing (exploration will retry). `Some(value)` means the BMC
    /// reported a vendor we don't support yet — `value` is what it sent.
    #[error(
        "BMC ServiceRoot (/redfish/v1) did not report a recognized vendor (observed vendor/oem = {observed:?}); an empty value usually means the BMC is still initializing and exploration will retry"
    )]
    MissingVendor {
        #[serde(default)]
        observed: Option<String>,
    },
    #[error(
        "site explorer will not explore this endpoint to avoid lockout: it could not login previously"
    )]
    AvoidLockout,
    /// An error which is not further detailed
    #[error("error: {details}")]
    #[serde(rename_all = "PascalCase")]
    Other { details: String },

    /// A known, intermittent HTTP 403 from the firmware-inventory endpoint on
    /// DGX H100 BMCs ("Viking" is the internal code name). The variant name is
    /// kept for backward-compatible serialization of stored reports; new
    /// operator-facing text uses the real product name.
    #[error("DGX H100 firmware inventory request was forbidden: {details}")]
    #[serde(rename_all = "PascalCase")]
    VikingFWInventoryForbiddenError {
        details: String,
        response_body: Option<String>,
        response_code: Option<u16>,
    },

    #[error("invalid redfish response for DPU BIOS: {details}")]
    #[serde(rename_all = "PascalCase")]
    InvalidDpuRedfishBiosResponse {
        details: String,
        response_body: Option<String>,
        response_code: Option<u16>,
    },

    /// An intermittent unauthorized error that occurred even when site-wide
    /// credentials are already set. This is a transient error that should be
    /// retried rather than triggering AvoidLockout behavior.
    /// After `consecutive_count` reaches the threshold, escalates to regular Unauthorized.
    #[error("intermittent unauthorized error (attempt {consecutive_count}): {details}")]
    #[serde(rename_all = "PascalCase")]
    IntermittentUnauthorized {
        details: String,
        response_body: Option<String>,
        response_code: Option<u16>,
        #[serde(default)]
        consecutive_count: u32,
    },
}

impl EndpointExplorationError {
    pub const INVALID_DPU_REDFISH_BIOS_RESPONSE_CODE: ErrorCode =
        ErrorCode::nico(ErrorSubsystem::Dpu, 134);
    pub const INVALID_DPU_REDFISH_BIOS_RESPONSE_MITIGATION: &'static str = "No action needed: site explorer automatically force-restarts the DPU to clear this \
         known UEFI/BMC race and re-explores on its next run (~2 min). It escalates to a BMC \
         reset if the empty BIOS attributes persist.";

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, EndpointExplorationError::Unauthorized { .. })
            || matches!(self, EndpointExplorationError::AvoidLockout)
    }

    pub fn is_unreachable(&self) -> bool {
        matches!(
            self,
            EndpointExplorationError::ConnectionTimeout { .. }
                | EndpointExplorationError::ConnectionRefused { .. }
                | EndpointExplorationError::Unreachable { .. }
        )
    }

    pub fn is_redfish(&self) -> bool {
        matches!(self, EndpointExplorationError::RedfishError { .. })
            || matches!(
                self,
                EndpointExplorationError::InvalidDpuRedfishBiosResponse { .. }
            )
    }

    pub fn is_dpu_redfish_bios_response_invalid(&self) -> bool {
        matches!(
            self,
            EndpointExplorationError::InvalidDpuRedfishBiosResponse { .. }
        )
    }

    /// Returns the consecutive count if this is an IntermittentUnauthorized error.
    pub fn intermittent_unauthorized_count(&self) -> Option<u32> {
        match self {
            EndpointExplorationError::IntermittentUnauthorized {
                consecutive_count, ..
            } => Some(*consecutive_count),
            _ => None,
        }
    }
}

impl OperatorError for EndpointExplorationError {
    fn operator_error_code(&self) -> ErrorCode {
        // Most variants use Site Explorer codes; the DPU BIOS variant below
        // keeps its existing DPU-specific code.
        use ErrorSubsystem::SiteExplorer;
        match self {
            EndpointExplorationError::ConnectionTimeout { .. } => {
                ErrorCode::nico(SiteExplorer, 100)
            }
            EndpointExplorationError::ConnectionRefused { .. } => {
                ErrorCode::nico(SiteExplorer, 101)
            }
            EndpointExplorationError::Unreachable { .. } => ErrorCode::nico(SiteExplorer, 102),
            EndpointExplorationError::UnsupportedVendor { .. } => {
                ErrorCode::nico(SiteExplorer, 120)
            }
            EndpointExplorationError::MissingRedfish { .. } => ErrorCode::nico(SiteExplorer, 121),
            EndpointExplorationError::MissingVendor { .. } => ErrorCode::nico(SiteExplorer, 122),
            EndpointExplorationError::RedfishError { .. } => ErrorCode::nico(SiteExplorer, 130),
            EndpointExplorationError::VikingFWInventoryForbiddenError { .. } => {
                ErrorCode::nico(SiteExplorer, 131)
            }
            EndpointExplorationError::Unauthorized { .. } => ErrorCode::nico(SiteExplorer, 140),
            EndpointExplorationError::MissingCredentials { .. } => {
                ErrorCode::nico(SiteExplorer, 141)
            }
            EndpointExplorationError::SecretsEngineError { .. } => {
                ErrorCode::nico(SiteExplorer, 142)
            }
            EndpointExplorationError::SetCredentials { .. } => ErrorCode::nico(SiteExplorer, 143),
            EndpointExplorationError::AvoidLockout => ErrorCode::nico(SiteExplorer, 144),
            EndpointExplorationError::IntermittentUnauthorized { .. } => {
                ErrorCode::nico(SiteExplorer, 145)
            }
            EndpointExplorationError::Other { .. } => ErrorCode::nico(SiteExplorer, 199),
            EndpointExplorationError::InvalidDpuRedfishBiosResponse { .. } => {
                Self::INVALID_DPU_REDFISH_BIOS_RESPONSE_CODE
            }
        }
    }

    fn operator_mitigation(&self) -> Option<&'static str> {
        match self {
            EndpointExplorationError::ConnectionTimeout { .. }
            | EndpointExplorationError::ConnectionRefused { .. }
            | EndpointExplorationError::Unreachable { .. } => Some(
                "Verify endpoint network reachability and that the BMC Redfish service is listening.",
            ),
            EndpointExplorationError::UnsupportedVendor { .. }
            | EndpointExplorationError::MissingVendor { .. } => Some(
                "Confirm the endpoint's BMC vendor and model are listed in the NICo Hardware \
                 Compatibility List \
                 (https://docs.nvidia.com/infra-controller/documentation/reference/hardware-compatibility-list); \
                 an unsupported or unidentified BMC cannot be explored.",
            ),
            EndpointExplorationError::Unauthorized { .. }
            | EndpointExplorationError::MissingCredentials { .. }
            | EndpointExplorationError::SecretsEngineError { .. }
            | EndpointExplorationError::SetCredentials { .. }
            | EndpointExplorationError::AvoidLockout => Some(
                "Set or correct this endpoint's BMC credentials with \
                 `PUT /v2/org/{org}/nico/credential/bmc` or \
                 `nicocli bmc-credential create`, then re-explore it with \
                 `nico-admin-cli site-explorer refresh <bmc-ip>`.",
            ),
            EndpointExplorationError::IntermittentUnauthorized { .. } => Some(
                "Transient: site explorer retries automatically on its next run (~2 min), or \
                 force one now with `nico-admin-cli site-explorer refresh <bmc-ip>`. If \
                 unauthorized responses persist across runs, correct the BMC credentials with \
                 `PUT /v2/org/{org}/nico/credential/bmc` or \
                 `nicocli bmc-credential create`.",
            ),
            EndpointExplorationError::InvalidDpuRedfishBiosResponse { .. } => {
                Some(Self::INVALID_DPU_REDFISH_BIOS_RESPONSE_MITIGATION)
            }
            EndpointExplorationError::VikingFWInventoryForbiddenError { .. } => Some(
                "No immediate action needed: site explorer treats this DGX H100 \
                 firmware-inventory response as transient and retries on its next run (~2 min). \
                 Force one now with `nico-admin-cli site-explorer refresh <bmc-ip>` if needed. \
                 For general DGX H100/H200 Redfish API information, see \
                 https://docs.nvidia.com/dgx/dgxh100-user-guide/redfish-api-supp.html.",
            ),
            _ => None,
        }
    }
}

/// The type of the endpoint
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum EndpointType {
    Bmc,
    #[default]
    Unknown,
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ComputerSystemAttributes {
    pub nic_mode: Option<BlueFieldOperatingMode>,
    pub is_infinite_boot_enabled: Option<bool>,
}

/// `ComputerSystem` definition. Matches redfish definition
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ComputerSystem {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ethernet_interfaces: Vec<EthernetInterface>,
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    #[serde(default)]
    pub attributes: ComputerSystemAttributes,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pcie_devices: Vec<PCIeDevice>,
    #[serde(default, deserialize_with = "base_mac_deserialize")]
    pub base_mac: Option<BaseMac>,
    #[serde(default)]
    pub power_state: PowerState,
    pub sku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_order: Option<BootOrder>,
    /// Version reported by the Redfish `ComputerSystem.BiosVersion` property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bios_version: Option<String>,
    /// SSH port for the system's Redfish serial-console service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_console_ssh_port: Option<u16>,
}

pub fn base_mac_deserialize<'a, D>(deserializer: D) -> Result<Option<BaseMac>, D::Error>
where
    D: Deserializer<'a>,
{
    let optional_value: Option<String> = Option::deserialize(deserializer)?;
    Ok(optional_value.and_then(|v| v.parse().ok()))
}

impl ComputerSystem {
    pub fn check_serial_number(&self, expected_serial_number: &String) -> bool {
        match self.serial_number {
            Some(ref serial_number) => serial_number == expected_serial_number,
            None => false,
        }
    }

    pub fn check_sku(&self, expected_sku: &String) -> bool {
        match self.sku {
            Some(ref sku) => sku == expected_sku,
            None => false,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    Off,
    #[default]
    On,
    PoweringOff,
    PoweringOn,
    Paused,
    Hibernating,
    Sleeping,
    Unknown,
}

/// `Manager` definition. Matches redfish definition
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Manager {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ethernet_interfaces: Vec<EthernetInterface>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipmi_port: Option<u16>,
}

/// `EthernetInterface` definition. Matches redfish definition
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct EthernetInterface {
    pub description: Option<String>,
    pub id: Option<String>,
    pub interface_enabled: Option<bool>,
    // We want to store as MACAddress in topology data (tbh I don't actually
    // know why, maybe it's fine if we store it as MacAddress), but there are
    // cases where the input data is MacAddress, so we'll allow MacAddress
    // as or MACAddress as inputs, but always serialize out to MACAddress.
    #[serde(
        rename = "MACAddress",
        alias = "MacAddress",
        deserialize_with = "carbide_network::deserialize_optional_mlx_mac"
    )]
    pub mac_address: Option<MacAddress>,

    /// Redfish `LinkStatus` as reported by the BMC (e.g. LinkUp, LinkDown, NoLink).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_status: Option<String>,

    pub uefi_device_path: Option<UefiDevicePath>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
pub struct UefiDevicePath(String);

impl UefiDevicePath {
    fn ordering_key(&self) -> Result<UefiPciOrderingKey, UefiPciOrderingKeyParseError> {
        UefiPciOrderingKey::from_normalized_uefi_path(&self.0)
    }
}

impl FromStr for UefiDevicePath {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        normalize_uefi_device_path(s)
            .map(UefiDevicePath)
            .map_err(|error| format!("could not parse PCI device path {s}: {error}"))
    }
}

/// `Chassis` definition. Matches redfish definition
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Chassis {
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub part_number: Option<String>,
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_adapters: Vec<NetworkAdapter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_slot_number: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute_tray_index: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<i32>,
}

/// `NetworkAdapter` definition. Matches redfish definition
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkAdapter {
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "PartNumber")]
    pub part_number: Option<String>,
    #[serde(rename = "SerialNumber")]
    pub serial_number: Option<String>,
    /// MAC addresses reported by the `Port` resources contained by this
    /// adapter.
    ///
    /// These remain attached to the adapter that reported them so callers can
    /// apply their own interface-selection policy without fabricating
    /// `ComputerSystem.EthernetInterfaces` inventory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port_mac_addresses: Vec<MacAddress>,
}

/// `SecureBootStatus` definition.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SecureBootStatus {
    pub is_enabled: bool,
}

/// `LockdownStatus` definition. Matches redfish definition
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct LockdownStatus {
    pub status: InternalLockdownStatus,
    pub message: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum InternalLockdownStatus {
    Enabled,
    Partial,
    #[default]
    Disabled,
}

/// `Service` definition. Matches redfish definition
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Service {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inventories: Vec<Inventory>,
}

/// `Inventory` definition. Matches redfish definition
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Inventory {
    pub id: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub release_date: Option<String>,
}

/// The result of one Redfish machine-setup check.
///
/// `is_done` and `diffs` mirror the vendor result. The evaluated boot interface
/// records the logical target NICo asked the backend to assess, so a later
/// controller never has to infer it from endpoint columns that may have changed
/// after the report was written.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MachineSetupStatus {
    pub is_done: bool,
    pub diffs: Vec<MachineSetupDiff>,
    /// The logical boot-interface target NICo asked the backend to assess.
    ///
    /// This does not claim that the backend used every identifier in the
    /// target: a backend may match with a subset (NvRedfish currently uses the
    /// MAC) while retaining the requested `Pair` identity. This lives inside
    /// `MachineSetupStatus` so the report's versioned JSON stores the
    /// observation and its target together. Reports written before target
    /// capture leave it as `None`, which tells a later controller not to treat
    /// the status as evidence for a newly selected interface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluated_boot_interface: Option<MachineBootInterfaceTarget>,
}

/// `BootOrder` definition.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BootOrder {
    pub boot_order: Vec<BootOption>,
}

/// `MachineSetupDiff` definition. Matches redfish definition
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MachineSetupDiff {
    pub key: String,
    pub expected: String,
    pub actual: String,
}

/// `BootOption` definition.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BootOption {
    pub display_name: String,
    pub id: String,
    pub boot_option_enabled: Option<bool>,
    pub uefi_device_path: Option<String>,
}

/// Whether a found/explored machine is in the set of expected machines,
/// currently defined by the expected_machines table in the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MachineExpectation {
    #[default]
    NotApplicable,
    Unexpected,
    Expected,
}

impl Display for MachineExpectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplicable => write!(f, "na"),
            Self::Unexpected => write!(f, "unexpected"),
            Self::Expected => write!(f, "expected"),
        }
    }
}

impl From<bool> for MachineExpectation {
    fn from(b: bool) -> Self {
        match b {
            true => MachineExpectation::Expected,
            false => MachineExpectation::Unexpected,
        }
    }
}

impl From<Option<bool>> for MachineExpectation {
    fn from(b: Option<bool>) -> Self {
        match b {
            None => MachineExpectation::NotApplicable,
            Some(true) => MachineExpectation::Expected,
            _ => MachineExpectation::Unexpected,
        }
    }
}

/// The operating mode reported by a BlueField device.
///
/// This is observed hardware state, not the policy NICo applies to the host;
/// see [`crate::expected_machine::HostDpuPolicy`] for the desired behavior.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BlueFieldOperatingMode {
    #[serde(rename = "DpuMode", alias = "Dpu")]
    Dpu,
    #[serde(rename = "NicMode", alias = "Nic")]
    Nic,
}

impl Display for BlueFieldOperatingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

// returns true if the part number is for a Bluefield-3 DPU
pub fn is_bf3_dpu_part_number(part_number: &str) -> bool {
    let normalized_part_number = part_number.trim().to_lowercase();
    // prefix matching for BlueField-3 DPUs (https://docs.nvidia.com/networking/display/bf3dpu)
    normalized_part_number.starts_with("900-9d3b6")
    // looks like Lenovo ThinkSystem SR675 V3s will report the part number of NVIDIA BlueField-3 VPI QSFP112 2P 200G PCIe Gen5 x16 as SN37B36732
    // https://windows-server.lenovo.com/repo/2024_05/html/SR675V3_7D9Q_7D9R-Windows_Server_2019.html
    ||  normalized_part_number == "sn37b36732"
}

// returns true if the part number is for a Bluefield-3 SuperNIC
pub fn is_bf3_supernic_part_number(part_number: &str) -> bool {
    let normalized_part_number = part_number.trim().to_lowercase();
    // prefix matching for BlueField-3 SuperNICs (https://docs.nvidia.com/networking/display/bf3dpu)
    normalized_part_number.starts_with("900-9d3b4")
        || normalized_part_number.starts_with("900-9d3d4")
}

// returns true if the part number is for a Bluefield-2
pub fn is_bf2_dpu_part_number(part_number: &str) -> bool {
    let normalized_part_number = part_number.trim().to_lowercase();
    // prefix matching for BlueField-2 DPU (https://docs.nvidia.com/nvidia-bluefield-2-ethernet-dpu-user-guide.pdf)
    normalized_part_number.starts_with("mbf2")
}

pub fn is_bf4_dpu_part_number(part_number: &str) -> bool {
    let normalized_part_number = part_number.to_lowercase();
    normalized_part_number.starts_with("900-9d4b4")
        || normalized_part_number.starts_with("900-9d4a4")
}

/// Whether a DPU BMC chassis member carries the card product identity
/// (part/model/serial).
///
/// Older Redfish reports publish this identity on `Card1`; newer BF4 firmware may
/// instead publish it on the integrated BMC chassis (`Bluefield_BMC` or
/// `BlueField_BMC_0`). These IDs are expected to be mutually exclusive as product
/// identity sources in real reports, so callers can select the first matching
/// chassis.
fn is_dpu_product_chassis_id(id: &str) -> bool {
    matches!(id, "Card1" | "Bluefield_BMC" | "BlueField_BMC_0")
}

/// Whether a Redfish ComputerSystem id identifies a BlueField DPU system.
///
/// Firmware is inconsistent: older dumps expose `/redfish/v1/Systems/Bluefield`
/// while newer BF4 firmware exposes `/redfish/v1/Systems/BlueField_0`. Accept
/// both so DPU detection is not silently skipped.
pub fn is_bluefield_system(system: &ComputerSystem) -> bool {
    matches!(system.id.as_str(), "Bluefield" | "BlueField_0")
}

fn chassis_part_number(chassis: &Chassis) -> Option<&str> {
    chassis
        .part_number
        .as_deref()
        .map(str::trim)
        .none_if_empty()
}

fn chassis_model(chassis: &Chassis) -> Option<&str> {
    chassis.model.as_deref().map(str::trim).none_if_empty()
}

// returns true if the passed in string is a BlueField part number
pub fn is_bluefield_part_number(part_number: &str) -> bool {
    let normalized_part_number = part_number.trim().to_lowercase();
    normalized_part_number.contains("bluefield")
        || is_bf3_dpu_part_number(&normalized_part_number)
        // prefix matching for BlueField-3 SuperNICs (https://docs.nvidia.com/networking/display/bf3dpu)
        || is_bf3_supernic_part_number(&normalized_part_number)
        // prefix matching for BlueField-2 DPU (https://docs.nvidia.com/nvidia-bluefield-2-ethernet-dpu-user-guide.pdf)
        // TODO (sp): should we be matching on all the individual models listed ("MBF2M516C-CECOT", .. etc)
        || is_bf2_dpu_part_number(&normalized_part_number)
        || is_bf4_dpu_part_number(&normalized_part_number)
}

/// The kind of BlueField/Mellanox device, classified from its Redfish part number.
///
/// The part number identifies the card's factory SKU, not the mode it is
/// operating in: `900-9D3B6` is a BlueField-3 DPU product, while `900-9D3B4`
/// and `900-9D3D4` are BlueField-3 SuperNIC products that ship running as
/// NICs. Reconfiguring a card between DPU and NIC mode (the DPU BMC's
/// `Mode.Set` action) does not change its part number -- a flipped `900-9D3B6`
/// still classifies as [`MlxDeviceKind::Bf3DpuMode`] here. For the mode a
/// device is actually operating in, read [`ExploredMlxDevice::nic_mode`],
/// which comes from the DPU's own BMC.
///
/// The `*Mode` variant names are frozen: they mirror the wire enum from when
/// this classification was believed to track the operating mode.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MlxDeviceKind {
    /// BlueField-3 SuperNIC (part number `900-9D3B4...`).
    Bf3NicMode,
    /// BlueField-3 DPU (part number `900-9D3B6...`).
    Bf3DpuMode,
    /// BlueField-3 SuperNIC (part number `900-9D3D4...`).
    Bf3SuperNic,
    /// BlueField-2 DPU (part number `MBF2...`).
    Bf2Dpu,
    /// A BlueField we recognized but could not pin to a known part-number prefix.
    Unknown,
}

impl MlxDeviceKind {
    /// Classifies a device by its Redfish part number, returning
    /// [`MlxDeviceKind::Unknown`] for a BlueField whose part number matches no
    /// known prefix (or is absent).
    pub fn from_part_number(part_number: Option<&str>) -> Self {
        let Some(part_number) = part_number else {
            return Self::Unknown;
        };
        let part_number = part_number.trim().to_lowercase();
        // `is_bf3_supernic_part_number` deliberately groups `900-9d3b4` and
        // `900-9d3d4`; here we keep them apart because the wire enum
        // distinguishes the two SuperNIC SKU families.
        if part_number.starts_with("900-9d3b6") || part_number == "sn37b36732" {
            Self::Bf3DpuMode
        } else if part_number.starts_with("900-9d3b4") {
            Self::Bf3NicMode
        } else if part_number.starts_with("900-9d3d4") {
            Self::Bf3SuperNic
        } else if part_number.starts_with("mbf2") {
            Self::Bf2Dpu
        } else {
            Self::Unknown
        }
    }
}

impl Display for MlxDeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            // Both SuperNIC SKU families render under NVIDIA's product name;
            // the part number alongside is the discriminator.
            Self::Bf3NicMode | Self::Bf3SuperNic => "BlueField-3 SuperNIC",
            Self::Bf3DpuMode => "BlueField-3 DPU",
            Self::Bf2Dpu => "BlueField-2 DPU",
            Self::Unknown => "Unknown",
        };
        write!(f, "{label}")
    }
}

/// A Mellanox/BlueField device surfaced from site exploration.
///
/// This is the explored counterpart to scout's live `MlxDeviceReport`: it is
/// derived from a host BMC's Redfish PCIe inventory -- already captured during
/// site exploration -- so it reports a device's NIC firmware, part number and
/// serial even for a BlueField in NIC mode, whose Arm OS is down and so cannot
/// report any of that over its own management channel. A single host exploration
/// report can produce several of these (a machine commonly holds one or two DPUs
/// and up to eight SuperNICs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExploredMlxDevice {
    /// The BMC IP of the host the device was found under.
    pub host_bmc_ip: IpAddr,
    /// The host's `MachineId`, once it has been ingested far enough to derive one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    /// The device kind, classified from its part number.
    pub device_kind: MlxDeviceKind,
    /// Redfish PCIe device id / slot (e.g. `188-0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcie_id: Option<String>,
    /// Manufacturer part number (e.g. `900-9D3B4-00EN-EA0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
    /// Board serial number (e.g. `MT2403X00984`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// The NIC firmware version currently installed (e.g. `32.42.1000`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    /// The long device description as reported by Redfish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The BMC IP of the device's own DPU endpoint, set when the device's serial
    /// matches a DPU we have explored. This is the address to target for a
    /// firmware push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpu_bmc_ip: Option<IpAddr>,
    /// The DPU's authoritative operating mode, read from its own Redfish endpoint
    /// when matched. This is the mode the card is running in right now;
    /// `device_kind` is its factory SKU, and the two legitimately differ for a
    /// DPU reconfigured to run as a NIC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nic_mode: Option<BlueFieldOperatingMode>,
}

impl EndpointExplorationReport {
    /// Projects this report's Redfish PCIe inventory into [`ExploredMlxDevice`]s --
    /// one per BlueField/Mellanox device, with its part number, NIC firmware and
    /// serial. `dpu_bmc_ip`/`nic_mode` are left unset here; they are filled by
    /// [`collect_explored_mlx_devices`] once a device is matched to its DPU endpoint.
    pub fn explored_mlx_devices(&self, host_bmc_ip: IpAddr) -> Vec<ExploredMlxDevice> {
        self.systems
            .iter()
            .flat_map(|system| system.pcie_devices.iter())
            .filter(|device| device.is_bluefield())
            .map(|device| ExploredMlxDevice {
                host_bmc_ip,
                machine_id: self.machine_id,
                device_kind: MlxDeviceKind::from_part_number(device.part_number.as_deref()),
                pcie_id: device.id.clone(),
                part_number: device.part_number.clone(),
                serial_number: device.serial_number.clone(),
                firmware_version: device.firmware_version.clone(),
                description: device.description.clone(),
                dpu_bmc_ip: None,
                nic_mode: None,
            })
            .collect()
    }

    /// Whether this report's Redfish PCIe inventory holds any BlueField/Mellanox
    /// device -- i.e. whether it would yield any [`ExploredMlxDevice`].
    pub fn has_bluefield_devices(&self) -> bool {
        self.systems
            .iter()
            .flat_map(|system| system.pcie_devices.iter())
            .any(|device| device.is_bluefield())
    }

    /// The (trimmed, non-empty) serial numbers of the BlueField devices in this
    /// report's PCIe inventory -- the keys used to match each device to its DPU
    /// endpoint, the same serials [`collect_explored_mlx_devices`] joins on.
    pub fn bluefield_device_serials(&self) -> Vec<String> {
        self.systems
            .iter()
            .flat_map(|system| system.pcie_devices.iter())
            .filter(|device| device.is_bluefield())
            .filter_map(|device| device.serial_number.as_deref())
            .map(str::trim)
            .filter(|serial| !serial.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Serial key used to join a DPU BMC endpoint to the same DPU as reported by
    /// its host BMC.
    pub fn dpu_pairing_serial_number(&self) -> Option<&str> {
        if !self.is_dpu() {
            return None;
        }

        self.systems
            .first()
            .and_then(|system| system.serial_number.as_deref())
            .map(str::trim)
            .none_if_empty()
            .or_else(|| {
                // BF4 Redfish does not currently expose the product serial or
                // DPU/NIC mode on the system object. The stable product serial
                // lives on the product BMC chassis and matches the serial the
                // host BMC reports for the PCIe/network-adapter device.
                self.chassis
                    .iter()
                    .filter(|chassis| is_dpu_product_chassis_id(&chassis.id))
                    .find_map(|chassis| {
                        chassis
                            .serial_number
                            .as_deref()
                            .map(str::trim)
                            .none_if_empty()
                    })
            })
    }
}

/// Builds the [`ExploredMlxDevice`] view across a set of explored endpoints.
///
/// Host endpoints contribute their BlueField PCIe devices; DPU endpoints are
/// indexed by serial so each device can be matched back to the DPU's own BMC --
/// yielding the DPU BMC IP to target for an upgrade and the authoritative NIC
/// mode. A device whose DPU BMC we have not (yet) explored still appears, just
/// without those two fields. This is the same serial correlation site
/// exploration already uses to attach DPUs to their hosts.
pub fn collect_explored_mlx_devices(endpoints: &[ExploredEndpoint]) -> Vec<ExploredMlxDevice> {
    // Index explored DPU endpoints by the serial that host BMCs report for the
    // same device. Empty serials are skipped, and a serial reported by more than
    // one DPU endpoint is dropped as ambiguous: better to attach nothing than to
    // join to the wrong DPU.
    let mut dpu_by_serial: HashMap<&str, &ExploredEndpoint> = HashMap::new();
    let mut ambiguous: HashSet<&str> = HashSet::new();
    for ep in endpoints.iter().filter(|ep| ep.report.is_dpu()) {
        let Some(serial) = ep.report.dpu_pairing_serial_number() else {
            continue;
        };
        if dpu_by_serial.insert(serial, ep).is_some() {
            ambiguous.insert(serial);
        }
    }
    for serial in ambiguous {
        dpu_by_serial.remove(serial);
    }

    endpoints
        .iter()
        // Project from host endpoints; a DPU's own BMC reports no meaningful PCIe
        // inventory, and shouldn't list itself as a host-side device.
        .filter(|ep| !ep.report.is_dpu())
        .flat_map(|ep| ep.report.explored_mlx_devices(ep.address))
        .map(|mut device| {
            if let Some(dpu_ep) = device
                .serial_number
                .as_deref()
                .map(str::trim)
                .none_if_empty()
                .and_then(|serial| dpu_by_serial.get(serial))
            {
                device.dpu_bmc_ip = Some(dpu_ep.address);
                device.nic_mode = dpu_ep.report.bluefield_operating_mode();
            }
            device
        })
        .collect()
}

#[cfg(test)]
mod explored_mlx_device_tests {
    use super::*;

    fn endpoint(address: &str, report: EndpointExplorationReport) -> ExploredEndpoint {
        ExploredEndpoint {
            address: address.parse().unwrap(),
            report,
            report_version: ConfigVersion::new(1),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
            pause_ingestion_and_poweron: false,
        }
    }

    fn pcie(part: &str, fw: &str, serial: &str, id: &str) -> PCIeDevice {
        PCIeDevice {
            description: Some(format!("NVIDIA BlueField-3 {part}")),
            firmware_version: Some(fw.to_string()),
            gpu_vendor: None,
            id: Some(id.to_string()),
            manufacturer: Some("Nvidia".to_string()),
            name: Some("Network Device".to_string()),
            part_number: Some(part.to_string()),
            serial_number: Some(serial.to_string()),
            status: None,
        }
    }

    fn dpu_report_with_card1_part_number(part_number: Option<&str>) -> EndpointExplorationReport {
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "Card1".to_string(),
                model: Some("BlueField-3 DPU".to_string()),
                part_number: part_number.map(str::to_string),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn dpu_report_with_bf4_bmc_chassis(
        bmc_chassis_id: &str,
        bmc_part_number: &str,
    ) -> EndpointExplorationReport {
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: if bmc_chassis_id == "BlueField_BMC_0" {
                    "BlueField_0".to_string()
                } else {
                    "Bluefield".to_string()
                },
                ..Default::default()
            }],
            chassis: vec![
                Chassis {
                    id: "Card1".to_string(),
                    ..Default::default()
                },
                Chassis {
                    id: bmc_chassis_id.to_string(),
                    part_number: Some(bmc_part_number.to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn missing_vendor_decodes_legacy_unit_variant() {
        // Records written before `observed` was added are stored as the bare
        // internally-tagged unit form. They must still deserialize, defaulting
        // `observed` to None.
        let legacy: EndpointExplorationError =
            serde_json::from_str(r#"{"Type":"MissingVendor"}"#).expect("legacy form must decode");
        assert_eq!(
            legacy,
            EndpointExplorationError::MissingVendor { observed: None }
        );
    }

    #[test]
    fn missing_vendor_round_trips_with_observed() {
        // New records carry the observed Vendor/Oem string and round-trip.
        let with_observed = EndpointExplorationError::MissingVendor {
            observed: Some("SomeNewVendor".to_string()),
        };
        let json = serde_json::to_string(&with_observed).expect("serialize");
        let decoded: EndpointExplorationError = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, with_observed);

        // And the absent case round-trips too.
        let absent = EndpointExplorationError::MissingVendor { observed: None };
        let json = serde_json::to_string(&absent).expect("serialize");
        let decoded: EndpointExplorationError = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, absent);
    }

    #[test]
    fn dpu_part_number_reads_card1_part_number() {
        assert_eq!(
            dpu_report_with_card1_part_number(Some("900-9D3B6-00CV-AA0")).dpu_part_number(),
            Some("900-9D3B6-00CV-AA0")
        );
        assert_eq!(
            dpu_report_with_card1_part_number(Some("900-9D3B6-00CV-AA0   ")).dpu_part_number(),
            Some("900-9D3B6-00CV-AA0")
        );
        assert_eq!(
            dpu_report_with_card1_part_number(None).dpu_part_number(),
            None
        );
        assert_eq!(
            dpu_report_with_card1_part_number(Some("   ")).dpu_part_number(),
            None
        );
    }

    #[test]
    fn dpu_part_number_falls_back_to_dpu_bmc_chassis_when_card1_empty() {
        const VR_BF4_PART: &str = "900-9D4A4-00CB-TS4";
        assert_eq!(
            dpu_report_with_bf4_bmc_chassis("Bluefield_BMC", VR_BF4_PART).dpu_part_number(),
            Some(VR_BF4_PART)
        );
        assert_eq!(
            dpu_report_with_bf4_bmc_chassis("BlueField_BMC_0", VR_BF4_PART).dpu_part_number(),
            Some(VR_BF4_PART)
        );
        let mut report = dpu_report_with_card1_part_number(Some("900-9D3B6-00CV-AA0"));
        report.chassis.push(Chassis {
            id: "Bluefield_BMC".to_string(),
            part_number: Some(VR_BF4_PART.to_string()),
            ..Default::default()
        });
        assert_eq!(
            report.dpu_part_number(),
            Some("900-9D3B6-00CV-AA0"),
            "Card1 part number must win when present"
        );
    }

    #[test]
    fn recognizes_legacy_and_new_bluefield_system_ids() {
        let system = |id: &str| ComputerSystem {
            id: id.to_string(),
            ..Default::default()
        };
        assert!(is_bluefield_system(&system("Bluefield")));
        assert!(is_bluefield_system(&system("BlueField_0")));
        assert!(!is_bluefield_system(&system("Bluefield_0")));
    }

    #[test]
    fn new_bf4_ids_use_bmc_chassis_for_identity_and_pairing() {
        const SERIAL: &str = "MT2610604VN4";
        let mut report = dpu_report_with_bf4_bmc_chassis("BlueField_BMC_0", "900-9D4A4-00CB-TS4");
        let bmc_chassis = report
            .chassis
            .iter_mut()
            .find(|chassis| chassis.id == "BlueField_BMC_0")
            .unwrap();
        bmc_chassis.serial_number = Some(SERIAL.to_string());
        bmc_chassis.model = Some("B4240".to_string());

        assert_eq!(report.identify_dpu(), Some(DpuModel::Unknown));
        assert_eq!(report.machine_id_serial_number(), Some(SERIAL));
        assert_eq!(report.dpu_pairing_serial_number(), Some(SERIAL));
    }

    #[test]
    fn is_bf4_dpu_part_number_matches_vera_rubin_sku() {
        assert!(is_bf4_dpu_part_number("900-9D4B4-CWAA-TSA"));
        assert!(is_bf4_dpu_part_number("900-9D4A4-00CB-TS4"));
        assert!(is_bluefield_part_number("900-9D4A4-00CB-TS4"));
        assert!(!is_bf4_dpu_part_number("900-9D3B6-00CV-AA0"));
    }

    #[test]
    fn classifies_bluefield_kind_by_part_number() {
        struct Case {
            name: &'static str,
            part_number: Option<&'static str>,
            expected: MlxDeviceKind,
        }
        let cases = [
            Case {
                name: "bf3 nic mode",
                part_number: Some("900-9D3B4-00EN-EA0"),
                expected: MlxDeviceKind::Bf3NicMode,
            },
            Case {
                name: "bf3 dpu mode",
                part_number: Some("900-9D3B6-00CV-AA0"),
                expected: MlxDeviceKind::Bf3DpuMode,
            },
            Case {
                name: "bf3 supernic",
                part_number: Some("900-9D3D4-00EN-HA0_Ax"),
                expected: MlxDeviceKind::Bf3SuperNic,
            },
            Case {
                name: "bf2 dpu",
                part_number: Some("MBF2H516A-CENOT"),
                expected: MlxDeviceKind::Bf2Dpu,
            },
            Case {
                name: "lenovo-branded bf3 dpu",
                part_number: Some("SN37B36732"),
                expected: MlxDeviceKind::Bf3DpuMode,
            },
            Case {
                name: "lenovo-branded bf3 dpu with trailing spaces",
                part_number: Some("SN37B36732   "),
                expected: MlxDeviceKind::Bf3DpuMode,
            },
            Case {
                name: "serial-like lenovo prefix",
                part_number: Some("SN37B36732XYZ"),
                expected: MlxDeviceKind::Unknown,
            },
            Case {
                name: "bluefield without a known prefix",
                part_number: Some("NVIDIA BlueField mystery board"),
                expected: MlxDeviceKind::Unknown,
            },
            Case {
                name: "absent part number",
                part_number: None,
                expected: MlxDeviceKind::Unknown,
            },
        ];
        for case in cases {
            assert_eq!(
                MlxDeviceKind::from_part_number(case.part_number),
                case.expected,
                "{}",
                case.name
            );
        }
        assert!(is_bf3_dpu_part_number(" SN37B36732 "));
        assert!(!is_bf3_dpu_part_number("SN37B36732XYZ"));
    }

    #[test]
    fn projects_pcie_inventory_and_joins_dpu_by_serial() {
        // A host that reports a NIC-mode DPU (outdated FW) and a native SuperNIC.
        let host = endpoint(
            "192.0.2.20",
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    pcie_devices: vec![
                        pcie("900-9D3B4-00EN-EA0", "32.38.1002", "MT2403X00984", "188-0"),
                        pcie(
                            "900-9D3D4-00EN-HA0_Ax",
                            "32.42.1000",
                            "MT2403X09999",
                            "204-0",
                        ),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        // The NIC-mode DPU's own BMC endpoint, keyed by the matching serial.
        let dpu = endpoint(
            "192.0.2.50",
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    id: "Bluefield".to_string(),
                    serial_number: Some("MT2403X00984".to_string()),
                    attributes: ComputerSystemAttributes {
                        nic_mode: Some(BlueFieldOperatingMode::Nic),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                chassis: vec![Chassis {
                    id: "Card1".to_string(),
                    model: Some("NVIDIA BlueField 3".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        let mut devices = collect_explored_mlx_devices(&[host, dpu]);
        devices.sort_by(|a, b| a.pcie_id.cmp(&b.pcie_id));
        assert_eq!(devices.len(), 2, "only the host's two BlueField devices");

        let nic_dpu = &devices[0];
        assert_eq!(nic_dpu.device_kind, MlxDeviceKind::Bf3NicMode);
        assert_eq!(nic_dpu.part_number.as_deref(), Some("900-9D3B4-00EN-EA0"));
        assert_eq!(nic_dpu.firmware_version.as_deref(), Some("32.38.1002"));
        assert_eq!(nic_dpu.serial_number.as_deref(), Some("MT2403X00984"));
        assert_eq!(nic_dpu.host_bmc_ip, "192.0.2.20".parse::<IpAddr>().unwrap());
        // matched to its DPU endpoint by serial
        assert_eq!(
            nic_dpu.dpu_bmc_ip,
            Some("192.0.2.50".parse::<IpAddr>().unwrap())
        );
        assert_eq!(nic_dpu.nic_mode, Some(BlueFieldOperatingMode::Nic));

        let supernic = &devices[1];
        assert_eq!(supernic.device_kind, MlxDeviceKind::Bf3SuperNic);
        // no DPU endpoint matched this serial, so the join fields stay unset
        assert_eq!(supernic.dpu_bmc_ip, None);
        assert_eq!(supernic.nic_mode, None);
    }

    #[test]
    fn projects_bf4_and_joins_dpu_by_bluefield_bmc_chassis_serial() {
        const BF4_SERIAL: &str = "MT020000000003";
        let host = endpoint(
            "192.0.2.20",
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    pcie_devices: vec![pcie(
                        "900-9D4B4-CWAA-TSA",
                        "82.48.0802",
                        BF4_SERIAL,
                        "mat_2",
                    )],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let dpu = endpoint(
            "192.0.2.50",
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    id: "Bluefield".to_string(),
                    // BF4 leaves the system serial unset; the stable product
                    // serial used for host pairing is on the Bluefield_BMC
                    // chassis below.
                    serial_number: None,
                    ..Default::default()
                }],
                chassis: vec![Chassis {
                    id: "Bluefield_BMC".to_string(),
                    serial_number: Some(BF4_SERIAL.to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        let devices = collect_explored_mlx_devices(&[host, dpu]);
        assert_eq!(devices.len(), 1);

        let bf4 = &devices[0];
        assert_eq!(bf4.device_kind, MlxDeviceKind::Unknown);
        assert_eq!(bf4.part_number.as_deref(), Some("900-9D4B4-CWAA-TSA"));
        assert_eq!(bf4.serial_number.as_deref(), Some(BF4_SERIAL));
        assert_eq!(bf4.dpu_bmc_ip, Some("192.0.2.50".parse().unwrap()));
        // BF4 does not currently expose DPU/NIC mode through Redfish; missing
        // mode must not prevent the serial join.
        assert_eq!(bf4.nic_mode, None);
    }

    #[test]
    fn serial_join_skips_empty_and_ambiguous_serials() {
        let dpu = |addr: &str, serial: &str| {
            endpoint(
                addr,
                EndpointExplorationReport {
                    endpoint_type: EndpointType::Bmc,
                    systems: vec![ComputerSystem {
                        id: "Bluefield".to_string(),
                        serial_number: Some(serial.to_string()),
                        attributes: ComputerSystemAttributes {
                            nic_mode: Some(BlueFieldOperatingMode::Nic),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    chassis: vec![Chassis {
                        id: "Card1".to_string(),
                        model: Some("NVIDIA BlueField 3".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
        };
        // Host reports one device with an empty serial and one whose serial is
        // claimed by two different DPU endpoints.
        let host = endpoint(
            "192.0.2.20",
            EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                systems: vec![ComputerSystem {
                    pcie_devices: vec![
                        pcie("900-9D3B4-00EN-EA0", "32.38.1002", "", "188-0"),
                        pcie("900-9D3B4-00EN-EA0", "32.38.1002", "DUP123", "204-0"),
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        let devices = collect_explored_mlx_devices(&[
            host,
            dpu("192.0.2.50", "DUP123"),
            dpu("192.0.2.51", "DUP123"),
        ]);

        // Both devices project, but neither joins: the empty serial is skipped and
        // the duplicated "DUP123" serial is ambiguous.
        assert_eq!(devices.len(), 2);
        for device in &devices {
            assert_eq!(device.dpu_bmc_ip, None);
            assert_eq!(device.nic_mode, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{
        Case, Check, check_cases, check_values, scenarios, value_scenarios,
    };

    use super::*;
    use crate::firmware::FirmwareComponent;
    use crate::machine::machine_id::from_hardware_info;

    #[test]
    fn identify_dpu_recognizes_bluefield_model_variants() {
        value_scenarios!(
            run = |(system_id, model)| {
                let report = EndpointExplorationReport {
                    systems: vec![ComputerSystem {
                        id: system_id.to_string(),
                        ..Default::default()
                    }],
                    chassis: vec![Chassis {
                        id: "Card1".to_string(),
                        model: Some(model.to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                let identified = report.identify_dpu();
                if let Some(dpu_model) = &identified {
                    assert_eq!(report.model(), Some(dpu_model.to_string()));
                }
                identified
            };
            "BlueField model identification" {
                ("Bluefield", "NVIDIA BlueField 2 DPU") => Some(DpuModel::BlueField2),
                ("Bluefield", "NVIDIA BlueField 3 DPU") => Some(DpuModel::BlueField3),
                ("Bluefield", "BlueField-3 DPU") => Some(DpuModel::BlueField3),
                ("Bluefield", "unrecognized DPU") => Some(DpuModel::Unknown),
                ("System", "BlueField-3 DPU") => None,
            }
        );
    }

    #[test]
    fn all_mac_addresses_combines_system_and_adapter_inventory_without_duplicates() {
        let system_mac = "02:aa:bb:cc:dd:01".parse().unwrap();
        let adapter_mac = "94:6d:ae:53:cb:9b".parse().unwrap();
        let report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![
                    EthernetInterface {
                        mac_address: Some(system_mac),
                        ..Default::default()
                    },
                    EthernetInterface {
                        mac_address: Some(adapter_mac),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            chassis: vec![Chassis {
                network_adapters: vec![NetworkAdapter {
                    port_mac_addresses: vec![system_mac, adapter_mac],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(report.all_mac_addresses(), vec![system_mac, adapter_mac]);
    }

    #[test]
    fn bluefield_operating_mode_preserves_legacy_serialized_values() {
        scenarios!(
            run = |json| serde_json::from_str::<BlueFieldOperatingMode>(json).map_err(drop);
            "DPU mode canonical value" {
                r#""DpuMode""# => Yields(BlueFieldOperatingMode::Dpu),
            }

            "DPU mode alias" {
                r#""Dpu""# => Yields(BlueFieldOperatingMode::Dpu),
            }

            "NIC mode canonical value" {
                r#""NicMode""# => Yields(BlueFieldOperatingMode::Nic),
            }

            "NIC mode alias" {
                r#""Nic""# => Yields(BlueFieldOperatingMode::Nic),
            }
        );

        assert_eq!(
            serde_json::to_string(&BlueFieldOperatingMode::Dpu).unwrap(),
            r#""DpuMode""#
        );
        assert_eq!(
            serde_json::to_string(&BlueFieldOperatingMode::Nic).unwrap(),
            r#""NicMode""#
        );
    }

    fn create_test_firmware(firmware_type: FirmwareComponentType, regex_pattern: &str) -> Firmware {
        let mut components = HashMap::new();
        components.insert(
            firmware_type,
            FirmwareComponent {
                current_version_reported_as: Some(Regex::new(regex_pattern).unwrap()),
                preingest_upgrade_when_below: None,
                known_firmware: vec![],
            },
        );

        Firmware {
            vendor: bmc_vendor::BMCVendor::Nvidia,
            model: "Test Model".to_string(),
            components,
            explicit_start_needed: false,
            ordering: vec![],
        }
    }

    fn create_test_endpoint(inventories: Vec<(&str, Option<&str>)>) -> ExploredEndpoint {
        let inventory_objects: Vec<Inventory> = inventories
            .into_iter()
            .map(|(id, version)| Inventory {
                id: id.to_string(),
                description: None,
                version: version.map(|v| v.to_string()),
                release_date: None,
            })
            .collect();

        ExploredEndpoint {
            address: "192.168.1.1".parse::<IpAddr>().unwrap(),
            report: EndpointExplorationReport {
                endpoint_type: EndpointType::Bmc,
                service: vec![Service {
                    id: "FirmwareInventory".to_string(),
                    inventories: inventory_objects,
                }],
                ..Default::default()
            },
            report_version: ConfigVersion::new(1),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
            pause_ingestion_and_poweron: false,
        }
    }

    #[test]
    fn endpoint_boot_interface_target_preserves_legacy_mac_only_records() {
        let mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]);

        value_scenarios!(run = |(boot_interface_mac, boot_interface_id)| {
            let mut endpoint = create_test_endpoint(Vec::new());
            endpoint.boot_interface_mac = boot_interface_mac;
            endpoint.boot_interface_id = boot_interface_id;
            endpoint.boot_interface_target()
        };
            "complete pair" {
                (Some(mac), Some("NIC.Slot.7-1-1".to_string())) =>
                    Some(MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: mac,
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                    })),
            }

            "legacy MAC only" {
                (Some(mac), None) => Some(MachineBootInterfaceTarget::MacOnly(mac)),
            }

            "interface id without MAC" {
                (None, Some("NIC.Slot.7-1-1".to_string())) => None,
            }

            "no stored target" {
                (None, None) => None,
            }
        );
    }

    #[test]
    fn machine_setup_status_json_correlates_the_evaluated_target() {
        let mac = MacAddress::new([0x02, 0, 0, 0, 0, 1]);

        value_scenarios!(run = |status: MachineSetupStatus| {
            let json = serde_json::to_value(&status).expect("status serializes");
            let round_trip =
                serde_json::from_value(json.clone()).expect("serialized status deserializes");
            (json, round_trip)
        };
            "status without a target remains backward compatible" {
                MachineSetupStatus {
                    is_done: true,
                    diffs: Vec::new(),
                    evaluated_boot_interface: None,
                } => (
                    serde_json::json!({
                        "IsDone": true,
                        "Diffs": [],
                    }),
                    MachineSetupStatus {
                        is_done: true,
                        diffs: Vec::new(),
                        evaluated_boot_interface: None,
                    },
                ),
            }

            "paired target" {
                MachineSetupStatus {
                    is_done: false,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(MachineBootInterfaceTarget::Pair(
                        MachineBootInterface {
                            mac_address: mac,
                            interface_id: "NIC.Slot.7-1-1".to_string(),
                        },
                    )),
                } => (
                    serde_json::json!({
                        "IsDone": false,
                        "Diffs": [],
                        "EvaluatedBootInterface": {
                            "Pair": {
                                "MacAddress": "02:00:00:00:00:01",
                                "InterfaceId": "NIC.Slot.7-1-1",
                            },
                        },
                    }),
                    MachineSetupStatus {
                        is_done: false,
                        diffs: Vec::new(),
                        evaluated_boot_interface: Some(MachineBootInterfaceTarget::Pair(
                            MachineBootInterface {
                                mac_address: mac,
                                interface_id: "NIC.Slot.7-1-1".to_string(),
                            },
                        )),
                    },
                ),
            }

            "legacy MAC-only target" {
                MachineSetupStatus {
                    is_done: true,
                    diffs: Vec::new(),
                    evaluated_boot_interface: Some(MachineBootInterfaceTarget::MacOnly(mac)),
                } => (
                    serde_json::json!({
                        "IsDone": true,
                        "Diffs": [],
                        "EvaluatedBootInterface": {
                            "MacOnly": "02:00:00:00:00:01",
                        },
                    }),
                    MachineSetupStatus {
                        is_done: true,
                        diffs: Vec::new(),
                        evaluated_boot_interface: Some(MachineBootInterfaceTarget::MacOnly(mac)),
                    },
                ),
            }
        );

        let legacy: MachineSetupStatus = serde_json::from_str(r#"{"IsDone":true,"Diffs":[]}"#)
            .expect("reports written before target capture still deserialize");
        assert_eq!(legacy.evaluated_boot_interface, None);
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MitigationGroup {
        Network,
        HardwareCompatibility,
        Credentials,
        IntermittentCredentials,
        DpuRecovery,
        DgxRetry,
        NoMitigation,
    }

    impl MitigationGroup {
        fn from_mitigation(mitigation: Option<&str>) -> Self {
            match mitigation {
                None => Self::NoMitigation,
                Some(mitigation) if mitigation.contains("force-restarts the DPU") => {
                    Self::DpuRecovery
                }
                Some(mitigation)
                    if mitigation.contains("general DGX H100/H200 Redfish API information") =>
                {
                    Self::DgxRetry
                }
                Some(mitigation) if mitigation.starts_with("Transient:") => {
                    Self::IntermittentCredentials
                }
                Some(mitigation)
                    if mitigation.contains("PUT /v2/org/{org}/nico/credential/bmc") =>
                {
                    Self::Credentials
                }
                Some(mitigation) if mitigation.contains("Hardware Compatibility List") => {
                    Self::HardwareCompatibility
                }
                Some(mitigation) if mitigation.contains("network reachability") => Self::Network,
                Some(mitigation) => panic!("unclassified mitigation: {mitigation}"),
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EndpointErrorBehavior {
        code: ErrorCode,
        unauthorized: bool,
        unreachable: bool,
        redfish: bool,
        invalid_dpu_bios_response: bool,
        intermittent_unauthorized_count: Option<u32>,
        mitigation_group: MitigationGroup,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct EndpointPredicates {
        unauthorized: bool,
        unreachable: bool,
        redfish: bool,
        invalid_dpu_bios_response: bool,
    }

    fn expected_error_behavior(
        code: ErrorCode,
        predicates: EndpointPredicates,
        intermittent_unauthorized_count: Option<u32>,
        mitigation_group: MitigationGroup,
    ) -> EndpointErrorBehavior {
        let EndpointPredicates {
            unauthorized,
            unreachable,
            redfish,
            invalid_dpu_bios_response,
        } = predicates;
        EndpointErrorBehavior {
            code,
            unauthorized,
            unreachable,
            redfish,
            invalid_dpu_bios_response,
            intermittent_unauthorized_count,
            mitigation_group,
        }
    }

    #[test]
    fn endpoint_exploration_error_behavior_is_stable() {
        use ErrorSubsystem::SiteExplorer;
        use MitigationGroup::*;

        check_values(
            [
                Check {
                    scenario: "connection timeout",
                    input: EndpointExplorationError::ConnectionTimeout {
                        details: "timeout".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 100),
                        EndpointPredicates {
                            unreachable: true,
                            ..Default::default()
                        },
                        None,
                        Network,
                    ),
                },
                Check {
                    scenario: "connection refused",
                    input: EndpointExplorationError::ConnectionRefused {
                        details: "refused".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 101),
                        EndpointPredicates {
                            unreachable: true,
                            ..Default::default()
                        },
                        None,
                        Network,
                    ),
                },
                Check {
                    scenario: "unreachable",
                    input: EndpointExplorationError::Unreachable {
                        details: Some("network".to_string()),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 102),
                        EndpointPredicates {
                            unreachable: true,
                            ..Default::default()
                        },
                        None,
                        Network,
                    ),
                },
                Check {
                    scenario: "unsupported vendor",
                    input: EndpointExplorationError::UnsupportedVendor {
                        vendor: "unknown".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 120),
                        EndpointPredicates::default(),
                        None,
                        HardwareCompatibility,
                    ),
                },
                Check {
                    scenario: "missing redfish",
                    input: EndpointExplorationError::MissingRedfish { uri: None },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 121),
                        EndpointPredicates::default(),
                        None,
                        NoMitigation,
                    ),
                },
                Check {
                    scenario: "missing vendor",
                    input: EndpointExplorationError::MissingVendor { observed: None },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 122),
                        EndpointPredicates::default(),
                        None,
                        HardwareCompatibility,
                    ),
                },
                Check {
                    scenario: "redfish error",
                    input: EndpointExplorationError::RedfishError {
                        details: "request failed".to_string(),
                        response_body: None,
                        response_code: Some(500),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 130),
                        EndpointPredicates {
                            redfish: true,
                            ..Default::default()
                        },
                        None,
                        NoMitigation,
                    ),
                },
                Check {
                    scenario: "DGX firmware inventory forbidden",
                    input: EndpointExplorationError::VikingFWInventoryForbiddenError {
                        details: "forbidden".to_string(),
                        response_body: None,
                        response_code: Some(403),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 131),
                        EndpointPredicates::default(),
                        None,
                        DgxRetry,
                    ),
                },
                Check {
                    scenario: "unauthorized",
                    input: EndpointExplorationError::Unauthorized {
                        details: "unauthorized".to_string(),
                        response_body: None,
                        response_code: Some(401),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 140),
                        EndpointPredicates {
                            unauthorized: true,
                            ..Default::default()
                        },
                        None,
                        Credentials,
                    ),
                },
                Check {
                    scenario: "missing credentials",
                    input: EndpointExplorationError::MissingCredentials {
                        key: "bmc".to_string(),
                        cause: "missing".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 141),
                        EndpointPredicates::default(),
                        None,
                        Credentials,
                    ),
                },
                Check {
                    scenario: "secrets engine",
                    input: EndpointExplorationError::SecretsEngineError {
                        cause: "unavailable".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 142),
                        EndpointPredicates::default(),
                        None,
                        Credentials,
                    ),
                },
                Check {
                    scenario: "set credentials",
                    input: EndpointExplorationError::SetCredentials {
                        key: "bmc".to_string(),
                        cause: "failed".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 143),
                        EndpointPredicates::default(),
                        None,
                        Credentials,
                    ),
                },
                Check {
                    scenario: "avoid lockout",
                    input: EndpointExplorationError::AvoidLockout,
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 144),
                        EndpointPredicates {
                            unauthorized: true,
                            ..Default::default()
                        },
                        None,
                        Credentials,
                    ),
                },
                Check {
                    scenario: "intermittent unauthorized",
                    input: EndpointExplorationError::IntermittentUnauthorized {
                        details: "temporary unauthorized response".to_string(),
                        response_body: None,
                        response_code: Some(401),
                        consecutive_count: 3,
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 145),
                        EndpointPredicates::default(),
                        Some(3),
                        IntermittentCredentials,
                    ),
                },
                Check {
                    scenario: "other",
                    input: EndpointExplorationError::Other {
                        details: "unexpected".to_string(),
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(SiteExplorer, 199),
                        EndpointPredicates::default(),
                        None,
                        NoMitigation,
                    ),
                },
                Check {
                    scenario: "invalid DPU BIOS response",
                    input: EndpointExplorationError::InvalidDpuRedfishBiosResponse {
                        details: "attributes not ready".to_string(),
                        response_body: None,
                        response_code: None,
                    },
                    expect: expected_error_behavior(
                        ErrorCode::nico(ErrorSubsystem::Dpu, 134),
                        EndpointPredicates {
                            redfish: true,
                            invalid_dpu_bios_response: true,
                            ..Default::default()
                        },
                        None,
                        DpuRecovery,
                    ),
                },
            ],
            |error| {
                let schema = error.operator_error_schema();
                EndpointErrorBehavior {
                    code: schema.error_code,
                    unauthorized: error.is_unauthorized(),
                    unreachable: error.is_unreachable(),
                    redfish: error.is_redfish(),
                    invalid_dpu_bios_response: error.is_dpu_redfish_bios_response_invalid(),
                    intermittent_unauthorized_count: error.intermittent_unauthorized_count(),
                    mitigation_group: MitigationGroup::from_mitigation(
                        schema.mitigation.as_deref(),
                    ),
                }
            },
        );
    }

    #[test]
    fn dpu_bios_error_schema_contains_operator_action() {
        let error = EndpointExplorationError::InvalidDpuRedfishBiosResponse {
            details: "DPU BMC BIOS attributes not ready".to_string(),
            response_body: None,
            response_code: None,
        };

        let schema = error.operator_error_schema();
        let mitigation = schema
            .mitigation
            .as_deref()
            .expect("DPU BIOS errors have recovery guidance");

        assert!(mitigation.contains("force-restarts the DPU"));
        assert!(mitigation.contains("BMC reset"));
        assert!(
            schema
                .text
                .contains("invalid redfish response for DPU BIOS")
        );
    }

    #[test]
    fn credential_error_schemas_use_rest_first_mitigation() {
        value_scenarios!(
            run = |error: EndpointExplorationError| error
                .operator_error_schema()
                .mitigation
                .is_some_and(|mitigation| {
                    mitigation.contains("PUT /v2/org/{org}/nico/credential/bmc")
                        && mitigation.contains("nicocli bmc-credential create")
                        && !mitigation.contains("nico-admin-cli credential add-bmc")
                });
            "credential errors" {
                EndpointExplorationError::Unauthorized {
                    details: "unauthorized".to_string(),
                    response_body: None,
                    response_code: Some(401),
                } => true,
                EndpointExplorationError::MissingCredentials {
                    key: "bmc".to_string(),
                    cause: "missing".to_string(),
                } => true,
                EndpointExplorationError::SecretsEngineError {
                    cause: "unavailable".to_string(),
                } => true,
                EndpointExplorationError::SetCredentials {
                    key: "bmc".to_string(),
                    cause: "failed".to_string(),
                } => true,
                EndpointExplorationError::AvoidLockout => true,
                EndpointExplorationError::IntermittentUnauthorized {
                    details: "temporary unauthorized response".to_string(),
                    response_body: None,
                    response_code: Some(401),
                    consecutive_count: 1,
                } => true,
            }
        );
    }

    #[test]
    fn intermittent_unauthorized_error_schema_describes_retryable_action() {
        let error = EndpointExplorationError::IntermittentUnauthorized {
            details: "temporary unauthorized response".to_string(),
            response_body: None,
            response_code: Some(401),
            consecutive_count: 1,
        };

        let schema = error.operator_error_schema();

        assert_eq!(
            schema.error_code,
            ErrorCode::nico(ErrorSubsystem::SiteExplorer, 145)
        );
        assert_eq!(schema.error_code.to_string(), "NICO-SITEEXPLORER-145");
        // The mitigation answers "how do I retry?" and "what does escalate mean?"
        // with concrete Site Explorer and credential operations.
        let mitigation = schema.mitigation.as_deref().expect("has a mitigation");
        assert!(mitigation.contains("nico-admin-cli site-explorer refresh"));
        assert!(mitigation.contains("PUT /v2/org/{org}/nico/credential/bmc"));
        assert!(mitigation.contains("nicocli bmc-credential create"));
    }

    #[test]
    fn unsupported_vendor_error_schema_points_at_hcl() {
        const HCL_URL: &str = "https://docs.nvidia.com/infra-controller/documentation/reference/hardware-compatibility-list";

        value_scenarios!(
            run = |error: EndpointExplorationError| error
                .operator_error_schema()
                .mitigation
                .is_some_and(|mitigation| mitigation.contains(HCL_URL));
            "vendor errors" {
                EndpointExplorationError::UnsupportedVendor {
                    vendor: "unknown".to_string(),
                } => true,
                EndpointExplorationError::MissingVendor { observed: None } => true,
            }
        );
    }

    #[test]
    fn dgx_h100_fw_inventory_error_schema_describes_retryable_action() {
        let error = EndpointExplorationError::VikingFWInventoryForbiddenError {
            details: "HTTP 403 at /redfish/v1/UpdateService/FirmwareInventory".to_string(),
            response_body: None,
            response_code: Some(403),
        };

        let serialized = serde_json::to_value(&error).expect("error serializes");
        let schema = error.operator_error_schema();
        let mitigation = schema.mitigation.expect("has a mitigation");

        assert!(schema.text.contains("DGX H100"));
        assert!(!schema.text.contains("Viking"));
        assert_eq!(serialized["Type"], "VikingFWInventoryForbiddenError");
        assert!(mitigation.contains("nico-admin-cli site-explorer refresh"));
        assert!(mitigation.contains("general DGX H100/H200 Redfish API information"));
        assert!(
            mitigation.contains("docs.nvidia.com/dgx/dgxh100-user-guide/redfish-api-supp.html")
        );
    }

    /// `find_version` locates the firmware version matching a component regex,
    /// yielding the version string when an inventory matches and absent otherwise.
    #[test]
    fn test_find_version() {
        let fw_info = create_test_firmware(FirmwareComponentType::Bmc, r"^BMC_Firmware$");
        scenarios!(
            // Build an endpoint from the inventories, then look up the BMC
            // version; absent -> error so the no-match row reads as a failure.
            run = |inventories| {
                create_test_endpoint(inventories)
                    .find_version(&fw_info, FirmwareComponentType::Bmc)
                    .cloned()
                    .ok_or(())
            };
            "single match" {
                vec![("BMC_Firmware", Some("1.2.3")), ("DPU_UEFI", Some("4.5.6"))] => Yields("1.2.3".to_string()),
            }

            "no match" {
                vec![
                    ("DPU_UEFI", Some("4.5.6")),
                    ("Other_Component", Some("7.8.9")),
                ] => Fails,
            }
        );
    }

    struct FindAllVersionsInput {
        regex_pattern: &'static str,
        inventories: Vec<(&'static str, Option<&'static str>)>,
    }

    #[test]
    fn find_all_versions_returns_each_populated_match() {
        check_values(
            [
                Check {
                    scenario: "anchored pattern returns its exact match",
                    input: FindAllVersionsInput {
                        regex_pattern: r"^BMC_Firmware$",
                        inventories: vec![
                            ("BMC_Firmware", Some("1.2.3")),
                            ("DPU_UEFI", Some("4.5.6")),
                        ],
                    },
                    expect: vec!["1.2.3".to_string()],
                },
                Check {
                    scenario: "returns matching versions in inventory order",
                    input: FindAllVersionsInput {
                        regex_pattern: r"BMC_Firmware",
                        inventories: vec![
                            ("BMC_Firmware_1", Some("1.2.3")),
                            ("BMC_Firmware_2", Some("2.3.4")),
                            ("BMC_Firmware_3", Some("3.4.5")),
                            ("DPU_UEFI", Some("4.5.6")),
                        ],
                    },
                    expect: vec![
                        "1.2.3".to_string(),
                        "2.3.4".to_string(),
                        "3.4.5".to_string(),
                    ],
                },
                Check {
                    scenario: "returns no versions when no inventory matches",
                    input: FindAllVersionsInput {
                        regex_pattern: r"^BMC_Firmware$",
                        inventories: vec![("DPU_UEFI", Some("4.5.6")), ("Other", Some("7.8.9"))],
                    },
                    expect: Vec::new(),
                },
                Check {
                    scenario: "skips a matching inventory without a version",
                    input: FindAllVersionsInput {
                        regex_pattern: r"BMC_Firmware",
                        inventories: vec![
                            ("BMC_Firmware_1", Some("1.2.3")),
                            ("BMC_Firmware_2", None),
                            ("BMC_Firmware_3", Some("3.4.5")),
                        ],
                    },
                    expect: vec!["1.2.3".to_string(), "3.4.5".to_string()],
                },
            ],
            |input| {
                let fw_info = create_test_firmware(FirmwareComponentType::Bmc, input.regex_pattern);
                let endpoint = create_test_endpoint(input.inventories);

                endpoint
                    .find_all_versions(&fw_info, FirmwareComponentType::Bmc)
                    .into_iter()
                    .cloned()
                    .collect()
            },
        );
    }

    #[test]
    fn serialize_endpoint_exploration_error() {
        // test handling legacy format for the Unreachable error
        let report =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::Unreachable {
                details: None,
            });

        let serialized = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serialized,
            r#"{"EndpointType":"Unknown","LastExplorationError":{"Type":"Unreachable","Details":null}}"#
        );
        assert_eq!(
            serde_json::from_str::<EndpointExplorationReport>(&serialized).unwrap(),
            report
        );

        let report =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::Unreachable {
                details: Some("test_details".to_string()),
            });

        let serialized = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serialized,
            r#"{"EndpointType":"Unknown","LastExplorationError":{"Type":"Unreachable","Details":"test_details"}}"#
        );
        assert_eq!(
            serde_json::from_str::<EndpointExplorationReport>(&serialized).unwrap(),
            report
        );

        let mut report =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::RedfishError {
                details: "test".to_string(),
                response_body: None,
                response_code: None,
            });

        let serialized = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serialized,
            r#"{"EndpointType":"Unknown","LastExplorationError":{"Type":"RedfishError","Details":"test","ResponseBody":null,"ResponseCode":null}}"#
        );
        assert_eq!(
            serde_json::from_str::<EndpointExplorationReport>(&serialized).unwrap(),
            report
        );

        let serialized_nobody = r#"{"EndpointType":"Unknown","LastExplorationError":{"Type":"RedfishError","Details":"test"}}"#;
        assert_eq!(
            serde_json::from_str::<EndpointExplorationReport>(serialized_nobody).unwrap(),
            report
        );

        report.last_exploration_latency = Some(std::time::Duration::from_millis(1111));
        let serialized = serde_json::to_string(&report).unwrap();
        assert_eq!(
            serialized,
            r#"{"EndpointType":"Unknown","LastExplorationError":{"Type":"RedfishError","Details":"test","ResponseBody":null,"ResponseCode":null},"LastExplorationLatency":{"secs":1,"nanos":111000000}}"#
        );
        assert_eq!(
            serde_json::from_str::<EndpointExplorationReport>(&serialized).unwrap(),
            report
        );
    }

    #[test]
    fn serialize_explored_managed_host() {
        let host = ExploredManagedHost {
            host_bmc_ip: "1.2.3.4".parse().unwrap(),
            dpus: vec![ExploredDpu {
                bmc_ip: "1.2.3.5".parse().unwrap(),
                host_pf_mac_address: Some("11:22:33:44:55:66".parse().unwrap()),
                host_chassis_id: Some("Riser_Slot2_BlueField_3_Card".to_string()),
                report: Default::default(),
            }],
        };
        let serialized = serde_json::to_string(&host).unwrap();
        assert_eq!(
            serialized,
            r#"{"HostBmcIp":"1.2.3.4","Dpus":[{"BmcIp":"1.2.3.5","HostPfMacAddress":"11:22:33:44:55:66","HostChassisId":"Riser_Slot2_BlueField_3_Card"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<ExploredManagedHost>(&serialized).unwrap(),
            host
        );

        let host = ExploredManagedHost {
            host_bmc_ip: "1.2.3.4".parse().unwrap(),
            dpus: vec![ExploredDpu {
                bmc_ip: "1.2.3.5".parse().unwrap(),
                host_pf_mac_address: None,
                host_chassis_id: None,
                report: Default::default(),
            }],
        };
        let serialized = serde_json::to_string(&host).unwrap();
        assert_eq!(
            serialized,
            r#"{"HostBmcIp":"1.2.3.4","Dpus":[{"BmcIp":"1.2.3.5","HostPfMacAddress":null}]}"#
        );
        assert_eq!(
            serde_json::from_str::<ExploredManagedHost>(&serialized).unwrap(),
            host
        );
    }

    #[test]
    fn test_firmware_inventory() {
        let uefi_version = Some("4.5.0-46-gf57517d".to_string());
        let uefi_inventory = Inventory {
            id: "DPU_UEFI".to_string(),
            description: Some("Host image".to_string()),
            version: uefi_version.clone(),
            release_date: None,
        };
        let report = EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            managers: vec![Manager {
                ethernet_interfaces: vec![],
                id: "bmc".to_string(),
                ipmi_port: None,
            }],
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![],
                id: "Bluefield".to_string(),
                manufacturer: None,
                model: None,
                serial_number: Some("MT2242XZ00NX".to_string()),
                attributes: ComputerSystemAttributes {
                    nic_mode: Some(BlueFieldOperatingMode::Dpu),
                    is_infinite_boot_enabled: None,
                },
                pcie_devices: vec![],
                base_mac: Some("A088C208804C".parse().unwrap()),
                power_state: PowerState::On,
                sku: None,
                boot_order: None,
                bios_version: None,
                serial_console_ssh_port: None,
            }],
            chassis: vec![Chassis {
                id: "NIC.Slot.1".to_string(),
                manufacturer: None,
                model: None,
                serial_number: Some("MT2242XZ00NX".to_string()),
                part_number: None,
                network_adapters: vec![],
                physical_slot_number: None,
                compute_tray_index: None,
                topology_id: None,
                revision_id: None,
            }],
            service: vec![
                Service {
                    id: "FirmwareInventory".to_string(),
                    inventories: vec![uefi_inventory],
                },
                Service {
                    id: "SoftwareInventory".to_string(),
                    inventories: vec![],
                },
            ],
            machine_id: None,
            versions: HashMap::default(),
            model: None,
            machine_setup_status: None,
            secure_boot_status: None,
            lockdown_status: None,
            power_shelf_id: None,
            switch_id: None,

            physical_slot_number: None,
            compute_tray_index: None,
            revision_id: None,
            topology_id: None,
            remediation_error: None,
        };

        let inventory_map = report.get_inventory_map();
        // SoftwareInventory doesn't have inventories in it. So map should have only FW inventory.
        assert_eq!(inventory_map.len(), 1);
        assert_eq!(report.dpu_uefi_version(), uefi_version);
    }

    #[test]
    fn observed_host_bmc_version_requires_exact_non_blank_inventory() {
        let report_with_inventory = |id: &str, version: &str| EndpointExplorationReport {
            service: vec![Service {
                id: "FirmwareInventory".to_string(),
                inventories: vec![Inventory {
                    id: id.to_string(),
                    version: Some(version.to_string()),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        };

        assert_eq!(
            report_with_inventory("BMC-Primary", "1.0.0").observed_host_bmc_version(),
            None,
            "only the exact Lenovo GB300 BMC inventory ID is accepted"
        );
        assert_eq!(
            report_with_inventory("BMC", " \t ").observed_host_bmc_version(),
            None,
            "blank BMC versions are treated as absent"
        );
        assert_eq!(
            report_with_inventory("BMC", " 1.0.0 ").observed_host_bmc_version(),
            Some("1.0.0"),
            "the exact BMC inventory version is trimmed"
        );
    }

    #[test]
    fn system_bios_version_selects_system_0_and_rejects_blank_values() {
        let report = EndpointExplorationReport {
            systems: vec![
                ComputerSystem {
                    id: "HGX_Baseboard_0".to_string(),
                    bios_version: Some("wrong-system-version".to_string()),
                    ..Default::default()
                },
                ComputerSystem {
                    id: "System_0".to_string(),
                    bios_version: Some(" GBHC01A_01.05.0 ".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            report.system_bios_version(),
            Some("GBHC01A_01.05.0"),
            "System_0 must be selected even when the HGX baseboard appears first"
        );

        let blank_report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "System_0".to_string(),
                bios_version: Some("  ".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            blank_report.system_bios_version(),
            None,
            "blank System_0 BIOS versions are treated as absent"
        );
    }

    #[test]
    fn computer_system_bios_version_is_json_compatible() {
        let system = ComputerSystem {
            id: "System_0".to_string(),
            bios_version: Some("GBHC01A_01.05.0".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&system).unwrap();
        assert_eq!(
            serde_json::from_str::<ComputerSystem>(&json).unwrap(),
            system,
            "BiosVersion must round-trip through the exploration-report JSON"
        );

        let without_bios = serde_json::from_str::<ComputerSystem>(r#"{"Id":"System_0"}"#).unwrap();
        assert_eq!(
            without_bios.bios_version, None,
            "older JSON without BiosVersion must remain deserializable"
        );
    }

    #[test]
    fn generate_machine_id_for_dpu() {
        let mut report = EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            managers: vec![Manager {
                ethernet_interfaces: vec![],
                id: "bmc".to_string(),
                ipmi_port: None,
            }],
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![],
                id: "Bluefield".to_string(),
                manufacturer: None,
                model: None,
                serial_number: Some("MT2242XZ00NX".to_string()),
                attributes: ComputerSystemAttributes {
                    nic_mode: Some(BlueFieldOperatingMode::Dpu),
                    is_infinite_boot_enabled: None,
                },
                pcie_devices: vec![],
                base_mac: Some("A088C208804C".parse().unwrap()),
                power_state: PowerState::On,
                sku: None,
                boot_order: None,
                bios_version: None,
                serial_console_ssh_port: None,
            }],
            chassis: vec![Chassis {
                id: "NIC.Slot.1".to_string(),
                manufacturer: None,
                model: None,
                serial_number: Some("MT2242XZ00NX".to_string()),
                part_number: None,
                network_adapters: vec![],
                physical_slot_number: None,
                compute_tray_index: None,
                topology_id: None,
                revision_id: None,
            }],
            service: vec![
                Service {
                    id: "FirmwareInventory".to_string(),
                    inventories: vec![],
                },
                Service {
                    id: "SoftwareInventory".to_string(),
                    inventories: vec![],
                },
            ],
            machine_id: None,
            versions: HashMap::default(),
            model: None,
            machine_setup_status: None,
            secure_boot_status: None,
            lockdown_status: None,
            power_shelf_id: None,
            switch_id: None,
            physical_slot_number: None,
            compute_tray_index: None,
            revision_id: None,
            topology_id: None,
            remediation_error: None,
        };
        report
            .generate_machine_id(false)
            .expect("Error generating machine ID");

        let machine_id = report.machine_id.unwrap();

        assert_eq!(
            machine_id.to_string(),
            "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0"
        );

        // Check whether the MachineId is equal to what we generate inband
        let data = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/hardware_info/test_data/dpu_info.json"
        ));
        let info = serde_json::from_slice::<HardwareInfo>(data).unwrap();
        let hardware_info_machine_id = from_hardware_info(&info).unwrap();
        assert_eq!(hardware_info_machine_id.to_string(), machine_id.to_string());

        // Check the MachineId serialization and deserialization
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains(
            r#""MachineId":"fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0""#
        ));
        let deserialized = serde_json::from_str::<EndpointExplorationReport>(&serialized).unwrap();
        assert_eq!(deserialized.machine_id.unwrap(), machine_id);
    }

    /// `UefiDevicePath::from_str` parses a UEFI PciRoot/Pci device path into a
    /// dotted decimal address, requiring at least one Pci node after PciRoot.
    #[test]
    fn test_uefi_device_path() {
        check_cases(
            [
                Case {
                    scenario: "two Pci nodes",
                    input: "PciRoot(0X2)/Pci(0x1,0X0)/Pci(0X0,0x1)",
                    expect: Yields("2.1.0.0.1".to_string()),
                },
                Case {
                    scenario: "trailing MAC discarded",
                    input: "PciRoot(0x11)/Pci(0x1,0x0)/Pci(0x0,0xa)/MAC(A088C20C87C6,0x1)",
                    expect: Yields("17.1.0.0.10".to_string()),
                },
                Case {
                    // NIC attached directly to a root port (no PCI-PCI bridge upstream).
                    scenario: "single Pci node on a root port",
                    input: "PciRoot(0x7)/Pci(0x0,0x0)/MAC(525400A8282F,0x1)",
                    expect: Yields("7.0.0".to_string()),
                },
                Case {
                    // Three Pci nodes (NIC behind two upstream bridges/switches).
                    scenario: "three Pci nodes",
                    input: "PciRoot(0x0)/Pci(0x1,0x0)/Pci(0x0,0x0)/Pci(0x0,0x0)",
                    expect: Yields("0.1.0.0.0.0.0".to_string()),
                },
                Case {
                    scenario: "vendor and memory-mapped prefix",
                    input: "VenHw(1E5A432C-0466-4D31-B009-D4D9239271D3)/MemoryMapped(0xB,0x14140000,0x14141FFF)/PciRoot(0x16)/Pci(0x0,0x0)/Pci(0x0,0x0)",
                    expect: Yields("22.0.0.0.0".to_string()),
                },
                Case {
                    // PciRoot without any Pci node should fail.
                    scenario: "PciRoot without any Pci node",
                    input: "PciRoot(0x7)/MAC(525400A8282F,0x1)",
                    expect: Fails,
                },
                Case {
                    scenario: "embedded hexadecimal prefix",
                    input: "PciRoot(0x10x2)/Pci(0x0,0x0)",
                    expect: Fails,
                },
            ],
            // The error type is String, but the failing row only asserts that it
            // errors, so discard it; yield the dotted address on success.
            |path| UefiDevicePath::from_str(path).map(|u| u.0).map_err(drop),
        );

        let malformed = "PciRoot(0x7)/Pci(not-hex,0x0)";
        assert!(
            UefiDevicePath::from_str(malformed)
                .unwrap_err()
                .contains(malformed)
        );
    }

    #[test]
    fn uefi_device_path_json_remains_a_normalized_string() {
        let path = UefiDevicePath::from_str(
            "PciRoot(0x11)/Pci(0x1,0x0)/Pci(0x0,0xa)/MAC(A088C20C87C6,0x1)",
        )
        .unwrap();

        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(json, r#""17.1.0.0.10""#);
        assert_eq!(serde_json::from_str::<UefiDevicePath>(&json).unwrap(), path);
    }

    #[test]
    fn test_parse_position_info_first_wins() {
        // Test that parse_position_info uses "first wins" strategy
        let mut report = EndpointExplorationReport {
            chassis: vec![
                Chassis {
                    id: "chassis_0".to_string(),
                    physical_slot_number: Some(1),
                    compute_tray_index: None,
                    topology_id: Some(10),
                    revision_id: None,
                    ..Default::default()
                },
                Chassis {
                    id: "chassis_1".to_string(),
                    physical_slot_number: Some(2), // should be ignored (first wins)
                    compute_tray_index: Some(5),
                    topology_id: Some(20), // should be ignored (first wins)
                    revision_id: Some(3),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        report.parse_position_info();

        // First chassis has physical_slot_number=1, so we get 1 (not 2)
        assert_eq!(report.physical_slot_number, Some(1));
        // First chassis has no compute_tray_index, second has 5, so we get 5
        assert_eq!(report.compute_tray_index, Some(5));
        // First chassis has topology_id=10, so we get 10 (not 20)
        assert_eq!(report.topology_id, Some(10));
        // First chassis has no revision_id, second has 3, so we get 3
        assert_eq!(report.revision_id, Some(3));
    }

    #[test]
    fn test_parse_position_info_all_none() {
        // Test when no chassis has position info
        let mut report = EndpointExplorationReport {
            chassis: vec![Chassis {
                id: "chassis_0".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        report.parse_position_info();

        assert_eq!(report.physical_slot_number, None);
        assert_eq!(report.compute_tray_index, None);
        assert_eq!(report.topology_id, None);
        assert_eq!(report.revision_id, None);
    }

    #[test]
    fn test_parse_position_info_empty_chassis() {
        // Test when there are no chassis entries
        let mut report = EndpointExplorationReport {
            chassis: vec![],
            ..Default::default()
        };

        report.parse_position_info();

        assert_eq!(report.physical_slot_number, None);
        assert_eq!(report.compute_tray_index, None);
        assert_eq!(report.topology_id, None);
        assert_eq!(report.revision_id, None);
    }

    // is_power_shelf identifies a power shelf either by a chassis id containing
    // "powershelf" (manufacturer irrelevant) or by the generic "chassis" id paired
    // with a Lite-On or Delta manufacturer. Any other id/manufacturer pairing is
    // not a power shelf. Each row supplies a single chassis's id + manufacturer.
    #[test]
    fn is_power_shelf_by_chassis_id_or_manufacturer() {
        struct ChassisInput {
            id: &'static str,
            manufacturer: Option<&'static str>,
        }
        value_scenarios!(
            run = |ChassisInput { id, manufacturer }| {
                EndpointExplorationReport {
                    chassis: vec![Chassis {
                        id: id.to_string(),
                        manufacturer: manufacturer.map(str::to_string),
                        ..Default::default()
                    }],
                    ..Default::default()
                }
                .is_power_shelf()
            };
            "powershelf chassis id (manufacturer irrelevant)" {
                ChassisInput {
                    id: "powershelf",
                    manufacturer: Some("doesnt-matter-in-this-case"),
                } => true,
            }

            "generic chassis id + Lite-On manufacturer" {
                ChassisInput {
                    id: "chassis",
                    manufacturer: Some("LITE-ON TECHNOLOGY CORP."),
                } => true,
            }

            "generic chassis id + Delta manufacturer" {
                ChassisInput {
                    id: "chassis",
                    manufacturer: Some("DELTA"),
                } => true,
            }

            "generic chassis id + other manufacturer" {
                ChassisInput {
                    id: "chassis",
                    manufacturer: Some("Dell Inc."),
                } => false,
            }

            "generic chassis id + no manufacturer" {
                ChassisInput {
                    id: "chassis",
                    manufacturer: None,
                } => false,
            }
        );
    }

    /// `find_interface_id_for_mac` returns the Redfish interface id of the host
    /// ethernet interface whose MAC matches, treating a missing or empty id (and
    /// an unknown MAC) as absent so a last-known-good capture is never clobbered.
    #[test]
    fn find_interface_id_for_mac() {
        let mac = MacAddress::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01]);
        let other = MacAddress::new([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

        // Report with two interfaces: `other` then `mac`, both with ids.
        let two_iface_report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![
                    EthernetInterface {
                        id: Some("NIC.Embedded.1".to_string()),
                        mac_address: Some(other),
                        ..Default::default()
                    },
                    EthernetInterface {
                        id: Some("NIC.Slot.7-1-1".to_string()),
                        mac_address: Some(mac),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Single interface carrying `mac` but no usable id.
        let single_iface_report = |id: Option<String>| EndpointExplorationReport {
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![EthernetInterface {
                    id,
                    mac_address: Some(mac),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        scenarios!(
            run = |(report, mac)| {
                report
                    .find_interface_id_for_mac(mac)
                    .map(str::to_string)
                    .ok_or(())
            };
            "matching MAC yields its interface id" {
                (two_iface_report.clone(), mac) => Yields("NIC.Slot.7-1-1".to_string()),
            }

            "unknown MAC -> None (keeps last-known-good record)" {
                (two_iface_report, MacAddress::new([0, 0, 0, 0, 0, 0])) => Fails,
            }

            "MAC present but no interface id -> no complete pair" {
                (single_iface_report(None), mac) => Fails,
            }

            "empty id treated as absent (don't clobber stored boot interface)" {
                (single_iface_report(Some(String::new())), mac) => Fails,
            }
        );
    }

    #[test]
    fn complete_boot_interfaces_yields_every_nic_regardless_of_type() {
        let dpu_mac = MacAddress::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01]);
        let integrated_mac = MacAddress::new([0xD4, 0x04, 0xE6, 0x84, 0x13, 0x98]);
        let id_less_mac = MacAddress::new([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let empty_id_mac = MacAddress::new([0x22, 0x33, 0x44, 0x55, 0x66, 0x77]);
        let report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![
                    // A DPU host-PF -- the only kind the DPU-only capture reached...
                    EthernetInterface {
                        id: Some("NIC.Slot.7-1-1".to_string()),
                        mac_address: Some(dpu_mac),
                        ..Default::default()
                    },
                    // ...and a non-DPU integrated NIC, which is now yielded too.
                    EthernetInterface {
                        id: Some("NIC.Embedded.1-1-1".to_string()),
                        mac_address: Some(integrated_mac),
                        ..Default::default()
                    },
                    // No id -> can't form a pair, skipped.
                    EthernetInterface {
                        id: None,
                        mac_address: Some(id_less_mac),
                        ..Default::default()
                    },
                    // No MAC -> nothing to key on, skipped.
                    EthernetInterface {
                        id: Some("NIC.Embedded.2-1-1".to_string()),
                        mac_address: None,
                        ..Default::default()
                    },
                    // Empty id -> not a usable id, skipped (don't clobber last-known-good).
                    EthernetInterface {
                        id: Some(String::new()),
                        mac_address: Some(empty_id_mac),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let boot_interfaces: Vec<MachineBootInterface> =
            report.complete_boot_interfaces().collect();
        assert_eq!(
            boot_interfaces,
            vec![
                MachineBootInterface {
                    mac_address: dpu_mac,
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                },
                MachineBootInterface {
                    mac_address: integrated_mac,
                    interface_id: "NIC.Embedded.1-1-1".to_string(),
                },
            ],
            "complete_boot_interfaces should yield a MachineBootInterface for every NIC with both a MAC and a non-empty id -- DPU or not -- and skip the rest",
        );
    }

    /// A `ComputerSystem` deserializes regardless of the `BaseMac` field: a valid
    /// value parses through, while an invalid, null, or missing one becomes `None`.
    /// Each row projects to the resulting `base_mac`.
    #[test]
    fn test_computer_system_base_mac_deserialization() {
        scenarios!(
            // Deserialize and project to base_mac; every row is expected to
            // deserialize, so the (non-PartialEq) serde error is discarded.
            run = |json| {
                serde_json::from_value::<ComputerSystem>(json)
                    .map(|system| system.base_mac)
                    .map_err(drop)
            };
            "invalid BaseMac -> None" {
                serde_json::json!({
                    "EthernetInterfaces": [],
                    "Id": "Bluefield",
                    "Manufacturer": "Nvidia",
                    "Model": "Bluefield-3 DPU",
                    "SerialNumber": "ABC1234",
                    "Attributes": {},
                    "PcieDevices": [],
                    "BaseMac": "pe:",
                    "PowerState": "On"
                }) => Yields(None),
            }

            "valid BaseMac parses through" {
                serde_json::json!({
                    "EthernetInterfaces": [],
                    "Id": "Bluefield",
                    "Manufacturer": "Nvidia",
                    "Model": "Bluefield-3 DPU",
                    "SerialNumber": "ABC1234",
                    "Attributes": {},
                    "PcieDevices": [],
                    "BaseMac": "A088C208804C",
                    "PowerState": "On"
                }) => Yields(Some("A088C208804C".parse().unwrap())),
            }

            "null BaseMac -> None" {
                serde_json::json!({
                    "EthernetInterfaces": [],
                    "Id": "Bluefield",
                    "Manufacturer": "Nvidia",
                    "Model": "Bluefield-3 DPU",
                    "SerialNumber": "ABC1234",
                    "Attributes": {},
                    "PcieDevices": [],
                    "BaseMac": null,
                    "PowerState": "On"
                }) => Yields(None),
            }

            "missing BaseMac -> None" {
                serde_json::json!({
                    "EthernetInterfaces": [],
                    "Id": "Bluefield",
                    "Manufacturer": "Nvidia",
                    "Model": "Bluefield-3 DPU",
                    "SerialNumber": "ABC1234",
                    "Attributes": {},
                    "PcieDevices": [],
                    "PowerState": "On"
                }) => Yields(None),
            }
        );
    }
}
