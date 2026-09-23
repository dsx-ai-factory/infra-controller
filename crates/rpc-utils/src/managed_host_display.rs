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

// The deprecated flat fields on `rpc::forge::Machine` must still be read here for
// backwards-compat until a follow-up PR migrates this crate to the new config/status sub-messages.
#![allow(deprecated)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::sync::Arc;
use std::time::SystemTime;

use byte_unit::UnitType;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::DpuMachineId;
use chrono::{DateTime, Utc};
use itertools::Itertools;
use rpc::common::DpuMachineIdList;
// use rpc::forge::forge_server::Forge;
use rpc::forge::{
    BmcInfo, ConnectedDevice, GetSiteExplorationRequest, MachineType, ManagedHostQuarantineState,
    NetworkDevice, NetworkDeviceIdList,
};
use rpc::machine_discovery::MemoryDeviceGroup;
use rpc::site_explorer::{EndpointExplorationReport, ExploredEndpoint};
use rpc::{DiscoveryInfo, DmiData, DynForge, Machine, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// This represents all the in-memory data needed to render information about a group of managed
/// hosts. This information is expected to be obtained via the API via individual calls, and
/// aggregated here for viewing.
#[derive(Debug)]
pub struct ManagedHostMetadata {
    /// All the machines involved in displaying this output, including hosts and DPUs
    pub machines: Vec<Machine>,
    /// Connected devices (switch connections) of the machines, for showing connection info for the
    /// machines' ports
    pub connected_devices: Vec<ConnectedDevice>,
    /// Network devices (the switches themselves) corresponding to connected_devices, for showing
    /// the actual switch name/description.
    pub network_devices: Vec<NetworkDevice>,
    /// Exploration reports for each endpoint, for showing Redfish data associated with a machine
    pub exploration_reports: Vec<ExploredEndpoint>,
}

impl ManagedHostMetadata {
    /// Given a set of machines to display as managed hosts, hydrate a ManagedHostMetadata struct
    /// via information from the API.
    pub async fn lookup_from_api(
        machines: Vec<Machine>,
        api: Arc<DynForge>,
    ) -> ManagedHostMetadata {
        let request = tonic::Request::new(GetSiteExplorationRequest {});

        let site_exploration_report = api
            .get_site_exploration_report(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|e| {
                warn!(error = ?e, "failed to get site exploration report");
            })
            .unwrap_or_default();

        // Find connected devices for this machines
        let dpu_id_request = tonic::Request::new(DpuMachineIdList {
            machine_ids: machines
                .iter()
                .flat_map(|m| m.associated_dpu_machine_ids.clone())
                .collect(),
        });
        let connected_devices = api
            .find_connected_devices_by_dpu_machine_ids(dpu_id_request)
            .await
            .map(|response| response.into_inner().connected_devices)
            .unwrap_or_default();

        let network_device_ids = connected_devices
            .iter()
            .filter_map(|d| d.network_device_id.clone())
            .unique()
            .collect();

        let exploration_reports = site_exploration_report.endpoints;

        let network_devices = api
            .find_network_devices_by_device_ids(tonic::Request::new(NetworkDeviceIdList {
                network_device_ids,
            }))
            .await
            .map_or_else(
                |_err| vec![],
                |response| response.into_inner().network_devices,
            );

        ManagedHostMetadata {
            machines,
            connected_devices,
            network_devices,
            exploration_reports,
        }
    }
}

#[derive(Default, Serialize, Deserialize, PartialEq)]
pub struct ManagedHostOutput {
    // discovery_info is expected to be part of managed-host.json, but is otherwise unused
    pub discovery_info: DiscoveryInfo,
    pub hostname: Option<String>,
    pub machine_id: Option<String>,
    pub state: String,
    pub state_version: String,
    pub state_sla_duration: Option<String>,
    pub time_in_state_above_sla: bool,
    pub time_in_state: String,
    pub state_reason: String,
    pub host_serial_number: Option<String>,
    pub host_bios_version: Option<String>,
    pub host_bmc_ip: Option<String>,
    pub host_bmc_mac: Option<String>,
    pub host_bmc_version: Option<String>,
    pub host_bmc_firmware_version: Option<String>,
    pub host_admin_ip: Option<String>,
    pub host_admin_mac: Option<String>,
    pub host_ib_ifs_count: usize,
    pub host_gpu_count: usize,
    pub host_memory: Option<String>,
    pub maintenance_reference: Option<String>,
    pub maintenance_start_time: Option<String>,
    pub host_last_reboot_time: Option<String>,
    pub host_last_reboot_requested_time_and_mode: Option<String>,
    pub health: health_report::HealthReport,
    pub health_sources: Vec<String>,
    pub dpus: Vec<ManagedHostAttachedDpu>,
    pub exploration_report: Option<EndpointExplorationReport>,
    pub failure_details: Option<String>,
    pub quarantine_state: Option<ManagedHostQuarantineState>,
    pub instance_type_id: Option<String>,
    pub slot_number: Option<i32>,
    pub tray_index: Option<i32>,
    pub rack_id: Option<String>,
    pub dpf: Option<rpc::forge::DpfMachineState>,
}

impl From<Machine> for ManagedHostOutput {
    fn from(machine: Machine) -> ManagedHostOutput {
        let primary_interface = machine.interfaces.iter().find(|x| x.primary_interface);
        let (host_admin_ip, host_admin_mac) = primary_interface
            .map(|x| {
                (
                    x.address.join(",").none_if_empty(),
                    Some(x.mac_address.clone()),
                )
            })
            .unwrap_or((None, None));

        let BmcInfoDisplay {
            ip: host_bmc_ip,
            mac: host_bmc_mac,
            version: host_bmc_version,
            firmware_version: host_bmc_firmware_version,
        } = machine.bmc_info.into();

        let discovery_info = machine.discovery_info;
        let host_gpu_count = discovery_info
            .as_ref()
            .map(|di| di.gpus.len())
            .unwrap_or_default();
        let host_ib_ifs_count = discovery_info
            .as_ref()
            .map(|di| di.infiniband_interfaces.len())
            .unwrap_or_default();
        let host_memory = discovery_info.as_ref().and_then(|di| {
            if di.memory_groups_are_authoritative() {
                let valid: Vec<_> = di
                    .memory_device_groups
                    .iter()
                    .cloned()
                    .filter_map(|g| g.nonzero())
                    .collect();
                get_memory_details(&valid)
            } else {
                #[allow(deprecated)]
                let groups: Vec<MemoryDeviceGroup> = di
                    .memory_devices
                    .iter()
                    .map(|md| MemoryDeviceGroup {
                        size_mb: md.size_mb,
                        mem_type: md.mem_type.clone(),
                        count: 1,
                    })
                    .collect();
                get_memory_details(&groups)
            }
        });

        let DmiDataDisplay {
            product_serial: _,
            chassis_serial: host_serial_number,
            bios_version: host_bios_version,
        } = discovery_info
            .as_ref()
            .and_then(|di| di.dmi_data.as_ref())
            .into();

        let health = machine
            .health
            .map(|h| {
                health_report::HealthReport::try_from(h)
                    .unwrap_or_else(health_report::HealthReport::malformed_report)
            })
            .unwrap_or_else(health_report::HealthReport::missing_report);
        let health_sources = machine
            .health_sources
            .into_iter()
            .map(|o| o.source)
            .collect();

        // If the rack ID is empty or "Unknown", set it to "N/A"
        let rack_id = match machine.rack_id.as_ref() {
            Some(id) => {
                if id.as_str().is_empty() || id.as_str() == "Unknown" {
                    Some("N/A".to_string())
                } else {
                    Some(id.to_string())
                }
            }
            None => Some("N/A".to_string()),
        };

        ManagedHostOutput {
            discovery_info: discovery_info.unwrap_or_default(),
            hostname: primary_interface
                .as_ref()
                .map(|i| i.hostname.clone())
                .filter(|h| !h.trim().is_empty()),
            machine_id: machine.id.as_ref().map(|i| i.to_string()),
            state: machine.state.clone(),
            time_in_state: config_version::since_state_change_humanized(&machine.state_version),
            time_in_state_above_sla: machine
                .state_sla
                .as_ref()
                .map(|sla| sla.time_in_state_above_sla)
                .unwrap_or_default(),
            state_sla_duration: machine
                .state_sla
                .as_ref()
                .and_then(|sla| sla.sla)
                .map(|sla| {
                    config_version::format_duration(
                        chrono::TimeDelta::try_from(sla).unwrap_or(chrono::TimeDelta::MAX),
                    )
                }),
            state_version: machine.state_version,
            state_reason: machine
                .state_reason
                .as_ref()
                .and_then(super::reason_to_user_string)
                .unwrap_or_default(),
            host_serial_number,
            host_bios_version,
            host_bmc_ip,
            host_bmc_mac,
            host_bmc_version,
            host_bmc_firmware_version,
            host_admin_ip,
            host_admin_mac,
            host_gpu_count,
            host_ib_ifs_count,
            host_memory,
            failure_details: machine.failure_details.clone(),
            maintenance_reference: machine.maintenance_reference.clone(),
            maintenance_start_time: to_time(machine.maintenance_start_time, machine.id),
            host_last_reboot_time: machine
                .id
                .as_ref()
                .and_then(|id| to_time(machine.last_reboot_time, Some(id))),
            host_last_reboot_requested_time_and_mode: machine.id.as_ref().map(|id| {
                format!(
                    "{}/{}",
                    to_time(machine.last_reboot_requested_time, Some(id))
                        .unwrap_or("Unknown".to_string()),
                    machine.last_reboot_requested_mode.unwrap_or_default()
                )
            }),
            quarantine_state: machine.quarantine_state.clone(),
            instance_type_id: machine.instance_type_id.clone(),
            slot_number: machine.placement_in_rack.and_then(|p| p.slot_number),
            tray_index: machine.placement_in_rack.and_then(|p| p.tray_index),
            rack_id,
            health,
            health_sources,
            dpf: machine.dpf,
            // dpus and exploration_report are filled in later
            dpus: Default::default(),
            exploration_report: Default::default(),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ManagedHostAttachedDpu {
    pub discovery_info: DiscoveryInfo,
    pub machine_id: Option<String>,
    pub state: Option<String>,
    pub serial_number: Option<String>,
    pub bios_version: Option<String>,
    pub bmc_ip: Option<String>,
    pub bmc_mac: Option<String>,
    pub bmc_version: Option<String>,
    pub bmc_firmware_version: Option<String>,
    pub oob_ip: Option<String>,
    pub oob_mac: Option<String>,
    pub last_reboot_time: Option<String>,
    pub last_reboot_requested_time_and_mode: Option<String>,
    pub last_observation_time: Option<String>,
    pub switch_connections: Vec<DpuSwitchConnection>,
    pub is_primary: bool,
    pub health: health_report::HealthReport,
    pub exploration_report: Option<EndpointExplorationReport>,
    pub failure_details: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DpuSwitchConnection {
    pub dpu_port: Option<String>,
    pub switch_id: Option<String>,
    pub switch_port: Option<String>,
    pub switch_name: Option<String>,
    pub switch_description: Option<String>,
}

impl DpuSwitchConnection {
    fn from(connected_device: ConnectedDevice, network_device: Option<&NetworkDevice>) -> Self {
        Self {
            dpu_port: Some(connected_device.local_port),
            switch_id: connected_device.network_device_id,
            switch_port: Some(connected_device.remote_port),
            switch_name: network_device.map(|n| n.name.clone()),
            switch_description: network_device.and_then(|n| n.description.clone()),
        }
    }
}

impl ManagedHostAttachedDpu {
    pub fn new_from_dpu_machine(
        dpu_machine: Machine,
        switch_connections: Vec<DpuSwitchConnection>,
        exploration_report: Option<EndpointExplorationReport>,
        is_primary: bool,
    ) -> Self {
        let last_reboot_requested_time_and_mode = Some(format!(
            "{}/{}",
            to_time(dpu_machine.last_reboot_requested_time, dpu_machine.id)
                .unwrap_or("Unknown".to_string()),
            dpu_machine.last_reboot_requested_mode()
        ));

        let (oob_ip, oob_mac) = match dpu_machine
            .interfaces
            .into_iter()
            .find(|x| x.primary_interface)
        {
            Some(primary_interface) => (
                Some(primary_interface.address.join(",")),
                Some(primary_interface.mac_address.to_owned()),
            ),
            None => (None, None),
        };

        let BmcInfoDisplay {
            ip: bmc_ip,
            mac: bmc_mac,
            version: bmc_version,
            firmware_version: bmc_firmware_version,
        } = dpu_machine.bmc_info.into();

        let DmiDataDisplay {
            product_serial: serial_number,
            chassis_serial: _,
            bios_version,
        } = dpu_machine
            .discovery_info
            .as_ref()
            .and_then(|d| d.dmi_data.as_ref())
            .into();

        ManagedHostAttachedDpu {
            discovery_info: dpu_machine.discovery_info.unwrap_or_default(),
            machine_id: dpu_machine.id.map(|i| i.to_string()),
            state: Some(dpu_machine.state),
            serial_number,
            bios_version,
            bmc_ip,
            bmc_mac,
            bmc_version,
            bmc_firmware_version,
            last_reboot_time: to_time(dpu_machine.last_reboot_time, dpu_machine.id),
            exploration_report,
            last_reboot_requested_time_and_mode,
            last_observation_time: to_time(dpu_machine.last_observation_time, dpu_machine.id),
            oob_ip,
            oob_mac,
            switch_connections,
            is_primary,
            health: dpu_machine
                .health
                .map(|h| {
                    health_report::HealthReport::try_from(h)
                        .unwrap_or_else(health_report::HealthReport::malformed_report)
                })
                .unwrap_or_else(health_report::HealthReport::missing_report),
            failure_details: dpu_machine.failure_details,
        }
    }
}

pub fn get_managed_host_output(source: ManagedHostMetadata) -> Vec<ManagedHostOutput> {
    let mut index = IndexedManagedHostMetadata::from(source);
    index
        .managed_hosts
        .into_iter()
        .map(|machine| {
            let primary_dpu_id = machine.interfaces.iter().find_map(|iface| {
                if iface.primary_interface {
                    iface.attached_dpu_machine_id
                } else {
                    None
                }
            });
            let dpu_machine_ids = machine.associated_dpu_machine_ids.clone();

            let mut managed_host_output = ManagedHostOutput::from(machine);
            managed_host_output.exploration_report = managed_host_output
                .host_bmc_ip
                .as_ref()
                .and_then(|address| index.exploration_reports_by_address.remove(address));

            managed_host_output.dpus = dpu_machine_ids
                .into_iter()
                .filter_map(|id| {
                    index
                        .dpus_by_id
                        .remove(&id)
                        .map(|dpu| (id, dpu))
                        .or_else(|| {
                            tracing::warn!(
                                dpu_machine_id = %id,
                                "could not find associated DPU"
                            );
                            None
                        })
                })
                .map(|(dpu_id, dpu)| {
                    let dpu_exploration_report = dpu
                        .bmc_info
                        .as_ref()
                        .and_then(|bmc_info| bmc_info.ip.as_ref())
                        .and_then(|ip| index.exploration_reports_by_address.remove(ip));

                    let switch_connections = index
                        .switch_connections_by_dpu_id
                        .remove(&dpu_id)
                        .unwrap_or_default();

                    ManagedHostAttachedDpu::new_from_dpu_machine(
                        dpu,
                        switch_connections,
                        dpu_exploration_report,
                        Some(dpu_id) == primary_dpu_id,
                    )
                })
                .collect();
            managed_host_output
        })
        .collect()
}

/// Data from ManagedHostMetadata, indexed into HashMaps for easy conversion into ManagedHostOutput
struct IndexedManagedHostMetadata {
    /// The managed hosts (non-dpu)
    managed_hosts: Vec<Machine>,
    /// DPU's indexed by their ID
    dpus_by_id: HashMap<DpuMachineId, Machine>,
    /// Switch connections for each DPU, indexed by the DPU MachineId
    switch_connections_by_dpu_id: HashMap<DpuMachineId, Vec<DpuSwitchConnection>>,
    /// Exploration reports, indexed by the BMC address
    exploration_reports_by_address: HashMap<String, EndpointExplorationReport>,
}

impl From<ManagedHostMetadata> for IndexedManagedHostMetadata {
    fn from(value: ManagedHostMetadata) -> Self {
        let network_devices_by_id: HashMap<String, NetworkDevice> = value
            .network_devices
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let switch_connections_by_dpu_id: HashMap<DpuMachineId, Vec<DpuSwitchConnection>> = value
            .connected_devices
            .into_iter()
            .filter_map(|cd| {
                let dpu_id = cd.id.and_then(|id| DpuMachineId::try_from(id).ok())?;
                let network_device = cd
                    .network_device_id
                    .as_ref()
                    .and_then(|nd_id| network_devices_by_id.get(nd_id));
                Some((dpu_id, DpuSwitchConnection::from(cd, network_device)))
            })
            .into_group_map();

        let (dpus, non_dpus): (Vec<Machine>, Vec<Machine>) = value
            .machines
            .into_iter()
            .partition(|m| m.machine_type() == MachineType::Dpu);

        let managed_hosts = non_dpus
            .into_iter()
            .filter(|m| m.machine_type() == MachineType::Host)
            .collect();

        let dpus_by_id: HashMap<DpuMachineId, Machine> = dpus
            .into_iter()
            .filter_map(|m| {
                m.id.and_then(|id| DpuMachineId::try_from(id).ok())
                    .map(|i| (i, m))
            })
            .collect();

        let exploration_reports_by_address: HashMap<String, EndpointExplorationReport> = value
            .exploration_reports
            .into_iter()
            .filter_map(|r| Some((r.address, r.report?)))
            .collect();

        Self {
            managed_hosts,
            dpus_by_id,
            switch_connections_by_dpu_id,
            exploration_reports_by_address,
        }
    }
}

pub fn get_memory_details(memory_device_groups: &[MemoryDeviceGroup]) -> Option<String> {
    let mut breakdown = BTreeMap::default();
    let mut total_size = 0u64;
    let mut total_count = 0u32;
    for group in memory_device_groups {
        if group.count == 0 {
            continue;
        }
        let size = byte_unit::Byte::from_f64_with_unit(
            group.size_mb.unwrap_or(0) as f64,
            byte_unit::Unit::MiB,
        )
        .unwrap_or_default();
        let group_size = size.as_u64().checked_mul(group.count as u64)?;
        total_size = total_size.checked_add(group_size)?;
        total_count = total_count.checked_add(group.count)?;
        *breakdown.entry(size).or_insert(0u32) += group.count;
    }

    let total_size = byte_unit::Byte::from(total_size);

    if total_count == 1 {
        Some(
            total_size
                .get_appropriate_unit(UnitType::Binary)
                .to_string(),
        )
    } else if total_size.as_u64() > 0 {
        let mut breakdown_str = String::default();
        for (ind, s) in breakdown.iter().enumerate() {
            if ind != 0 {
                breakdown_str.push_str(", ");
            }
            breakdown_str.push_str(
                format!("{}x{}", s.0.get_appropriate_unit(UnitType::Binary), s.1).as_ref(),
            );
        }
        Some(format!(
            "{} ({})",
            total_size.get_appropriate_unit(UnitType::Binary),
            breakdown_str
        ))
    } else {
        None
    }
}

// Prepare an Option<rpc::Timestamp> for display:
// - Parse the timestamp into a chrono::Time and format as string.
// - Or return empty string
// machine_id is only for logging a more useful error.
pub fn to_time<M: Display>(t: Option<Timestamp>, machine_id: Option<M>) -> Option<String> {
    match t {
        None => None,
        Some(tt) => match SystemTime::try_from(tt) {
            Ok(system_time) => {
                let dt: DateTime<Utc> = DateTime::from(system_time);
                Some(dt.to_string())
            }
            Err(err) => {
                warn!(
                    machine_id = %machine_id
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "(no machine ID)".to_string()),
                    error = %err,
                    "managed host has an invalid timestamp"
                );
                None
            }
        },
    }
}

/// Helper to easily turn an Option<BmcInfo> into optional fields for display, without cloning
#[derive(Default)]
struct BmcInfoDisplay {
    ip: Option<String>,
    mac: Option<String>,
    version: Option<String>,
    firmware_version: Option<String>,
}

impl From<Option<BmcInfo>> for BmcInfoDisplay {
    fn from(value: Option<BmcInfo>) -> Self {
        if let Some(bmc_info) = value {
            Self {
                ip: bmc_info.ip.none_if_empty(),
                mac: bmc_info.mac.none_if_empty(),
                version: bmc_info.version.none_if_empty(),
                firmware_version: bmc_info.firmware_version.none_if_empty(),
            }
        } else {
            Self::default()
        }
    }
}

/// Helper to easily turn an Option<DmiData> into optional fields for display, without cloning
#[derive(Default)]
struct DmiDataDisplay {
    product_serial: Option<String>,
    chassis_serial: Option<String>,
    bios_version: Option<String>,
}

impl From<Option<&DmiData>> for DmiDataDisplay {
    fn from(value: Option<&DmiData>) -> Self {
        if let Some(dmi_data) = value {
            Self {
                product_serial: dmi_data.product_serial.clone().none_if_empty(),
                chassis_serial: dmi_data.chassis_serial.clone().none_if_empty(),
                bios_version: dmi_data.bios_version.clone().none_if_empty(),
            }
        } else {
            Self {
                product_serial: None,
                chassis_serial: None,
                bios_version: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use carbide_test_support::value_scenarios;

    use super::*;

    fn memory(size_mb: Option<u32>) -> MemoryDeviceGroup {
        MemoryDeviceGroup {
            size_mb,
            mem_type: None,
            count: 1,
        }
    }

    #[test]
    fn formats_memory_details() {
        value_scenarios!(
            run = |devices: Vec<MemoryDeviceGroup>| get_memory_details(&devices);
            "missing memory" {
                vec![] => None,
                vec![memory(Some(0)), memory(None)] => None,
            }

            "single device" {
                vec![memory(Some(32768))] => Some("32 GiB".to_string()),
            }

            "device breakdown" {
                vec![
                    MemoryDeviceGroup { size_mb: Some(32768), mem_type: None, count: 2 },
                    memory(Some(65536)),
                ] => Some("128 GiB (32 GiBx2, 64 GiBx1)".to_string()),
            }

            "overflowing total size" {
                vec![
                    MemoryDeviceGroup { size_mb: Some(u32::MAX), mem_type: None, count: u32::MAX },
                ] => None,
            }

            "mixed zero and nonzero count groups" {
                vec![
                    MemoryDeviceGroup { size_mb: Some(65536), mem_type: None, count: 0 },
                    MemoryDeviceGroup { size_mb: Some(32768), mem_type: None, count: 2 },
                ] => Some("64 GiB (32 GiBx2)".to_string()),
            }
        );
    }

    #[test]
    fn formats_timestamps_for_display() {
        value_scenarios!(
            run = |timestamp| to_time(timestamp, Some("machine-1"));
            "timestamp values" {
                None => None,
                Some(Timestamp::from(UNIX_EPOCH)) => Some("1970-01-01 00:00:00 UTC".to_string()),
                Some(Timestamp::from(UNIX_EPOCH + Duration::from_secs(1_700_000_000))) => Some("2023-11-14 22:13:20 UTC".to_string()),
            }
        );
    }

    // When every group in `memory_device_groups` has count == 0, the field is
    // logically empty. Per the wire contract, display must fall back to the
    // legacy `memory_devices` field.
    #[test]
    #[allow(deprecated)]
    fn managed_host_output_falls_back_to_legacy_memory_when_all_groups_are_zero_count() {
        let host_memory = ManagedHostOutput::from(Machine {
            discovery_info: Some(DiscoveryInfo {
                memory_device_groups: vec![MemoryDeviceGroup {
                    size_mb: Some(8192),
                    mem_type: Some("DDR4".to_string()),
                    count: 0,
                }],
                memory_devices: vec![
                    rpc::machine_discovery::MemoryDevice {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".to_string()),
                    },
                    rpc::machine_discovery::MemoryDevice {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".to_string()),
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        })
        .host_memory;
        assert_eq!(host_memory, Some("32 GiB (16 GiBx2)".to_string()));
    }

    #[test]
    fn managed_host_output_preserves_all_primary_interface_addresses() {
        value_scenarios!(
            run = |addresses| ManagedHostOutput::from(Machine {
                interfaces: vec![rpc::MachineInterface {
                    primary_interface: true,
                    address: addresses,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .host_admin_ip;
            "primary interface address lists" {
                Vec::<String>::new() => None,
                vec!["192.0.2.10".to_string()] => Some("192.0.2.10".to_string()),
                vec!["192.0.2.10".to_string(), "2001:db8::10".to_string()]
                    => Some("192.0.2.10,2001:db8::10".to_string()),
            }
        );
    }
}
