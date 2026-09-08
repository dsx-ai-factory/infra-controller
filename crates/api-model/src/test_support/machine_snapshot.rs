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

//! In-memory machine-snapshot fixtures sized like a production GPU host.
//!
//! These builders produce a fully-populated [`MachineSnapshotPgJson`] /
//! [`Machine`] / [`ManagedHostStateSnapshot`] (8 GPUs, 9 NICs, IB devices,
//! history, health reports, ...) without a database, so unit tests and the
//! `api-model` / `carbide-rpc` benches all measure the same realistic shape.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use carbide_uuid::machine::{
    DpuMachineId, MachineId, MachineIdSource, MachineInterfaceId, MachineType,
};
use carbide_uuid::network::NetworkSegmentId;
use chrono::{DateTime, TimeZone, Utc};
use config_version::ConfigVersion;
use health_report::HealthReport;

use crate::bmc_info::BmcInfo;
use crate::hardware_info::{
    BlockDevice, CpuInfo, DmiData, Gpu, HardwareInfo, InfinibandInterface, MachineInventory,
    MachineInventorySoftwareComponent, MemoryDeviceGroup, NetworkInterface, NvmeDevice,
    PciDeviceProperties, TpmEkCertificate,
};
use crate::health::HealthReportSources;
use crate::machine::infiniband::{
    MachineIbInterfaceStatusObservation, MachineInfinibandStatusObservation,
};
use crate::machine::json::MachineSnapshotPgJson;
use crate::machine::network::{MachineNetworkStatusObservation, ManagedHostNetworkConfig};
use crate::machine::topology::{DiscoveryData, MachineTopology, TopologyData};
use crate::machine::{
    CURRENT_STATE_MODEL_VERSION, Dpf, FailureCause, FailureDetails, FailureSource, HostProfile,
    Machine, MachineInterfaceSnapshot, MachineLastRebootRequested, MachineLastRebootRequestedMode,
    ManagedHostState, ManagedHostStateSnapshot, UpgradeDecision,
};
use crate::machine_interface::InterfaceType;
use crate::network_segment::NetworkSegmentType;
use crate::site_explorer::BlueFieldOperatingMode;
use crate::state_history::StateHistoryRecord;
use crate::test_support::dpu::DPU_BF3_INFO_JSON;
use crate::test_support::{DpuConfig, HardwareInfoTemplate};

/// Deterministic machine id for the fixture host.
pub fn host_machine_id() -> MachineId {
    MachineId::new(MachineIdSource::Tpm, [0x11; 32], MachineType::Host)
}

/// Deterministic machine ids for the fixture host's DPUs.
pub fn dpu_machine_id(index: u8) -> DpuMachineId {
    // Widen before adding: `0x20 + index` on a u8 overflows past index 223,
    // the same bound `fixture_dpu_index` documents. Fail with a clear message
    // instead of an overflow panic.
    let hash_byte = 0x20u16 + u16::from(index);
    assert!(
        hash_byte <= u16::from(u8::MAX),
        "dpu_machine_id supports indexes 0..=223 (index {index} overflows the id byte)"
    );
    MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [hash_byte as u8; 32],
        MachineType::Dpu,
    )
    .try_into()
    .unwrap()
}

/// Recovers the index a [`dpu_machine_id`] was created from, so id-keyed
/// builders can give each fixture DPU its own hardware identity. `None` for
/// ids that no fixture index produces.
fn fixture_dpu_index(machine_id: DpuMachineId) -> Option<u8> {
    // `dpu_machine_id` fills the id bytes with `0x20 + index`, so indexes
    // beyond `u8::MAX - 0x20` are not constructible.
    (0..=u8::MAX - 0x20).find(|&index| dpu_machine_id(index) == machine_id)
}

fn fixture_time(offset_secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_750_000_000 + offset_secs, 0).unwrap()
}

fn fixture_mac(index: u8) -> mac_address::MacAddress {
    mac_address::MacAddress::new([0x0a, 0x00, 0x00, 0x00, 0x01, index])
}

