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

pub mod config;
mod errors;
mod metrics;
pub mod nmx_c_endpoint;
pub mod nvlink;
mod switch_cert_monitor;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use carbide_secrets::credentials::CredentialManager;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::machine::MachineId;
use carbide_uuid::nvlink::{NvLinkDomainId, NvLinkLogicalPartitionId, NvLinkPartitionId};
use carbide_uuid::rack::RackId;
use chrono::Utc;
use config::NvLinkConfig;
use config_version::Versioned;
use db::machine::find_machine_ids;
use db::managed_host::load_by_machine_ids;
use db::nvl_logical_partition::IdColumn as LpIdColumn;
use db::nvl_partition::IdColumn;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{self, ObjectColumnFilter, TransactionVending, machine};
use errors::{NvLinkManagerError, NvLinkManagerResult};
use libnmxc::nmxc_model::{
    GetComputeNodeInfoListRequest, GetGpuInfoListRequest, GetPartitionInfoListRequest,
    PartitionInfo,
};
use libnmxc::{Endpoint, NMX_C_GATEWAY_ID, Nmxc, NmxcPool};
use metrics::{
    AppliedChange, ChassisNmxCUnreachableReason, NmxcMetricOperationStatus,
    NvlPartitionMonitorMetrics,
};
use model::hardware_info::{HardwareInfo, MachineNvLinkInfo, NvLinkGpu};
use model::instance::status::SyncState;
use model::instance::status::nvlink::InstanceNvLinkStatus;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::nvlink::{MachineNvLinkGpuStatusObservation, MachineNvLinkStatusObservation};
use model::machine::{HostHealthConfig, LoadSnapshotOptions, ManagedHostStateSnapshot};
use model::nvl_logical_partition::LogicalPartition;
use model::nvl_partition::{NvlPartition, NvlPartitionName};
use model::rack_type::RackProfileConfig;
use sqlx::PgPool;
#[cfg(feature = "test-support")]
pub use switch_cert_monitor::{SwitchCertificateMonitor, SwitchCertificateMonitorIterationResult};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Default NMX-M instance identifier for credentials and client lookup when none is specified.
pub const DEFAULT_NMX_M_NAME: &str = "default";

/// Multicast groups limit for new NMX-C partitions. Must be a multiple of 4. Assuming at most 1
/// partition per tray and 18 tray default partitions, this is floor(1024 / 18) rounded down
/// to the nearest multiple of 4.
const NMX_C_PARTITION_MULTICAST_GROUPS_LIMIT: u32 = 56;

fn rack_id_from_chassis_snapshots(
    chassis_snapshots: &[&ManagedHostStateSnapshot],
) -> Option<RackId> {
    chassis_snapshots
        .iter()
        .find_map(|snapshot| snapshot.host_snapshot.rack_id.clone())
}

fn domain_uuid_from_nmx_c_hello(
    hello: &libnmxc::nmxc_model::ServerHello,
) -> NvLinkManagerResult<NvLinkDomainId> {
    hello
        .server_header
        .as_ref()
        .and_then(|header| uuid::Uuid::parse_str(&header.domain_uuid).ok())
        .map(NvLinkDomainId::from)
        .ok_or_else(|| {
            NvLinkManagerError::internal(format!(
                "Failed to parse domain UUID from NMX-C hello response: {hello:?}"
            ))
        })
}

fn parse_nvlink_gpu_fabric_guid(fabric_guid: &str) -> u64 {
    let s = fabric_guid.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).unwrap_or(0)
    } else {
        s.parse::<u64>().unwrap_or(0)
    }
}

fn nvlink_gpus_from_hardware_info(hardware_info: &HardwareInfo) -> Vec<NvLinkGpu> {
    hardware_info
        .gpus
        .iter()
        .filter_map(|gpu| gpu.platform_info.as_ref())
        .map(|platform_info| NvLinkGpu {
            tray_index: platform_info.tray_index as i32,
            slot_id: platform_info.slot_number as i32,
            device_id: platform_info.module_id as i32,
            guid: parse_nvlink_gpu_fabric_guid(&platform_info.fabric_guid),
        })
        .collect()
}

fn build_machine_nvlink_info_from_nmx_c_hello(
    existing: Option<&MachineNvLinkInfo>,
    snapshot: Option<&ManagedHostStateSnapshot>,
    chassis_serial: &str,
    domain_uuid: NvLinkDomainId,
) -> MachineNvLinkInfo {
    if let Some(existing) = existing {
        let mut info = existing.clone();
        if info.domain_uuid == NvLinkDomainId::nil() {
            info.domain_uuid = domain_uuid;
        }
        if info.chassis_serial.trim().is_empty() {
            info.chassis_serial = chassis_serial.to_string();
        }
        return info;
    }

    if let Some(snapshot_info) =
        snapshot.and_then(|snapshot| snapshot.host_snapshot.nvlink_info.as_ref())
    {
        return MachineNvLinkInfo {
            domain_uuid,
            chassis_serial: if snapshot_info.chassis_serial.trim().is_empty() {
                chassis_serial.to_string()
            } else {
                snapshot_info.chassis_serial.clone()
            },
            gpus: snapshot_info.gpus.clone(),
        };
    }

    let gpus = snapshot
        .and_then(|snapshot| snapshot.host_snapshot.hardware_info.as_ref())
        .map(nvlink_gpus_from_hardware_info)
        .unwrap_or_default();

    MachineNvLinkInfo {
        domain_uuid,
        chassis_serial: chassis_serial.to_string(),
        gpus,
    }
}

/// Populates missing `machines.nvlink_info` entries, nil `domain_uuid`, or empty `chassis_serial`
/// using NMX-C hello.
fn populate_machine_nvlink_info_if_needed(
    machine_nvlink_info: &mut HashMap<MachineId, Option<MachineNvLinkInfo>>,
    managed_host_snapshots: &HashMap<MachineId, ManagedHostStateSnapshot>,
    chassis_serial: &str,
    machine_ids: &[MachineId],
    domain_uuid: NvLinkDomainId,
) -> Vec<(MachineId, MachineNvLinkInfo)> {
    let mut updates = Vec::new();
    for machine_id in machine_ids {
        let existing = machine_nvlink_info
            .get(machine_id)
            .and_then(|info| info.as_ref());
        let needs_update = match existing {
            None => true,
            Some(info) => {
                info.domain_uuid == NvLinkDomainId::nil() || info.chassis_serial.trim().is_empty()
            }
        };
        if !needs_update {
            continue;
        }

        let snapshot = managed_host_snapshots.get(machine_id);
        let updated = build_machine_nvlink_info_from_nmx_c_hello(
            existing,
            snapshot,
            chassis_serial,
            domain_uuid,
        );
        machine_nvlink_info.insert(*machine_id, Some(updated.clone()));
        updates.push((*machine_id, updated));
    }
    updates
}

fn nmx_c_partition_create_attr_with_multicast_groups_limit(
    multicast_groups_limit: u32,
) -> libnmxc::nmxc_model::PartitionAttr {
    libnmxc::nmxc_model::PartitionAttr {
        resiliency_mode: libnmxc::nmxc_model::ResiliencyMode::NmxResiliencyModeUndefined as i32,
        multicast_groups_limit,
    }
}

fn nmx_c_create_partition_request(
    name: String,
    gpu_uids: &[u64],
    multicast_groups_limit: u32,
) -> libnmxc::nmxc_model::CreatePartitionRequest {
    libnmxc::nmxc_model::CreatePartitionRequest {
        context: None,
        name,
        gpu_resource_id: gpu_uids
            .iter()
            .map(|&uid| libnmxc::nmxc_model::GpuResourceId {
                resource_id: Some(libnmxc::nmxc_model::gpu_resource_id::ResourceId::GpuUid(
                    uid,
                )),
            })
            .collect(),
        attr: Some(nmx_c_partition_create_attr_with_multicast_groups_limit(
            multicast_groups_limit,
        )),
        partition_id: None,
        gateway_id: NMX_C_GATEWAY_ID.into(),
    }
}

#[derive(Debug, Clone)]
struct NmxcPartitionOperation {
    domain_uuid: Option<NvLinkDomainId>,
    operation_type: NmxcPartitionOperationType,
    gpu_uids: Vec<u64>,
    name: String,
    db_partition_id: Option<NvLinkPartitionId>,
}

#[derive(Debug, Clone)]
pub enum NmxcPartitionOperationType {
    Create,
    Remove(u32),                 // NMX-C partition ID
    RemoveUnknownPartition(u32), // NMX-C partition ID
    Update(u32),                 // NMX-C partition ID
}

#[derive(Debug, Clone)]
enum GpuAction {
    AddToPartition,
    RemoveFromPartition,
    RemoveFromUnknownPartition,
    NoOp,
}

// Context for GPU helper functions in check_nv_link_partitions
struct GpuProcessingContext {
    gpu_guid: u64,
    domain_uuid: NvLinkDomainId,
    logical_partition_id: Option<NvLinkLogicalPartitionId>,
    partition_id: Option<NvLinkPartitionId>,
    partition_name: String,
    partition_nmx_c_id: libnmxc::nmxc_model::PartitionId,
}

impl Default for GpuProcessingContext {
    fn default() -> Self {
        Self {
            gpu_guid: 0,
            domain_uuid: NvLinkDomainId::default(),
            logical_partition_id: None,
            partition_id: None,
            partition_name: "".to_string(),
            partition_nmx_c_id: libnmxc::nmxc_model::PartitionId::default(),
        }
    }
}

// Context for partition helper functions in check_nv_link_partitions.
pub struct PartitionProcessingContext {
    nmx_c_partitions: HashMap<libnmxc::nmxc_model::PartitionId, PartitionInfo>,
    db_nvl_logical_partitions: HashMap<NvLinkLogicalPartitionId, LogicalPartition>,
    db_nvl_partitions: HashMap<u32, NvlPartition>, // NMX-C partition ID to NvlPartition
    machine_nvlink_info: HashMap<MachineId, Option<MachineNvLinkInfo>>,
    gpu_to_partition_map: HashMap<u64, PartitionInfo>, // GPU UID to NMX-C partition
    nmx_c_operations: HashMap<NvLinkLogicalPartitionId, Vec<NmxcPartitionOperation>>,
    unknown_partition_removal_operations: HashMap<u32, Vec<NmxcPartitionOperation>>,
    unknown_partition_addition_operations: HashMap<u32, NmxcPartitionOperation>,
    /// Pending NMX-C `Create` for tray default partitions (key: GPU `slot_id`), merged after scanning hosts.
    pending_tray_partition_creates_by_slot: HashMap<i32, NmxcPartitionOperation>,
}

fn nmx_c_partition_id_string(pi: &PartitionInfo) -> String {
    pi.partition_id
        .as_ref()
        .map(|id| id.partition_id.to_string())
        .unwrap_or_default()
}

fn is_nmx_c_default_partition(partition: &PartitionInfo) -> bool {
    let id_is_default = partition
        .partition_id
        .as_ref()
        .is_some_and(|id| id.partition_id == 32766);
    id_is_default || partition.name.contains("Default")
}

fn tray_default_partition_name(slot_id: i32) -> String {
    format!("tray_partition_{slot_id}")
}

fn is_gpu_in_tray_default_partition(partition: &PartitionInfo, slot_id: i32) -> bool {
    partition.name == tray_default_partition_name(slot_id)
}

impl PartitionProcessingContext {
    fn new(
        nmx_c_partitions: Vec<PartitionInfo>,
        db_nvl_logical_partitions: Vec<LogicalPartition>,
        db_nvl_partitions: Vec<NvlPartition>,
        machine_nvlink_info: HashMap<MachineId, Option<MachineNvLinkInfo>>,
    ) -> Self {
        let gpu_map = Self::build_gpu_to_partition_map(&nmx_c_partitions);
        let nmx_c_partitions = nmx_c_partitions
            .into_iter()
            .filter_map(|p| p.partition_id.map(|id| (id, p)))
            .collect();
        let db_nvl_logical_partitions = db_nvl_logical_partitions
            .into_iter()
            .map(|p| (p.id, p))
            .collect();
        let db_nvl_partitions = db_nvl_partitions
            .into_iter()
            .filter_map(|p| u32::try_from(p.nmx_c_partition_id).ok().map(|id| (id, p)))
            .collect();
        Self {
            nmx_c_partitions,
            db_nvl_logical_partitions,
            db_nvl_partitions,
            machine_nvlink_info,
            gpu_to_partition_map: gpu_map,
            nmx_c_operations: HashMap::new(),
            unknown_partition_removal_operations: HashMap::new(),
            unknown_partition_addition_operations: HashMap::new(),
            pending_tray_partition_creates_by_slot: HashMap::new(),
        }
    }