/// A deterministic [`ConfigVersion`]: built from its string form because
/// `ConfigVersion::new()` records `now()`, which makes two calls unequal.
pub fn config_version(nr: u64) -> ConfigVersion {
    // `V<nr>-T<micros>` is the persisted `version_string()` format.
    format!("V{nr}-T{}", fixture_time(nr as i64).timestamp_micros())
        .parse()
        .expect("fixture config version string is valid")
}

fn health_report_with_source(source: &str) -> HealthReport {
    let mut report = HealthReport::empty(source.to_string());
    report.observed_at = Some(fixture_time(500));
    report
}

/// Hardware info shaped like an 8-GPU compute host: 9 NICs (2 of them
/// DPU-backed), 6 IB devices, 8 memory DIMMs, NVMe + block storage, DMI data.
pub fn host_hardware_info() -> HardwareInfo {
    let network_interfaces = (0..9)
        .map(|i| NetworkInterface {
            mac_address: fixture_mac(0x10 + i),
            pci_properties: Some(PciDeviceProperties {
                vendor: "0x15b3".to_string(),
                device: format!("0xa2d{i}"),
                path: format!("0000:0{i}:00.0"),
                numa_node: i32::from(i % 2),
                description: Some(if i < 2 {
                    "MT43244 BlueField-3 integrated ConnectX-7 network controller".to_string()
                } else {
                    "MT2910 Family [ConnectX-7]".to_string()
                }),
                slot: Some(format!("{i}")),
            }),
        })
        .collect();

    let infiniband_interfaces = (0..6)
        .map(|i: i32| InfinibandInterface {
            guid: format!("0xb8cef603004f00{i:02}"),
            pci_properties: Some(PciDeviceProperties {
                vendor: "0x15b3".to_string(),
                device: "0x1021".to_string(),
                path: format!("0000:1{i}:00.0"),
                numa_node: i % 2,
                description: Some("MT2910 Family [ConnectX-7]".to_string()),
                slot: Some(format!("ib{i}")),
            }),
        })
        .collect();

    HardwareInfo {
        network_interfaces,
        infiniband_interfaces,
        cpu_info: vec![
            CpuInfo {
                model: "Intel(R) Xeon(R) Platinum 8480C".to_string(),
                vendor: "GenuineIntel".to_string(),
                sockets: 2,
                cores: 56,
                threads: 112,
            };
            2
        ],
        block_devices: (1..=2)
            .map(|i| BlockDevice {
                model: "MZILT3T8HBLS/007".to_string(),
                revision: "GXA0".to_string(),
                serial: format!("S5G0NC0R80000{i}"),
                device_type: "disk".to_string(),
            })
            .collect(),
        machine_type: carbide_utils::arch::CpuArchitecture::X86_64,
        nvme_devices: (0..4)
            .map(|i| NvmeDevice {
                model: "Dell Ent NVMe CM6 RI 3.84TB".to_string(),
                firmware_rev: "2.2.0".to_string(),
                serial: format!("Y2Q0A05DT2Q{i}"),
                size_mb: None,
                pci_path: None,
            })
            .collect(),
        dmi_data: Some(DmiData {
            board_name: "0WXKrJ".to_string(),
            board_version: "A02".to_string(),
            bios_version: "1.13.2".to_string(),
            bios_date: "12/19/2025".to_string(),
            product_serial: "BENCH001".to_string(),
            board_serial: ".BENCH001.".to_string(),
            chassis_serial: "BENCH001C".to_string(),
            product_name: "PowerEdge XE9680".to_string(),
            ..Default::default()
        }),
        tpm_ek_certificate: Some(TpmEkCertificate::from(vec![0xa5; 512])),
        dpu_info: None,
        gpus: (0..8)
            .map(|i| Gpu {
                name: "NVIDIA H100 80GB HBM3".to_string(),
                serial: format!("165223018060{i}"),
                driver_version: "550.54.15".to_string(),
                vbios_version: "96.00.74.00.01".to_string(),
                inforom_version: "G520.0200.00.05".to_string(),
                total_memory: "81559 MiB".to_string(),
                frequency: "1980 MHz".to_string(),
                pci_bus_id: format!("00000000:1{i}:00.0"),
                platform_info: None,
            })
            .collect(),
        memory_devices: vec![MemoryDeviceGroup {
            size_mb: Some(65536),
            mem_type: Some("DDR5".to_string()),
            count: 8,
        }],
        tpm_description: None,
    }
}

/// Hardware info for a BlueField-3 DPU, derived from the shared BF3
/// exploration template via [`DpuConfig`]. `index` selects a per-DPU
/// identity: the product serial and factory MAC differ per index.
pub fn dpu_hardware_info(index: u8) -> HardwareInfo {
    // Each DPU takes a 0x10-wide MAC block starting at 0x40, and the block
    // must fit in the MAC's final byte. Widen before multiplying (u8 math
    // would overflow past index 15) and fail with a clear message instead.
    let mac_base = 0x40u16 + u16::from(index) * 0x10;
    assert!(
        mac_base + 2 <= u16::from(u8::MAX),
        "dpu_hardware_info supports DPU indexes 0..=11 (index {index} overflows the MAC byte)"
    );
    let config = DpuConfig {
        serial: format!("MT2318X0042{index}"),
        host_mac_address: fixture_mac(mac_base as u8),
        oob_mac_address: fixture_mac(mac_base as u8 + 1),
        bmc_mac_address: fixture_mac(mac_base as u8 + 2),
        bmc_firmware_version: "BF-24.10".to_string(),
        last_exploration_error: None,
        override_hosts_uefi_device_path: None,
        hardware_info_template: HardwareInfoTemplate::Custom(DPU_BF3_INFO_JSON),
        nic_mode: Some(BlueFieldOperatingMode::Dpu),
    };
    HardwareInfo::from(&config)
}

fn interface(
    index: u8,
    machine_id: MachineId,
    interface_type: InterfaceType,
    primary: bool,
    attached_dpu: Option<carbide_uuid::machine::DpuMachineId>,
    segment_type: Option<NetworkSegmentType>,
) -> MachineInterfaceSnapshot {
    MachineInterfaceSnapshot {
        id: MachineInterfaceId::from(uuid::Uuid::from_u128(0x1000 + u128::from(index))),
        hostname: format!("bench-host-01-if{index}"),
        interface_type,
        primary_interface: primary,
        mac_address: fixture_mac(0x10 + index),
        boot_interface_id: Some(format!("DEC0DE.Slot.{index}")),
        attached_dpu_machine_id: attached_dpu,
        domain_id: None,
        machine_id: Some(machine_id),
        segment_id: NetworkSegmentId::from(uuid::Uuid::from_u128(0x2000 + u128::from(index % 3))),
        vendors: vec!["Mellanox Technologies".to_string()],
        created: fixture_time(10 + i64::from(index)),
        last_dhcp: Some(fixture_time(1000 + i64::from(index))),
        addresses: vec![IpAddr::from([10, 180, 4, 10 + index])],
        network_segment_type: segment_type,
        power_shelf_id: None,
        switch_id: None,
        association_type: Some(crate::machine_interface_address::InterfaceAssociationType::Machine),
    }
}

/// The host's 9 interfaces: one BMC, one primary boot interface, two
/// DPU-attached ports (matching `host_hardware_info` MACs), two on
/// `HostInband` segments, and regular tenant/data ports for the rest.
fn host_interfaces(machine_id: MachineId) -> Vec<MachineInterfaceSnapshot> {
    (0..9)
        .map(|i| {
            let attached_dpu = match i {
                1 => Some(dpu_machine_id(0)),
                2 => Some(dpu_machine_id(1)),
                _ => None,
            };
            let segment_type = match i {
                0 => None,
                3 | 4 => Some(NetworkSegmentType::HostInband),
                _ => Some(NetworkSegmentType::Tenant),
            };
            interface(
                i,
                machine_id,
                if i == 0 {
                    InterfaceType::Bmc
                } else {
                    InterfaceType::Data
                },
                i == 1,
                attached_dpu,
                segment_type,
            )
        })
        .collect()
}