    /// If the NMX-C default partition exists, enqueue its removal and return true.
    fn enqueue_nmx_c_default_partition_removal_if_present(&mut self) -> bool {
        let Some(default_nmx_c_id) = self
            .nmx_c_partitions
            .values()
            .find(|p| is_nmx_c_default_partition(p))
            .and_then(|p| p.partition_id.map(|id| id.partition_id))
        else {
            return false;
        };
        self.nmx_c_operations
            .entry(NvLinkLogicalPartitionId::default())
            .or_default()
            .push(NmxcPartitionOperation {
                domain_uuid: None,
                operation_type: NmxcPartitionOperationType::RemoveUnknownPartition(
                    default_nmx_c_id,
                ),
                gpu_uids: vec![],
                name: String::new(),
                db_partition_id: None,
            });
        true
    }

    /// Coalesce one `Create` per `slot_id` for GPUs that need a new tray default partition in the same monitor pass.
    fn enqueue_tray_default_partition_create(
        &mut self,
        slot_id: i32,
        domain_uuid: NvLinkDomainId,
        gpu_guid: u64,
        partition_name: String,
    ) {
        self.pending_tray_partition_creates_by_slot
            .entry(slot_id)
            .and_modify(|op| {
                if !op.gpu_uids.contains(&gpu_guid) {
                    op.gpu_uids.push(gpu_guid);
                }
            })
            .or_insert(NmxcPartitionOperation {
                domain_uuid: Some(domain_uuid),
                operation_type: NmxcPartitionOperationType::Create,
                gpu_uids: vec![gpu_guid],
                name: partition_name,
                db_partition_id: None,
            });
    }

    /// Queue NMX-C work so `gpu` is added to `tray_partition_{slot_id}` (existing partition or `Create`).
    fn ensure_gpu_enqueued_into_tray_partition(
        &mut self,
        machine_id: &MachineId,
        domain_uuid: NvLinkDomainId,
        gpu: &NvLinkGpu,
    ) -> NvLinkManagerResult<()> {
        let tray_partition_nm = tray_default_partition_name(gpu.slot_id);

        if let Some(tray_nmxc) = self
            .nmx_c_partitions
            .values()
            .find(|p| p.name == tray_partition_nm)
        {
            let Some(partition_id_struct) = tray_nmxc.partition_id else {
                tracing::warn!(
                    machine_id = %machine_id,
                    gpu_guid = %gpu.guid,
                    tray_partition = %tray_partition_nm,
                    "Tray default NMX-C partition has no partition_id; skipping"
                );
                return Ok(());
            };
            let nmx_c_id = partition_id_struct.partition_id;
            let gpus_in_partition = tray_nmxc.gpu_uid_list.clone();

            tracing::info!(
                machine_id = %machine_id,
                gpu_guid = %gpu.guid,
                nmx_c_id,
                tray_partition = %tray_partition_nm,
                "Enqueueing add to tray default partition"
            );
            self.handle_gpu_addition_to_unknown_partition(
                &partition_id_struct,
                gpu.guid,
                gpus_in_partition,
            )?;
        } else {
            tracing::info!(
                machine_id = %machine_id,
                gpu_guid = %gpu.guid,
                tray_partition = %tray_partition_nm,
                "Enqueueing create of tray default partition"
            );
            self.enqueue_tray_default_partition_create(
                gpu.slot_id,
                domain_uuid,
                gpu.guid,
                tray_partition_nm,
            );
        }
        Ok(())
    }

    // Build a map from GPU UIDs (as string) to partition from NMX-C partition info list.
    fn build_gpu_to_partition_map(
        nmx_c_partitions: &[PartitionInfo],
    ) -> HashMap<u64, PartitionInfo> {
        let mut gpu_map = HashMap::new();
        for partition in nmx_c_partitions {
            for gpu_uid in &partition.gpu_uid_list {
                gpu_map.insert(*gpu_uid, partition.clone());
            }
        }
        gpu_map
    }

    // Validate that a logical partition exists and is not deleted
    fn validate_logical_partition(&self, logical_partition_id: &NvLinkLogicalPartitionId) -> bool {
        if let Some(matching_logical_partition) =
            self.db_nvl_logical_partitions.get(logical_partition_id)
        {
            if model::nvl_logical_partition::is_marked_as_deleted(matching_logical_partition) {
                tracing::error!(
                    "logical partition already marked as deleted, cannot modify physical partition"
                );
                return false;
            }
            true
        } else {
            tracing::error!("logical partition {} not found!!", logical_partition_id);
            false
        }
    }

    // Get partition information from the database for a given NMX-C partition ID (numeric key).
    fn get_db_partition_info(
        &self,
        nmx_c_partition_id: u32,
    ) -> Option<(
        Option<NvLinkPartitionId>,
        Option<NvLinkLogicalPartitionId>,
        String,
        libnmxc::nmxc_model::PartitionId,
    )> {
        self.db_nvl_partitions.get(&nmx_c_partition_id).map(|p| {
            (
                Some(p.id),
                p.logical_partition_id,
                p.name.clone().into(),
                libnmxc::nmxc_model::PartitionId {
                    partition_id: nmx_c_partition_id,
                },
            )
        })
    }

    // Get the list of GPUs that should remain in a partition after removing a specific GPU from a logical partition.
    // To remove a GPU from a partition in NMX-C, we need to do an update op with every other GPU in the partition except the one
    // getting removed.
    fn get_gpus_to_keep_after_removal(
        &self,
        logical_partition_id: Option<NvLinkLogicalPartitionId>,
        partition_nmx_c_id: &libnmxc::nmxc_model::PartitionId,
        gpu_guid: u64,
        machine_id: &MachineId,
        device_instance: u32,
    ) -> Option<Vec<u64>> {
        let Some(logical_partition_id) = logical_partition_id else {
            tracing::error!(
                "Logical partition ID is required for getting GPUs to keep after removal"
            );
            return None;
        };
        let gpus_to_keep = match self.nmx_c_operations.get(&logical_partition_id) {
            Some(ops) => {
                if let Some(op) = ops.iter().find(|op| op.gpu_uids.contains(&gpu_guid)) {
                    op.gpu_uids
                        .iter()
                        .copied()
                        .filter(|id| *id != gpu_guid)
                        .collect()
                } else {
                    // No operation found for this physical partition, so get the partition members from NMX-C.
                    match self.nmx_c_partitions.get(partition_nmx_c_id) {
                        Some(p) => p
                            .gpu_uid_list
                            .iter()
                            .copied()
                            .filter(|&id| id != gpu_guid)
                            .collect(),
                        None => {
                            tracing::error!(
                                "NMX-C partition not found for machine {}, GPU index {}",
                                machine_id,
                                device_instance
                            );
                            return None;
                        }
                    }
                }
            }
            None => {
                // No pending operations found, so get the GPUs from NMX-C.
                match self.nmx_c_partitions.get(partition_nmx_c_id) {
                    Some(p) => p
                        .gpu_uid_list
                        .iter()
                        .copied()
                        .filter(|id| *id != gpu_guid)
                        .collect(),
                    None => {
                        tracing::error!(
                            "NMX-C partition not found for machine {}, GPU index {}",
                            machine_id,
                            device_instance
                        );
                        return None;
                    }
                }
            }
        }; // Some(gpus_to_keep)
        Some(gpus_to_keep)
    }

    fn get_gpus_to_keep_in_unknown_partition_after_removal(
        &self,
        partition_nmx_c_id: &libnmxc::nmxc_model::PartitionId,
        gpu_guid: u64,
        machine_id: &MachineId,
        device_instance: u32,
    ) -> Option<Vec<u64>> {
        let gpus_to_keep = match self
            .unknown_partition_removal_operations
            .get(&partition_nmx_c_id.partition_id)
        {
            Some(ops) => {
                if let Some(op) = ops.iter().find(|op| op.gpu_uids.contains(&gpu_guid)) {
                    op.gpu_uids
                        .iter()
                        .copied()
                        .filter(|id| *id != gpu_guid)
                        .collect()
                } else {
                    // No operation found for this GPU, so get the GPUs from the default partition.
                    match self.nmx_c_partitions.get(partition_nmx_c_id) {
                        Some(p) => p
                            .gpu_uid_list
                            .iter()
                            .copied()
                            .filter(|id| *id != gpu_guid)
                            .collect(),
                        None => {
                            tracing::error!(
                                "NMX-C partition not found for machine {}, GPU index {}",
                                machine_id,
                                device_instance
                            );
                            return None;
                        }
                    }
                }
            }
            None => {
                // No removal operations found, so get the GPUs from the unknown partition.
                match self.nmx_c_partitions.get(partition_nmx_c_id) {
                    Some(p) => p
                        .gpu_uid_list
                        .iter()
                        .copied()
                        .filter(|id| *id != gpu_guid)
                        .collect(),
                    None => {
                        tracing::error!(
                            "NMX-C partition not found for machine {}, GPU index {}",
                            machine_id,
                            device_instance
                        );
                        return None;
                    }
                }
            }
        }; // Some(gpus_to_keep)
        Some(gpus_to_keep) // Some(gpus_to_keep)
    }

    // Handle GPU removal from a logical partition
    fn handle_gpu_removal(
        &mut self,
        ctx: &GpuProcessingContext,
        gpus_to_keep: Vec<u64>,
    ) -> NvLinkManagerResult<()> {
        let Some(logical_partition_id) = ctx.logical_partition_id else {
            return Err(NvLinkManagerError::internal(
                "Logical partition ID is required for GPU removal".to_string(),
            ));
        };
        if gpus_to_keep.is_empty() {
            // All members need to be removed, enqueue a Remove request
            let operation = NmxcPartitionOperation {
                domain_uuid: Some(ctx.domain_uuid),
                operation_type: NmxcPartitionOperationType::Remove(
                    ctx.partition_nmx_c_id.partition_id,
                ),
                gpu_uids: gpus_to_keep.clone(),
                name: ctx.partition_name.clone(),
                db_partition_id: ctx.partition_id,
            };

            self.nmx_c_operations
                .entry(logical_partition_id)
                .and_modify(|ops| {
                    if let Some(op) = ops
                        .iter_mut()
                        .find(|op| op.gpu_uids.contains(&ctx.gpu_guid))
                    {
                        op.operation_type =
                            NmxcPartitionOperationType::Remove(ctx.partition_nmx_c_id.partition_id);
                        op.gpu_uids = gpus_to_keep.clone();
                        op.name = ctx.partition_name.clone();
                    } else {
                        ops.push(operation.clone());
                    }
                })
                .or_insert(vec![operation]);
        } else {
            // Some members remain, enqueue an Update request
            let operation = NmxcPartitionOperation {
                domain_uuid: Some(ctx.domain_uuid),
                operation_type: NmxcPartitionOperationType::Update(
                    ctx.partition_nmx_c_id.partition_id,
                ),
                gpu_uids: gpus_to_keep.clone(),
                name: ctx.partition_name.clone(),
                db_partition_id: ctx.partition_id,
            };

            self.nmx_c_operations
                .entry(logical_partition_id)
                .and_modify(|ops| {
                    if let Some(op) = ops
                        .iter_mut()
                        .find(|op| op.gpu_uids.contains(&ctx.gpu_guid))
                    {
                        op.operation_type =
                            NmxcPartitionOperationType::Update(ctx.partition_nmx_c_id.partition_id);
                        op.gpu_uids = gpus_to_keep.clone();
                        op.name = ctx.partition_name.clone();
                    } else {
                        ops.push(operation.clone());
                    }
                })
                .or_insert(vec![operation]);
        }
        Ok(())
    }