fn state_history() -> Vec<StateHistoryRecord> {
    ["Created", "DPUInit", "HostInit", "Validation", "Ready"]
        .iter()
        .enumerate()
        .map(|(i, state)| StateHistoryRecord {
            state: (*state).to_string(),
            state_version: config_version(i as u64 + 1),
            time: Some(fixture_time(100 + i as i64)),
        })
        .collect()
}

fn health_reports(agent_source: &str) -> HealthReportSources {
    HealthReportSources {
        replace: None,
        merges: BTreeMap::from([
            (
                agent_source.to_string(),
                health_report_with_source(agent_source),
            ),
            (
                HealthReport::MACHINE_VALIDATION_SOURCE.to_string(),
                health_report_with_source(HealthReport::MACHINE_VALIDATION_SOURCE),
            ),
        ]),
    }
}

/// A fully-populated machine snapshot in its Postgres row-JSON form.
///
/// `machine_type` decides between the host shape (8 GPUs, 9 NICs) and the
/// DPU shape (BlueField hardware info, small interface set).
pub fn machine_snapshot_pg_json(machine_id: MachineId) -> MachineSnapshotPgJson {
    let (hardware_info, is_dpu) = if let Ok(dpu_machine_id) = DpuMachineId::try_from(machine_id) {
        (
            dpu_hardware_info(fixture_dpu_index(dpu_machine_id).unwrap_or(0)),
            true,
        )
    } else {
        (host_hardware_info(), false)
    };
    let interfaces = if is_dpu {
        vec![interface(
            0,
            machine_id,
            InterfaceType::Data,
            true,
            None,
            Some(NetworkSegmentType::Underlay),
        )]
    } else {
        host_interfaces(machine_id)
    };
    let bmc_info = BmcInfo {
        machine_interface_id: Some(MachineInterfaceId::from(uuid::Uuid::from_u128(0x1000))),
        ip: Some(IpAddr::from([10, 180, 0, 9])),
        port: Some(443),
        mac: Some(fixture_mac(0x10)),
        version: Some("iDRAC 7.10.30.00".to_string()),
        firmware_version: Some("BF-24.10".to_string()),
    };

    MachineSnapshotPgJson {
        machine_maintenance_requested: None,
        decommission_requested: false,
        bmc_credential_rotation_requested: false,
        uefi_credential_rotation_requested: false,
        lockdown_ikm_credential_rotation_requested: false,
        id: machine_id,
        rack_id: Some("rack-bench-01".parse().expect("valid rack id")),
        created: fixture_time(0),
        updated: fixture_time(2000),
        deployed: Some(fixture_time(1500)),
        agent_reported_inventory: Some(MachineInventory {
            components: vec![
                MachineInventorySoftwareComponent {
                    name: "doca".to_string(),
                    version: "2.9.1".to_string(),
                    url: "nvcr.io/doca".to_string(),
                },
                MachineInventorySoftwareComponent {
                    name: "dpu-agent".to_string(),
                    version: "1.4.2".to_string(),
                    url: "nvcr.io/agent".to_string(),
                },
            ],
        }),
        network_config_version: config_version(3).version_string(),
        network_config: ManagedHostNetworkConfig {
            loopback_ip: Some(IpAddr::from([172, 20, 0, 42])),
            loopback_ip_v6: None,
            use_admin_network: Some(false),
            quarantine_state: None,
            use_admin_network_changed: None,
        },
        network_status_observation: Some(MachineNetworkStatusObservation {
            machine_id,
            agent_version: Some("1.4.2".to_string()),
            observed_at: fixture_time(1900),
            network_config_version: Some(config_version(3)),
            client_certificate_expiry: Some(1_781_536_000),
            agent_version_superseded_at: None,
            instance_network_observation: None,
            fabric_interfaces: vec![],
        }),
        infiniband_status_observation: Some(MachineInfinibandStatusObservation {
            ib_interfaces: (0..6)
                .map(|i: u16| MachineIbInterfaceStatusObservation {
                    guid: format!("0xb8cef603004f00{i:02}"),
                    lid: 100 + i,
                    fabric_id: "compute".to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                })
                .collect(),
            observed_at: fixture_time(1800),
        }),
        nvlink_status_observation: None,
        spx_status_observation: None,
        extension_service_status_observations: Default::default(),
        controller_state_version: config_version(5).version_string(),
        controller_state: ManagedHostState::Ready,
        last_discovery_time: Some(fixture_time(200)),
        last_scout_contact_time: Some(fixture_time(1950)),
        last_scout_observed_version: Some("0.9.77".to_string()),
        last_reboot_time: Some(fixture_time(300)),
        last_reboot_requested: Some(MachineLastRebootRequested {
            time: fixture_time(290),
            mode: MachineLastRebootRequestedMode::Reboot,
            restart_verified: Some(true),
            verification_attempts: Some(1),
        }),
        last_cleanup_time: Some(fixture_time(250)),
        failure_details: FailureDetails {
            cause: FailureCause::NoError,
            failed_at: fixture_time(0),
            source: FailureSource::NoError,
        },
        reprovisioning_requested: None,
        host_reprovisioning_requested: None,
        manual_firmware_upgrade_completed: Some(fixture_time(400)),
        bios_password_set_time: Some(fixture_time(350)),
        last_machine_validation_time: Some(fixture_time(1700)),
        discovery_machine_validation_id: Some(uuid::Uuid::from_u128(0x3001).into()),
        cleanup_machine_validation_id: Some(uuid::Uuid::from_u128(0x3002).into()),
        dpu_agent_upgrade_requested: Some(UpgradeDecision {
            should_upgrade: false,
            to_version: "1.4.2".to_string(),
            last_updated: fixture_time(1600),
        }),
        firmware_autoupdate: Some(true),
        health_reports: Some(health_reports(if is_dpu {
            HealthReport::DPU_AGENT_SOURCE
        } else {
            "platform-health"
        })),
        on_demand_machine_validation_id: None,
        on_demand_machine_validation_request: Some(false),
        asn: Some(4_200_000_042),
        controller_state_outcome: None,
        current_machine_validation_id: None,
        machine_state_model_version: i32::from(CURRENT_STATE_MODEL_VERSION),
        instance_type_id: Some(uuid::Uuid::from_u128(0x4001).into()),
        interfaces,
        topology: vec![MachineTopology {
            machine_id,
            topology: TopologyData {
                discovery_data: DiscoveryData {
                    info: hardware_info,
                },
                bmc_info: bmc_info.clone(),
            },
            created: fixture_time(200),
            updated: fixture_time(1900),
            topology_update_needed: false,
        }],
        bmc_info,
        labels: HashMap::from([
            ("pool".to_string(), "compute-a".to_string()),
            ("env".to_string(), "bench".to_string()),
        ]),
        name: "bench-host-01".to_string(),
        description: "fully populated benchmark machine".to_string(),
        history: state_history(),
        version: config_version(7).version_string(),
        hw_sku: Some("SKU-8xH100-2xBF3".to_string()),
        desired_boot_interface_mac: None,
        desired_boot_interface_id: None,
        desired_boot_interface_version: None,
        boot_interface_selection_source: None,
        boot_interface_selection_updated_at: None,
        boot_interface_verified_version: None,
        boot_interface_observed_at: None,
        boot_interface_observation_assumed: false,
        hw_sku_status: None,
        power_options: None,
        hw_sku_device_type: Some("compute".to_string()),
        update_complete: true,
        nvlink_info: None,
        dpf: Dpf {
            enabled: false,
            used_for_ingestion: false,
        },
        host_profile: HostProfile::default(),
        rack_fw_details: None,
        slot_number: Some(3),
        tray_index: Some(1),
    }
}