    // Handle GPU removal from the unknown partition
    fn handle_gpu_removal_from_unknown_partition(
        &mut self,
        partition_nmx_c_id: &libnmxc::nmxc_model::PartitionId,
        gpu_guid: u64,
        gpus_to_keep: Vec<u64>,
    ) -> NvLinkManagerResult<()> {
        if gpus_to_keep.is_empty() {
            let operation = NmxcPartitionOperation {
                domain_uuid: None,
                operation_type: NmxcPartitionOperationType::RemoveUnknownPartition(
                    partition_nmx_c_id.partition_id,
                ),
                gpu_uids: gpus_to_keep.clone(),
                name: "".to_string(),
                db_partition_id: None,
            };

            self.unknown_partition_removal_operations
                .entry(partition_nmx_c_id.partition_id)
                .and_modify(|ops| {
                    if let Some(op) = ops.iter_mut().find(|op| op.gpu_uids.contains(&gpu_guid)) {
                        op.operation_type = NmxcPartitionOperationType::RemoveUnknownPartition(
                            partition_nmx_c_id.partition_id,
                        );
                        op.gpu_uids = gpus_to_keep.clone();
                    } else {
                        ops.push(operation.clone());
                    }
                })
                .or_insert(vec![operation.clone()]);
        } else {
            let operation = NmxcPartitionOperation {
                domain_uuid: None,
                operation_type: NmxcPartitionOperationType::Update(partition_nmx_c_id.partition_id),
                gpu_uids: gpus_to_keep.clone(),
                name: "".to_string(),
                db_partition_id: None,
            };
            self.unknown_partition_removal_operations
                .entry(partition_nmx_c_id.partition_id)
                .and_modify(|ops| {
                    if let Some(op) = ops.iter_mut().find(|op| op.gpu_uids.contains(&gpu_guid)) {
                        op.operation_type =
                            NmxcPartitionOperationType::Update(partition_nmx_c_id.partition_id);
                        op.gpu_uids = gpus_to_keep.clone();
                    } else {
                        ops.push(operation.clone());
                    }
                })
                .or_insert(vec![operation.clone()]);
        }
        Ok(())
    }

    fn handle_gpu_addition_to_unknown_partition(
        &mut self,
        partition_nmx_c_id: &libnmxc::nmxc_model::PartitionId,
        gpu_guid: u64,
        gpus_in_partition: Vec<u64>,
    ) -> NvLinkManagerResult<()> {
        let pid = partition_nmx_c_id.partition_id;
        let mut gpu_uids = gpus_in_partition;
        gpu_uids.push(gpu_guid);
        let operation = NmxcPartitionOperation {
            domain_uuid: None,
            operation_type: NmxcPartitionOperationType::Update(pid),
            gpu_uids,
            name: "".to_string(),
            db_partition_id: None,
        };
        self.unknown_partition_addition_operations
            .entry(pid)
            .and_modify(|op| {
                if !op.gpu_uids.contains(&gpu_guid) {
                    op.gpu_uids.push(gpu_guid);
                }
            })
            .or_insert(operation);
        Ok(())
    }

    // Handle GPU addition to a logical partition when no other partitions exist in the logical partition.
    fn handle_gpu_addition_new_partition(
        &mut self,
        ctx: &GpuProcessingContext,
    ) -> NvLinkManagerResult<()> {
        let Some(logical_partition_id) = ctx.logical_partition_id else {
            return Err(NvLinkManagerError::internal(
                "Logical partition ID is required for GPU addition to new partition".to_string(),
            ));
        };
        let operation = NmxcPartitionOperation {
            domain_uuid: Some(ctx.domain_uuid),
            operation_type: NmxcPartitionOperationType::Create,
            gpu_uids: vec![ctx.gpu_guid],
            name: format!("{}{}", logical_partition_id, ctx.gpu_guid),
            db_partition_id: None,
        };

        self.nmx_c_operations
            .entry(logical_partition_id)
            .and_modify(|ops| {
                if let Some(op) = ops
                    .iter_mut()
                    .find(|op| op.domain_uuid.unwrap_or_default() == ctx.domain_uuid)
                {
                    op.gpu_uids.push(ctx.gpu_guid);
                } else {
                    ops.push(operation.clone());
                }
            })
            .or_insert(vec![operation]);
        Ok(())
    }

    // Handle GPU addition to an existing partition in the same domain
    fn handle_gpu_addition_existing_partition(
        &mut self,
        ctx: &GpuProcessingContext,
        partition: &NvlPartition,
    ) -> NvLinkManagerResult<()> {
        let Some(logical_partition_id) = ctx.logical_partition_id else {
            return Err(NvLinkManagerError::internal(
                "Logical partition ID is required for GPU addition to existing partition"
                    .to_string(),
            ));
        };

        // Get the GPU IDs that are already in the partition, plus the GPU being added.
        let Ok(nmx_c_partition_id) = u32::try_from(partition.nmx_c_partition_id) else {
            return Err(NvLinkManagerError::internal(format!(
                "NMX-C partition ID is required for DB partition {}",
                partition.id
            )));
        };
        let gpu_uids: Vec<u64> = if let Some(nmx_c_partition) =
            self.nmx_c_partitions
                .get(&libnmxc::nmxc_model::PartitionId {
                    partition_id: nmx_c_partition_id,
                }) {
            nmx_c_partition
                .gpu_uid_list
                .iter()
                .copied()
                .chain(std::iter::once(ctx.gpu_guid))
                .collect()
        } else {
            return Err(NvLinkManagerError::internal(
                "NMX-C partition not found for GPU addition to existing partition".to_string(),
            ));
        };

        let operation = NmxcPartitionOperation {
            domain_uuid: Some(ctx.domain_uuid),
            operation_type: NmxcPartitionOperationType::Update(nmx_c_partition_id),
            gpu_uids,
            name: partition.name.clone().into(),
            db_partition_id: ctx.partition_id, // TODO: should try to verify that these are not nil
        };

        self.nmx_c_operations
            .entry(logical_partition_id)
            .and_modify(|ops| {
                if let Some(op) = ops.iter_mut().find(|op| match &op.operation_type {
                    NmxcPartitionOperationType::Update(partition_id) => {
                        *partition_id == nmx_c_partition_id
                    }
                    _ => false,
                }) {
                    op.gpu_uids.push(ctx.gpu_guid);
                } else {
                    ops.push(operation.clone());
                }
            })
            .or_insert(vec![operation]);
        Ok(())
    }
}

pub struct NvlPartitionMonitor {
    db_pool: PgPool,
    nmxc_client_pool: Arc<dyn NmxcPool>,
    config: NvLinkConfig,
    host_health: HostHealthConfig,
    metric_holder: Arc<metrics::MetricHolder>,
    work_lock_manager_handle: WorkLockManagerHandle,
}

pub struct NvLinkManager {
    db_pool: PgPool,
    nmxc_client_pool: Arc<dyn NmxcPool>,
    meter: opentelemetry::metrics::Meter,
    config: NvLinkConfig,
    host_health: HostHealthConfig,
    rms_client: Option<Arc<dyn librms::RmsApi>>,
    credential_manager: Arc<dyn CredentialManager>,
    rack_profiles: RackProfileConfig,
    work_lock_manager_handle: WorkLockManagerHandle,
}

pub struct NvLinkManagerArgs {
    pub db_pool: PgPool,
    pub nmxc_client_pool: Arc<dyn NmxcPool>,
    pub meter: opentelemetry::metrics::Meter,
    pub config: NvLinkConfig,
    pub host_health: HostHealthConfig,
    pub rms_client: Option<Arc<dyn librms::RmsApi>>,
    pub credential_manager: Arc<dyn CredentialManager>,
    pub rack_profiles: RackProfileConfig,
    pub work_lock_manager_handle: WorkLockManagerHandle,
}

impl NvLinkManager {
    pub fn new(args: NvLinkManagerArgs) -> Self {
        Self {
            db_pool: args.db_pool,
            nmxc_client_pool: args.nmxc_client_pool,
            meter: args.meter,
            config: args.config,
            host_health: args.host_health,
            rms_client: args.rms_client,
            credential_manager: args.credential_manager,
            rack_profiles: args.rack_profiles,
            work_lock_manager_handle: args.work_lock_manager_handle,
        }
    }

    pub fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        NvlPartitionMonitor::new(
            self.db_pool.clone(),
            self.nmxc_client_pool,
            self.meter.clone(),
            self.config.clone(),
            self.host_health,
            self.work_lock_manager_handle.clone(),
        )
        .start(join_set, cancel_token.clone())?;

        if self.config.nmx_c_certificate_rotation.enabled {
            let switch_cert_monitor = switch_cert_monitor::SwitchCertificateMonitor::new(
                self.db_pool,
                self.meter,
                self.config,
                self.rms_client,
                self.credential_manager,
                self.rack_profiles,
                self.work_lock_manager_handle,
            );
            join_set
                .build_task()
                .name("nmx-c-switch-cert-monitor")
                .spawn(async move { switch_cert_monitor.run(cancel_token).await })?;
        }

        Ok(())
    }
}

struct CheckPartitionsInput {
    db_nvl_logical_partitions: Vec<LogicalPartition>,
    db_nvl_partitions: Vec<NvlPartition>,
    machine_nvlink_info: HashMap<MachineId, Option<MachineNvLinkInfo>>,
    managed_host_snapshots: HashMap<MachineId, ManagedHostStateSnapshot>,
    nvlink_info_db_updates: Vec<(MachineId, MachineNvLinkInfo)>,
}

/// Work queued when NMX-C cannot be used for a chassis and observations must be cleared.
struct PendingNullNvlinkObservation {
    /// Chassis serial for the machines whose observations will be cleared.
    chassis_serial: String,
    /// Failure reason recorded in partition-monitor metrics.
    reason: ChassisNmxCUnreachableReason,
    /// Host machines on the chassis that will receive a null `nvlink_status_observation`.
    machine_ids: Vec<MachineId>,
}

impl NvlPartitionMonitor {
    const ITERATION_WORK_KEY: &'static str = "NvlPartitionMonitor::run_single_iteration";

    pub fn new(
        db_pool: PgPool,
        nmxc_client_pool: Arc<dyn NmxcPool>,
        meter: opentelemetry::metrics::Meter,
        config: NvLinkConfig,
        host_health: HostHealthConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        let hold_period = config
            .monitor_run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, hold_period));

        Self {
            db_pool,
            nmxc_client_pool,
            config,
            host_health,
            metric_holder,
            work_lock_manager_handle,
        }
    }

    pub fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        if self.config.enabled {
            join_set
                .build_task()
                .name("nvl-partition-monitor")
                .spawn(async move { self.run(cancel_token).await })?;
        }

        Ok(())
    }

    pub async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.monitor_run_interval);
        loop {
            let mut tick = timer.tick();
            match self.run_single_iteration().await {
                Ok(num_changes) => {
                    if num_changes > 0 {
                        // Decrease the interval if changes have been made.
                        tick.set_interval(Duration::from_millis(1000));
                    }
                }
                Err(e) => {
                    tracing::warn!("NvlPartitionMonitor error: {}", e);
                }
            }

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("NvlPartitionMonitor stop was requested");
                    return;
                }
            }
        }
    }

    pub async fn run_single_iteration(&self) -> NvLinkManagerResult<usize> {
        let mut metrics = NvlPartitionMonitorMetrics::new();
        let span_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
        let check_nvl_partition_span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "nvl_partition_monitor",
            span_id,
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
            metrics = tracing::field::Empty,
        );
        let result = self
            .run_single_iteration_inner(&mut metrics)
            .instrument(check_nvl_partition_span.clone())
            .await;
        check_nvl_partition_span.record(
            "otel.status_code",
            if result.is_ok() { "ok" } else { "error" },
        );
        if let Err(ref e) = result {
            check_nvl_partition_span.record("otel.status_message", format!("{e:?}"));
        }
        check_nvl_partition_span.record("metrics", metrics.to_string());
        self.metric_holder.update_metrics(metrics);
        result
    }

    async fn run_single_iteration_inner(
        &self,
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<usize> {
        let _lock = match self
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(e) => {
                tracing::warn!(
                    "NvlPartitionMonitor failed to acquire work lock: Another instance of carbide running? {e}"
                );
                return Ok(0);
            }
        };
        tracing::trace!(
            lock = Self::ITERATION_WORK_KEY,
            "NvlPartitionMonitor acquired the lock",
        );

        let mut txn = self.db_pool.txn_begin().await?;
        let managed_host_snapshots = self.load_mnnvl_managed_host_snapshots(txn.as_mut()).await?;
        let mut machine_nvlink_info = machine::find_nvlink_info_by_machine_ids(
            &mut txn,
            &managed_host_snapshots.keys().copied().collect::<Vec<_>>(),
        )
        .await?;

        let managed_host_snapshots_by_chassis_serial: HashMap<
            String,
            Vec<&ManagedHostStateSnapshot>,
        > = managed_host_snapshots.iter().fold(
            HashMap::new(),
            |mut acc, (_machine_id, snapshot)| {
                if let Some(nvlink_info) = snapshot.host_snapshot.nvlink_info.as_ref() {
                    let serial = nvlink_info.chassis_serial.trim();
                    if !serial.is_empty() {
                        acc.entry(serial.to_string()).or_default().push(snapshot);
                    }
                }
                acc
            },
        );

        let db_nvl_partitions =
            db::nvl_partition::find_by(&mut txn, ObjectColumnFilter::<IdColumn>::All).await?;

        let db_nvl_logical_partitions =
            db::nvl_logical_partition::find_by(&mut txn, ObjectColumnFilter::<LpIdColumn>::All)
                .await?;

        let mut chassis_serial_to_resolved_endpoint = HashMap::new();
        for (chassis_serial, chassis_snapshots) in &managed_host_snapshots_by_chassis_serial {
            let rack_id = rack_id_from_chassis_snapshots(chassis_snapshots);
            if let Some(endpoint_url) = nmx_c_endpoint::resolve_nmx_c_endpoint_url(
                &mut txn,
                rack_id.as_ref(),
                chassis_serial,
                &self.config,
            )
            .await
            .map_err(NvLinkManagerError::from)?
            {
                chassis_serial_to_resolved_endpoint.insert(chassis_serial.clone(), endpoint_url);
            }
        }

        // Don't hold the transaction across unrelated awaits
        txn.commit().await?;

        metrics.num_logical_partitions = db_nvl_logical_partitions.len();
        metrics.num_physical_partitions = db_nvl_partitions.len();

        let mut total_completed_operations = 0;
        let mut pending_null_nvlink_observations = Vec::new();

        for (chassis_serial, chassis_snapshots) in &managed_host_snapshots_by_chassis_serial {
            let Some(endpoint_url) = chassis_serial_to_resolved_endpoint.get(chassis_serial) else {
                tracing::warn!(
                    %chassis_serial,
                    "No NMX-C endpoint for chassis (switch NVOS IP or nvlink_nmxc_endpoints mapping); skipping partition monitor work"
                );
                Self::queue_null_nvlink_status_observation(
                    &mut pending_null_nvlink_observations,
                    chassis_serial,
                    chassis_snapshots,
                    ChassisNmxCUnreachableReason::NoEndpoint,
                );
                continue;
            };

            let nmxc_endpoint = match Endpoint::new(endpoint_url) {
                Ok(ep) => ep,
                Err(e) => {
                    tracing::warn!(
                        %chassis_serial,
                        endpoint = %endpoint_url,
                        error = %e,
                        "Invalid NMX-C endpoint URI; skipping partition monitor work for this chassis"
                    );
                    Self::queue_null_nvlink_status_observation(
                        &mut pending_null_nvlink_observations,
                        chassis_serial,
                        chassis_snapshots,
                        ChassisNmxCUnreachableReason::InvalidEndpointUri,
                    );
                    continue;
                }
            };

            let mut nmxc_client = match self.nmxc_client_pool.create_client(nmxc_endpoint).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        %chassis_serial,
                        endpoint = %endpoint_url,
                        error = %e,
                        "Failed to create NMX-C client; skipping partition monitor work for this chassis"
                    );
                    Self::queue_null_nvlink_status_observation(
                        &mut pending_null_nvlink_observations,
                        chassis_serial,
                        chassis_snapshots,
                        ChassisNmxCUnreachableReason::ClientCreateFailed,
                    );
                    continue;
                }
            };
            let hello = match nmxc_client.hello(NMX_C_GATEWAY_ID).await {
                Ok(hello) => hello,
                Err(e) => {
                    tracing::warn!(
                        %chassis_serial,
                        endpoint = %endpoint_url,
                        error = %e,
                        "NMX-C hello failed; skipping partition monitor work for this chassis"
                    );
                    Self::queue_null_nvlink_status_observation(
                        &mut pending_null_nvlink_observations,
                        chassis_serial,
                        chassis_snapshots,
                        ChassisNmxCUnreachableReason::HelloFailed,
                    );
                    continue;
                }
            };
            let domain_uuid = match domain_uuid_from_nmx_c_hello(&hello) {
                Ok(domain_uuid) => domain_uuid,
                Err(e) => {
                    tracing::warn!(
                        %chassis_serial,
                        endpoint = %endpoint_url,
                        error = %e,
                        "Failed to parse domain UUID from NMX-C hello; skipping partition monitor work for this chassis"
                    );
                    Self::queue_null_nvlink_status_observation(
                        &mut pending_null_nvlink_observations,
                        chassis_serial,
                        chassis_snapshots,
                        ChassisNmxCUnreachableReason::DomainUuidParseFailed,
                    );
                    continue;
                }
            };

            // Endpoint + component versions for this chassis, so the metric-read failures below log them.
            metrics.nmxc.endpoint = endpoint_url.clone();
            metrics.nmxc.version = hello
                .components_ver
                .iter()
                .map(|kv| format!("{}={}", kv.key, kv.value))
                .collect::<Vec<_>>()
                .join(", ");

            // Filter managed host snapshots, nvlink info, and DB partitions for this chassis.
            let mut managed_host_snapshots_domain: HashMap<MachineId, ManagedHostStateSnapshot> =
                chassis_snapshots
                    .iter()
                    .map(|s| (s.host_snapshot.id, (*s).clone()))
                    .collect();
            let machine_ids_in_domain: Vec<MachineId> =
                managed_host_snapshots_domain.keys().copied().collect();
            let nvlink_info_db_updates = populate_machine_nvlink_info_if_needed(
                &mut machine_nvlink_info,
                &managed_host_snapshots,
                chassis_serial,
                &machine_ids_in_domain,
                domain_uuid,
            );
            if !nvlink_info_db_updates.is_empty() {
                tracing::info!(
                    %chassis_serial,
                    %domain_uuid,
                    machine_ids = ?nvlink_info_db_updates.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                    "Populated machine nvlink_info from NMX-C hello"
                );
                for (machine_id, nvlink_info) in &nvlink_info_db_updates {
                    if let Some(snapshot) = managed_host_snapshots_domain.get_mut(machine_id) {
                        snapshot.host_snapshot.nvlink_info = Some(nvlink_info.clone());
                    }
                }
            }
            let machine_nvlink_info_domain: HashMap<MachineId, Option<MachineNvLinkInfo>> =
                machine_nvlink_info
                    .iter()
                    .filter(|(id, _)| machine_ids_in_domain.contains(id))
                    .map(|(k, v)| (*k, v.clone()))
                    .collect();
            let domain_uuids: HashSet<NvLinkDomainId> = machine_nvlink_info_domain
                .values()
                .filter_map(|info| info.as_ref().map(|info| info.domain_uuid))
                .collect();
            let db_nvl_partitions_domain: Vec<NvlPartition> = db_nvl_partitions
                .iter()
                .filter(|p| domain_uuids.contains(&p.domain_uuid))
                .cloned()
                .collect();

            let num_completed = match self
                .check_partitions_and_apply_nmx_c_operations(
                    nmxc_client.as_mut(),
                    metrics,
                    domain_uuid,
                    CheckPartitionsInput {
                        db_nvl_logical_partitions: db_nvl_logical_partitions.clone(),
                        db_nvl_partitions: db_nvl_partitions_domain,
                        machine_nvlink_info: machine_nvlink_info_domain,
                        managed_host_snapshots: managed_host_snapshots_domain,
                        nvlink_info_db_updates,
                    },
                )
                .await
            {
                Ok(num_completed) => num_completed,
                Err(e) => {
                    tracing::warn!(
                        %chassis_serial,
                        error = %e,
                        "Partition monitor work failed for chassis; queuing null nvlink status observations"
                    );
                    Self::queue_null_nvlink_status_observation(
                        &mut pending_null_nvlink_observations,
                        chassis_serial,
                        chassis_snapshots,
                        ChassisNmxCUnreachableReason::PartitionMonitorWorkFailed,
                    );
                    0
                }
            };
            total_completed_operations += num_completed;
        }

        self.record_null_nvlink_status_observations(&pending_null_nvlink_observations, metrics)
            .await?;

        metrics.num_completed_operations = total_completed_operations;

        Ok(total_completed_operations)
    }

    /// Fetches partition list from NMX-C, checks for needed create/update/delete operations,
    /// executes them, polls for completion, and updates the DB with the results.
    async fn check_partitions_and_apply_nmx_c_operations(
        &self,
        nmxc_client: &mut dyn Nmxc,
        metrics: &mut NvlPartitionMonitorMetrics,
        domain_uuid: NvLinkDomainId,
        input: CheckPartitionsInput,
    ) -> NvLinkManagerResult<usize> {
        let domain = domain_uuid.to_string();
        let CheckPartitionsInput {
            db_nvl_logical_partitions,
            db_nvl_partitions,
            machine_nvlink_info,
            managed_host_snapshots,
            nvlink_info_db_updates,
        } = input;
        let partition_info_list = nmxc_client
            .get_partition_info_list(GetPartitionInfoListRequest {
                context: Some(libnmxc::nmxc_model::Context {
                    context: String::new(),
                }),
                partition_id_list: vec![],
                partition_name_list: vec![],
                gateway_id: NMX_C_GATEWAY_ID.into(),
            })
            .await
            .map_err(|e| {
                metrics.nmxc.connect_error = "Failed to get NMX-C partition info list".to_string();
                NvLinkManagerError::internal(format!(
                    "Failed to get NMX-C partition info list: {e}"
                ))
            })?
            .partition_info_list;

        record_domain_health(
            &mut metrics.nmxc.partition_health,
            &domain,
            &PARTITION_HEALTH_STATES,
            aggregate_partition_health(&partition_info_list),
        );
        log_partition_multicast_usage(domain_uuid, &partition_info_list);

        // GPU health for metrics, best-effort: log and skip on failure, never blocking reconciliation.
        match nmxc_client
            .get_gpu_info_list(GetGpuInfoListRequest {
                context: Some(libnmxc::nmxc_model::Context {
                    context: String::new(),
                }),
                attr: libnmxc::nmxc_model::GpuAttr::NmxGpuAttrAll as i32,
                num_gpus: 0,
                loc: None,
                partition_id: None,
                gateway_id: NMX_C_GATEWAY_ID.into(),
                gpu_health: 0,
            })
            .await
        {
            Ok(resp) => {
                let counts = aggregate_gpu_health(&resp.gpu_info_list);
                record_domain_health(
                    &mut metrics.nmxc.gpu_health,
                    &domain,
                    &GPU_HEALTH_STATES,
                    counts,
                );
            }
            Err(e) => tracing::warn!(
                %domain_uuid,
                endpoint = %metrics.nmxc.endpoint,
                version = %metrics.nmxc.version,
                error = %e,
                "NMX-C GetGpuInfoList failed; GPU health metrics missing for this domain this iteration"
            ),
        }

        // Compute-node health for metrics, best-effort (same handling as above).
        match nmxc_client
            .get_compute_node_info_list(GetComputeNodeInfoListRequest {
                context: Some(libnmxc::nmxc_model::Context {
                    context: String::new(),
                }),
                loc_list: vec![],
                gateway_id: NMX_C_GATEWAY_ID.into(),
            })
            .await
        {
            Ok(resp) => {
                let counts = aggregate_compute_node_health(&resp.node_info_list);
                record_domain_health(
                    &mut metrics.nmxc.compute_node_health,
                    &domain,
                    &NODE_HEALTH_STATES,
                    counts,
                );
            }
            Err(e) => tracing::warn!(
                %domain_uuid,
                endpoint = %metrics.nmxc.endpoint,
                version = %metrics.nmxc.version,
                error = %e,
                "NMX-C GetComputeNodeInfoList failed; compute-node health metrics missing for this domain this iteration"
            ),
        }

        let mut partition_processing_context = PartitionProcessingContext::new(
            partition_info_list,
            db_nvl_logical_partitions.clone(),
            db_nvl_partitions,
            machine_nvlink_info,
        );

        // Check if any partitions need to be created, updated, or deleted.
        let observations = self.check_nv_link_partitions(
            &mut partition_processing_context,
            managed_host_snapshots,
            metrics,
        )?;

        self.record_nvlink_status_observation(observations).await?;

        let nmx_c_operations = partition_processing_context.nmx_c_operations;

        if !nmx_c_operations.is_empty() {
            tracing::debug!("NMX-C operations: {:?}", nmx_c_operations);
        }

        // Execute any NMX-C operations and collect successful completions.
        let completed_nmx_c_operations = self
            .execute_nmx_c_operations(nmxc_client, nmx_c_operations, metrics)
            .await?;

        if !completed_nmx_c_operations.is_empty() {
            tracing::debug!(
                "Completed NMX-C operations: {:?}",
                completed_nmx_c_operations
            );
        }

        let num_completed_operations = completed_nmx_c_operations
            .values()
            .map(|ops| ops.len())
            .sum::<usize>();

        // Get a fresh list of partitions from NMX-C.
        let partition_info_list = nmxc_client
            .get_partition_info_list(GetPartitionInfoListRequest {
                context: Some(libnmxc::nmxc_model::Context {
                    context: String::new(),
                }),
                partition_id_list: vec![],
                partition_name_list: vec![],
                gateway_id: NMX_C_GATEWAY_ID.into(),
            })
            .await
            .map_err(|e| {
                metrics.nmxc.connect_error =
                    "Failed to get NMX-C partition info list when updating db".to_string();
                NvLinkManagerError::internal(format!(
                    "Failed to get NMX-C partition info list: {e}"
                ))
            })?
            .partition_info_list;
        let nmx_c_partitions: HashMap<String, PartitionInfo> = partition_info_list
            .into_iter()
            .map(|p| (nmx_c_partition_id_string(&p), p))
            .collect();

        // Update db with the operations that completed successfully.
        let mut txn = self.db_pool.txn_begin().await?;
        for (machine_id, nvlink_info) in nvlink_info_db_updates {
            machine::update_nvlink_info(&mut txn, &machine_id, nvlink_info).await?;
        }
        self.update_db_with_nmx_c_operations(
            &mut txn,
            completed_nmx_c_operations,
            &db_nvl_logical_partitions,
            &nmx_c_partitions,
        )
        .await?;
        txn.commit().await?;

        Ok(num_completed_operations)
    }

    // Check the passed NvLink partition "observations" (physical partition info from NMX-C supplemented by physical and logical partition info from DB)
    // against the instance config and generate NMX-C operations to bring the observations into alignment with the config.
    fn check_nv_link_partitions(
        &self,
        partition_ctx: &mut PartitionProcessingContext,
        mh_snapshots: HashMap<MachineId, ManagedHostStateSnapshot>,
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<HashMap<MachineId, MachineNvLinkStatusObservation>> {
        let mut machine_gpu_statuses = HashMap::new();

        // If the default partition is present, enqueue a removal operation and return early.
        // no observations will be generated
        if partition_ctx.enqueue_nmx_c_default_partition_removal_if_present() {
            return Ok(machine_gpu_statuses);
        }

        for mh in mh_snapshots.values() {
            metrics.num_machines_scanned += 1;
            let Some(instance) = &mh.instance else {
                // For machines with no instance, check if machine is in admin network and any cleanup is required
                let _ = self.check_machine_and_handle_gpu_removals(mh, partition_ctx);
                continue;
            };
            metrics.num_instances_scanned += 1;
            let mut instance_gpu_statuses = Vec::new();
            let Some(info) = partition_ctx
                .machine_nvlink_info
                .get(&instance.machine_id)
                .cloned()
            else {
                tracing::warn!("No nvlink_info found for machine {}", instance.machine_id);
                machine_gpu_statuses.insert(
                    instance.machine_id,
                    MachineNvLinkStatusObservation {
                        observed_at: Utc::now(),
                        nvlink_gpus: instance_gpu_statuses,
                    },
                );
                continue;
            };
            match info {
                Some(info) => {
                    for nvlink_gpu in &info.gpus {
                        metrics.num_gpus_scanned += 1;
                        let device_instance: u32 = nvlink_gpu.device_id as u32 - 1;
                        let instance_gpu_config = &instance
                            .config
                            .nvlink
                            .gpu_configs
                            .iter()
                            .find(|gpu| gpu.device_instance == device_instance);
                        let mut gpu_status_observation = MachineNvLinkGpuStatusObservation {
                            device_instance,
                            domain_id: info.domain_uuid,
                            gpu_id: nvlink_gpu.guid.to_string(),
                            guid: nvlink_gpu.guid,
                            ..Default::default()
                        };
                        let mut gpu_ctx = GpuProcessingContext {
                            gpu_guid: nvlink_gpu.guid,
                            domain_uuid: info.domain_uuid,
                            ..Default::default()
                        };

                        let nmxc_partition = partition_ctx
                            .gpu_to_partition_map
                            .get(&nvlink_gpu.guid)
                            .cloned();

                        // Decide on what action the monitor will take with this GPU, and finish building the gpu_ctx.
                        let gpu_action: GpuAction;
                        if let Some(nmxc_partition) = nmxc_partition {
                            let partition_id = nmxc_partition
                                .partition_id
                                .map(|id| id.partition_id)
                                .unwrap_or_default();
                            match partition_ctx.get_db_partition_info(partition_id) {
                                Some((
                                    db_partition_id,
                                    db_logical_partition_id,
                                    db_partition_name,
                                    db_partition_nmx_c_id,
                                )) => {
                                    if let Some(gpu_config) = instance_gpu_config {
                                        gpu_ctx.logical_partition_id =
                                            gpu_config.logical_partition_id;
                                        if db_logical_partition_id.is_none() {
                                            // How can this happen?
                                            tracing::error!(
                                                "No logical partition ID associated with physical partition {:?}",
                                                partition_id.to_string()
                                            );
                                            continue;
                                        } else if gpu_config.logical_partition_id
                                            != db_logical_partition_id
                                        {
                                            // This covers both the case where the tenant has asked for the GPU to be removed from the partition
                                            // (i.e. gpu_config.logical_partition_id is None), and the case where the GPU is in logical partition
                                            // A and the tenant wants it to be in logical partition B. In the latter case, we need to remove the GPU
                                            // from the current partition before adding it to the new one.
                                            gpu_action = GpuAction::RemoveFromPartition;
                                        } else {
                                            gpu_action = GpuAction::NoOp;
                                        }
                                    } else {
                                        // There is no gpu config, which means the tenant does not want it to be part of a partition.
                                        gpu_action = GpuAction::RemoveFromPartition;
                                    }
                                    gpu_ctx.logical_partition_id = db_logical_partition_id;
                                    gpu_ctx.partition_id = db_partition_id;
                                    gpu_ctx.partition_name = db_partition_name;
                                    gpu_ctx.partition_nmx_c_id = db_partition_nmx_c_id;

                                    // Update the observation.
                                    gpu_status_observation.logical_partition_id =
                                        db_logical_partition_id;
                                    gpu_status_observation.partition_id = db_partition_id;
                                }
                                None => {
                                    // TODO: should we add the partition NMX-C ID to the status obs?
                                    if is_nmx_c_default_partition(&nmxc_partition)
                                        || is_gpu_in_tray_default_partition(
                                            &nmxc_partition,
                                            nvlink_gpu.slot_id,
                                        )
                                    {
                                        if instance_gpu_config.is_some() {
                                            tracing::info!(
                                                "Removing configured GPU {} in machine {} and instance {} from NMX-C holding partition {}",
                                                nvlink_gpu.guid,
                                                instance.machine_id,
                                                instance.id,
                                                partition_id
                                            );
                                            gpu_action = GpuAction::RemoveFromUnknownPartition;
                                            gpu_ctx.partition_nmx_c_id =
                                                nmxc_partition.partition_id.unwrap_or_default();
                                        } else {
                                            // An omitted GPU config means this GPU should remain in its holding partition.
                                            gpu_action = GpuAction::NoOp;
                                        }
                                    } else {
                                        // The monitor cannot safely preserve membership in an unrecognized partition.
                                        tracing::warn!(
                                            "Removing GPU {} from unknown partition with NMX-C ID {}",
                                            nvlink_gpu.guid,
                                            partition_id
                                        );
                                        gpu_action = GpuAction::RemoveFromUnknownPartition;
                                        gpu_ctx.partition_nmx_c_id =
                                            libnmxc::nmxc_model::PartitionId { partition_id };
                                    }
                                }
                            }
                        } else {
                            // This GPU isn't in a partition yet.
                            if let Some(gpu_config) = instance_gpu_config
                                && let Some(logical_partition_id) = gpu_config.logical_partition_id
                            {
                                // Tenant has asked to put it in a partition
                                gpu_action = GpuAction::AddToPartition;
                                gpu_ctx.logical_partition_id = Some(logical_partition_id);
                            } else {
                                gpu_action = GpuAction::NoOp;
                            }
                        }

                        instance_gpu_statuses.push(gpu_status_observation);

                        if let Some(logical_partition_id) = gpu_ctx.logical_partition_id
                            && !partition_ctx.validate_logical_partition(&logical_partition_id)
                        {
                            tracing::warn!(
                                machine_id = %instance.machine_id,
                                gpu_guid = %gpu_ctx.gpu_guid,
                                logical_partition_id = %logical_partition_id,
                                "Logical partition is marked as deleted, skipping GPU action"
                            );
                            continue;
                        }

                        match gpu_action {
                            GpuAction::AddToPartition => {
                                // Check if there are other physical partitions in the logical partition
                                if let Some(partition) = partition_ctx
                                    .db_nvl_partitions
                                    .values()
                                    .find(|p| {
                                        p.logical_partition_id == gpu_ctx.logical_partition_id
                                            && p.domain_uuid == info.domain_uuid
                                            && u32::try_from(p.nmx_c_partition_id).ok().is_some_and(
                                                |partition_id| {
                                                    partition_ctx.nmx_c_partitions.contains_key(
                                                        &libnmxc::nmxc_model::PartitionId {
                                                            partition_id,
                                                        },
                                                    )
                                                },
                                            )
                                    })
                                    .cloned()
                                {
                                    // Add to existing partition in the same domain
                                    if let Err(e) = partition_ctx
                                        .handle_gpu_addition_existing_partition(
                                            &gpu_ctx, &partition,
                                        )
                                    {
                                        tracing::error!(
                                            gpu_guid = %gpu_ctx.gpu_guid,
                                            machine_id = %instance.machine_id,
                                            "Failed to handle GPU addition to existing partition: {e}"
                                        );
                                    }
                                } else {
                                    // Create new partition in a different domain
                                    if let Err(e) =
                                        partition_ctx.handle_gpu_addition_new_partition(&gpu_ctx)
                                    {
                                        tracing::error!(
                                            gpu_guid = %gpu_ctx.gpu_guid,
                                            machine_id = %instance.machine_id,
                                            "Failed to handle GPU addition to new partition: {e}"
                                        );
                                    }
                                }
                            }
                            GpuAction::RemoveFromPartition => {
                                let Some(gpus_to_keep) = partition_ctx
                                    .get_gpus_to_keep_after_removal(
                                        gpu_ctx.logical_partition_id,
                                        &gpu_ctx.partition_nmx_c_id,
                                        gpu_ctx.gpu_guid,
                                        &instance.machine_id,
                                        device_instance,
                                    )
                                else {
                                    continue;
                                };

                                if let Err(e) =
                                    partition_ctx.handle_gpu_removal(&gpu_ctx, gpus_to_keep)
                                {
                                    tracing::error!(
                                        gpu_guid = %gpu_ctx.gpu_guid,
                                        machine_id = %instance.machine_id,
                                        "Failed to handle GPU removal from partition: {e}"
                                    );
                                }
                            }
                            GpuAction::RemoveFromUnknownPartition => {
                                if let Some(gpus_to_keep) = partition_ctx
                                    .get_gpus_to_keep_in_unknown_partition_after_removal(
                                        &gpu_ctx.partition_nmx_c_id,
                                        gpu_ctx.gpu_guid,
                                        &instance.machine_id,
                                        device_instance,
                                    )
                                {
                                    if let Err(e) = partition_ctx
                                        .handle_gpu_removal_from_unknown_partition(
                                            &gpu_ctx.partition_nmx_c_id,
                                            gpu_ctx.gpu_guid,
                                            gpus_to_keep,
                                        )
                                    {
                                        tracing::error!(
                                            gpu_guid = %gpu_ctx.gpu_guid,
                                            machine_id = %instance.machine_id,
                                            "Failed to handle GPU removal from unknown partition: {e}"
                                        );
                                    }
                                } else {
                                    tracing::error!(
                                        gpu_guid = %gpu_ctx.gpu_guid,
                                        machine_id = %instance.machine_id,
                                        "No default partition found with NMX-C ID = {}",
                                        gpu_ctx.partition_nmx_c_id.partition_id
                                    );
                                    continue;
                                }
                            }
                            GpuAction::NoOp => (),
                        }
                    }
                }
                None => {
                    tracing::warn!("No nvlink_info found for machine {}", instance.machine_id);
                }
            }
            // Now we've generated the operations, record an observation.
            let observation = MachineNvLinkStatusObservation {
                observed_at: Utc::now(),
                nvlink_gpus: instance_gpu_statuses,
            };
            machine_gpu_statuses.insert(instance.machine_id, observation);
        }

        self.record_nvlink_config_pending_durations(&mh_snapshots, &machine_gpu_statuses, metrics);

        metrics.num_machine_nvl_status_updates = machine_gpu_statuses.len();

        // Add all default partition removals to the normal list so they get executed.
        for (_partition_nmx_c_id, operations) in
            partition_ctx.unknown_partition_removal_operations.iter()
        {
            for operation in operations {
                partition_ctx
                    .nmx_c_operations
                    .entry(NvLinkLogicalPartitionId::default())
                    .and_modify(|ops| {
                        ops.push(operation.clone());
                    })
                    .or_insert(vec![operation.clone()]);
            }
        }
        for (_partition_nmx_c_id, operation) in
            partition_ctx.unknown_partition_addition_operations.iter()
        {
            partition_ctx
                .nmx_c_operations
                .entry(NvLinkLogicalPartitionId::default())
                .and_modify(|ops| {
                    ops.push(operation.clone());
                })
                .or_insert(vec![operation.clone()]);
        }
        for (_, operation) in
            std::mem::take(&mut partition_ctx.pending_tray_partition_creates_by_slot)
        {
            partition_ctx
                .nmx_c_operations
                .entry(NvLinkLogicalPartitionId::default())
                .or_default()
                .push(operation);
        }
        Ok(machine_gpu_statuses)
    }

    /// Records time from nvlink_config_version for instances currently in Pending (time spent in Pending).
    fn record_nvlink_config_pending_durations(
        &self,
        mh_snapshots: &HashMap<MachineId, ManagedHostStateSnapshot>,
        machine_gpu_statuses: &HashMap<MachineId, MachineNvLinkStatusObservation>,
        metrics: &mut NvlPartitionMonitorMetrics,
    ) {
        for (machine_id, observation) in machine_gpu_statuses {
            let Some(mh) = mh_snapshots.get(machine_id) else {
                continue;
            };
            let Some(instance) = &mh.instance else {
                continue;
            };
            if instance.config.nvlink.gpu_configs.is_empty() {
                continue;
            }
            let nvlink_status = InstanceNvLinkStatus::from_config_and_observation(
                Versioned::new(&instance.config.nvlink, instance.nvlink_config_version),
                Some(observation),
            );
            if nvlink_status.configs_synced == SyncState::Pending {
                let duration_ms = (Utc::now() - instance.nvlink_config_version.timestamp())
                    .num_milliseconds()
                    .max(0) as f64;
                metrics.nvlink_config_apply_durations_ms.push(duration_ms);
            }
        }
    }

    // Managed hosts that are no longer an instance should not have GPUs in tenant or NMX-C default
    // partitions: move every GPU into its tray default partition (`tray_partition_{slot_id}`).
    pub fn check_machine_and_handle_gpu_removals(
        &self,
        mh: &ManagedHostStateSnapshot,
        partition_ctx: &mut PartitionProcessingContext,
    ) -> NvLinkManagerResult<()> {
        // If not in admin-network mode, skip processing. GPUs should stay
        // attached to tenant partitions, but zero-DPU hosts are always
        // considered admin network (since they don't have a DPU to put them
        // in an overlay network). In other words, zero-DPU hosts get GPU
        // removals, but hosts with DPUs in tenant networks don't.
        if !mh.use_admin_network() {
            return Ok(());
        }

        if let Some(nvlink_info) = &mh.host_snapshot.nvlink_info {
            for gpu in &nvlink_info.gpus {
                let nmxc_partition = match partition_ctx.gpu_to_partition_map.get(&gpu.guid) {
                    // GPU is in a partition, so we need to remove it from the partition.
                    Some(p) => p,
                    None => {
                        // GPU is not in any NMX-C partition; place it in the tray default partition
                        // (named from this GPU's slot id), creating that partition if needed.
                        partition_ctx.ensure_gpu_enqueued_into_tray_partition(
                            &mh.host_snapshot.id,
                            nvlink_info.domain_uuid,
                            gpu,
                        )?;
                        continue;
                    }
                };

                if is_gpu_in_tray_default_partition(nmxc_partition, gpu.slot_id) {
                    continue;
                }

                let partition_id = nmxc_partition
                    .partition_id
                    .map(|id| id.partition_id)
                    .unwrap_or_default();

                if let Some((
                    db_partition_id,
                    db_logical_partition_id,
                    db_partition_name,
                    db_partition_nmx_c_id,
                )) = partition_ctx.get_db_partition_info(partition_id)
                {
                    let gpu_ctx = GpuProcessingContext {
                        gpu_guid: gpu.guid,
                        domain_uuid: nvlink_info.domain_uuid,
                        partition_id: db_partition_id,
                        partition_name: db_partition_name.clone(),
                        logical_partition_id: db_logical_partition_id,
                        partition_nmx_c_id: db_partition_nmx_c_id,
                    };

                    let Some(gpus_to_keep) = partition_ctx.get_gpus_to_keep_after_removal(
                        db_logical_partition_id,
                        &gpu_ctx.partition_nmx_c_id,
                        gpu.guid,
                        &mh.host_snapshot.id,
                        gpu.device_id.try_into().unwrap(),
                    ) else {
                        continue;
                    };

                    let logical_id = db_logical_partition_id.unwrap_or_default();
                    tracing::info!(
                        machine_id = %mh.host_snapshot.id,
                        gpu_guid = %gpu.guid,
                        logical_partition_id = %logical_id,
                        gpus_to_keep = ?gpus_to_keep,
                        "Handling GPU removal from partition for machine in admin network"
                    );
                    partition_ctx.handle_gpu_removal(&gpu_ctx, gpus_to_keep)?;
                } else {
                    let Some(pid_struct) = nmxc_partition.partition_id else {
                        tracing::warn!(
                            machine_id = %mh.host_snapshot.id,
                            gpu_guid = %gpu.guid,
                            nmx_c_partition_id = partition_id,
                            "NMX-C partition has no partition_id; cannot remove GPU before tray move"
                        );
                        continue;
                    };
                    let Some(gpus_to_keep) = partition_ctx
                        .get_gpus_to_keep_in_unknown_partition_after_removal(
                            &pid_struct,
                            gpu.guid,
                            &mh.host_snapshot.id,
                            gpu.device_id.try_into().unwrap(),
                        )
                    else {
                        continue;
                    };
                    tracing::info!(
                        machine_id = %mh.host_snapshot.id,
                        gpu_guid = %gpu.guid,
                        nmx_c_partition_id = pid_struct.partition_id,
                        gpus_to_keep = ?gpus_to_keep,
                        "Handling GPU removal from NMX-C partition without DB row (admin network)"
                    );
                    partition_ctx.handle_gpu_removal_from_unknown_partition(
                        &pid_struct,
                        gpu.guid,
                        gpus_to_keep,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Queues machines on `chassis_serial` for a batched null `nvlink_status_observation` write.
    ///
    /// Entries are flushed in one transaction by [`Self::record_null_nvlink_status_observations`].
    fn queue_null_nvlink_status_observation(
        pending: &mut Vec<PendingNullNvlinkObservation>,
        chassis_serial: &str,
        chassis_snapshots: &[&ManagedHostStateSnapshot],
        reason: ChassisNmxCUnreachableReason,
    ) {
        let machine_ids: Vec<MachineId> = chassis_snapshots
            .iter()
            .map(|snapshot| snapshot.host_snapshot.id)
            .collect();
        if machine_ids.is_empty() {
            return;
        }
        pending.push(PendingNullNvlinkObservation {
            chassis_serial: chassis_serial.to_string(),
            reason,
            machine_ids,
        });
    }

    /// Clears `nvlink_status_observation` for all queued chassis in one transaction and updates metrics.
    async fn record_null_nvlink_status_observations(
        &self,
        pending: &[PendingNullNvlinkObservation],
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<()> {
        if pending.is_empty() {
            return Ok(());
        }

        for entry in pending {
            *metrics
                .num_nmx_c_unreachable_chassis
                .entry(entry.reason)
                .or_insert(0) += 1;
        }

        let machine_ids: Vec<MachineId> = pending
            .iter()
            .flat_map(|entry| entry.machine_ids.iter().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let mut obs_txn = self.db_pool.begin().await.map_err(|e| {
            NvLinkManagerError::internal(format!(
                "Failed to create transaction for clearing nvlink status observations: {e}"
            ))
        })?;
        machine::clear_nvlink_status_observations(&mut obs_txn, &machine_ids).await?;
        obs_txn.commit().await.map_err(|e| {
            NvLinkManagerError::internal(format!(
                "Failed to commit transaction for clearing nvlink status observations: {e}"
            ))
        })?;

        for entry in pending {
            tracing::info!(
                chassis_serial = %entry.chassis_serial,
                reason = ?entry.reason,
                machine_ids = ?entry.machine_ids,
                "Posted null nvlink status observations because NMX-C is unreachable for chassis"
            );
        }
        Ok(())
    }

    // Use a separate transaction to record the observations to avoid blocking the main transaction when we poll NMX-C.
    async fn record_nvlink_status_observation(
        &self,
        observations: HashMap<MachineId, MachineNvLinkStatusObservation>,
    ) -> NvLinkManagerResult<()> {
        let mut obs_txn = self.db_pool.begin().await.map_err(|e| {
            NvLinkManagerError::internal(format!(
                "Failed to create transaction for nvlink status observation: {e}"
            ))
        })?;
        for (machine_id, observations) in observations {
            db::machine::update_nvlink_status_observation(&mut obs_txn, &machine_id, &observations)
                .await?;
        }
        obs_txn.commit().await.map_err(|e| {
            NvLinkManagerError::internal(format!(
                "Failed to commit transaction for nvlink status observation: {e}"
            ))
        })?;
        Ok(())
    }

    async fn execute_nmx_c_operations(
        &self,
        nmxc_client: &mut dyn Nmxc,
        nmx_c_operations: HashMap<NvLinkLogicalPartitionId, Vec<NmxcPartitionOperation>>,
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<HashMap<NvLinkLogicalPartitionId, Vec<NmxcPartitionOperation>>> {
        let mut completed_operations: HashMap<
            NvLinkLogicalPartitionId,
            Vec<NmxcPartitionOperation>,
        > = HashMap::new();

        for (logical_partition_id, operations) in nmx_c_operations {
            for operation in operations {
                let start_time = std::time::Instant::now();
                let success = match &operation.operation_type {
                    NmxcPartitionOperationType::Create => {
                        let name = if operation.name.starts_with("tray_partition_") {
                            operation.name.chars().take(240).collect::<String>()
                        } else {
                            let name = format!(
                                "{}{}",
                                logical_partition_id,
                                operation
                                    .gpu_uids
                                    .iter()
                                    .map(|u| u.to_string())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            );
                            name.chars().take(240).collect::<String>()
                        };
                        let request = nmx_c_create_partition_request(
                            name.clone(),
                            &operation.gpu_uids,
                            NMX_C_PARTITION_MULTICAST_GROUPS_LIMIT,
                        );
                        match nmxc_client.create_partition(request.clone()).await {
                            Err(e) if e.is_nmx_resource_exhausted() => {
                                tracing::info!(
                                    %logical_partition_id,
                                    partition_name = %name,
                                    create_partition_request = ?request,
                                    "NMX-C create partition returned NMX_ST_RESOURCE_EXHAUSTED; retrying with multicast_groups_limit=0"
                                );
                                let retry_request =
                                    nmx_c_create_partition_request(name, &operation.gpu_uids, 0);
                                match nmxc_client.create_partition(retry_request).await {
                                    Ok(_) => true,
                                    Err(e) => {
                                        tracing::warn!(
                                            %logical_partition_id,
                                            "Failed to retry create partition on NMX-C with multicast_groups_limit=0: {e}"
                                        );
                                        false
                                    }
                                }
                            }
                            Ok(_) => true,
                            Err(e) => {
                                tracing::warn!(
                                    %logical_partition_id,
                                    create_partition_request = ?request,
                                    "Failed to issue create partition to NMX-C, continuing with other operations: {e}"
                                );
                                false
                            }
                        }
                    }
                    NmxcPartitionOperationType::Remove(nmx_c_partition_id) => {
                        let request = libnmxc::nmxc_model::DeletePartitionRequest {
                            context: None,
                            partition_id: Some(libnmxc::nmxc_model::PartitionId {
                                partition_id: *nmx_c_partition_id,
                            }),
                            gateway_id: NMX_C_GATEWAY_ID.into(),
                            name: String::new(),
                        };
                        match nmxc_client.delete_partition(request).await {
                            Ok(_) => true,
                            Err(e) => {
                                tracing::warn!(
                                    %logical_partition_id,
                                    %nmx_c_partition_id,
                                    "Failed to issue delete partition to NMX-C, continuing with other operations: {e}"
                                );
                                false
                            }
                        }
                    }
                    NmxcPartitionOperationType::RemoveUnknownPartition(nmx_c_partition_id) => {
                        let request = libnmxc::nmxc_model::DeletePartitionRequest {
                            context: None,
                            partition_id: Some(libnmxc::nmxc_model::PartitionId {
                                partition_id: *nmx_c_partition_id,
                            }),
                            gateway_id: NMX_C_GATEWAY_ID.into(),
                            name: String::new(),
                        };
                        match nmxc_client.delete_partition(request).await {
                            Ok(_) => true,
                            Err(e) => {
                                return Err(NvLinkManagerError::internal(format!(
                                    "Failed to delete default partition: {e}"
                                )));
                            }
                        }
                    }
                    NmxcPartitionOperationType::Update(nmx_c_partition_id) => {
                        let pid = libnmxc::nmxc_model::PartitionId {
                            partition_id: *nmx_c_partition_id,
                        };
                        let list_req = libnmxc::nmxc_model::GetPartitionInfoListRequest {
                            context: None,
                            partition_id_list: vec![pid],
                            partition_name_list: vec![],
                            gateway_id: NMX_C_GATEWAY_ID.into(),
                        };
                        match nmxc_client.get_partition_info_list(list_req).await {
                            Err(e) => {
                                tracing::warn!(
                                    %logical_partition_id,
                                    %nmx_c_partition_id,
                                    "Failed to get partition info from NMX-C before update: {e}"
                                );
                                false
                            }
                            Ok(resp) => {
                                let current_uids = resp
                                    .partition_info_list
                                    .into_iter()
                                    .find(|info| {
                                        info.partition_id
                                            .as_ref()
                                            .map(|id| id.partition_id == *nmx_c_partition_id)
                                            .unwrap_or(false)
                                    })
                                    .map(|info| info.gpu_uid_list)
                                    .unwrap_or_default();

                                let desired: HashSet<u64> =
                                    operation.gpu_uids.iter().copied().collect();
                                let current: HashSet<u64> = current_uids.iter().copied().collect();
                                let to_remove: Vec<u64> =
                                    current.difference(&desired).copied().collect();
                                let to_add: Vec<u64> =
                                    desired.difference(&current).copied().collect();

                                let mut ok = true;
                                if !to_remove.is_empty() {
                                    let req = libnmxc::nmxc_model::UpdatePartitionRequest {
                                        context: None,
                                        partition_id: Some(pid),
                                        location_list: vec![],
                                        gpu_uid: to_remove,
                                        gateway_id: NMX_C_GATEWAY_ID.into(),
                                        name: String::new(),
                                        reroute: true,
                                    };
                                    match nmxc_client.remove_gpus_from_partition(req).await {
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!(
                                                %logical_partition_id,
                                                %nmx_c_partition_id,
                                                "Failed to remove GPUs from partition on NMX-C: {e}"
                                            );
                                            ok = false;
                                        }
                                    }
                                }
                                if ok && !to_add.is_empty() {
                                    let req = libnmxc::nmxc_model::UpdatePartitionRequest {
                                        context: None,
                                        partition_id: Some(pid),
                                        location_list: vec![],
                                        gpu_uid: to_add,
                                        gateway_id: NMX_C_GATEWAY_ID.into(),
                                        name: String::new(),
                                        reroute: true,
                                    };
                                    match nmxc_client.add_gpus_to_partition(req).await {
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!(
                                                %logical_partition_id,
                                                %nmx_c_partition_id,
                                                "Failed to add GPUs to partition on NMX-C: {e}"
                                            );
                                            ok = false;
                                        }
                                    }
                                }
                                ok
                            }
                        }
                    }
                };
                let applied_change = AppliedChange {
                    operation: operation.operation_type.clone().into(),
                    status: if success {
                        NmxcMetricOperationStatus::Completed
                    } else {
                        NmxcMetricOperationStatus::Failed
                    },
                };
                *metrics
                    .applied_changes
                    .entry(applied_change.clone())
                    .or_default() += 1;
                metrics
                    .operation_latencies
                    .entry(applied_change)
                    .or_default()
                    .push(start_time.elapsed());
                if success {
                    completed_operations
                        .entry(logical_partition_id)
                        .or_default()
                        .push(operation);
                }
            }
        }
        Ok(completed_operations)
    }

    async fn update_db_with_nmx_c_operations(
        &self,
        txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        completed_nmx_c_operations: HashMap<NvLinkLogicalPartitionId, Vec<NmxcPartitionOperation>>,
        db_nvl_logical_partitions: &[LogicalPartition],
        nmx_c_partitions: &HashMap<String, PartitionInfo>,
    ) -> NvLinkManagerResult<()> {
        for (logical_partition_id, operations) in completed_nmx_c_operations {
            for operation in operations {
                match operation.operation_type {
                    NmxcPartitionOperationType::Create => {
                        let matching_partition = match nmx_c_partitions.values().find(|p| {
                            let p_uids: HashSet<u64> = p.gpu_uid_list.iter().copied().collect();
                            let op_uids: HashSet<u64> =
                                operation.gpu_uids.iter().copied().collect();
                            p_uids == op_uids
                        }) {
                            Some(p) => p,
                            None => {
                                tracing::error!(
                                    "NMX-C partition not found for name {}",
                                    operation.name
                                );
                                continue;
                            }
                        };
                        let Some(nmx_c_partition_id) = matching_partition
                            .partition_id
                            .as_ref()
                            .map(|id| id.partition_id)
                        else {
                            tracing::error!(
                                "NMX-C partition ID not found for name {}",
                                operation.name
                            );
                            continue;
                        };
                        let Ok(nmx_c_partition_id) = i32::try_from(nmx_c_partition_id) else {
                            tracing::error!(
                                "NMX-C partition ID does not fit in database column for name {}",
                                operation.name
                            );
                            continue;
                        };

                        if operation.name.starts_with("tray_partition_") {
                            tracing::debug!(
                                logical_partition_id = %logical_partition_id,
                                name = %operation.name,
                                "Skipping nvl_partition DB insert for tray partition"
                            );
                            continue;
                        }
                        // Create the nvl partition in the database
                        let new_partition = model::nvl_partition::NewNvlPartition {
                            id: NvLinkPartitionId::new(),
                            logical_partition_id,
                            name: NvlPartitionName::try_from(operation.name.clone())?,
                            domain_uuid: operation.domain_uuid.unwrap_or_default(),
                            nmx_c_partition_id,
                        };
                        let _partition = db::nvl_partition::create(&new_partition, txn).await?;
                    }
                    NmxcPartitionOperationType::Remove(_) => {
                        db::nvl_partition::final_delete(
                            operation.db_partition_id.unwrap_or_default(),
                            txn,
                        )
                        .await?;
                    }
                    NmxcPartitionOperationType::Update(_) => {
                        // Partition membership is not tracked in the partitions table. The status observation of the
                        // added/removed GPUs will be updated.
                    }
                    NmxcPartitionOperationType::RemoveUnknownPartition(_) => {
                        // No-op, since default partition membership is not tracked in the partitions table. The status observation of the
                        // added/removed GPUs will be updated.
                    }
                }
            }
        }

        // walk the logical partition list and check if any logical partitions need to be cleaned up
        for lp in db_nvl_logical_partitions {
            if model::nvl_logical_partition::is_marked_as_deleted(lp) {
                tracing::info!(logical_partition_id = %lp.id, "Deleting logical partition");
                db::nvl_logical_partition::final_delete(lp.id, txn).await?;
            }
        }

        Ok(())
    }

    async fn load_mnnvl_managed_host_snapshots(
        &self,
        txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> NvLinkManagerResult<HashMap<MachineId, ManagedHostStateSnapshot>> {
        let mnvvl_machine_ids = find_machine_ids(
            txn.as_mut(),
            MachineSearchConfig {
                mnnvl_only: true,
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;
        load_by_machine_ids(
            txn.as_mut(),
            mnvvl_machine_ids.as_slice(),
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: true,
                host_health_config: self.host_health,
            },
        )
        .await
        .map_err(NvLinkManagerError::from)
    }
}

// One label per value of the NMX-C health enums defined in `nmx_c.proto`. The `*_health_label`
// matches below are compile-checked against those enums, so a new health variant won't build
// until you add its match arm there AND its label to the matching list here.
/// All GPU health labels, used to zero-seed every state per domain each iteration.
const GPU_HEALTH_STATES: [&str; 5] = ["healthy", "degraded", "no_nvlink", "degraded_bw", "unknown"];
/// All compute-node health labels, used to zero-seed every state per domain each iteration.
const NODE_HEALTH_STATES: [&str; 4] = ["healthy", "degraded", "unhealthy", "unknown"];
/// All partition health labels, used to zero-seed every state per domain each iteration.
const PARTITION_HEALTH_STATES: [&str; 5] =
    ["healthy", "degraded_bw", "degraded", "unhealthy", "unknown"];

/// Maps an NMX-C `GpuHealth` enum value to a metric label. Matching on the generated enum (not raw
/// ints) keeps labels correct if the proto renumbers, and a new variant fails to compile until handled.
fn gpu_health_label(h: i32) -> &'static str {
    use libnmxc::nmxc_model::GpuHealth::{self, *};
    match GpuHealth::try_from(h) {
        Ok(NmxGpuHealthHealthy) => "healthy",
        Ok(NmxGpuHealthDegraded) => "degraded",
        Ok(NmxGpuHealthNoNvlink) => "no_nvlink",
        Ok(NmxGpuHealthDegradedBw) => "degraded_bw",
        Ok(NmxGpuHealthUnknown) | Err(_) => "unknown",
    }
}

/// Maps an NMX-C `ComputeNodeHealth` enum value to a metric label (matched on the generated enum).
fn node_health_label(h: i32) -> &'static str {
    use libnmxc::nmxc_model::ComputeNodeHealth::{self, *};
    match ComputeNodeHealth::try_from(h) {
        Ok(NmxComputeNodeHealthHealthy) => "healthy",
        Ok(NmxComputeNodeHealthDegraded) => "degraded",
        Ok(NmxComputeNodeHealthUnhealthy) => "unhealthy",
        Ok(NmxComputeNodeHealthUnknown) | Err(_) => "unknown",
    }
}

/// Maps an NMX-C `PartitionHealth` enum value to a metric label (matched on the generated enum).
fn partition_health_label(h: i32) -> &'static str {
    use libnmxc::nmxc_model::PartitionHealth::{self, *};
    match PartitionHealth::try_from(h) {
        Ok(NmxPartitionHealthHealthy) => "healthy",
        Ok(NmxPartitionHealthDegradedBandwidth) => "degraded_bw",
        Ok(NmxPartitionHealthDegraded) => "degraded",
        Ok(NmxPartitionHealthUnhealthy) => "unhealthy",
        Ok(NmxPartitionHealthUnknown) | Err(_) => "unknown",
    }
}

/// Logs multicast group usage for each NMX-C partition that has allocated groups.
/// Wans when a partition with a non-zero limit has allocated all of its groups.
fn log_partition_multicast_usage(domain_uuid: NvLinkDomainId, partitions: &[PartitionInfo]) {
    for partition in partitions {
        let allocated = partition.num_allocated_multicast_groups;
        if allocated == 0 {
            continue;
        }
        let limit = partition
            .attr
            .as_ref()
            .map(|attr| attr.multicast_groups_limit)
            .unwrap_or_default();
        let nmx_c_partition_id = nmx_c_partition_id_string(partition);
        if limit != 0 && allocated >= limit {
            tracing::warn!(
                %domain_uuid,
                nmx_c_partition_id,
                partition_name = %partition.name,
                multicast_groups_allocated = allocated,
                multicast_groups_limit = limit,
                "NMX-C partition has allocated all of its multicast groups"
            );
        } else {
            tracing::info!(
                %domain_uuid,
                nmx_c_partition_id,
                partition_name = %partition.name,
                multicast_groups_allocated = allocated,
                multicast_groups_limit = limit,
                "NMX-C partition multicast group usage"
            );
        }
    }
}

/// Counts partitions by health label.
fn aggregate_partition_health(partitions: &[PartitionInfo]) -> HashMap<&'static str, usize> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for p in partitions {
        *counts.entry(partition_health_label(p.health)).or_default() += 1;
    }
    counts
}

/// Counts GPUs by health label. An undiscovered GPU reports `gpu_health` UNKNOWN (and `gpu_uid` 0),
/// so it lands in the `unknown` bucket — matching how undiscovered compute nodes are reported.
fn aggregate_gpu_health(gpus: &[libnmxc::nmxc_model::GpuInfo]) -> HashMap<&'static str, usize> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for gpu in gpus {
        *counts.entry(gpu_health_label(gpu.gpu_health)).or_default() += 1;
    }
    counts
}

/// Counts compute nodes by health label.
fn aggregate_compute_node_health(
    nodes: &[libnmxc::nmxc_model::ComputeNodeInfo],
) -> HashMap<&'static str, usize> {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for node in nodes {
        *counts
            .entry(node_health_label(node.node_health))
            .or_default() += 1;
    }
    counts
}