/// A fully-populated host [`Machine`], as loaded from the database.
pub fn host_machine() -> Machine {
    machine_snapshot_pg_json(host_machine_id())
        .try_into()
        .expect("fixture host snapshot converts to Machine")
}

/// A fully-populated DPU [`Machine`], as loaded from the database.
pub fn dpu_machine(index: u8) -> Machine {
    machine_snapshot_pg_json(dpu_machine_id(index).into())
        .try_into()
        .expect("fixture DPU snapshot converts to Machine")
}

/// A managed-host snapshot bundling the fixture host with two DPUs, as the
/// GetMachines / FindMachinesByIds handlers see it.
pub fn managed_host_state_snapshot() -> ManagedHostStateSnapshot {
    let host_snapshot = host_machine();
    let managed_state = host_snapshot.state.value.clone();
    ManagedHostStateSnapshot {
        host_snapshot,
        dpu_snapshots: vec![dpu_machine(0), dpu_machine(1)],
        dpa_interface_snapshots: vec![],
        instance: None,
        managed_state,
        aggregate_health: health_report_with_source("aggregate-health"),
        rack_health_overrides: None,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use serde_json::json;

    use super::*;

    fn memory_devices(memory_devices_json: serde_json::Value) -> Vec<MemoryDeviceGroup> {
        let json = json!({
            "machine_type": "x86_64",
            "memory_devices": memory_devices_json,
        });
        let info: HardwareInfo = serde_json::from_value(json).unwrap();
        info.memory_devices
    }

    /// `HardwareInfo.memory_devices` accepts both the condensed `{size_mb, mem_type, count}`
    /// shape and the legacy flat shape (`{size_mb, mem_type}`, one object per DIMM, implicit
    /// `count: 1`). Consecutive entries with the same `(size_mb, mem_type)` merge regardless of
    /// which shape produced them.
    #[test]
    fn hardware_info_normalizes_mixed_legacy_and_counted_memory_devices() {
        check_values(
            [
                Check {
                    scenario: "legacy entries without count merge like a single counted group",
                    input: json!([
                        {"size_mb": 16384, "mem_type": "DDR5"},
                        {"size_mb": 16384, "mem_type": "DDR5"},
                        {"size_mb": 16384, "mem_type": "DDR5"}
                    ]),
                    expect: vec![MemoryDeviceGroup {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".into()),
                        count: 3,
                    }],
                },
                Check {
                    scenario: "a legacy entry directly followed by a counted entry of the same key merges",
                    input: json!([
                        {"size_mb": 16384, "mem_type": "DDR5"},
                        {"size_mb": 16384, "mem_type": "DDR5", "count": 4}
                    ]),
                    expect: vec![MemoryDeviceGroup {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".into()),
                        count: 5,
                    }],
                },
                Check {
                    scenario: "a counted entry directly followed by a legacy entry of the same key merges",
                    input: json!([
                        {"size_mb": 16384, "mem_type": "DDR5", "count": 4},
                        {"size_mb": 16384, "mem_type": "DDR5"}
                    ]),
                    expect: vec![MemoryDeviceGroup {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".into()),
                        count: 5,
                    }],
                },
                Check {
                    scenario: "legacy and counted entries with different keys stay separate",
                    input: json!([
                        {"size_mb": 8192, "mem_type": "DDR4"},
                        {"size_mb": 16384, "mem_type": "DDR5", "count": 4},
                        {"size_mb": 8192, "mem_type": "DDR4"}
                    ]),
                    expect: vec![
                        MemoryDeviceGroup {
                            size_mb: Some(8192),
                            mem_type: Some("DDR4".into()),
                            count: 1,
                        },
                        MemoryDeviceGroup {
                            size_mb: Some(16384),
                            mem_type: Some("DDR5".into()),
                            count: 4,
                        },
                        MemoryDeviceGroup {
                            size_mb: Some(8192),
                            mem_type: Some("DDR4".into()),
                            count: 1,
                        },
                    ],
                },
                Check {
                    // Ties the legacy wire shape back to the fixture host's own memory
                    // layout: 8 flat DIMM entries should normalize to exactly what
                    // `host_hardware_info()` already carries as a single counted group.
                    scenario: "a full host's worth of legacy DIMMs matches the fixture's counted group",
                    input: json!(
                        std::iter::repeat_n(json!({"size_mb": 65536, "mem_type": "DDR5"}), 8)
                            .collect::<Vec<_>>()
                    ),
                    expect: host_hardware_info().memory_devices,
                },
            ],
            memory_devices,
        );
    }
}