/// Records per-domain health counts: seeds every state to `0`, then overlays the observed counts,
/// so a state with no entities this iteration is recorded as `0` rather than omitted.
fn record_domain_health(
    map: &mut HashMap<(String, &'static str), usize>,
    domain: &str,
    all_states: &[&'static str],
    counts: HashMap<&'static str, usize>,
) {
    for &state in all_states {
        map.insert((domain.to_string(), state), 0);
    }
    for (state, count) in counts {
        map.insert((domain.to_string(), state), count);
    }
}

#[cfg(test)]
mod partition_classification_tests {
    use libnmxc::nmxc_model::PartitionInfo;

    use super::is_gpu_in_tray_default_partition;

    #[test]
    fn tray_default_partition_match_is_slot_specific() {
        let cases = [
            ("tray_partition_1", 1, true),
            ("tray_partition_1", 2, false),
            ("tray_partition_1_extra", 1, false),
            ("Default", 1, false),
            ("unknown-partition", 1, false),
        ];

        for (name, slot_id, expected) in cases {
            let partition = PartitionInfo {
                name: name.to_string(),
                ..Default::default()
            };
            assert_eq!(
                is_gpu_in_tray_default_partition(&partition, slot_id),
                expected,
                "partition name {name:?}, slot {slot_id}",
            );
        }
    }
}

#[cfg(test)]
mod health_aggregation_tests {
    use std::collections::HashMap;

    use libnmxc::nmxc_model::{
        ComputeNodeHealth, ComputeNodeInfo, GpuHealth, GpuInfo, PartitionHealth, PartitionInfo,
    };

    use super::{
        GPU_HEALTH_STATES, aggregate_compute_node_health, aggregate_gpu_health,
        aggregate_partition_health, record_domain_health,
    };

    #[test]
    fn aggregate_partition_health_buckets_each_state() {
        fn part(health: PartitionHealth) -> PartitionInfo {
            PartitionInfo {
                health: health as i32,
                ..Default::default()
            }
        }
        let counts = aggregate_partition_health(&[
            part(PartitionHealth::NmxPartitionHealthHealthy),
            part(PartitionHealth::NmxPartitionHealthHealthy),
            part(PartitionHealth::NmxPartitionHealthDegradedBandwidth),
            part(PartitionHealth::NmxPartitionHealthDegraded),
            part(PartitionHealth::NmxPartitionHealthUnhealthy),
        ]);
        assert_eq!(counts.get("healthy"), Some(&2));
        assert_eq!(counts.get("degraded_bw"), Some(&1));
        assert_eq!(counts.get("degraded"), Some(&1));
        assert_eq!(counts.get("unhealthy"), Some(&1));
        assert_eq!(counts.values().sum::<usize>(), 5);
    }

    #[test]
    fn record_domain_health_zero_seeds_emptied_states() {
        let mut map = HashMap::new();
        // Pass 1: a 72-GPU domain — 70 healthy, 2 no_nvlink.
        record_domain_health(
            &mut map,
            "d1",
            &GPU_HEALTH_STATES,
            HashMap::from([("healthy", 70), ("no_nvlink", 2)]),
        );
        assert_eq!(map.get(&("d1".to_string(), "healthy")), Some(&70));
        assert_eq!(map.get(&("d1".to_string(), "no_nvlink")), Some(&2));
        assert_eq!(map.get(&("d1".to_string(), "degraded")), Some(&0)); // seeded, not observed
        // Pass 2: the 2 recovered, so all 72 are healthy. no_nvlink must read 0, not the stale 2.
        record_domain_health(
            &mut map,
            "d1",
            &GPU_HEALTH_STATES,
            HashMap::from([("healthy", 72)]),
        );
        assert_eq!(map.get(&("d1".to_string(), "healthy")), Some(&72));
        assert_eq!(map.get(&("d1".to_string(), "no_nvlink")), Some(&0));
    }

    fn gpu(gpu_health: GpuHealth) -> GpuInfo {
        GpuInfo {
            gpu_health: gpu_health as i32,
            ..Default::default()
        }
    }

    fn node(node_health: ComputeNodeHealth) -> ComputeNodeInfo {
        ComputeNodeInfo {
            node_health: node_health as i32,
            ..Default::default()
        }
    }

    #[test]
    fn aggregate_gpu_health_buckets_each_state() {
        let gpus = vec![
            gpu(GpuHealth::NmxGpuHealthHealthy),
            gpu(GpuHealth::NmxGpuHealthHealthy),
            gpu(GpuHealth::NmxGpuHealthDegraded),
            gpu(GpuHealth::NmxGpuHealthNoNvlink),
            gpu(GpuHealth::NmxGpuHealthDegradedBw),
            gpu(GpuHealth::NmxGpuHealthUnknown), // undiscovered GPU -> unknown
        ];
        let counts = aggregate_gpu_health(&gpus);
        assert_eq!(counts.get("healthy"), Some(&2));
        assert_eq!(counts.get("degraded"), Some(&1));
        assert_eq!(counts.get("no_nvlink"), Some(&1));
        assert_eq!(counts.get("degraded_bw"), Some(&1));
        assert_eq!(counts.get("unknown"), Some(&1));
        assert_eq!(counts.values().sum::<usize>(), 6);
    }

    #[test]
    fn aggregate_compute_node_health_buckets_each_state() {
        // Distinct counts per state (3/2/1) so this verifies the tally, not just bucketing.
        let nodes = vec![
            node(ComputeNodeHealth::NmxComputeNodeHealthHealthy),
            node(ComputeNodeHealth::NmxComputeNodeHealthHealthy),
            node(ComputeNodeHealth::NmxComputeNodeHealthHealthy),
            node(ComputeNodeHealth::NmxComputeNodeHealthDegraded),
            node(ComputeNodeHealth::NmxComputeNodeHealthDegraded),
            node(ComputeNodeHealth::NmxComputeNodeHealthUnhealthy),
        ];
        let counts = aggregate_compute_node_health(&nodes);
        assert_eq!(counts.get("healthy"), Some(&3));
        assert_eq!(counts.get("degraded"), Some(&2));
        assert_eq!(counts.get("unhealthy"), Some(&1));
        assert_eq!(counts.values().sum::<usize>(), 6);
    }

    /// End-to-end: a populated metrics snapshot is emitted as the right per-(domain, health)
    /// series through the real OpenTelemetry → Prometheus exporter (an in-memory `TestMeter`).
    #[test]
    fn emits_per_domain_health_series_through_the_exporter() {
        use std::time::Duration;

        use carbide_utils::test_support::test_meter::TestMeter;

        use crate::metrics::{MetricHolder, NvlPartitionMonitorMetrics};

        let test_meter = TestMeter::default();
        let holder = MetricHolder::new(test_meter.meter(), Duration::from_secs(60));

        // One NMX-C scenario for domain "domA": 70 healthy + 2 no_nvlink GPUs,
        // 1 degraded compute node, 1 healthy partition.
        let mut metrics = NvlPartitionMonitorMetrics::new();
        record_domain_health(
            &mut metrics.nmxc.gpu_health,
            "domA",
            &GPU_HEALTH_STATES,
            HashMap::from([("healthy", 70), ("no_nvlink", 2)]),
        );
        record_domain_health(
            &mut metrics.nmxc.compute_node_health,
            "domA",
            &super::NODE_HEALTH_STATES,
            HashMap::from([("degraded", 1)]),
        );
        record_domain_health(
            &mut metrics.nmxc.partition_health,
            "domA",
            &super::PARTITION_HEALTH_STATES,
            HashMap::from([("healthy", 1)]),
        );
        holder.update_metrics(metrics);

        // The exporter emits one series per (domain, health), including zero-seeded states.
        let gpu = test_meter.parsed_metrics("carbide_nvlink_partition_monitor_nmxc_gpu_count");
        let gpu_val = |health: &str| {
            gpu.iter()
                .find(|(attrs, _)| {
                    attrs.contains(&format!("health=\"{health}\""))
                        && attrs.contains("nvlink_domain_uuid=\"domA\"")
                })
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(gpu_val("healthy"), Some("70"));
        assert_eq!(gpu_val("no_nvlink"), Some("2"));
        assert_eq!(gpu_val("degraded"), Some("0")); // zero-seeded, not absent
        assert_eq!(gpu_val("unknown"), Some("0")); // zero-seeded

        let nodes =
            test_meter.parsed_metrics("carbide_nvlink_partition_monitor_nmxc_compute_node_count");
        assert!(nodes.iter().any(|(a, v)| a.contains("health=\"degraded\"")
            && a.contains("nvlink_domain_uuid=\"domA\"")
            && v == "1"));
        let parts =
            test_meter.parsed_metrics("carbide_nvlink_partition_monitor_nmxc_partition_count");
        assert!(parts.iter().any(|(a, v)| a.contains("health=\"healthy\"")
            && a.contains("nvlink_domain_uuid=\"domA\"")
            && v == "1"));
    }
}
