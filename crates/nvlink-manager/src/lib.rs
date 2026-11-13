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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub mod config;
mod errors;
mod group_state;
mod metrics;
pub mod nmx_c_endpoint;
pub mod nvlink;
mod rack_alert;
mod switch_cert_monitor;
mod switch_reachability;

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use carbide_instrument::Instrumentation;
use carbide_uuid::machine::{HostMachineId, MachineId};
use carbide_uuid::nvlink::{NvLinkDomainId, NvLinkLogicalPartitionId, NvLinkPartitionId};
use carbide_uuid::rack::RackId;
use chrono::Utc;
use component_manager::component_manager::ComponentManager;
use config::NvLinkConfig;
use config_version::Versioned;
use db::machine::find_machine_ids;
use db::managed_host::load_by_machine_ids;
use db::nvl_logical_partition::IdColumn as LpIdColumn;
use db::nvl_partition::IdColumn;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{self, ObjectColumnFilter, TransactionVending, machine};
use errors::{NvLinkManagerError, NvLinkManagerResult};
use futures::future;
use group_state::{
    FullCheckOutcome, GroupReconcileState, group_fingerprint, group_instances_synced,
};
use health_report::HealthReportApplyMode;
use libnmxc::nmxc_model::{
    GetComputeNodeInfoListRequest, GetGpuInfoListRequest, GetPartitionInfoListRequest,
    PartitionInfo,
};
use libnmxc::{Endpoint, NMX_C_GATEWAY_ID, Nmxc, NmxcPool};
use metrics::{
    AppliedChange, ChassisNmxCUnreachableReason, GroupOutcome, MonitorStage, NmxcMetricOperation,
    NmxcMetricOperationStatus, NmxcMetrics, NmxcOperationFailureStage, NmxcOperationFinished,
    NvlPartitionMonitorGroupFinished, NvlPartitionMonitorIterationFinished,
    NvlPartitionMonitorIterationOverran, NvlPartitionMonitorMetrics,
    NvlPartitionMonitorStageFinished, NvlinkUnhealthyAlertWrite, NvlinkUnhealthyAlertWriteFailed,
};
use model::hardware_info::{HardwareInfo, MachineNvLinkInfo, NvLinkGpu};
use model::instance::status::SyncState;
use model::instance::status::nvlink::InstanceNvLinkStatus;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::nvlink::{MachineNvLinkGpuStatusObservation, MachineNvLinkStatusObservation};
use model::machine::{HostHealthConfig, LoadSnapshotOptions, ManagedHostStateSnapshot};
use model::nvl_logical_partition::LogicalPartition;
use model::nvl_partition::{NvlPartition, NvlPartitionName};
use rack_alert::{RackAlertAction, RackUnreachableTracker, nmxc_unreachable_report};
use sqlx::PgPool;
#[cfg(feature = "test-support")]
pub use switch_cert_monitor::{SwitchCertificateMonitor, SwitchCertificateMonitorIterationResult};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Default NMX-M instance identifier for credentials and client lookup when none is specified.
pub const DEFAULT_NMX_M_NAME: &str = "default";

/// Multicast groups limit for new tenant NMX-C partitions. Must be a multiple of 4. Assuming at
/// most 1 partition per tray and 18 tray default partitions, this is floor(1024 / 18) rounded down
/// to the nearest multiple of 4.
const NMX_C_PARTITION_MULTICAST_GROUPS_LIMIT: u32 = 56;

/// Multicast groups limit for new tray default partitions (`tray_partition_{slot}`). Must be a
/// multiple of 4.
const TRAY_PARTITION_MULTICAST_GROUPS_LIMIT: u32 = 16;

/// Idle time between two partition monitor iterations.
///
/// An iteration longer than `interval` idles a quarter of the interval, even
/// when it applied changes. Otherwise an iteration that applied changes is
/// followed by a short re-check, and any other cycle is padded to `interval`,
/// never idling less than a quarter of it.
fn idle_after_iteration(interval: Duration, iteration: Duration, num_changes: usize) -> Duration {
    const FAST_RECHECK_IDLE: Duration = Duration::from_secs(1);
    let floor = interval / 4;
    if iteration > interval {
        floor
    } else if num_changes > 0 {
        FAST_RECHECK_IDLE.min(interval)
    } else {
        interval.saturating_sub(iteration).max(floor)
    }
}

/// Samples the time one monitor stage took since `started_at`.
fn record_stage(stage: MonitorStage, started_at: Instant, instrumentation: &Instrumentation) {
    instrumentation.emit(NvlPartitionMonitorStageFinished {
        stage,
        latency: started_at.elapsed(),
    });
}

fn managed_host_chassis_serial(snapshot: &ManagedHostStateSnapshot) -> Option<String> {
    snapshot
        .host_snapshot
        .status
        .nvlink_info
        .as_ref()
        .map(|info| info.chassis_serial.trim())
        .filter(|serial| !serial.is_empty())
        .map(str::to_string)
        .or_else(|| {
            snapshot
                .host_snapshot
                .status
                .hardware_info
                .as_ref()
                .and_then(HardwareInfo::first_gpu_platform_chassis_serial)
                .map(str::trim)
                .filter(|serial| !serial.is_empty())
                .map(str::to_string)
        })
}

type ManagedHostsByChassisSerial<'a> = HashMap<String, Vec<&'a ManagedHostStateSnapshot>>;
type ManagedHostsByRackId<'a> = HashMap<RackId, Vec<&'a ManagedHostStateSnapshot>>;

/// Groups managed hosts for monitor work.
///
/// Hosts with a `rack_id` are grouped into the rack's single current NMX-C
/// domain. Hosts without a `rack_id` are grouped using chassis serial.
fn group_managed_hosts_by_group_type(
    snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
) -> (ManagedHostsByChassisSerial<'_>, ManagedHostsByRackId<'_>) {
    let by_chassis_serial: ManagedHostsByChassisSerial<'_> =
        snapshots
            .iter()
            .fold(HashMap::new(), |mut acc, (_machine_id, snapshot)| {
                if snapshot.host_snapshot.rack_id.is_some() {
                    return acc;
                }
                if let Some(serial) = managed_host_chassis_serial(snapshot) {
                    acc.entry(serial).or_default().push(snapshot);
                }
                acc
            });

    let by_rack_id: ManagedHostsByRackId<'_> =
        snapshots
            .iter()
            .fold(HashMap::new(), |mut acc, (_machine_id, snapshot)| {
                if let Some(rack_id) = snapshot.host_snapshot.rack_id.clone() {
                    acc.entry(rack_id).or_default().push(snapshot);
                }
                acc
            });

    (by_chassis_serial, by_rack_id)
}

/// Extracts the domain identifier from a successful NMX-C Hello response.
///
/// Transport success does not guarantee that the server header or UUID is
/// present and well formed, so callers must handle this as protocol data.
fn domain_uuid_from_nmx_c_hello(
    hello: &libnmxc::nmxc_model::ServerHello,
) -> NvLinkManagerResult<NvLinkDomainId> {
    hello
        .server_header
        .as_ref()
        .and_then(|header| uuid::Uuid::parse_str(&header.domain_uuid).ok())
        .map(NvLinkDomainId::from)
        .filter(|domain_uuid| *domain_uuid != NvLinkDomainId::nil())
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
        info.domain_uuid = domain_uuid;

        if info.chassis_serial.trim().is_empty() {
            info.chassis_serial = chassis_serial.to_string();
        }
        return info;
    }

    if let Some(snapshot_info) =
        snapshot.and_then(|snapshot| snapshot.host_snapshot.status.nvlink_info.as_ref())
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
        .and_then(|snapshot| snapshot.host_snapshot.status.hardware_info.as_ref())
        .map(nvlink_gpus_from_hardware_info)
        .unwrap_or_default();

    MachineNvLinkInfo {
        domain_uuid,
        chassis_serial: chassis_serial.to_string(),
        gpus,
    }
}

/// Updates missing or stale machine NVLink domains using a validated NMX-C Hello.
fn populate_machine_nvlink_info_if_needed(
    machine_nvlink_info: &mut HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
    managed_host_snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    snapshot_chassis_serial: Option<&str>,
    machine_ids: &[HostMachineId],
    domain_uuid: NvLinkDomainId,
) -> Vec<(HostMachineId, MachineNvLinkInfo)> {
    let mut updates = Vec::new();
    for machine_id in machine_ids {
        let existing = machine_nvlink_info
            .get(machine_id)
            .and_then(|info| info.as_ref());
        let needs_update = match existing {
            None => true,
            Some(info) => info.domain_uuid != domain_uuid || info.chassis_serial.trim().is_empty(),
        };
        if !needs_update {
            continue;
        }

        // Fall back to persisted chassis inventory so a missing snapshot serial does
        // not block refreshing the machine's NVLink domain UUID.
        let Some(chassis_serial) = snapshot_chassis_serial
            .or_else(|| existing.map(|info| info.chassis_serial.as_str()))
            .map(str::trim)
            .filter(|serial| !serial.is_empty())
        else {
            continue;
        };

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
    RemoveDefaultPartition(u32), // NMX-C partition ID
    RemoveUnknownPartition(u32), // NMX-C partition ID
    Update(u32),                 // NMX-C partition ID
}

impl NmxcPartitionOperationType {
    /// The failure stage a rejected delete of an untracked partition reports. Only the NMX-C
    /// default partition's delete is fatal to the pass; an emptied unknown or tray default
    /// partition that NMX-C refuses to delete is retried next pass like any other operation.
    fn untracked_delete_failure_stage(&self) -> NmxcOperationFailureStage {
        match self {
            Self::RemoveDefaultPartition(_) => NmxcOperationFailureStage::DeleteDefaultPartition,
            _ => NmxcOperationFailureStage::DeletePartition,
        }
    }
}

struct NmxcOperationError {
    failure_stage: NmxcOperationFailureStage,
    error: String,
    nmx_c_partition_id: String,
    create_partition_request: String,
}

/// `finish_nmxc_operation` records one terminal NMX-C operation before deciding
/// whether its failure is recoverable. A default-partition delete failure still
/// aborts the monitor pass, but its latency and applied-change counter are
/// recorded before propagation. The helper also emits the stage-specific
/// warning with partition context; the later chassis-level summary still
/// reports the aborted pass.
fn finish_nmxc_operation(
    metrics: &mut NvlPartitionMonitorMetrics,
    logical_partition_id: &NvLinkLogicalPartitionId,
    operation: &NmxcPartitionOperation,
    latency: Duration,
    result: Result<(), NmxcOperationError>,
    instrumentation: &Instrumentation,
) -> NvLinkManagerResult<bool> {
    let succeeded = result.is_ok();
    let (failure_stage, error, nmx_c_partition_id, create_partition_request) = match result {
        Ok(()) => (
            NmxcOperationFailureStage::None,
            String::new(),
            String::new(),
            String::new(),
        ),
        Err(error) => (
            error.failure_stage,
            error.error,
            error.nmx_c_partition_id,
            error.create_partition_request,
        ),
    };
    let status = if succeeded {
        NmxcMetricOperationStatus::Completed
    } else {
        NmxcMetricOperationStatus::Failed
    };
    let metric_operation: NmxcMetricOperation = operation.operation_type.clone().into();
    *metrics
        .applied_changes
        .entry(AppliedChange {
            operation: metric_operation,
            status,
        })
        .or_default() += 1;

    let fatal_error =
        (failure_stage == NmxcOperationFailureStage::DeleteDefaultPartition).then(|| {
            NvLinkManagerError::internal(format!("failed to delete default partition: {error}"))
        });
    instrumentation.emit(NmxcOperationFinished {
        operation: metric_operation,
        status,
        latency,
        failure_stage,
        nvlink_logical_partition_id: logical_partition_id.to_string(),
        nmx_c_partition_id,
        create_partition_request,
        error,
    });

    if let Some(error) = fatal_error {
        return Err(error);
    }
    Ok(succeeded)
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
    /// Every logical partition by id, with whether it is marked as deleted.
    logical_partition_deleted: HashMap<NvLinkLogicalPartitionId, bool>,
    db_nvl_partitions: HashMap<u32, NvlPartition>, // NMX-C partition ID to NvlPartition
    machine_nvlink_info: HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
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

/// Whether an operator marked the host for NVLink isolation with the
/// `IsolateFromNvLinkPartitions` health classification (for example through the
/// admin-cli `compute-tray-due-for-swap` template) before moving or replacing its tray.
fn is_marked_for_nvlink_isolation(snapshot: &ManagedHostStateSnapshot) -> bool {
    snapshot
        .aggregate_health
        .find_alert_by_classification(
            &health_report::HealthAlertClassification::isolate_from_nvlink_partitions(),
        )
        .is_some()
}

/// How [`NvlPartitionMonitor::check_machine_and_handle_gpu_removals`] treats a host's GPUs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuRemovalMode {
    /// Host without an instance in admin network mode: remove GPUs from tenant and unknown
    /// partitions and keep them in the tray default partition.
    Idle,
    /// Host marked for NVLink isolation: remove GPUs from every partition, including the tray
    /// default partition, and leave them out.
    Isolate,
}

/// Consecutive checked passes a physical partition row must be missing from
/// its domain's NMX-C partition list before the monitor removes it. One pass
/// is not trusted on its own: an NMX-C still restoring its fabric state after
/// a restart may answer with an incomplete list.
const STALE_PARTITION_MISSES_BEFORE_DELETE: u32 = 2;

/// Consecutive iterations each physical partition row was missing from its
/// domain's NMX-C partition list, held between iterations. A pass that lists
/// the partition again resets its count; a pass that cannot reach NMX-C makes
/// no observation. A domain is observed at most once per iteration: chassis
/// groups that share an endpoint reconcile the same domain against the same
/// listing, and their passes together count as one miss, not one each.
#[derive(Debug, Default)]
struct StalePartitionTracker {
    misses: HashMap<NvLinkPartitionId, u32>,
    /// Domains whose listing this iteration has already been counted.
    observed_domains: HashSet<NvLinkDomainId>,
}

impl StalePartitionTracker {
    /// Starts an iteration: drops counts of rows that no longer exist and
    /// lets every domain be counted again.
    fn begin_iteration(&mut self, existing: &[NvlPartition]) {
        let existing: HashSet<NvLinkPartitionId> = existing.iter().map(|p| p.id).collect();
        self.misses.retain(|id, _| existing.contains(id));
        self.observed_domains.clear();
    }

    /// Records one checked pass of `domain_uuid`: `missing` rows were absent
    /// from NMX-C's list and `present` rows were listed. Returns the rows
    /// missing on `threshold` consecutive iterations, which stay counted until
    /// [`Self::forget`] confirms their removal so a failed delete is retried.
    /// A later pass of the same domain in the same iteration advances no
    /// count; it only reports the rows already due.
    fn observe(
        &mut self,
        domain_uuid: NvLinkDomainId,
        missing: &[NvLinkPartitionId],
        present: &[NvLinkPartitionId],
        threshold: u32,
    ) -> Vec<NvLinkPartitionId> {
        let first_pass_this_iteration = self.observed_domains.insert(domain_uuid);
        if first_pass_this_iteration {
            for id in present {
                self.misses.remove(id);
            }
        }
        missing
            .iter()
            .filter(|id| {
                let misses = self.misses.entry(**id).or_default();
                if first_pass_this_iteration {
                    *misses = misses.saturating_add(1);
                }
                *misses >= threshold
            })
            .copied()
            .collect()
    }

    fn forget(&mut self, ids: &[NvLinkPartitionId]) {
        for id in ids {
            self.misses.remove(id);
        }
    }
}

/// Physical partition rows recorded under an NVLink domain that no managed
/// host belongs to. A host's domain is the one its stored NVLink info names
/// or, when that is absent or nil (as after scout discovery), the one this
/// pass's Hello returned for its group. The monitor refreshes the stored
/// domain from Hello, so when NMX-C comes back with a new domain uuid (for
/// example after losing its fabric state) the rows under the old uuid stop
/// matching any host and any group, and nothing else would ever remove them.
///
/// Nothing is selected while any host's domain is unknown this pass, since
/// the rows of that host's domain cannot be told from orphans, and nothing is
/// selected while no host names a domain at all, so an empty or undiscovered
/// fleet cannot empty the table. Rows under a domain some host names are left
/// to that domain's group, which compares them against NMX-C's partition
/// list, including rows of a group that is unreachable or skipped this pass.
fn partitions_of_unknown_domains(
    db_nvl_partitions: impl IntoIterator<Item = (NvLinkPartitionId, NvLinkDomainId)>,
    host_domains_before_pass: &HashMap<HostMachineId, Option<NvLinkDomainId>>,
    host_groups: &HashMap<HostMachineId, String>,
    hello_domains: &HashMap<&str, NvLinkDomainId>,
) -> Vec<(NvLinkPartitionId, NvLinkDomainId)> {
    let mut known_domains: HashSet<NvLinkDomainId> = hello_domains.values().copied().collect();
    for (machine_id, domain) in host_domains_before_pass {
        match domain {
            Some(domain) => {
                known_domains.insert(*domain);
            }
            None => {
                let resolved = host_groups
                    .get(machine_id)
                    .is_some_and(|group_id| hello_domains.contains_key(group_id.as_str()));
                if !resolved {
                    tracing::debug!(
                        %machine_id,
                        "Host's NVLink domain is unknown this pass; keeping every physical partition row"
                    );
                    return Vec::new();
                }
            }
        }
    }
    if known_domains.is_empty() {
        return Vec::new();
    }
    db_nvl_partitions
        .into_iter()
        .filter(|(_, domain)| !known_domains.contains(domain))
        .collect()
}

impl PartitionProcessingContext {
    fn new(
        nmx_c_partitions: Vec<PartitionInfo>,
        db_nvl_logical_partitions: &[LogicalPartition],
        db_nvl_partitions: Vec<NvlPartition>,
        machine_nvlink_info: HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
    ) -> Self {
        let gpu_map = Self::build_gpu_to_partition_map(&nmx_c_partitions);
        let nmx_c_partitions = nmx_c_partitions
            .into_iter()
            .filter_map(|p| p.partition_id.map(|id| (id, p)))
            .collect();
        let logical_partition_deleted = db_nvl_logical_partitions
            .iter()
            .map(|p| (p.id, model::nvl_logical_partition::is_marked_as_deleted(p)))
            .collect();
        let db_nvl_partitions = db_nvl_partitions
            .into_iter()
            .filter_map(|p| u32::try_from(p.nmx_c_partition_id).ok().map(|id| (id, p)))
            .collect();
        Self {
            nmx_c_partitions,
            logical_partition_deleted,
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
                operation_type: NmxcPartitionOperationType::RemoveDefaultPartition(
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
        machine_id: &HostMachineId,
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
                nmx_c_partition_id = nmx_c_id,
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
        match self.logical_partition_deleted.get(logical_partition_id) {
            Some(true) => {
                tracing::error!(
                    "logical partition already marked as deleted, cannot modify physical partition"
                );
                false
            }
            Some(false) => true,
            None => {
                tracing::error!(
                    nvlink_logical_partition_id = %logical_partition_id,
                    "Logical partition not found",
                );
                false
            }
        }
    }

    /// Physical partition rows of `domain_uuid`, split into those whose NMX-C
    /// partition is missing from the domain's current partition list, with the
    /// NMX-C id to recheck them by, and those listed. NMX-C forgets its
    /// partitions when it restarts without fabric state, and the row of a
    /// forgotten partition is never removed through GPU membership, so the
    /// monitor removes it once it stays missing. Rows recorded under another or
    /// a nil domain are left to their owner.
    fn partition_presence_in_domain(
        &self,
        domain_uuid: &NvLinkDomainId,
    ) -> (Vec<(NvLinkPartitionId, u32)>, Vec<NvLinkPartitionId>) {
        let (mut missing, mut present) = (Vec::new(), Vec::new());
        for (nmx_c_partition_id, partition) in &self.db_nvl_partitions {
            if partition.domain_uuid != *domain_uuid {
                continue;
            }
            let listed = self
                .nmx_c_partitions
                .contains_key(&libnmxc::nmxc_model::PartitionId {
                    partition_id: *nmx_c_partition_id,
                });
            if listed {
                present.push(partition.id);
            } else {
                missing.push((partition.id, *nmx_c_partition_id));
            }
        }
        (missing, present)
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

    // Observation for a host marked for NVLink isolation. Each GPU reports the tenant partition
    // NMX-C currently has it in, which is none once the isolation removals complete.
    fn isolated_host_observation(
        &self,
        mh: &ManagedHostStateSnapshot,
    ) -> MachineNvLinkStatusObservation {
        let nvlink_gpus = mh
            .host_snapshot
            .status
            .nvlink_info
            .iter()
            .flat_map(|info| {
                info.gpus.iter().map(move |gpu| {
                    let mut observation = MachineNvLinkGpuStatusObservation {
                        device_instance: gpu.device_id as u32 - 1,
                        domain_id: info.domain_uuid,
                        gpu_id: gpu.guid.to_string(),
                        guid: gpu.guid,
                        ..Default::default()
                    };
                    if let Some((partition_id, logical_partition_id, _, _)) = self
                        .gpu_to_partition_map
                        .get(&gpu.guid)
                        .and_then(|partition| partition.partition_id)
                        .and_then(|id| self.get_db_partition_info(id.partition_id))
                    {
                        observation.partition_id = partition_id;
                        observation.logical_partition_id = logical_partition_id;
                    }
                    observation
                })
            })
            .collect();
        MachineNvLinkStatusObservation {
            observed_at: Utc::now(),
            nvlink_gpus,
        }
    }

    // Get the list of GPUs that should remain in a partition after removing a specific GPU from a logical partition.
    // To remove a GPU from a partition in NMX-C, we need to do an update op with every other GPU in the partition except the one
    // getting removed.
    fn get_gpus_to_keep_after_removal(
        &self,
        logical_partition_id: Option<NvLinkLogicalPartitionId>,
        partition_nmx_c_id: &libnmxc::nmxc_model::PartitionId,
        gpu_guid: u64,
        machine_id: &HostMachineId,
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
                                machine_id = %machine_id,
                                device_instance,
                                nmx_c_partition_id = partition_nmx_c_id.partition_id,
                                "NMX-C partition not found",
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
                            machine_id = %machine_id,
                            device_instance,
                            nmx_c_partition_id = partition_nmx_c_id.partition_id,
                            "NMX-C partition not found",
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
        machine_id: &HostMachineId,
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
                                machine_id = %machine_id,
                                device_instance,
                                nmx_c_partition_id = partition_nmx_c_id.partition_id,
                                "NMX-C partition not found",
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
                            machine_id = %machine_id,
                            device_instance,
                            nmx_c_partition_id = partition_nmx_c_id.partition_id,
                            "NMX-C partition not found",
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
    instrumentation: Instrumentation,
    work_lock_manager_handle: WorkLockManagerHandle,
    /// Change-detection state per group id, held only between iterations.
    group_states: Mutex<HashMap<String, GroupReconcileState>>,
    /// Consecutive passes each rack's NMX-C was unusable, for the rack's
    /// debounced `NmxcUnreachable` alert.
    rack_unreachable: Mutex<RackUnreachableTracker>,
    /// Consecutive checked passes each physical partition row was missing
    /// from NMX-C, for the debounced removal of stale rows.
    stale_partitions: Mutex<StalePartitionTracker>,
}

pub struct NvLinkManager {
    db_pool: PgPool,
    nmxc_client_pool: Arc<dyn NmxcPool>,
    meter: opentelemetry::metrics::Meter,
    instrumentation: Instrumentation,
    config: NvLinkConfig,
    host_health: HostHealthConfig,
    component_manager: Option<Arc<ComponentManager>>,
    work_lock_manager_handle: WorkLockManagerHandle,
}

pub struct NvLinkManagerArgs {
    pub db_pool: PgPool,
    pub nmxc_client_pool: Arc<dyn NmxcPool>,
    pub meter: opentelemetry::metrics::Meter,
    /// Destination for Event metrics emitted by this manager and its monitors.
    pub instrumentation: Instrumentation,
    pub config: NvLinkConfig,
    pub host_health: HostHealthConfig,
    pub component_manager: Option<Arc<ComponentManager>>,
    pub work_lock_manager_handle: WorkLockManagerHandle,
}

impl NvLinkManager {
    /// Creates the manager and routes child-monitor Event metrics through its instrumentation.
    pub fn new(args: NvLinkManagerArgs) -> Self {
        Self {
            db_pool: args.db_pool,
            nmxc_client_pool: args.nmxc_client_pool,
            meter: args.meter,
            instrumentation: args.instrumentation,
            config: args.config,
            host_health: args.host_health,
            component_manager: args.component_manager,
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
            self.instrumentation.clone(),
        )
        .start(join_set, cancel_token.clone())?;

        if self.config.switch_reachability_enabled() {
            switch_reachability::SwitchReachabilityMonitor::new(
                self.db_pool.clone(),
                self.config.switch_reachability.clone(),
                self.work_lock_manager_handle.clone(),
            )
            .start(join_set, cancel_token.clone())?;
        }

        if self.config.certificate_rotation_enabled() {
            let switch_cert_monitor = switch_cert_monitor::SwitchCertificateMonitor::new(
                self.db_pool,
                self.meter,
                self.config,
                self.component_manager,
                self.work_lock_manager_handle,
                self.instrumentation,
            );
            join_set
                .build_task()
                .name("nmx-c-switch-cert-monitor")
                .spawn(async move { switch_cert_monitor.run(cancel_token).await })?;
        }

        Ok(())
    }
}

struct CheckPartitionsInput<'a> {
    db_nvl_logical_partitions: &'a [LogicalPartition],
    db_nvl_partitions: Vec<NvlPartition>,
    machine_nvlink_info: HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
    managed_host_snapshots: HashMap<HostMachineId, ManagedHostStateSnapshot>,
    nvlink_info_db_updates: Vec<(HostMachineId, MachineNvLinkInfo)>,
}

/// What one group's partition check left behind.
struct PartitionCheckOutcome {
    /// NMX-C operations that completed.
    completed_operations: usize,
    /// Whether GPU health was fetched from NMX-C and every host alert and
    /// isolation status write required succeeded. Partition reconciliation
    /// does not depend on it.
    health_recorded: bool,
}

/// Work queued when NMX-C cannot be used for a machine group and observations must be cleared.
struct PendingNullNvlinkObservation {
    /// Chassis serial or rack id for the group whose observations will be cleared.
    group_id: String,
    /// Whether `group_id` is a chassis serial or rack id.
    group_type: nmx_c_endpoint::ManagedHostGroupType,
    /// Failure reason recorded in partition-monitor metrics.
    reason: ChassisNmxCUnreachableReason,
    /// Host machines in the group that will receive a null `nvlink_status_observation`.
    machine_ids: Vec<HostMachineId>,
}

/// Shared inputs for processing one chassis- or rack-scoped NMX-C monitor group.
struct ProcessMachineGroupInput<'a> {
    group_id: String,
    group_type: nmx_c_endpoint::ManagedHostGroupType,
    snapshots: &'a [&'a ManagedHostStateSnapshot],
    endpoint_url: Option<&'a str>,

    /// Rack associated with the selected switch endpoint; absent for chassis mappings.
    rack_id: Option<&'a RackId>,
    all_managed_host_snapshots: &'a HashMap<HostMachineId, ManagedHostStateSnapshot>,
    /// Pre-split shard of `machine_nvlink_info` containing only this group's machines.
    machine_nvlink_info: HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
    db_nvl_partitions: &'a [NvlPartition],
    db_nvl_logical_partitions: &'a [LogicalPartition],
}

/// Output of processing one NMX-C monitor group, collected and merged by the caller.
struct GroupResult {
    completed_operations: usize,
    null_observations: Vec<PendingNullNvlinkObservation>,
    partial_metrics: NvlPartitionMonitorMetrics,
    /// Whether NMX-C was reached, every operation issued to it completed, and
    /// the GPU health pass was fetched and persisted.
    check_ok: bool,
    /// Domain UUID observed by this pass's Hello for a rack group. `None` for
    /// chassis groups and when Hello did not yield a usable UUID.
    rack_domain_uuid: Option<NvLinkDomainId>,
    /// Why NMX-C could not be used for the group this pass, when the pass
    /// stopped before reconciling. Feeds the rack's `NmxcUnreachable` alert.
    unreachable_reason: Option<ChassisNmxCUnreachableReason>,
    /// Domain UUID this pass's Hello returned for the group, for any group
    /// type. `None` when the group was skipped or NMX-C was not reached.
    hello_domain_uuid: Option<NvLinkDomainId>,
}

impl GroupResult {
    /// A group skipped by change detection: NMX-C was not contacted, and the
    /// health counts of its last full check keep the per-domain gauges
    /// populated. The pass republishes a skipped rack's last domain UUID
    /// separately when a switch or power shelf has a missing or different one.
    fn skipped(last_health: NmxcMetrics) -> Self {
        let mut partial_metrics = NvlPartitionMonitorMetrics::new();
        partial_metrics.nmxc = last_health;
        Self {
            completed_operations: 0,
            null_observations: Vec::new(),
            partial_metrics,
            check_ok: true,
            rack_domain_uuid: None,
            unreachable_reason: None,
            hello_domain_uuid: None,
        }
    }
}

impl NvlPartitionMonitor {
    const ITERATION_WORK_KEY: &'static str = "NvlPartitionMonitor::run_single_iteration";

    /// Creates a partition monitor using the supplied Event instrumentation.
    pub fn new(
        db_pool: PgPool,
        nmxc_client_pool: Arc<dyn NmxcPool>,
        meter: opentelemetry::metrics::Meter,
        config: NvLinkConfig,
        host_health: HostHealthConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
        instrumentation: Instrumentation,
    ) -> Self {
        let hold_period = config
            .monitor_run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, hold_period));

        let max_concurrent_groups = config.partition_monitor_max_concurrent_groups.get();
        if max_concurrent_groups > NvLinkConfig::MAX_DOCUMENTED_PARTITION_MONITOR_CONCURRENT_GROUPS
        {
            tracing::warn!(
                partition_monitor_max_concurrent_groups = max_concurrent_groups,
                documented_maximum =
                    NvLinkConfig::MAX_DOCUMENTED_PARTITION_MONITOR_CONCURRENT_GROUPS,
                "partition_monitor_max_concurrent_groups is above the documented range"
            );
        }

        Self {
            db_pool,
            nmxc_client_pool,
            config,
            host_health,
            metric_holder,
            instrumentation,
            work_lock_manager_handle,
            group_states: Mutex::new(HashMap::new()),
            rack_unreachable: Mutex::new(RackUnreachableTracker::default()),
            stale_partitions: Mutex::new(StalePartitionTracker::default()),
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
        let interval = self.config.monitor_run_interval;
        loop {
            let started_at = Instant::now();
            // `run_single_iteration` owns the completion and overrun events;
            // this loop only needs the change count to pick its idle time.
            let num_changes = self.run_single_iteration().await.unwrap_or(0);
            let idle = idle_after_iteration(interval, started_at.elapsed(), num_changes);

            tokio::select! {
                _ = tokio::time::sleep(idle) => {},
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
        let latency = metrics.recording_started_at.elapsed();
        check_nvl_partition_span.in_scope(|| {
            self.instrumentation.emit(match result.as_ref().err() {
                None => NvlPartitionMonitorIterationFinished::Succeeded { latency },
                Some(error) => NvlPartitionMonitorIterationFinished::Failed {
                    latency,
                    error: error.to_string(),
                },
            });
            if latency > self.config.monitor_run_interval {
                self.instrumentation
                    .emit(NvlPartitionMonitorIterationOverran {
                        latency_ms: latency.as_millis(),
                        interval_ms: self.config.monitor_run_interval.as_millis(),
                        groups: metrics.num_groups,
                    });
            }
        });
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
                    error = %e,
                    "NvlPartitionMonitor failed to acquire work lock: Another instance of carbide running?",
                );
                return Ok(0);
            }
        };
        tracing::trace!(
            lock = Self::ITERATION_WORK_KEY,
            "NvlPartitionMonitor acquired the lock",
        );

        let fleet_load_started = Instant::now();
        let mut txn = self.db_pool.txn_begin().await?;
        let managed_host_snapshots = self.load_mnnvl_managed_host_snapshots(txn.as_mut()).await?;
        let mut machine_nvlink_info = machine::find_nvlink_info_by_machine_ids(
            &mut txn,
            &managed_host_snapshots.keys().copied().collect::<Vec<_>>(),
        )
        .await?;

        let (managed_host_snapshots_by_chassis_serial, managed_host_snapshots_by_rack_id) =
            group_managed_hosts_by_group_type(&managed_host_snapshots);

        let db_nvl_partitions =
            db::nvl_partition::find_by(&mut txn, ObjectColumnFilter::<IdColumn>::All).await?;
        // Taken before the shards below consume the map, and resolved against
        // this pass's Hello results once the groups have run.
        let host_domains_before_pass: HashMap<HostMachineId, Option<NvLinkDomainId>> =
            machine_nvlink_info
                .iter()
                .map(|(machine_id, info)| {
                    let domain = info
                        .as_ref()
                        .map(|info| info.domain_uuid)
                        .filter(|domain| *domain != NvLinkDomainId::nil());
                    (*machine_id, domain)
                })
                .collect();
        let mut host_groups: HashMap<HostMachineId, String> = HashMap::new();
        self.stale_partitions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin_iteration(&db_nvl_partitions);

        let db_nvl_logical_partitions =
            db::nvl_logical_partition::find_by(&mut txn, ObjectColumnFilter::<LpIdColumn>::All)
                .await?;

        let chassis_serials: Vec<&str> = managed_host_snapshots_by_chassis_serial
            .keys()
            .map(String::as_str)
            .collect();
        let chassis_serial_to_resolved_endpoint: HashMap<String, String> =
            db::nvlink_nmxc_endpoints::find_by_chassis_serials(&mut txn, &chassis_serials)
                .await
                .map_err(NvLinkManagerError::from)?
                .into_iter()
                .map(|row| (row.chassis_serial, row.endpoint))
                .collect();

        // Close the inventory transaction before contacting NMX-C. Endpoint
        // lookup is best effort only when no rack partition work depends on it.
        let rack_endpoint_rows =
            match db::switch::find_ready_control_plane_configured_switch_endpoints(&mut txn)
                .await
                .map_err(NvLinkManagerError::from)
            {
                Ok(rows) => {
                    txn.commit().await?;
                    rows
                }
                Err(error) if managed_host_snapshots_by_rack_id.is_empty() => {
                    tracing::warn!(
                        %error,
                        "Unable to load rack NMX-C endpoints; switch domain publication skipped"
                    );

                    txn.rollback().await?;
                    Vec::new()
                }
                Err(error) => return Err(error),
            };

        let rack_id_to_resolved_endpoint = rack_endpoint_rows
            .into_iter()
            .map(|row| {
                let endpoint_url = nmx_c_endpoint::nmx_c_endpoint_url_from_nvos_ip(
                    &row.nvos_ip,
                    None,
                    &self.config,
                );

                (row.rack_id, endpoint_url)
            })
            .collect::<HashMap<_, _>>();
        record_stage(
            MonitorStage::FleetLoad,
            fleet_load_started,
            &self.instrumentation,
        );

        metrics.num_logical_partitions = db_nvl_logical_partitions.len();
        metrics.num_physical_partitions = db_nvl_partitions.len();

        // Pre-split machine_nvlink_info into per-group shards before concurrent execution.
        // Groups are disjoint (each MachineId belongs to exactly one group), so the split
        // is lossless: each entry is moved into exactly one shard via remove().
        let mut all_group_inputs: Vec<ProcessMachineGroupInput<'_>> = Vec::new();

        for (serial, snapshots) in &managed_host_snapshots_by_chassis_serial {
            let shard = snapshots
                .iter()
                .filter_map(|s| {
                    machine_nvlink_info
                        .remove(&s.host_snapshot.id)
                        .map(|info| (s.host_snapshot.id, info))
                })
                .collect();
            all_group_inputs.push(ProcessMachineGroupInput {
                group_id: serial.clone(),
                group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                snapshots,
                endpoint_url: chassis_serial_to_resolved_endpoint
                    .get(serial)
                    .map(String::as_str),
                rack_id: None,
                all_managed_host_snapshots: &managed_host_snapshots,
                machine_nvlink_info: shard,
                db_nvl_partitions: &db_nvl_partitions,
                db_nvl_logical_partitions: &db_nvl_logical_partitions,
            });
        }

        // A rack is one NVLink domain, so all hosts in the rack are reconciled
        // through the same NMX-C endpoint and hello response.
        for (rack_id, snapshots) in &managed_host_snapshots_by_rack_id {
            let shard = snapshots
                .iter()
                .filter_map(|s| {
                    machine_nvlink_info
                        .remove(&s.host_snapshot.id)
                        .map(|info| (s.host_snapshot.id, info))
                })
                .collect();
            all_group_inputs.push(ProcessMachineGroupInput {
                group_id: rack_id.to_string(),
                group_type: nmx_c_endpoint::ManagedHostGroupType::Rack,
                snapshots,
                endpoint_url: rack_id_to_resolved_endpoint
                    .get(rack_id)
                    .map(String::as_str),
                rack_id: Some(rack_id),
                all_managed_host_snapshots: &managed_host_snapshots,
                machine_nvlink_info: shard,
                db_nvl_partitions: &db_nvl_partitions,
                db_nvl_logical_partitions: &db_nvl_logical_partitions,
            });
        }

        metrics.num_groups = all_group_inputs.len();

        // A group whose inputs are unchanged since a good check skips NMX-C
        // until its full check is due; its gauges keep that check's values.
        // A skipped rack's last domain UUID is republished only when one of
        // its switches or power shelves has a missing or different one.
        let full_check_interval = self.config.partition_monitor_full_check_interval;
        let detect_changes = !full_check_interval.is_zero();
        let now = Instant::now();
        let mut seen_group_ids: HashSet<String> = HashSet::new();
        let mut checked_group_inputs = Vec::new();
        let mut all_group_results = Vec::new();
        let mut skipped_rack_domains: Vec<(RackId, NvLinkDomainId)> = Vec::new();
        // This pass's NMX-C outcome per rack group, for the rack's `NmxcUnreachable` alert.
        let mut rack_outcomes: Vec<(RackId, Option<ChassisNmxCUnreachableReason>)> = Vec::new();
        {
            let group_states = self
                .group_states
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for input in all_group_inputs {
                seen_group_ids.insert(input.group_id.clone());
                for snapshot in input.snapshots {
                    host_groups.insert(snapshot.host_snapshot.id, input.group_id.clone());
                }
                let fingerprint = if detect_changes {
                    group_fingerprint(&input)
                } else {
                    0
                };
                let skipped = detect_changes
                    && group_states.get(&input.group_id).is_some_and(|state| {
                        state.can_skip(fingerprint, group_instances_synced(input.snapshots), now)
                    });
                if skipped {
                    let state = &group_states[&input.group_id];
                    self.instrumentation.emit(NvlPartitionMonitorGroupFinished {
                        outcome: GroupOutcome::SkippedUnchanged,
                    });
                    all_group_results.push(GroupResult::skipped(state.last_health().clone()));
                    if let Some(skipped_rack) = input.rack_id.cloned().zip(state.rack_domain_uuid())
                    {
                        skipped_rack_domains.push(skipped_rack);
                    }
                    // A skipped rack is not contacted, so it yields no outcome
                    // for its `NmxcUnreachable` alert. Preserve any stored alert:
                    // another API replica may have observed a more recent outage.
                } else {
                    checked_group_inputs.push((input, fingerprint));
                }
            }
        }
        metrics.num_groups_skipped = all_group_results.len();

        let stale_rack_domains = self
            .skipped_racks_with_stale_domain_uuid(skipped_rack_domains)
            .await;

        // Bound concurrency so group processing cannot exhaust the shared DB pool
        // or open an unbounded number of NMX-C gRPC clients at once.
        let concurrency = Semaphore::new(self.config.partition_monitor_max_concurrent_groups.get());
        let republish_stale_racks =
            future::join_all(stale_rack_domains.iter().map(|(rack_id, domain_uuid)| {
                let concurrency = &concurrency;
                async move {
                    let _permit = concurrency
                        .acquire()
                        .await
                        .expect("NMX-C group concurrency semaphore is never closed");
                    let publish_started = Instant::now();
                    self.record_rack_switch_domain_uuid(rack_id, *domain_uuid)
                        .await;
                    record_stage(
                        MonitorStage::GroupDomainPublish,
                        publish_started,
                        &self.instrumentation,
                    );
                }
            }));
        let check_groups = future::join_all(checked_group_inputs.into_iter().map(
            |(input, fingerprint)| {
                // Borrow outside `async move` so the closure copies the &Semaphore reference
                // (which is Copy) rather than trying to move the Semaphore itself.
                let concurrency = &concurrency;
                async move {
                    let _permit = concurrency
                        .acquire()
                        .await
                        .expect("NMX-C group concurrency semaphore is never closed");
                    let group_id = input.group_id.clone();
                    let rack_id = input.rack_id.cloned();
                    let result = self.process_nmx_c_partition_monitor_group(input).await;
                    (group_id, fingerprint, rack_id, result)
                }
            },
        ));
        let (_, checked_group_results) = future::join(republish_stale_racks, check_groups).await;

        if detect_changes {
            let now = Instant::now();
            let mut group_states = self
                .group_states
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for (group_id, fingerprint, _, result) in &checked_group_results {
                let state = GroupReconcileState::after_full_check(
                    group_states.get(group_id),
                    group_id,
                    FullCheckOutcome {
                        fingerprint: *fingerprint,
                        check_ok: result.check_ok,
                        applied_changes: result.completed_operations > 0,
                        health: result.partial_metrics.nmxc.clone(),
                        rack_domain_uuid: result.rack_domain_uuid,
                    },
                    now,
                    full_check_interval,
                );
                group_states.insert(group_id.clone(), state);
            }
            group_states.retain(|group_id, _| seen_group_ids.contains(group_id));
        }
        for (_, _, rack_id, result) in &checked_group_results {
            if let Some(rack_id) = rack_id {
                rack_outcomes.push((rack_id.clone(), result.unreachable_reason));
            }
        }
        let hello_domains: HashMap<&str, NvLinkDomainId> = checked_group_results
            .iter()
            .filter_map(|(group_id, _, _, result)| {
                result
                    .hello_domain_uuid
                    .map(|domain| (group_id.as_str(), domain))
            })
            .collect();
        let orphaned_domain_partitions = partitions_of_unknown_domains(
            db_nvl_partitions
                .iter()
                .map(|partition| (partition.id, partition.domain_uuid)),
            &host_domains_before_pass,
            &host_groups,
            &hello_domains,
        );
        all_group_results.extend(
            checked_group_results
                .into_iter()
                .map(|(_, _, _, result)| result),
        );

        let mut total_completed_operations = 0;
        let mut pending_null_nvlink_observations = Vec::new();
        for result in all_group_results {
            total_completed_operations += result.completed_operations;
            pending_null_nvlink_observations.extend(result.null_observations);
            metrics.merge_from(result.partial_metrics);
        }

        // Rack groups already observe Hello while reconciling partitions. Racks
        // without managed hosts need a separate Hello solely to publish switch
        // metadata, so each endpoint is contacted at most once per iteration.
        for (rack_id, endpoint_url) in &rack_id_to_resolved_endpoint {
            if !managed_host_snapshots_by_rack_id.contains_key(rack_id) {
                self.observe_and_record_rack_switch_domain_uuid(rack_id, endpoint_url)
                    .await;
            }
        }

        // Before the fallible observation write: a failure there must not
        // lose this pass's rack outcomes or leave a recovered rack alerted.
        let managed_rack_ids = managed_host_snapshots_by_rack_id.keys().cloned().collect();
        self.reconcile_rack_unreachable_alerts(
            &rack_outcomes,
            &rack_id_to_resolved_endpoint,
            &managed_rack_ids,
        )
        .await;

        self.record_null_nvlink_status_observations(&pending_null_nvlink_observations, metrics)
            .await?;

        // Retried next iteration; the groups above have already been reconciled.
        if let Err(error) = self
            .delete_partitions_of_unknown_domains(&orphaned_domain_partitions, metrics)
            .await
        {
            tracing::warn!(%error, "Failed to delete physical partitions of unknown domains");
        }
        if let Err(error) = self
            .delete_marked_logical_partitions(&db_nvl_logical_partitions)
            .await
        {
            tracing::warn!(%error, "Failed to delete logical partitions marked as deleted");
        }

        metrics.num_completed_operations = total_completed_operations;

        Ok(total_completed_operations)
    }

    /// Raises or clears each rack's `NmxcUnreachable` merge report from this pass's outcomes.
    ///
    /// The alert is debounced by `partition_monitor_unreachable_alert_after`
    /// consecutive unusable passes and cleared on the first pass that reaches
    /// NMX-C. Which racks currently carry the report is read from the database
    /// so a restart or an operator removal cannot desynchronise the two.
    /// Skipped racks keep their stored reports because another API replica
    /// may have observed an outage since this replica's last check. Racks
    /// with no managed hosts left have their reports cleared. Failures are
    /// logged and never block partition reconciliation.
    async fn reconcile_rack_unreachable_alerts(
        &self,
        outcomes: &[(RackId, Option<ChassisNmxCUnreachableReason>)],
        endpoints: &HashMap<RackId, String>,
        managed_rack_ids: &HashSet<RackId>,
    ) {
        let stored = match db::rack::find_ids_with_health_report_merge_source(
            &self.db_pool,
            NVLINK_UNHEALTHY_OVERRIDE_SOURCE,
        )
        .await
        {
            Ok(rack_ids) => rack_ids.into_iter().collect::<HashSet<RackId>>(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Failed to load racks with NmxcUnreachable alerts; rack alerts not updated this pass"
                );
                return;
            }
        };
        if outcomes.is_empty() && stored.is_empty() {
            return;
        }

        let threshold = self.config.partition_monitor_unreachable_alert_after.get();
        let mut actions: Vec<(RackId, RackAlertAction)> = {
            let mut tracker = self
                .rack_unreachable
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            tracker.retain(|rack_id| managed_rack_ids.contains(rack_id));
            let mut actions = outcomes
                .iter()
                .map(|(rack_id, reason)| {
                    let action =
                        tracker.observe(rack_id, *reason, threshold, stored.contains(rack_id));
                    (rack_id.clone(), action)
                })
                .collect::<Vec<_>>();
            // A rack skipped this pass yields no outcome, so a clear that
            // failed on an earlier pass is retried here.
            let checked: HashSet<&RackId> = outcomes.iter().map(|(rack_id, _)| rack_id).collect();
            actions.extend(
                tracker
                    .pending_clears(&stored)
                    .filter(|rack_id| !checked.contains(rack_id))
                    .map(|rack_id| (rack_id, RackAlertAction::Clear)),
            );
            actions
        };
        actions.extend(
            stored
                .iter()
                .filter(|rack_id| !managed_rack_ids.contains(rack_id))
                .map(|rack_id| (rack_id.clone(), RackAlertAction::Clear)),
        );

        for (rack_id, action) in actions {
            if action == RackAlertAction::Keep {
                continue;
            }
            let write = async {
                let mut conn = self.db_pool.acquire().await.map_err(|e| {
                    NvLinkManagerError::internal(format!("Failed to acquire connection: {e}"))
                })?;
                match action {
                    RackAlertAction::Keep => Ok(()),
                    RackAlertAction::Set { reason, since } => {
                        let endpoint = endpoints.get(&rack_id).map(String::as_str);
                        tracing::warn!(
                            %rack_id,
                            endpoint = endpoint.unwrap_or("<none>"),
                            ?reason,
                            consecutive_passes = threshold,
                            unreachable_since = %since,
                            "NMX-C unusable for consecutive passes - setting NmxcUnreachable alert on rack"
                        );
                        let report = nmxc_unreachable_report(
                            NVLINK_UNHEALTHY_OVERRIDE_SOURCE,
                            endpoint,
                            reason,
                            since,
                        );
                        db::rack::insert_health_report(
                            &mut conn,
                            &rack_id,
                            HealthReportApplyMode::Merge,
                            &report,
                        )
                        .await
                        .map_err(NvLinkManagerError::from)
                    }
                    RackAlertAction::Clear => {
                        tracing::info!(%rack_id, "NMX-C reachable again - clearing NmxcUnreachable alert on rack");
                        db::rack::remove_health_report(
                            &mut conn,
                            &rack_id,
                            HealthReportApplyMode::Merge,
                            NVLINK_UNHEALTHY_OVERRIDE_SOURCE,
                        )
                        .await
                        .map_err(NvLinkManagerError::from)
                    }
                }
            };
            let written = match write.await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        %rack_id,
                        ?action,
                        %error,
                        "Failed to update the rack's NmxcUnreachable alert; retried next pass"
                    );
                    false
                }
            };
            self.rack_unreachable
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .confirm(&rack_id, action, written);
        }
    }

    /// Connects to NMX-C for one chassis- or rack-scoped host group and reconciles partitions.
    ///
    /// A valid rack-scoped Hello is published before partition work. Publication
    /// is best effort and cannot turn successful partition reconciliation into a
    /// failure.
    async fn process_nmx_c_partition_monitor_group(
        &self,
        input: ProcessMachineGroupInput<'_>,
    ) -> GroupResult {
        let ProcessMachineGroupInput {
            group_id,
            group_type,
            snapshots,
            endpoint_url,
            rack_id,
            all_managed_host_snapshots,
            mut machine_nvlink_info,
            db_nvl_partitions,
            db_nvl_logical_partitions,
        } = input;
        let group_type_label = group_type.as_str();
        let mut group_metrics = NvlPartitionMonitorMetrics::new();
        let mut null_observations: Vec<PendingNullNvlinkObservation> = Vec::new();

        macro_rules! early_return {
            ($reason:expr) => {{
                Self::queue_null_nvlink_status_observation(
                    &mut null_observations,
                    &group_id,
                    group_type,
                    snapshots,
                    $reason,
                );
                self.instrumentation.emit(NvlPartitionMonitorGroupFinished {
                    outcome: GroupOutcome::Unreachable,
                });
                return GroupResult {
                    completed_operations: 0,
                    null_observations,
                    partial_metrics: group_metrics,
                    check_ok: false,
                    rack_domain_uuid: None,
                    unreachable_reason: Some($reason),
                    hello_domain_uuid: None,
                };
            }};
        }

        let Some(endpoint_url) = endpoint_url else {
            tracing::warn!(
                group_id,
                group_type = group_type_label,
                "No NMX-C endpoint (switch NVOS IP or nvlink_nmxc_endpoints mapping); skipping partition monitor work"
            );
            early_return!(ChassisNmxCUnreachableReason::NoEndpoint);
        };

        let hello_started = Instant::now();
        let nmxc_endpoint = match Endpoint::new(endpoint_url) {
            Ok(ep) => ep,
            Err(e) => {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    endpoint = %endpoint_url,
                    error = %e,
                    "Invalid NMX-C endpoint URI; skipping partition monitor work"
                );
                early_return!(ChassisNmxCUnreachableReason::InvalidEndpointUri);
            }
        };

        let mut nmxc_client = match self.nmxc_client_pool.create_client(nmxc_endpoint).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    endpoint = %endpoint_url,
                    error = %e,
                    "Failed to create NMX-C client; skipping partition monitor work"
                );
                early_return!(ChassisNmxCUnreachableReason::ClientCreateFailed);
            }
        };
        let hello = match nmxc_client.hello(NMX_C_GATEWAY_ID).await {
            Ok(hello) => hello,
            Err(e) => {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    endpoint = %endpoint_url,
                    error = %e,
                    "NMX-C hello failed; skipping partition monitor work"
                );
                early_return!(ChassisNmxCUnreachableReason::HelloFailed);
            }
        };
        let domain_uuid = match domain_uuid_from_nmx_c_hello(&hello) {
            Ok(domain_uuid) => domain_uuid,
            Err(e) => {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    endpoint = %endpoint_url,
                    error = %e,
                    "Failed to parse domain UUID from NMX-C hello; skipping partition monitor work"
                );
                early_return!(ChassisNmxCUnreachableReason::DomainUuidParseFailed);
            }
        };
        record_stage(
            MonitorStage::GroupHello,
            hello_started,
            &self.instrumentation,
        );

        // Current ingestion treats one rack as one NMX-C domain. Publish the
        // rack's Hello observation to the switches using that same boundary.
        if let Some(rack_id) = rack_id {
            let publish_started = Instant::now();
            self.record_rack_switch_domain_uuid(rack_id, domain_uuid)
                .await;
            record_stage(
                MonitorStage::GroupDomainPublish,
                publish_started,
                &self.instrumentation,
            );
        }

        // Endpoint + component versions for this group, so the metric-read failures below log them.
        group_metrics.nmxc.endpoint = endpoint_url.to_string();
        group_metrics.nmxc.version = hello
            .components_ver
            .iter()
            .map(|kv| format!("{}={}", kv.key, kv.value))
            .collect::<Vec<_>>()
            .join(", ");

        // Filter managed host snapshots, nvlink info, and DB partitions for this group.
        let mut managed_host_snapshots_domain: HashMap<HostMachineId, ManagedHostStateSnapshot> =
            snapshots
                .iter()
                .map(|s| (s.host_snapshot.id, (*s).clone()))
                .collect();
        let machine_ids_in_domain: HashSet<HostMachineId> =
            managed_host_snapshots_domain.keys().copied().collect();
        let mut nvlink_info_db_updates = Vec::new();
        for snapshot in snapshots {
            let machine_id = snapshot.host_snapshot.id;
            let snapshot_chassis_serial = managed_host_chassis_serial(snapshot);
            let has_persisted_chassis_serial = machine_nvlink_info
                .get(&machine_id)
                .and_then(|info| info.as_ref())
                .is_some_and(|info| !info.chassis_serial.trim().is_empty());
            if snapshot_chassis_serial.is_none() && !has_persisted_chassis_serial {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    %machine_id,
                    "Skipping nvlink_info population; chassis serial unavailable"
                );
                continue;
            }
            nvlink_info_db_updates.extend(populate_machine_nvlink_info_if_needed(
                &mut machine_nvlink_info,
                all_managed_host_snapshots,
                snapshot_chassis_serial.as_deref(),
                &[machine_id],
                domain_uuid,
            ));
        }
        if !nvlink_info_db_updates.is_empty() {
            tracing::info!(
                group_id,
                group_type = group_type_label,
                %domain_uuid,
                machine_ids = ?nvlink_info_db_updates.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                "Populated machine nvlink_info from NMX-C hello"
            );
            for (machine_id, nvlink_info) in &nvlink_info_db_updates {
                if let Some(snapshot) = managed_host_snapshots_domain.get_mut(machine_id) {
                    snapshot.host_snapshot.status.nvlink_info = Some(nvlink_info.clone());
                }
            }
        }
        let machine_nvlink_info_domain: HashMap<HostMachineId, Option<MachineNvLinkInfo>> =
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

        let (completed_operations, check_ok) = match self
            .check_partitions_and_apply_nmx_c_operations(
                nmxc_client.as_mut(),
                &mut group_metrics,
                domain_uuid,
                CheckPartitionsInput {
                    db_nvl_logical_partitions,
                    db_nvl_partitions: db_nvl_partitions_domain,
                    machine_nvlink_info: machine_nvlink_info_domain,
                    managed_host_snapshots: managed_host_snapshots_domain,
                    nvlink_info_db_updates,
                },
            )
            .await
        {
            Ok(PartitionCheckOutcome {
                completed_operations,
                health_recorded,
            }) => {
                // An operation that failed or timed out leaves NMX-C short of
                // the desired state, and an unrecorded GPU health pass leaves
                // the host alerts stale, so the group is checked again next time.
                let every_operation_completed =
                    group_metrics.applied_changes.iter().all(|(change, count)| {
                        change.status == NmxcMetricOperationStatus::Completed || *count == 0
                    });
                self.instrumentation.emit(NvlPartitionMonitorGroupFinished {
                    outcome: if !every_operation_completed {
                        GroupOutcome::OperationsFailed
                    } else if !health_recorded {
                        GroupOutcome::HealthNotRecorded
                    } else {
                        GroupOutcome::Reconciled
                    },
                });
                (
                    completed_operations,
                    every_operation_completed && health_recorded,
                )
            }
            Err(e) => {
                tracing::warn!(
                    group_id,
                    group_type = group_type_label,
                    error = %e,
                    "Partition monitor work failed; queuing null nvlink status observations"
                );
                Self::queue_null_nvlink_status_observation(
                    &mut null_observations,
                    &group_id,
                    group_type,
                    snapshots,
                    ChassisNmxCUnreachableReason::PartitionMonitorWorkFailed,
                );
                self.instrumentation.emit(NvlPartitionMonitorGroupFinished {
                    outcome: GroupOutcome::Unreachable,
                });
                (0, false)
            }
        };

        GroupResult {
            completed_operations,
            null_observations,
            partial_metrics: group_metrics,
            check_ok,
            rack_domain_uuid: rack_id.map(|_| domain_uuid),
            unreachable_reason: None,
            hello_domain_uuid: Some(domain_uuid),
        }
    }

    /// Observes a rack domain when no managed-host group triggers reconciliation.
    ///
    /// Invalid endpoints, failed Hello calls, and nil domain identifiers retain
    /// the last valid observation and are retried on the next monitor iteration.
    async fn observe_and_record_rack_switch_domain_uuid(
        &self,
        rack_id: &RackId,
        endpoint_url: &str,
    ) {
        let endpoint = match Endpoint::new(endpoint_url) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                tracing::warn!(
                    %rack_id,
                    endpoint = %endpoint_url,
                    %error,
                    "Invalid rack NMX-C endpoint; switch domain publication skipped"
                );

                return;
            }
        };

        let mut client = match self.nmxc_client_pool.create_client(endpoint).await {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(
                    %rack_id,
                    endpoint = %endpoint_url,
                    %error,
                    "Failed to create rack NMX-C client; switch domain publication skipped"
                );

                return;
            }
        };

        let hello = match client.hello(NMX_C_GATEWAY_ID).await {
            Ok(hello) => hello,
            Err(error) => {
                tracing::warn!(
                    %rack_id,
                    endpoint = %endpoint_url,
                    %error,
                    "Rack NMX-C hello failed; switch domain publication skipped"
                );

                return;
            }
        };

        let domain_uuid = match domain_uuid_from_nmx_c_hello(&hello) {
            Ok(domain_uuid) => domain_uuid,
            Err(error) => {
                tracing::warn!(
                    %rack_id,
                    endpoint = %endpoint_url,
                    %error,
                    "Failed to parse rack NMX-C domain UUID; switch domain publication skipped"
                );

                return;
            }
        };

        self.record_rack_switch_domain_uuid(rack_id, domain_uuid)
            .await;
    }

    /// The skipped racks with a switch or power shelf whose domain UUID is
    /// missing or differs from the remembered one. Two read queries on one
    /// connection; a failure logs and leaves the fix to the staggered full
    /// check.
    async fn skipped_racks_with_stale_domain_uuid(
        &self,
        skipped_rack_domains: Vec<(RackId, NvLinkDomainId)>,
    ) -> Vec<(RackId, NvLinkDomainId)> {
        if skipped_rack_domains.is_empty() {
            return skipped_rack_domains;
        }

        let stale_racks: NvLinkManagerResult<HashSet<RackId>> = async {
            let mut conn = self.db_pool.acquire().await.map_err(|e| {
                NvLinkManagerError::internal(format!("Failed to acquire connection: {e}"))
            })?;
            let mut stale_racks: HashSet<RackId> =
                db::switch::find_racks_with_stale_nvlink_domain_uuid(
                    &mut *conn,
                    &skipped_rack_domains,
                )
                .await?
                .into_iter()
                .collect();
            stale_racks.extend(
                db::power_shelf::find_racks_with_stale_nvlink_domain_uuid(
                    &mut *conn,
                    &skipped_rack_domains,
                )
                .await?,
            );
            Ok(stale_racks)
        }
        .await;

        match stale_racks {
            Ok(stale_racks) => skipped_rack_domains
                .into_iter()
                .filter(|(rack_id, _)| stale_racks.contains(rack_id))
                .collect(),
            Err(error) => {
                tracing::warn!(
                    skipped_rack_count = skipped_rack_domains.len(),
                    %error,
                    "Failed to check skipped racks for a missing NVLink domain UUID; deferring to the next full check"
                );
                Vec::new()
            }
        }
    }

    /// Persists a non-nil rack observation on the rack's switches and power
    /// shelves in one short, independent transaction.
    ///
    /// Nil observations and database failures leave the last valid value
    /// unchanged. Publication must not block partition reconciliation.
    async fn record_rack_switch_domain_uuid(&self, rack_id: &RackId, domain_uuid: NvLinkDomainId) {
        if domain_uuid == NvLinkDomainId::nil() {
            return;
        }

        let update_result: NvLinkManagerResult<_> = async {
            let mut txn = self.db_pool.txn_begin().await?;

            let changed_switches =
                db::switch::update_nvlink_domain_uuid_for_rack(&mut txn, rack_id, domain_uuid)
                    .await?;
            let changed_power_shelves =
                db::power_shelf::update_nvlink_domain_uuid_for_rack(&mut txn, rack_id, domain_uuid)
                    .await?;

            txn.commit().await?;

            Ok((changed_switches, changed_power_shelves))
        }
        .await;

        match update_result {
            Ok((changed_switches, changed_power_shelves))
                if !changed_switches.is_empty() || !changed_power_shelves.is_empty() =>
            {
                tracing::info!(
                    %rack_id,
                    %domain_uuid,
                    changed_switch_count = changed_switches.len(),
                    changed_power_shelf_count = changed_power_shelves.len(),
                    "Recorded NMX-C NVLink domain for rack switches and power shelves"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    %rack_id,
                    %domain_uuid,
                    %error,
                    "Failed to record NMX-C NVLink domain for rack switches and power shelves; continuing partition monitor work"
                );
            }
        }
    }

    /// Fetches partition list from NMX-C, checks for needed create/update/delete operations,
    /// executes them, polls for completion, and updates the DB with the results. GPU health
    /// is read and persisted on the way; its outcome is reported, not returned as an error.
    async fn check_partitions_and_apply_nmx_c_operations(
        &self,
        nmxc_client: &mut dyn Nmxc,
        metrics: &mut NvlPartitionMonitorMetrics,
        domain_uuid: NvLinkDomainId,
        input: CheckPartitionsInput<'_>,
    ) -> NvLinkManagerResult<PartitionCheckOutcome> {
        let domain = domain_uuid.to_string();
        let CheckPartitionsInput {
            db_nvl_logical_partitions,
            db_nvl_partitions,
            machine_nvlink_info,
            managed_host_snapshots,
            nvlink_info_db_updates,
        } = input;
        let inventory_started = Instant::now();
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

        // GPU health for metrics and host alerts. A failure never blocks
        // reconciliation; it is reported so the group is checked again next pass.
        let health_recorded = match nmxc_client
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

                self.record_nvlink_unhealthy_alerts(
                    &domain_uuid,
                    &resp.gpu_info_list,
                    &machine_nvlink_info,
                    &managed_host_snapshots,
                )
                .await
            }
            Err(e) => {
                tracing::warn!(
                    %domain_uuid,
                    endpoint = %metrics.nmxc.endpoint,
                    version = %metrics.nmxc.version,
                    error = %e,
                    "NMX-C GetGpuInfoList failed; GPU health metrics missing for this domain this iteration"
                );
                false
            }
        };

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
        record_stage(
            MonitorStage::GroupInventory,
            inventory_started,
            &self.instrumentation,
        );

        let mut partition_processing_context = PartitionProcessingContext::new(
            partition_info_list,
            db_nvl_logical_partitions,
            db_nvl_partitions,
            machine_nvlink_info,
        );
        // Rows missing from this pass's listing, with the NMX-C id each is
        // rechecked by against the listing taken after the operations, and
        // those of them due for removal after this miss.
        let (missing_partitions, stale_candidates) = {
            let (missing, present) =
                partition_processing_context.partition_presence_in_domain(&domain_uuid);
            let missing_ids: Vec<NvLinkPartitionId> = missing.iter().map(|(id, _)| *id).collect();
            let due = self
                .stale_partitions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .observe(
                    domain_uuid,
                    &missing_ids,
                    &present,
                    STALE_PARTITION_MISSES_BEFORE_DELETE,
                );
            let candidates: Vec<(NvLinkPartitionId, u32)> = missing
                .iter()
                .filter(|(id, _)| due.contains(id))
                .copied()
                .collect();
            (missing, candidates)
        };

        // Report isolation progress for marked hosts from this pass's partition membership. Like
        // the GPU health alerts, a failed write never blocks reconciliation.
        let isolation_recorded = self
            .record_nvlink_isolation_status(&partition_processing_context, &managed_host_snapshots)
            .await;
        let health_recorded = health_recorded && isolation_recorded;

        // Check if any partitions need to be created, updated, or deleted.
        let observations = self.check_nv_link_partitions(
            &mut partition_processing_context,
            managed_host_snapshots,
            metrics,
        )?;

        let status_write_started = Instant::now();
        self.record_nvlink_status_observation(
            observations
                .into_iter()
                .map(|(machine_id, observation)| (machine_id.into(), observation))
                .collect(),
        )
        .await?;
        record_stage(
            MonitorStage::GroupStatusWrite,
            status_write_started,
            &self.instrumentation,
        );

        let nmx_c_operations = partition_processing_context.nmx_c_operations;

        if !nmx_c_operations.is_empty() {
            tracing::debug!(
                nmx_c_operations = ?nmx_c_operations,
                "Starting NMX-C operations",
            );
        }

        // Execute any NMX-C operations and collect successful completions.
        let apply_started = Instant::now();
        let had_operations = !nmx_c_operations.is_empty();
        let completed_nmx_c_operations = self
            .execute_nmx_c_operations(nmxc_client, nmx_c_operations, metrics)
            .await?;

        if !completed_nmx_c_operations.is_empty() {
            tracing::debug!(
                completed_nmx_c_operations = ?completed_nmx_c_operations,
                "Completed NMX-C operations",
            );
        }

        let num_completed_operations = completed_nmx_c_operations
            .values()
            .map(|ops| ops.len())
            .sum::<usize>();

        if nvlink_info_db_updates.is_empty()
            && completed_nmx_c_operations.is_empty()
            && missing_partitions.is_empty()
        {
            if had_operations {
                record_stage(
                    MonitorStage::GroupApply,
                    apply_started,
                    &self.instrumentation,
                );
            }
            return Ok(PartitionCheckOutcome {
                completed_operations: num_completed_operations,
                health_recorded,
            });
        }

        // Created partitions are matched to their NMX-C ids through a fresh
        // partition list, and rows missing from the first listing are
        // rechecked against it: a partition that reappeared while the pass
        // ran, as during fabric restoration, keeps its row and its miss count
        // starts over.
        let nmx_c_partitions: HashMap<String, PartitionInfo> =
            if completed_nmx_c_operations.is_empty() && missing_partitions.is_empty() {
                HashMap::new()
            } else {
                nmxc_client
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
                    .partition_info_list
                    .into_iter()
                    .map(|p| (nmx_c_partition_id_string(&p), p))
                    .collect()
            };

        // Update db with the operations that completed successfully.
        let mut txn = self.db_pool.txn_begin().await?;
        for (machine_id, nvlink_info) in nvlink_info_db_updates {
            machine::update_nvlink_info(&mut txn, &machine_id, nvlink_info).await?;
        }
        self.update_db_with_nmx_c_operations(
            &mut txn,
            completed_nmx_c_operations,
            &nmx_c_partitions,
        )
        .await?;
        let listed_again: Vec<NvLinkPartitionId> = missing_partitions
            .iter()
            .filter(|(_, nmx_c_partition_id)| {
                nmx_c_partitions.contains_key(&nmx_c_partition_id.to_string())
            })
            .map(|(id, _)| *id)
            .collect();
        let (reappeared, stale_partition_ids): (Vec<NvLinkPartitionId>, Vec<NvLinkPartitionId>) =
            stale_candidates
                .iter()
                .map(|(id, _)| *id)
                .partition(|id| listed_again.contains(id));
        let stale_deleted = if stale_partition_ids.is_empty() {
            Vec::new()
        } else {
            db::nvl_partition::final_delete_many(&stale_partition_ids, &mut txn).await?
        };
        txn.commit().await?;
        {
            let mut tracker = self
                .stale_partitions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            tracker.forget(&stale_partition_ids);
            // Listed again, whether or not already due: the miss streak starts over.
            tracker.forget(&listed_again);
        }
        for partition_id in &reappeared {
            tracing::info!(
                %domain_uuid,
                nvlink_partition_id = %partition_id,
                "NMX-C lists the partition again; keeping its physical partition row"
            );
        }
        for partition_id in &stale_deleted {
            tracing::warn!(
                %domain_uuid,
                nvlink_partition_id = %partition_id,
                "Deleted physical partition row whose NMX-C partition no longer exists"
            );
        }
        metrics.num_stale_partitions_deleted += stale_deleted.len();
        record_stage(
            MonitorStage::GroupApply,
            apply_started,
            &self.instrumentation,
        );

        Ok(PartitionCheckOutcome {
            completed_operations: num_completed_operations,
            health_recorded,
        })
    }

    /// Correlates NMX-C per-GPU health for a domain with the locally known NVLink GPUs for
    /// each machine, and sets/clears a `NvlinkUnhealthy` health alert per machine. The alert
    /// carries `prevent_allocations`, so an affected machine cannot be allocated until its
    /// GPUs recover.
    ///
    /// Failures are logged and never block partition reconciliation, matching the handling of
    /// the `GetGpuInfoList` call itself. Returns whether every write it attempted succeeded, so
    /// the caller keeps the group out of the skip cache until the writes are retried.
    async fn record_nvlink_unhealthy_alerts(
        &self,
        domain_uuid: &NvLinkDomainId,
        gpu_info_list: &[libnmxc::nmxc_model::GpuInfo],
        machine_nvlink_info: &HashMap<HostMachineId, Option<MachineNvLinkInfo>>,
        mh_snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> bool {
        // UID 0 is NMX-C's placeholder for a GPU under rediscovery, not an identity, so it
        // must never be matched against inventory (see `next_nvlink_unhealthy_alerts`).
        let health_by_guid: HashMap<u64, i32> = gpu_info_list
            .iter()
            .filter(|gpu| gpu.gpu_uid != 0)
            .map(|gpu| (gpu.gpu_uid, gpu.gpu_health))
            .collect();

        let mut every_write_succeeded = true;
        for (machine_id, mh) in mh_snapshots {
            let gpus: &[NvLinkGpu] = machine_nvlink_info
                .get(machine_id)
                .and_then(|info| info.as_ref())
                .map_or(&[], |info| info.gpus.as_slice());

            // This monitor owns exactly one thing: the Merge entry under its own source. Read
            // that entry directly, in the same mode `set_`/`clear_monitor_health_report`
            // mutate, rather than `by_source` (which prefers a Replace report) or derived
            // aggregate health (which a Replace override hides). A Replace report an operator
            // placed under this source name is a deliberate host-level override: it is neither
            // read for dedup nor removed on recovery, so recovery can never silently delete it,
            // and a clear here always removes what it claims to.
            let existing = mh
                .host_snapshot
                .health_reports
                .merges
                .get(NVLINK_UNHEALTHY_OVERRIDE_SOURCE);
            let previous = existing.map_or(&[][..], |report| report.alerts.as_slice());

            let alerts = next_nvlink_unhealthy_alerts(previous, gpus, &health_by_guid);

            let (write, result) = if !alerts.is_empty() {
                let mut report = nvlink_unhealthy_report(alerts);
                report.update_in_alert_since(existing);
                // Skip the write when the affected GPUs and their blocking classifications
                // are unchanged. Comparing keys rather than full alerts means a link
                // oscillating between degraded states (which only changes the message label)
                // does not rewrite the row every pass; the label catches up the next time the
                // key set changes. A stored copy with `prevent_allocations` stripped has a
                // different key, so it is rewritten and the block restored.
                if existing.is_some_and(|prev| {
                    nvlink_alert_keys(&prev.alerts) == nvlink_alert_keys(&report.alerts)
                }) {
                    continue;
                }
                tracing::warn!(
                    %machine_id,
                    %domain_uuid,
                    affected_gpu_count = report.alerts.len(),
                    total_gpu_count = gpus.len(),
                    "NVLink GPU(s) unhealthy - setting PreventAllocations alert"
                );
                (
                    NvlinkUnhealthyAlertWrite::Set,
                    set_monitor_health_report(&self.db_pool, machine_id, &report).await,
                )
            } else if existing.is_some() {
                tracing::info!(
                    %machine_id,
                    %domain_uuid,
                    "All affected NVLink GPUs report healthy - clearing NvlinkUnhealthy alert"
                );
                (
                    NvlinkUnhealthyAlertWrite::Clear,
                    clear_monitor_health_report(
                        &self.db_pool,
                        machine_id,
                        NVLINK_UNHEALTHY_OVERRIDE_SOURCE,
                    )
                    .await,
                )
            } else {
                continue;
            };

            // Partition reconciliation still proceeds; this counter is the signal that the
            // allocation block was not applied or cleared. The write is retried next pass.
            if let Err(error) = result {
                every_write_succeeded = false;
                self.instrumentation.emit(NvlinkUnhealthyAlertWriteFailed {
                    write,
                    machine_id: machine_id.to_string(),
                    domain_uuid: domain_uuid.to_string(),
                    error: error.to_string(),
                });
            }
        }
        every_write_succeeded
    }

    /// Keeps the isolation status report on each host in the group current: hosts marked for
    /// NVLink isolation get the status from this pass's NMX-C partition membership (see
    /// [`nvlink_isolation_status_report`]), and the report is removed from hosts that are no
    /// longer marked. The status reflects membership before this pass's removals, so a host
    /// reads as isolated on the pass after its removals complete.
    ///
    /// Returns whether every write it attempted succeeded, so the caller keeps the group out
    /// of the skip cache until the writes are retried.
    async fn record_nvlink_isolation_status(
        &self,
        partition_ctx: &PartitionProcessingContext,
        mh_snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> bool {
        let mut every_write_succeeded = true;
        for (machine_id, mh) in mh_snapshots {
            let existing = mh
                .host_snapshot
                .health_reports
                .merges
                .get(NVLINK_ISOLATION_REPORT_SOURCE);

            let result = if is_marked_for_nvlink_isolation(mh) {
                let gpus = mh
                    .host_snapshot
                    .status
                    .nvlink_info
                    .as_ref()
                    .map_or(&[][..], |info| info.gpus.as_slice());
                let mut report =
                    nvlink_isolation_status_report(gpus, &partition_ctx.gpu_to_partition_map);
                if existing.is_some_and(|previous| same_nvlink_isolation_status(previous, &report))
                {
                    continue;
                }
                report.update_in_alert_since(existing);
                tracing::info!(
                    %machine_id,
                    isolated = report.alerts.is_empty(),
                    "Recording NVLink isolation status"
                );
                set_monitor_health_report(&self.db_pool, machine_id, &report).await
            } else if existing.is_some() {
                tracing::info!(
                    %machine_id,
                    "Host is no longer marked for NVLink isolation - clearing isolation status"
                );
                clear_monitor_health_report(
                    &self.db_pool,
                    machine_id,
                    NVLINK_ISOLATION_REPORT_SOURCE,
                )
                .await
            } else {
                continue;
            };

            if let Err(error) = result {
                every_write_succeeded = false;
                tracing::warn!(
                    %machine_id,
                    %error,
                    "Failed to record NVLink isolation status; retrying next pass"
                );
            }
        }
        every_write_succeeded
    }

    // Check the passed NvLink partition "observations" (physical partition info from NMX-C supplemented by physical and logical partition info from DB)
    // against the instance config and generate NMX-C operations to bring the observations into alignment with the config.
    fn check_nv_link_partitions(
        &self,
        partition_ctx: &mut PartitionProcessingContext,
        mh_snapshots: HashMap<HostMachineId, ManagedHostStateSnapshot>,
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<HashMap<HostMachineId, MachineNvLinkStatusObservation>> {
        let mut machine_gpu_statuses = HashMap::new();

        // If the default partition is present, enqueue a removal operation and return early.
        // no observations will be generated
        if partition_ctx.enqueue_nmx_c_default_partition_removal_if_present() {
            return Ok(machine_gpu_statuses);
        }

        for mh in mh_snapshots.values() {
            metrics.num_machines_scanned += 1;
            if is_marked_for_nvlink_isolation(mh) {
                // The host is about to have its tray moved or replaced. Remove its GPUs from every
                // partition while NMX-C still recognizes them, and skip instance reconciliation,
                // which would add them back to the tenant's partition.
                if let Err(e) = self.check_machine_and_handle_gpu_removals(
                    mh,
                    partition_ctx,
                    GpuRemovalMode::Isolate,
                ) {
                    tracing::warn!(
                        machine_id = %mh.host_snapshot.id,
                        error = %e,
                        "Failed to enqueue NVLink isolation removals",
                    );
                }
                if let Some(instance) = &mh.instance {
                    machine_gpu_statuses.insert(
                        instance.machine_id,
                        partition_ctx.isolated_host_observation(mh),
                    );
                }
                continue;
            }
            let Some(instance) = &mh.instance else {
                // For machines with no instance, check if machine is in admin network and any cleanup is required
                let _ = self.check_machine_and_handle_gpu_removals(
                    mh,
                    partition_ctx,
                    GpuRemovalMode::Idle,
                );
                continue;
            };
            metrics.num_instances_scanned += 1;
            let mut instance_gpu_statuses = Vec::new();
            let Some(info) = partition_ctx
                .machine_nvlink_info
                .get(&instance.machine_id)
                .cloned()
            else {
                tracing::warn!(
                    machine_id = %instance.machine_id,
                    "No NVLink info found",
                );
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
                                                nmx_c_partition_id = partition_id,
                                                "No logical partition ID associated with physical partition",
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
                                    let in_nmx_c_default_partition =
                                        is_nmx_c_default_partition(&nmxc_partition);
                                    if in_nmx_c_default_partition
                                        || is_gpu_in_tray_default_partition(
                                            &nmxc_partition,
                                            nvlink_gpu.slot_id,
                                        )
                                    {
                                        let wants_tenant_partition = instance_gpu_config
                                            .is_some_and(|gpu_config| {
                                                gpu_config.logical_partition_id.is_some()
                                            });
                                        // A GPU leaves its holding partition when the tenant
                                        // wants it in a logical partition. The NMX-C default
                                        // partition is also vacated for any configured GPU, but
                                        // a config that names no logical partition, like an
                                        // omitted config, leaves the GPU parked in its tray
                                        // default partition.
                                        if wants_tenant_partition
                                            || (in_nmx_c_default_partition
                                                && instance_gpu_config.is_some())
                                        {
                                            tracing::info!(
                                                gpu_guid = nvlink_gpu.guid,
                                                machine_id = %instance.machine_id,
                                                instance_id = %instance.id,
                                                nmx_c_partition_id = partition_id,
                                                "Removing configured GPU from NMX-C holding partition",
                                            );
                                            gpu_action = GpuAction::RemoveFromUnknownPartition;
                                            gpu_ctx.partition_nmx_c_id =
                                                nmxc_partition.partition_id.unwrap_or_default();
                                        } else {
                                            gpu_action = GpuAction::NoOp;
                                        }
                                    } else {
                                        // The monitor cannot safely preserve membership in an
                                        // unrecognized partition.
                                        tracing::warn!(
                                            gpu_guid = nvlink_gpu.guid,
                                            nmx_c_partition_id = partition_id,
                                            "Removing GPU from unknown partition with NMX-C ID",
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
                                // Neither a tenant partition nor a holding partition claims
                                // this GPU, so park it in its tray default partition rather
                                // than leave it outside every partition.
                                if let Err(error) = partition_ctx
                                    .ensure_gpu_enqueued_into_tray_partition(
                                        &mh.host_snapshot.id,
                                        info.domain_uuid,
                                        nvlink_gpu,
                                    )
                                {
                                    tracing::warn!(
                                        gpu_guid = nvlink_gpu.guid,
                                        machine_id = %instance.machine_id,
                                        instance_id = %instance.id,
                                        error = %error,
                                        "Failed to enqueue unpartitioned instance GPU into its tray default partition",
                                    );
                                }
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
                                nvlink_logical_partition_id = %logical_partition_id,
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
                                            error = %e,
                                            "Failed to handle GPU addition to existing partition",
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
                                            error = %e,
                                            "Failed to handle GPU addition to new partition",
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
                                        error = %e,
                                        "Failed to handle GPU removal from partition",
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
                                            error = %e,
                                            "Failed to handle GPU removal from unknown partition",
                                        );
                                    }
                                } else {
                                    tracing::error!(
                                        gpu_guid = %gpu_ctx.gpu_guid,
                                        machine_id = %instance.machine_id,
                                        nmx_c_partition_id = gpu_ctx.partition_nmx_c_id.partition_id,
                                        "NMX-C partition not found for GPU removal",
                                    );
                                    continue;
                                }
                            }
                            GpuAction::NoOp => (),
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        machine_id = %instance.machine_id,
                        "No NVLink info found",
                    );
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
        for operations in partition_ctx.unknown_partition_removal_operations.values() {
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
        for operation in partition_ctx.unknown_partition_addition_operations.values() {
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
        mh_snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
        machine_gpu_statuses: &HashMap<HostMachineId, MachineNvLinkStatusObservation>,
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
    //
    // With `GpuRemovalMode::Isolate`, the host is marked for NVLink isolation: every GPU leaves
    // every partition, including the tray default partition, and is not placed anywhere.
    pub fn check_machine_and_handle_gpu_removals(
        &self,
        mh: &ManagedHostStateSnapshot,
        partition_ctx: &mut PartitionProcessingContext,
        mode: GpuRemovalMode,
    ) -> NvLinkManagerResult<()> {
        // If not in admin-network mode, skip processing. GPUs should stay
        // attached to tenant partitions, but zero-DPU hosts are always
        // considered admin network (since they don't have a DPU to put them
        // in an overlay network). In other words, zero-DPU hosts get GPU
        // removals, but hosts with DPUs in tenant networks don't.
        // Isolation applies regardless: the tray may be failed with its
        // instance still assigned.
        if mode == GpuRemovalMode::Idle && !mh.use_admin_network() {
            return Ok(());
        }

        if let Some(nvlink_info) = &mh.host_snapshot.status.nvlink_info {
            for gpu in &nvlink_info.gpus {
                let nmxc_partition = match partition_ctx.gpu_to_partition_map.get(&gpu.guid) {
                    // GPU is in a partition, so we need to remove it from the partition.
                    Some(p) => p,
                    None if mode == GpuRemovalMode::Isolate => continue,
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

                if mode == GpuRemovalMode::Idle
                    && is_gpu_in_tray_default_partition(nmxc_partition, gpu.slot_id)
                {
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
                        partition_name: db_partition_name,
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
                        nvlink_logical_partition_id = %logical_id,
                        gpus_to_keep = ?gpus_to_keep,
                        removal_mode = ?mode,
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
                        removal_mode = ?mode,
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

    /// Queues machines in a machine group for a batched null `nvlink_status_observation` write.
    ///
    /// Entries are flushed in one transaction by [`Self::record_null_nvlink_status_observations`].
    fn queue_null_nvlink_status_observation(
        pending: &mut Vec<PendingNullNvlinkObservation>,
        group_id: &str,
        group_type: nmx_c_endpoint::ManagedHostGroupType,
        snapshots: &[&ManagedHostStateSnapshot],
        reason: ChassisNmxCUnreachableReason,
    ) {
        let machine_ids: Vec<HostMachineId> = snapshots
            .iter()
            .map(|snapshot| snapshot.host_snapshot.id)
            .collect();
        if machine_ids.is_empty() {
            return;
        }
        pending.push(PendingNullNvlinkObservation {
            group_id: group_id.to_string(),
            group_type,
            reason,
            machine_ids,
        });
    }

    /// Clears `nvlink_status_observation` for all queued machine groups in one transaction and updates metrics.
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

        let machine_ids: Vec<HostMachineId> = pending
            .iter()
            .flat_map(|entry| entry.machine_ids.iter().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let clear_started = Instant::now();
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
        record_stage(
            MonitorStage::NullObservations,
            clear_started,
            &self.instrumentation,
        );

        for entry in pending {
            tracing::info!(
                group_id = %entry.group_id,
                group_type = entry.group_type.as_str(),
                reason = ?entry.reason,
                machine_ids = ?entry.machine_ids,
                "Posted null nvlink status observations because NMX-C is unreachable for machine group"
            );
        }
        Ok(())
    }

    // Use a separate transaction to record the observations to avoid blocking the main transaction when we poll NMX-C.
    async fn record_nvlink_status_observation(
        &self,
        observations: HashMap<MachineId, MachineNvLinkStatusObservation>,
    ) -> NvLinkManagerResult<()> {
        if observations.is_empty() {
            return Ok(());
        }
        let mut obs_txn = self.db_pool.begin().await.map_err(|e| {
            NvLinkManagerError::internal(format!(
                "Failed to create transaction for nvlink status observation: {e}"
            ))
        })?;
        for (machine_id, observations) in observations {
            if let db::ConditionalWrite::NotApplied(reason) =
                db::machine::update_nvlink_status_observation(
                    &mut obs_txn,
                    &machine_id,
                    &observations,
                )
                .await?
            {
                return Err(db::DatabaseError::from(reason).into());
            }
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
                let result = match &operation.operation_type {
                    NmxcPartitionOperationType::Create => {
                        let is_tray_partition = operation.name.starts_with("tray_partition_");
                        let name = if is_tray_partition {
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
                            if is_tray_partition {
                                TRAY_PARTITION_MULTICAST_GROUPS_LIMIT
                            } else {
                                NMX_C_PARTITION_MULTICAST_GROUPS_LIMIT
                            },
                        );
                        match nmxc_client.create_partition(request.clone()).await {
                            Err(e) if e.is_nmx_resource_exhausted() => {
                                tracing::info!(
                                    nvlink_logical_partition_id = %logical_partition_id,
                                    partition_name = %name,
                                    create_partition_request = ?request,
                                    "NMX-C create partition returned NMX_ST_RESOURCE_EXHAUSTED; retrying with multicast_groups_limit=0"
                                );
                                let retry_request =
                                    nmx_c_create_partition_request(name, &operation.gpu_uids, 0);
                                let retry_request_context = format!("{retry_request:?}");
                                match nmxc_client.create_partition(retry_request).await {
                                    Ok(_) => Ok(()),
                                    Err(e) => Err(NmxcOperationError {
                                        failure_stage:
                                            NmxcOperationFailureStage::CreatePartitionRetry,
                                        error: e.to_string(),
                                        nmx_c_partition_id: String::new(),
                                        create_partition_request: retry_request_context,
                                    }),
                                }
                            }
                            Ok(_) => Ok(()),
                            Err(e) => Err(NmxcOperationError {
                                failure_stage: NmxcOperationFailureStage::CreatePartition,
                                error: e.to_string(),
                                nmx_c_partition_id: String::new(),
                                create_partition_request: format!("{request:?}"),
                            }),
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
                            Ok(_) => Ok(()),
                            Err(e) => Err(NmxcOperationError {
                                failure_stage: NmxcOperationFailureStage::DeletePartition,
                                error: e.to_string(),
                                nmx_c_partition_id: nmx_c_partition_id.to_string(),
                                create_partition_request: String::new(),
                            }),
                        }
                    }
                    NmxcPartitionOperationType::RemoveDefaultPartition(nmx_c_partition_id)
                    | NmxcPartitionOperationType::RemoveUnknownPartition(nmx_c_partition_id) => {
                        let request = libnmxc::nmxc_model::DeletePartitionRequest {
                            context: None,
                            partition_id: Some(libnmxc::nmxc_model::PartitionId {
                                partition_id: *nmx_c_partition_id,
                            }),
                            gateway_id: NMX_C_GATEWAY_ID.into(),
                            name: String::new(),
                        };
                        match nmxc_client.delete_partition(request).await {
                            Ok(_) => Ok(()),
                            Err(e) => Err(NmxcOperationError {
                                failure_stage: operation
                                    .operation_type
                                    .untracked_delete_failure_stage(),
                                error: e.to_string(),
                                nmx_c_partition_id: nmx_c_partition_id.to_string(),
                                create_partition_request: String::new(),
                            }),
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
                            Err(e) => Err(NmxcOperationError {
                                failure_stage: NmxcOperationFailureStage::GetPartitionInfo,
                                error: e.to_string(),
                                nmx_c_partition_id: nmx_c_partition_id.to_string(),
                                create_partition_request: String::new(),
                            }),
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

                                let remove_result = if to_remove.is_empty() {
                                    Ok(())
                                } else {
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
                                        Ok(_) => Ok(()),
                                        Err(e) => Err(NmxcOperationError {
                                            failure_stage: NmxcOperationFailureStage::RemoveGpus,
                                            error: e.to_string(),
                                            nmx_c_partition_id: nmx_c_partition_id.to_string(),
                                            create_partition_request: String::new(),
                                        }),
                                    }
                                };
                                if let Err(error) = remove_result {
                                    Err(error)
                                } else if !to_add.is_empty() {
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
                                        Ok(_) => Ok(()),
                                        Err(e) => Err(NmxcOperationError {
                                            failure_stage: NmxcOperationFailureStage::AddGpus,
                                            error: e.to_string(),
                                            nmx_c_partition_id: nmx_c_partition_id.to_string(),
                                            create_partition_request: String::new(),
                                        }),
                                    }
                                } else {
                                    Ok(())
                                }
                            }
                        }
                    }
                };
                let success = finish_nmxc_operation(
                    metrics,
                    &logical_partition_id,
                    &operation,
                    start_time.elapsed(),
                    result,
                    &self.instrumentation,
                )?;
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
                                    operation_name = %operation.name,
                                    "NMX-C partition not found",
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
                                operation_name = %operation.name,
                                "NMX-C partition ID not found",
                            );
                            continue;
                        };
                        let Ok(nmx_c_partition_id) = i32::try_from(nmx_c_partition_id) else {
                            tracing::error!(
                                operation_name = %operation.name,
                                "NMX-C partition ID does not fit in database column",
                            );
                            continue;
                        };

                        if operation.name.starts_with("tray_partition_") {
                            tracing::debug!(
                                nvlink_logical_partition_id = %logical_partition_id,
                                operation_name = %operation.name,
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
                    NmxcPartitionOperationType::RemoveDefaultPartition(_)
                    | NmxcPartitionOperationType::RemoveUnknownPartition(_) => {
                        // No-op, since default partition membership is not tracked in the partitions table. The status observation of the
                        // added/removed GPUs will be updated.
                    }
                }
            }
        }

        Ok(())
    }

    /// Deletes the physical partition rows that [`partitions_of_unknown_domains`]
    /// selected, once per iteration after every group ran, and counts them as
    /// stale partitions. Removing a row never touches NMX-C: a partition it
    /// described is already gone with its domain, or is reconciled through the
    /// domain's current group like any other partition NICo has no record of.
    async fn delete_partitions_of_unknown_domains(
        &self,
        partitions: &[(NvLinkPartitionId, NvLinkDomainId)],
        metrics: &mut NvlPartitionMonitorMetrics,
    ) -> NvLinkManagerResult<()> {
        if partitions.is_empty() {
            return Ok(());
        }
        let ids: Vec<NvLinkPartitionId> = partitions.iter().map(|(id, _)| *id).collect();
        let mut txn = self.db_pool.txn_begin().await?;
        let deleted = db::nvl_partition::final_delete_many(&ids, &mut txn).await?;
        txn.commit().await?;
        for (partition_id, domain_uuid) in partitions {
            if deleted.contains(partition_id) {
                tracing::warn!(
                    %domain_uuid,
                    nvlink_partition_id = %partition_id,
                    "Deleted physical partition row whose NVLink domain no managed host belongs to"
                );
            }
        }
        metrics.num_stale_partitions_deleted += deleted.len();
        Ok(())
    }

    /// Deletes the logical partitions marked as deleted once per iteration,
    /// after every group has removed its physical partitions. A logical
    /// partition spans domains, so this is not group work.
    ///
    /// A marked logical partition that an `nvlink_partitions` row still
    /// references (for example one NMX-C forgot, so no group removes it) is
    /// left in place and logged: the foreign key would fail the whole purge
    /// and keep every other eligible partition from being removed. The
    /// reference check and the delete are one statement, so a row removed or
    /// added after the iteration's snapshot cannot make the purge fail.
    async fn delete_marked_logical_partitions(
        &self,
        db_nvl_logical_partitions: &[LogicalPartition],
    ) -> NvLinkManagerResult<()> {
        let marked: Vec<NvLinkLogicalPartitionId> = db_nvl_logical_partitions
            .iter()
            .filter(|lp| model::nvl_logical_partition::is_marked_as_deleted(lp))
            .map(|lp| lp.id)
            .collect();
        if marked.is_empty() {
            return Ok(());
        }

        let mut txn = self.db_pool.txn_begin().await?;
        let deleted =
            db::nvl_logical_partition::final_delete_unreferenced(&marked, &mut txn).await?;
        txn.commit().await?;

        for id in &deleted {
            tracing::info!(nvlink_logical_partition_id = %id, "Deleted logical partition");
        }
        for id in marked.iter().filter(|id| !deleted.contains(id)) {
            tracing::warn!(
                nvlink_logical_partition_id = %id,
                "Logical partition marked as deleted still has physical partition rows; skipping deletion"
            );
        }

        Ok(())
    }

    async fn load_mnnvl_managed_host_snapshots(
        &self,
        txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> NvLinkManagerResult<HashMap<HostMachineId, ManagedHostStateSnapshot>> {
        let mnvvl_machine_ids = find_machine_ids::<HostMachineId>(
            txn.as_mut(),
            MachineSearchConfig {
                mnnvl_only: true,
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

        Ok(load_by_machine_ids(
            txn.as_mut(),
            mnvvl_machine_ids.as_slice(),
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: true,
                host_health_config: self.host_health,
            },
        )
        .await?)
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

/// Alerting classification of an NMX-C `GpuHealth` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuHealthState {
    Healthy,
    /// NMX-C explicitly reports the GPU in a degraded state.
    Unhealthy,
    /// NMX-C has not (yet) determined the GPU's health; neither raises nor clears an alert.
    Unknown,
}

/// Exhaustive on the generated enum so a new `GpuHealth` variant fails to compile until classified.
fn classify_gpu_health(h: i32) -> GpuHealthState {
    use libnmxc::nmxc_model::GpuHealth::{self, *};
    match GpuHealth::try_from(h) {
        Ok(NmxGpuHealthHealthy) => GpuHealthState::Healthy,
        Ok(NmxGpuHealthDegraded | NmxGpuHealthNoNvlink | NmxGpuHealthDegradedBw) => {
            GpuHealthState::Unhealthy
        }
        Ok(NmxGpuHealthUnknown) | Err(_) => GpuHealthState::Unknown,
    }
}

/// Computes the per-GPU `NvlinkUnhealthy` alerts a machine should carry after one NMX-C pass.
///
/// Each alert's `target` is the GPU UID, so previously flagged GPUs are known structurally. A
/// flagged GPU stays until NMX-C reports *that GPU* `Healthy`. NMX-C represents a GPU under
/// rediscovery as UID 0 with `Unknown` health, so it does not match its stored GUID; treating
/// "absent" as recovered would let one healthy sibling clear `prevent_allocations` before the
/// fault is gone. GPUs that were never flagged and are `Unknown` or absent are ignored, so a
/// swapped-in GPU awaiting discovery does not pin the machine out of the pool.
fn next_nvlink_unhealthy_alerts(
    previous: &[health_report::HealthProbeAlert],
    gpus: &[NvLinkGpu],
    health_by_guid: &HashMap<u64, i32>,
) -> Vec<health_report::HealthProbeAlert> {
    let prevent_allocations = health_report::HealthAlertClassification::prevent_allocations();
    let mut alerts: BTreeMap<String, health_report::HealthProbeAlert> = previous
        .iter()
        .filter_map(|alert| {
            let guid = alert.target.clone()?;
            let mut alert = alert.clone();
            // The API lets a caller replace this source's report with the same probe/target
            // but `prevent_allocations` stripped. A retained alert must still block, including
            // while the GPU is `Unknown` or absent, so restore it here rather than only on the
            // next explicit unhealthy observation. Other classifications are left as stored.
            if alert.id == health_report::HealthProbeId::nvlink_unhealthy()
                && !alert.classifications.contains(&prevent_allocations)
            {
                alert.classifications.push(prevent_allocations.clone());
            }
            Some((guid, alert))
        })
        .collect();

    for gpu in gpus {
        // A stored guid of 0 is the parse-failure placeholder from
        // `parse_nvlink_gpu_fabric_guid`, not a GPU identity; it can only ever collide with
        // NMX-C's UID-0 rediscovery rows, so it is never evaluated.
        if gpu.guid == 0 {
            continue;
        }
        let Some(&health) = health_by_guid.get(&gpu.guid) else {
            continue;
        };
        let guid = gpu.guid.to_string();
        match classify_gpu_health(health) {
            GpuHealthState::Healthy => {
                alerts.remove(&guid);
            }
            GpuHealthState::Unhealthy => {
                let alert = health_report::HealthProbeAlert::nvlink_unhealthy(
                    guid.clone(),
                    gpu_health_label(health),
                );
                alerts.insert(guid, alert);
            }
            GpuHealthState::Unknown => {}
        }
    }

    alerts.into_values().collect()
}

#[cfg(test)]
mod stale_partition_tracker_tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    /// A row is due only after the threshold of consecutive iterations, a pass
    /// that lists it resets the count, a confirmed delete forgets it, and a
    /// second pass of the same domain in one iteration advances nothing.
    #[test]
    fn stale_partition_tracker_debounces_misses() {
        let domain = NvLinkDomainId::new();
        let row = |nmx_c_partition_id: i32| NvlPartition {
            id: NvLinkPartitionId::new(),
            nmx_c_partition_id,
            domain_uuid: domain,
            name: NvlPartitionName::try_from(format!("p{nmx_c_partition_id}")).unwrap(),
            created: Utc::now(),
            updated: Utc::now(),
            deleted: None,
            logical_partition_id: None,
        };
        let rows = [row(1), row(2)];
        let (a, b) = (rows[0].id, rows[1].id);
        let mut tracker = StalePartitionTracker::default();

        tracker.begin_iteration(&rows);
        assert!(tracker.observe(domain, &[a, b], &[], 2).is_empty());
        // Another chassis sharing the endpoint and domain, same iteration.
        assert!(tracker.observe(domain, &[a, b], &[], 2).is_empty());

        tracker.begin_iteration(&rows);
        // `b` is listed again before its second miss.
        assert_eq!(tracker.observe(domain, &[a], &[b], 2), vec![a]);
        // The sharing chassis only sees that `a` is due.
        assert_eq!(tracker.observe(domain, &[a, b], &[], 2), vec![a]);

        tracker.begin_iteration(&rows);
        // `a` stays due until its delete is confirmed.
        assert_eq!(tracker.observe(domain, &[a, b], &[], 2), vec![a]);
        tracker.forget(&[a]);
        tracker.begin_iteration(&rows);
        assert_eq!(tracker.observe(domain, &[a, b], &[], 2), vec![b]);
        // `b`'s count is dropped with its row at the next iteration start and
        // restarts at one; `a` reaches its second miss since it was forgotten.
        tracker.begin_iteration(&rows[..1]);
        assert_eq!(tracker.observe(domain, &[a, b], &[], 2), vec![a]);
    }

    /// Which rows the iteration treats as orphans, given what the hosts' stored
    /// NVLink info and this pass's Hello results say about domains.
    #[test]
    fn orphaned_rows_follow_known_host_domains() {
        let (known, learned, stale) = (
            NvLinkDomainId::new(),
            NvLinkDomainId::new(),
            NvLinkDomainId::new(),
        );
        let rows = [
            (NvLinkPartitionId::new(), known),
            (NvLinkPartitionId::new(), learned),
            (NvLinkPartitionId::new(), stale),
        ];
        let stale_row = rows[2];
        let host = |seed: u8| {
            HostMachineId::try_from(MachineId::new(
                MachineIdSource::Tpm,
                [seed; 32],
                MachineType::Host,
            ))
            .expect("host machine id")
        };
        let (a, b) = (host(1), host(2));
        let groups = HashMap::from([(a, "rack-a".to_string()), (b, "rack-b".to_string())]);
        let select = |hosts: &[(HostMachineId, Option<NvLinkDomainId>)],
                      hellos: &[(&str, NvLinkDomainId)]| {
            partitions_of_unknown_domains(
                rows,
                &hosts.iter().copied().collect(),
                &groups,
                &hellos.iter().copied().collect(),
            )
        };

        // Every host names a domain: only the row under neither is an orphan.
        assert_eq!(
            select(&[(a, Some(known)), (b, Some(learned))], &[]),
            vec![stale_row]
        );
        // A host without a stored domain whose group's Hello answered this
        // pass: the Hello domain protects its rows.
        assert_eq!(
            select(&[(a, Some(known)), (b, None)], &[("rack-b", learned)]),
            vec![stale_row]
        );
        // The same host while its group was skipped or unreachable: defer.
        assert!(select(&[(a, Some(known)), (b, None)], &[]).is_empty());
        // No host names any domain: defer.
        assert!(select(&[(a, None), (b, None)], &[]).is_empty());
    }
}

#[cfg(test)]
mod gpu_health_tests {
    use carbide_test_support::value_scenarios;
    use libnmxc::nmxc_model::GpuHealth::*;

    use super::*;

    fn gpu(guid: u64) -> NvLinkGpu {
        NvLinkGpu {
            tray_index: 0,
            slot_id: 0,
            device_id: 1,
            guid,
        }
    }

    fn flagged(guid: u64) -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert::nvlink_unhealthy(guid.to_string(), "degraded")
    }

    fn targets(alerts: Vec<health_report::HealthProbeAlert>) -> Vec<String> {
        alerts
            .into_iter()
            .filter_map(|alert| alert.target)
            .collect()
    }

    #[test]
    fn affected_gpu_stays_flagged_until_it_reports_healthy() {
        value_scenarios!(
            run = |(previous, health): (Vec<u64>, Vec<(u64, i32)>)| {
                let previous: Vec<_> = previous.into_iter().map(flagged).collect();
                // guid 0 is the parse-failure placeholder and must never be evaluated.
                let gpus = vec![gpu(0), gpu(1), gpu(2)];
                let health_by_guid: HashMap<u64, i32> = health.into_iter().collect();
                targets(next_nvlink_unhealthy_alerts(&previous, &gpus, &health_by_guid))
            };

            "degraded GPU is flagged" {
                (vec![], vec![(1, NmxGpuHealthDegraded as i32), (2, NmxGpuHealthHealthy as i32)])
                    => vec!["1".to_string()],
            }

            "rediscovery (UID 0, Unknown) with a healthy sibling keeps the flag" {
                (vec![1], vec![(0, NmxGpuHealthUnknown as i32), (2, NmxGpuHealthHealthy as i32)])
                    => vec!["1".to_string()],
            }

            "explicit Unknown for the affected GPU keeps the flag" {
                (vec![1], vec![(1, NmxGpuHealthUnknown as i32), (2, NmxGpuHealthHealthy as i32)])
                    => vec!["1".to_string()],
            }

            "affected GPU reporting Healthy clears it" {
                (vec![1], vec![(1, NmxGpuHealthHealthy as i32), (2, NmxGpuHealthHealthy as i32)])
                    => Vec::<String>::new(),
            }

            "unflagged absent GPU is ignored" {
                (vec![], vec![(2, NmxGpuHealthHealthy as i32)]) => Vec::<String>::new(),
            }

            "empty NMX-C response holds the flag" {
                (vec![1], vec![]) => vec!["1".to_string()],
            }

            "placeholder guid 0 never matches a UID-0 report" {
                (vec![], vec![(0, NmxGpuHealthNoNvlink as i32), (1, NmxGpuHealthHealthy as i32), (2, NmxGpuHealthHealthy as i32)])
                    => Vec::<String>::new(),
            }
        );
    }

    #[test]
    fn retained_alert_restores_prevent_allocations_while_gpu_is_unknown_or_absent() {
        let mut stripped = flagged(1);
        stripped.classifications.clear();
        let prevent_allocations = health_report::HealthAlertClassification::prevent_allocations();

        value_scenarios!(
            run = |health: Vec<(u64, i32)>| {
                let health_by_guid: HashMap<u64, i32> = health.into_iter().collect();
                let alerts = next_nvlink_unhealthy_alerts(
                    std::slice::from_ref(&stripped),
                    &[gpu(1), gpu(2)],
                    &health_by_guid,
                );
                alerts
                    .iter()
                    .map(|alert| {
                        (
                            alert.target.clone(),
                            alert.classifications.contains(&prevent_allocations),
                        )
                    })
                    .collect::<Vec<_>>()
            };

            "retained while Unknown" {
                vec![(1, NmxGpuHealthUnknown as i32)] => vec![(Some("1".to_string()), true)],
            }

            "retained while absent" {
                vec![(2, NmxGpuHealthHealthy as i32)] => vec![(Some("1".to_string()), true)],
            }
        );
    }

    #[test]
    fn alert_keys_ignore_message_but_track_blocking_classifications() {
        let blocking = flagged(1);

        // Same GPU, different NMX-C label: same key, so oscillation does not rewrite.
        let relabeled =
            health_report::HealthProbeAlert::nvlink_unhealthy("1".to_string(), "degraded_bw");
        assert_eq!(
            nvlink_alert_keys(std::slice::from_ref(&blocking)),
            nvlink_alert_keys(std::slice::from_ref(&relabeled))
        );

        // Same GPU with prevent_allocations stripped (as the API allows): different key, so
        // the monitor rewrites and restores the block.
        let mut stripped = blocking.clone();
        stripped.classifications.clear();
        assert_ne!(
            nvlink_alert_keys(std::slice::from_ref(&blocking)),
            nvlink_alert_keys(std::slice::from_ref(&stripped))
        );
    }

    #[test]
    fn classify_gpu_health_buckets_each_value() {
        value_scenarios!(
            run = |h: i32| classify_gpu_health(h);

            "unhealthy" {
                NmxGpuHealthDegraded as i32 => GpuHealthState::Unhealthy,
                NmxGpuHealthNoNvlink as i32 => GpuHealthState::Unhealthy,
                NmxGpuHealthDegradedBw as i32 => GpuHealthState::Unhealthy,
            }

            "healthy" {
                NmxGpuHealthHealthy as i32 => GpuHealthState::Healthy,
            }

            "unknown" {
                NmxGpuHealthUnknown as i32 => GpuHealthState::Unknown,
                i32::MAX => GpuHealthState::Unknown,
            }
        );
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

const NVLINK_UNHEALTHY_OVERRIDE_SOURCE: &str = "nvlink-partition-monitor";

/// What a `NvlinkUnhealthy` report asserts, per alert: probe id, GPU UID target, and the
/// classifications that gate allocation. Message labels are deliberately excluded so a link
/// oscillating between degraded states does not rewrite the row every pass. Classifications
/// are deliberately included: the API lets a caller replace this source's merge report with
/// the same probe/target but empty classifications, and identity-only comparison would then
/// skip restoring `prevent_allocations` on an alert the monitor still asserts.
fn nvlink_alert_keys(
    alerts: &[health_report::HealthProbeAlert],
) -> BTreeSet<(
    health_report::HealthProbeId,
    Option<String>,
    BTreeSet<health_report::HealthAlertClassification>,
)> {
    alerts
        .iter()
        .map(|alert| {
            (
                alert.id.clone(),
                alert.target.clone(),
                alert.classifications.iter().cloned().collect(),
            )
        })
        .collect()
}

/// Builds the merge report this monitor writes for a machine with unhealthy NVLink GPUs: one
/// `prevent_allocations` alert per affected GPU, as computed by
/// [`next_nvlink_unhealthy_alerts`].
fn nvlink_unhealthy_report(
    alerts: Vec<health_report::HealthProbeAlert>,
) -> health_report::HealthReport {
    health_report::HealthReport {
        source: NVLINK_UNHEALTHY_OVERRIDE_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![],
        alerts,
    }
}

/// Source of the isolation status report this monitor keeps on hosts marked for NVLink
/// isolation.
const NVLINK_ISOLATION_REPORT_SOURCE: &str = "nvlink-isolation";

/// Builds the isolation status report for a host marked for NVLink isolation, from the NMX-C
/// partition membership seen this pass. It reports `NvlinkIsolated` as a success only when the
/// host's GPU inventory is known and NMX-C lists none of those GPUs in a partition. Otherwise
/// it carries an `NvlinkIsolationPending` alert naming the GPUs still partitioned, so an
/// operator never has to read the absence of an alert as confirmation.
fn nvlink_isolation_status_report(
    gpus: &[NvLinkGpu],
    gpu_to_partition_map: &HashMap<u64, PartitionInfo>,
) -> health_report::HealthReport {
    // A GUID of 0 is the parse-failure placeholder from `parse_nvlink_gpu_fabric_guid`, so
    // a GPU carrying it cannot be checked against NMX-C.
    let pending_message = if gpus.is_empty() || gpus.iter().any(|gpu| gpu.guid == 0) {
        Some(
            "Cannot confirm NVLink isolation: the host's NVLink GPU inventory is incomplete"
                .to_string(),
        )
    } else {
        let partitioned: Vec<String> = gpus
            .iter()
            .filter(|gpu| gpu_to_partition_map.contains_key(&gpu.guid))
            .map(|gpu| gpu.guid.to_string())
            .collect();
        (!partitioned.is_empty()).then(|| {
            format!(
                "{} of {} NVLink GPUs are still in a partition: {}",
                partitioned.len(),
                gpus.len(),
                partitioned.join(", ")
            )
        })
    };

    let (successes, alerts) = match pending_message {
        Some(message) => (
            vec![],
            vec![health_report::HealthProbeAlert {
                id: health_report::HealthProbeId::nvlink_isolation_pending(),
                target: None,
                in_alert_since: Some(Utc::now()),
                message,
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations(),
                    health_report::HealthAlertClassification::suppress_external_alerting(),
                ],
            }],
        ),
        None => (
            vec![health_report::HealthProbeSuccess {
                id: health_report::HealthProbeId::nvlink_isolated(),
                target: None,
            }],
            vec![],
        ),
    };

    health_report::HealthReport {
        source: NVLINK_ISOLATION_REPORT_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes,
        alerts,
    }
}

/// Whether two isolation status reports state the same status, ignoring timestamps, so an
/// unchanged status is not rewritten every pass.
fn same_nvlink_isolation_status(
    a: &health_report::HealthReport,
    b: &health_report::HealthReport,
) -> bool {
    let status = |report: &health_report::HealthReport| {
        (
            report
                .successes
                .iter()
                .map(|success| success.id.clone())
                .collect::<Vec<_>>(),
            report
                .alerts
                .iter()
                .map(|alert| {
                    (
                        alert.id.clone(),
                        alert.message.clone(),
                        alert.classifications.clone(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    status(a) == status(b)
}

/// Writes one of this monitor's merge reports for the machine, replacing the previous entry
/// under the report's source and leaving reports from other sources untouched.
async fn set_monitor_health_report(
    db_pool: &PgPool,
    machine_id: &MachineId,
    report: &health_report::HealthReport,
) -> NvLinkManagerResult<()> {
    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| NvLinkManagerError::internal(format!("Failed to acquire connection: {e}")))?;

    db::machine::insert_health_report(
        &mut conn,
        machine_id,
        HealthReportApplyMode::Merge,
        report,
        false, // overwrite existing
    )
    .await
    .map_err(|e| {
        NvLinkManagerError::internal(format!(
            "Failed to set {} health report: {e}",
            report.source
        ))
    })?;

    Ok(())
}

/// Removes this monitor's merge report under `source` from the machine.
async fn clear_monitor_health_report(
    db_pool: &PgPool,
    machine_id: &MachineId,
    source: &str,
) -> NvLinkManagerResult<()> {
    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| NvLinkManagerError::internal(format!("Failed to acquire connection: {e}")))?;

    db::machine::remove_health_report(&mut conn, machine_id, HealthReportApplyMode::Merge, source)
        .await
        .map_err(|e| {
            NvLinkManagerError::internal(format!("Failed to clear {source} health report: {e}"))
        })?;

    Ok(())
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
mod domain_uuid_observation_tests {
    use carbide_test_support::{Check, check_values, value_scenarios};
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use libnmxc::nmxc_model::{ServerHeader, ServerHello};

    use super::*;

    #[test]
    fn hello_domain_uuid_accepts_only_valid_non_nil_values() {
        value_scenarios!(
            run = |domain_uuid| {
                domain_uuid_from_nmx_c_hello(&ServerHello {
                    server_header: Some(ServerHeader {
                        domain_uuid: domain_uuid.to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .is_ok()
            };

            "accepted" {
                "11111111-1111-1111-1111-111111111111" => true,
            }

            "rejected" {
                "not-a-uuid" => false,
                "00000000-0000-0000-0000-000000000000" => false,
            }

        );
    }

    #[test]
    fn valid_domain_observations_update_only_changed_machine_domains()
    -> Result<(), Box<dyn std::error::Error>> {
        let machine_id = HostMachineId::try_from(MachineId::new(
            MachineIdSource::Tpm,
            [1; 32],
            MachineType::Host,
        ))?;
        let old_domain: NvLinkDomainId = "11111111-1111-1111-1111-111111111111".parse()?;
        let observed_domain: NvLinkDomainId = "22222222-2222-2222-2222-222222222222".parse()?;

        let gpu = NvLinkGpu {
            tray_index: 1,
            slot_id: 2,
            device_id: 3,
            guid: 4,
        };

        let old_info = MachineNvLinkInfo {
            domain_uuid: old_domain,
            chassis_serial: "CHASSIS-A".to_string(),
            gpus: vec![gpu],
        };

        let observed_info = MachineNvLinkInfo {
            domain_uuid: observed_domain,
            ..old_info.clone()
        };

        let initial_info = MachineNvLinkInfo {
            domain_uuid: observed_domain,
            chassis_serial: "CHASSIS-A".to_string(),
            gpus: Vec::new(),
        };

        check_values(
            [
                Check {
                    scenario: "initial population",
                    input: (None, Some("CHASSIS-A")),
                    expect: (1, initial_info),
                },
                Check {
                    scenario: "changed UUID",
                    input: (Some(old_info.clone()), Some("CHASSIS-A")),
                    expect: (1, observed_info.clone()),
                },
                Check {
                    scenario: "changed UUID with persisted chassis serial only",
                    input: (Some(old_info), None),
                    expect: (1, observed_info.clone()),
                },
                Check {
                    scenario: "identical persisted UUID after restart",
                    input: (Some(observed_info.clone()), Some("CHASSIS-A")),
                    expect: (0, observed_info),
                },
            ],
            |(existing, snapshot_chassis_serial)| {
                let mut machine_nvlink_info = HashMap::from([(machine_id, existing)]);

                let updates = populate_machine_nvlink_info_if_needed(
                    &mut machine_nvlink_info,
                    &HashMap::new(),
                    snapshot_chassis_serial,
                    &[machine_id],
                    observed_domain,
                );

                (
                    updates.len(),
                    machine_nvlink_info
                        .remove(&machine_id)
                        .flatten()
                        .expect("observation populates machine NVLink info"),
                )
            },
        );

        Ok(())
    }
}

#[cfg(test)]
mod pacing_tests {
    use std::time::Duration;

    use carbide_test_support::{Check, check_values};

    use super::idle_after_iteration;

    #[test]
    fn idle_pads_to_the_interval_and_never_drops_below_a_quarter() {
        const INTERVAL: Duration = Duration::from_secs(60);

        struct Iteration {
            took: Duration,
            num_changes: usize,
        }

        check_values(
            [
                Check {
                    scenario: "short iteration pads the cycle to the interval",
                    input: Iteration {
                        took: Duration::from_secs(10),
                        num_changes: 0,
                    },
                    expect: Duration::from_secs(50),
                },
                Check {
                    scenario: "iteration near the interval keeps the quarter floor",
                    input: Iteration {
                        took: Duration::from_secs(50),
                        num_changes: 0,
                    },
                    expect: Duration::from_secs(15),
                },
                Check {
                    scenario: "iteration over the interval idles the quarter floor",
                    input: Iteration {
                        took: Duration::from_secs(90),
                        num_changes: 0,
                    },
                    expect: Duration::from_secs(15),
                },
                Check {
                    scenario: "changes re-check after one second",
                    input: Iteration {
                        took: Duration::from_secs(10),
                        num_changes: 3,
                    },
                    expect: Duration::from_secs(1),
                },
                Check {
                    scenario: "overrun wins over the fast re-check",
                    input: Iteration {
                        took: Duration::from_secs(90),
                        num_changes: 3,
                    },
                    expect: Duration::from_secs(15),
                },
            ],
            |Iteration { took, num_changes }| idle_after_iteration(INTERVAL, took, num_changes),
        );
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

    /// The isolation status is pending, never isolated, whenever the host's GPUs cannot all be
    /// checked, and it names only the GPUs NMX-C still lists in a partition.
    #[test]
    fn isolation_status_is_pending_until_every_known_gpu_is_unpartitioned() {
        use std::collections::HashMap;

        use carbide_test_support::{Check, check_values};
        use model::hardware_info::NvLinkGpu;

        use super::nvlink_isolation_status_report;

        let gpu = |guid: u64| NvLinkGpu {
            tray_index: 1,
            slot_id: 1,
            device_id: 1,
            guid,
        };
        let partitioned = HashMap::from([(0x10, PartitionInfo::default())]);

        check_values(
            [
                Check {
                    scenario: "no GPU inventory",
                    input: vec![],
                    expect: "Cannot confirm NVLink isolation: the host's NVLink GPU inventory is incomplete".to_string(),
                },
                Check {
                    scenario: "placeholder GUID",
                    input: vec![gpu(0x11), gpu(0)],
                    expect: "Cannot confirm NVLink isolation: the host's NVLink GPU inventory is incomplete".to_string(),
                },
                Check {
                    scenario: "one of two GPUs partitioned",
                    input: vec![gpu(0x10), gpu(0x11)],
                    expect: "1 of 2 NVLink GPUs are still in a partition: 16".to_string(),
                },
            ],
            |gpus: Vec<NvLinkGpu>| {
                let report = nvlink_isolation_status_report(&gpus, &partitioned);
                assert!(report.successes.is_empty(), "pending is never reported as isolated");
                assert!(report.alerts[0].classifications.contains(
                    &health_report::HealthAlertClassification::prevent_allocations()
                ));
                report.alerts[0].message.clone()
            },
        );
    }
}

#[cfg(test)]
mod nmxc_operation_tests {
    use std::time::Duration;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::*;

    #[test]
    fn fatal_default_partition_delete_is_recorded_before_it_propagates() {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds";
        let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
        let instrumentation = Instrumentation::new(test_meter.meter());
        let captured = MetricsCapture::for_registry(test_meter.registry());
        let logical_partition_id = NvLinkLogicalPartitionId::default();
        let operation = NmxcPartitionOperation {
            domain_uuid: None,
            operation_type: NmxcPartitionOperationType::RemoveDefaultPartition(42),
            gpu_uids: vec![],
            name: String::new(),
            db_partition_id: None,
        };
        let mut metrics = NvlPartitionMonitorMetrics::new();
        let mut result = None;
        let logs = capture_logs(|| {
            result = Some(finish_nmxc_operation(
                &mut metrics,
                &logical_partition_id,
                &operation,
                Duration::from_millis(250),
                Err(NmxcOperationError {
                    failure_stage: operation.operation_type.untracked_delete_failure_stage(),
                    error: "delete failed".to_string(),
                    nmx_c_partition_id: "42".to_string(),
                    create_partition_request: String::new(),
                }),
                &instrumentation,
            ));
        });

        let error = result
            .expect("operation result")
            .expect_err("fatal failure");
        assert_eq!(
            error.to_string(),
            "internal error: failed to delete default partition: delete failed"
        );
        let change = AppliedChange {
            operation: NmxcMetricOperation::RemoveDefaultPartition,
            status: NmxcMetricOperationStatus::Failed,
        };
        assert_eq!(metrics.applied_changes.get(&change), Some(&1));
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(logs[0].message, "Failed to delete default partition");
        assert_eq!(logs[0].field("nmx_c_partition_id"), Some("42"));
        assert_eq!(
            captured.histogram_count_delta(
                METRIC_NAME,
                &[
                    ("operation", "remove_default_partition"),
                    ("status", "failed"),
                ],
            ),
            1
        );
    }

    /// A rejected delete of an emptied unknown or tray default partition, such as one left
    /// behind by NVLink isolation, is recorded as a failed operation but does not abort the pass.
    #[test]
    fn rejected_unknown_partition_delete_is_not_fatal() {
        let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
        let instrumentation = Instrumentation::new(test_meter.meter());
        let operation = NmxcPartitionOperation {
            domain_uuid: None,
            operation_type: NmxcPartitionOperationType::RemoveUnknownPartition(9),
            gpu_uids: vec![],
            name: String::new(),
            db_partition_id: None,
        };
        let mut metrics = NvlPartitionMonitorMetrics::new();

        let result = finish_nmxc_operation(
            &mut metrics,
            &NvLinkLogicalPartitionId::default(),
            &operation,
            Duration::from_millis(250),
            Err(NmxcOperationError {
                failure_stage: operation.operation_type.untracked_delete_failure_stage(),
                error: "delete failed".to_string(),
                nmx_c_partition_id: "9".to_string(),
                create_partition_request: String::new(),
            }),
            &instrumentation,
        );

        assert!(
            !result.expect("not fatal to the pass"),
            "reported as not succeeded"
        );
        let change = AppliedChange {
            operation: NmxcMetricOperation::RemoveDefaultPartition,
            status: NmxcMetricOperationStatus::Failed,
        };
        assert_eq!(metrics.applied_changes.get(&change), Some(&1));
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

#[cfg(test)]
mod machine_group_tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Duration;

    use carbide_instrument::Instrumentation;
    use carbide_instrument::testing::MetricsCapture;
    use carbide_macros::sqlx_test;
    use carbide_test_support::{Check, check_values};
    use carbide_utils::test_support::test_meter::TestMeter;
    use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::rack::{RackId, RackProfileId};
    use db::test_support::power_shelf::create_seeded;
    use db::test_support::switch::create_seeded_discovered;
    use model::hardware_info::{MachineNvLinkInfo, NvLinkGpu};
    use model::machine::{HostHealthConfig, ManagedHostStateSnapshot};
    use model::rack::RackConfig;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;
    use tokio::task::JoinSet;

    use super::{
        ChassisNmxCUnreachableReason, GroupResult, LoadSnapshotOptions,
        NVLINK_ISOLATION_REPORT_SOURCE, NVLINK_UNHEALTHY_OVERRIDE_SOURCE, NvLinkConfig,
        NvlPartitionMonitor, NvlPartitionMonitorMetrics, ProcessMachineGroupInput,
        group_managed_hosts_by_group_type, load_by_machine_ids, nmx_c_endpoint,
    };
    use crate::group_state::group_fingerprint;
    use crate::nvlink::test_support::NmxcSimClient;

    #[derive(Clone, Debug)]
    struct HostSpec {
        id_byte: u8,
        chassis_serial: Option<&'static str>,
        rack_id: Option<&'static str>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct GroupingSummary {
        chassis: BTreeMap<String, BTreeSet<u8>>,
        racks: BTreeMap<String, BTreeSet<u8>>,
    }

    fn machine_id(byte: u8) -> HostMachineId {
        MachineId::new(MachineIdSource::Tpm, [byte; 32], MachineType::Host)
            .try_into()
            .unwrap()
    }

    fn snapshot(spec: &HostSpec) -> ManagedHostStateSnapshot {
        let mut snapshot = managed_host_state_snapshot();
        snapshot.host_snapshot.id = machine_id(spec.id_byte);
        snapshot.host_snapshot.rack_id = spec.rack_id.map(RackId::new);
        snapshot.host_snapshot.status.hardware_info = None;
        snapshot.host_snapshot.status.nvlink_info =
            spec.chassis_serial.map(|serial| MachineNvLinkInfo {
                domain_uuid: NvLinkDomainId::nil(),
                chassis_serial: serial.to_string(),
                gpus: vec![],
            });
        snapshot
    }

    fn summarize(hosts: &[HostSpec]) -> GroupingSummary {
        let snapshots: HashMap<HostMachineId, ManagedHostStateSnapshot> = hosts
            .iter()
            .map(|spec| {
                let snapshot = snapshot(spec);
                (snapshot.host_snapshot.id, snapshot)
            })
            .collect();
        let id_bytes: HashMap<HostMachineId, u8> = hosts
            .iter()
            .map(|spec| (machine_id(spec.id_byte), spec.id_byte))
            .collect();

        let (by_chassis, by_rack) = group_managed_hosts_by_group_type(&snapshots);

        let chassis = by_chassis
            .into_iter()
            .map(|(serial, members)| {
                let ids = members
                    .into_iter()
                    .map(|snapshot| id_bytes[&snapshot.host_snapshot.id])
                    .collect();
                (serial, ids)
            })
            .collect();
        let racks = by_rack
            .into_iter()
            .map(|(rack_id, members)| {
                let ids = members
                    .into_iter()
                    .map(|snapshot| id_bytes[&snapshot.host_snapshot.id])
                    .collect();
                (rack_id.to_string(), ids)
            })
            .collect();
        GroupingSummary { chassis, racks }
    }

    #[test]
    fn groups_hosts_by_chassis_or_rack() {
        check_values(
            [
                Check {
                    scenario: "chassis only",
                    input: vec![HostSpec {
                        id_byte: 1,
                        chassis_serial: Some("CHASSIS-A"),
                        rack_id: None,
                    }],
                    expect: GroupingSummary {
                        chassis: BTreeMap::from([("CHASSIS-A".to_string(), BTreeSet::from([1]))]),
                        racks: BTreeMap::new(),
                    },
                },
                Check {
                    scenario: "rack only even with chassis serial",
                    input: vec![HostSpec {
                        id_byte: 2,
                        chassis_serial: Some("CHASSIS-A"),
                        rack_id: Some("rack-1"),
                    }],
                    expect: GroupingSummary {
                        chassis: BTreeMap::new(),
                        racks: BTreeMap::from([("rack-1".to_string(), BTreeSet::from([2]))]),
                    },
                },
                Check {
                    scenario: "mixed chassis and rack hosts are disjoint",
                    input: vec![
                        HostSpec {
                            id_byte: 3,
                            chassis_serial: Some("CHASSIS-A"),
                            rack_id: None,
                        },
                        HostSpec {
                            id_byte: 4,
                            chassis_serial: Some("CHASSIS-B"),
                            rack_id: Some("rack-1"),
                        },
                    ],
                    expect: GroupingSummary {
                        chassis: BTreeMap::from([("CHASSIS-A".to_string(), BTreeSet::from([3]))]),
                        racks: BTreeMap::from([("rack-1".to_string(), BTreeSet::from([4]))]),
                    },
                },
                Check {
                    scenario: "no serial and no rack is dropped",
                    input: vec![HostSpec {
                        id_byte: 5,
                        chassis_serial: None,
                        rack_id: None,
                    }],
                    expect: GroupingSummary {
                        chassis: BTreeMap::new(),
                        racks: BTreeMap::new(),
                    },
                },
                Check {
                    scenario: "same chassis two hosts",
                    input: vec![
                        HostSpec {
                            id_byte: 6,
                            chassis_serial: Some("CHASSIS-A"),
                            rack_id: None,
                        },
                        HostSpec {
                            id_byte: 7,
                            chassis_serial: Some("CHASSIS-A"),
                            rack_id: None,
                        },
                    ],
                    expect: GroupingSummary {
                        chassis: BTreeMap::from([(
                            "CHASSIS-A".to_string(),
                            BTreeSet::from([6, 7]),
                        )]),
                        racks: BTreeMap::new(),
                    },
                },
                Check {
                    scenario: "same rack two chassis",
                    input: vec![
                        HostSpec {
                            id_byte: 8,
                            chassis_serial: Some("CHASSIS-A"),
                            rack_id: Some("rack-1"),
                        },
                        HostSpec {
                            id_byte: 9,
                            chassis_serial: Some("CHASSIS-B"),
                            rack_id: Some("rack-1"),
                        },
                    ],
                    expect: GroupingSummary {
                        chassis: BTreeMap::new(),
                        racks: BTreeMap::from([("rack-1".to_string(), BTreeSet::from([8, 9]))]),
                    },
                },
                Check {
                    scenario: "empty chassis serial is treated as missing",
                    input: vec![HostSpec {
                        id_byte: 10,
                        chassis_serial: Some("   "),
                        rack_id: None,
                    }],
                    expect: GroupingSummary {
                        chassis: BTreeMap::new(),
                        racks: BTreeMap::new(),
                    },
                },
            ],
            |hosts| summarize(&hosts),
        );
    }

    /// Reloads the persisted snapshot of a seeded machine.
    async fn load_snapshot(
        pool: &sqlx::PgPool,
        id: &MachineId,
    ) -> Result<ManagedHostStateSnapshot, Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let mut snapshots = load_by_machine_ids(
            txn.as_mut(),
            &[*id],
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: false,
                host_health_config: HostHealthConfig::default(),
            },
        )
        .await?;
        Ok(snapshots.remove(id).expect("seeded machine loads"))
    }

    /// Persistence contract for `record_nvlink_unhealthy_alerts`, which the pure helpers cannot
    /// prove: an unhealthy GPU writes a `prevent_allocations` merge report under this monitor's
    /// source that the aggregate health gate sees, and recovery removes exactly that source,
    /// leaving other sources untouched.
    #[sqlx_test]
    async fn monitor_pass_persists_then_clears_nvlink_unhealthy_report(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use libnmxc::nmxc_model::{GpuHealth, GpuInfo};
        use model::hardware_info::NvLinkGpu;
        use model::machine::ManagedHostState;

        const OTHER_SOURCE: &str = "other-monitor";
        let gpu_guid: u64 = 0x1234_5678_9abc_def0;
        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;
        let id = machine_id(41);

        let blocks_allocation = |snapshot: &ManagedHostStateSnapshot| {
            snapshot
                .aggregate_health
                .find_alert_by_classification(
                    &health_report::HealthAlertClassification::prevent_allocations(),
                )
                .is_some()
        };

        let mut txn = pool.begin().await?;
        db::machine::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        db::machine::update_nvlink_info(
            txn.as_mut(),
            &id,
            MachineNvLinkInfo {
                domain_uuid,
                chassis_serial: "CHASSIS-A".to_string(),
                gpus: vec![NvLinkGpu {
                    tray_index: 0,
                    slot_id: 0,
                    device_id: 1,
                    guid: gpu_guid,
                }],
            },
        )
        .await?;
        // An unrelated merge source the monitor must never touch.
        db::machine::insert_health_report(
            txn.as_mut(),
            &id,
            health_report::HealthReportApplyMode::Merge,
            &health_report::HealthReport::empty(OTHER_SOURCE.to_string()),
            false,
        )
        .await?;
        txn.commit().await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();
        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            Arc::new(NmxcSimClient::default()),
            test_meter.meter(),
            NvLinkConfig::default(),
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );

        let nmx_c_reports = |health: GpuHealth| {
            vec![GpuInfo {
                gpu_uid: gpu_guid,
                gpu_health: health as i32,
                ..Default::default()
            }]
        };

        let before = load_snapshot(&pool, &id).await?;
        assert!(
            !blocks_allocation(&before),
            "seeded machine starts allocatable"
        );
        let machine_nvlink_info =
            HashMap::from([(id, before.host_snapshot.status.nvlink_info.clone())]);

        // Pass 1: NMX-C reports the GPU degraded.
        monitor
            .record_nvlink_unhealthy_alerts(
                &domain_uuid,
                &nmx_c_reports(GpuHealth::NmxGpuHealthDegraded),
                &machine_nvlink_info,
                &HashMap::from([(id, before)]),
            )
            .await;

        let after_set = load_snapshot(&pool, &id).await?;
        let stored = after_set
            .host_snapshot
            .health_reports
            .merges
            .get(NVLINK_UNHEALTHY_OVERRIDE_SOURCE)
            .expect("monitor persisted its merge report");
        assert_eq!(stored.alerts.len(), 1, "one alert per affected GPU");
        assert_eq!(
            stored.alerts[0].target.as_deref(),
            Some(gpu_guid.to_string().as_str()),
            "alert targets the affected GPU UID"
        );
        assert!(
            after_set
                .host_snapshot
                .health_reports
                .merges
                .contains_key(OTHER_SOURCE),
            "unrelated source survives the set"
        );
        assert!(
            blocks_allocation(&after_set),
            "aggregate health blocks allocation"
        );

        // Pass 2: the same GPU reports healthy.
        monitor
            .record_nvlink_unhealthy_alerts(
                &domain_uuid,
                &nmx_c_reports(GpuHealth::NmxGpuHealthHealthy),
                &machine_nvlink_info,
                &HashMap::from([(id, after_set)]),
            )
            .await;

        let after_clear = load_snapshot(&pool, &id).await?;
        assert!(
            !after_clear
                .host_snapshot
                .health_reports
                .merges
                .contains_key(NVLINK_UNHEALTHY_OVERRIDE_SOURCE),
            "monitor removed its merge report on recovery"
        );
        assert!(
            after_clear
                .host_snapshot
                .health_reports
                .merges
                .contains_key(OTHER_SOURCE),
            "unrelated source survives the clear"
        );
        assert!(
            !blocks_allocation(&after_clear),
            "machine is allocatable again"
        );

        join_set.shutdown().await;
        Ok(())
    }

    /// A GPU health result that was not persisted is not a good check: cached
    /// as one, the group would be skipped and the allocation block left stale
    /// until its next full check. The write is retried by the next pass.
    #[sqlx_test]
    async fn failed_health_report_write_fails_the_check_and_is_retried_next_pass(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use libnmxc::nmxc_model::{GpuHealth, GpuInfo};
        use model::hardware_info::NvLinkGpu;
        use model::machine::ManagedHostState;

        const GROUPS_METRIC: &str = "carbide_nvlink_partition_monitor_groups_total";
        let gpu_guid: u64 = 0x1234_5678_9abc_def0;
        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;
        let id = machine_id(42);

        // The per-group check an iteration runs for a chassis, from the persisted machine.
        async fn check_group(
            monitor: &NvlPartitionMonitor,
            snapshot: ManagedHostStateSnapshot,
        ) -> GroupResult {
            let machine_id = snapshot.host_snapshot.id;
            let nvlink_info = snapshot.host_snapshot.status.nvlink_info.clone();
            let all_snapshots = HashMap::from([(machine_id, snapshot)]);
            let chassis_snapshots = vec![&all_snapshots[&machine_id]];
            monitor
                .process_nmx_c_partition_monitor_group(ProcessMachineGroupInput {
                    group_id: "CHASSIS-A".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                    snapshots: &chassis_snapshots,
                    endpoint_url: Some("http://nmxc.example:9370"),
                    rack_id: None,
                    all_managed_host_snapshots: &all_snapshots,
                    machine_nvlink_info: HashMap::from([(machine_id, nvlink_info)]),
                    db_nvl_partitions: &[],
                    db_nvl_logical_partitions: &[],
                })
                .await
        }

        let mut txn = pool.begin().await?;
        db::machine::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        db::machine::update_nvlink_info(
            txn.as_mut(),
            &id,
            MachineNvLinkInfo {
                domain_uuid,
                chassis_serial: "CHASSIS-A".to_string(),
                gpus: vec![NvLinkGpu {
                    tray_index: 0,
                    slot_id: 0,
                    device_id: 1,
                    guid: gpu_guid,
                }],
            },
        )
        .await?;
        // Rejects health report writes until dropped; other machine columns stay writable.
        sqlx::query(
            "CREATE FUNCTION reject_health_report_write() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RAISE EXCEPTION 'health_reports write rejected'; END $$",
        )
        .execute(txn.as_mut())
        .await?;
        sqlx::query(
            "CREATE TRIGGER reject_health_report_write BEFORE UPDATE OF health_reports ON machines
             FOR EACH ROW EXECUTE FUNCTION reject_health_report_write()",
        )
        .execute(txn.as_mut())
        .await?;
        txn.commit().await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();
        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            Arc::new(NmxcSimClient::with_gpu_info_list(vec![GpuInfo {
                gpu_uid: gpu_guid,
                gpu_health: GpuHealth::NmxGpuHealthDegraded as i32,
                ..Default::default()
            }])),
            test_meter.meter(),
            NvLinkConfig::default(),
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );

        // Pass 1: the write is rejected.
        let metrics = MetricsCapture::for_registry(test_meter.registry());
        let rejected = check_group(&monitor, load_snapshot(&pool, &id).await?).await;
        assert_eq!(
            metrics.counter_delta(GROUPS_METRIC, &[("outcome", "health_not_recorded")]),
            1.0
        );
        drop(metrics);
        assert!(
            !rejected.check_ok,
            "an unrecorded health pass is not a good check"
        );
        assert_eq!(
            rejected.partial_metrics.num_machines_scanned, 1,
            "partition reconciliation still ran"
        );
        let after_rejected = load_snapshot(&pool, &id).await?;
        assert!(
            !after_rejected
                .host_snapshot
                .health_reports
                .merges
                .contains_key(NVLINK_UNHEALTHY_OVERRIDE_SOURCE),
            "nothing persisted while the write is rejected"
        );

        // Pass 2: the write goes through.
        sqlx::query("DROP TRIGGER reject_health_report_write ON machines")
            .execute(&pool)
            .await?;
        let retried = check_group(&monitor, after_rejected).await;
        assert!(retried.check_ok, "the retried pass is a good check");
        let after_retry = load_snapshot(&pool, &id).await?;
        let stored = after_retry
            .host_snapshot
            .health_reports
            .merges
            .get(NVLINK_UNHEALTHY_OVERRIDE_SOURCE)
            .expect("retried write persisted the report");
        assert_eq!(stored.alerts.len(), 1, "one alert per affected GPU");
        assert_eq!(
            stored.alerts[0].id,
            health_report::HealthProbeId::nvlink_unhealthy()
        );
        assert!(
            stored.alerts[0]
                .classifications
                .contains(&health_report::HealthAlertClassification::prevent_allocations()),
            "alert blocks allocation"
        );

        join_set.shutdown().await;
        Ok(())
    }

    /// A skipped rack can carry an outage report written by another API replica.
    /// Only a reachable check or removal from managed inventory clears it.
    #[sqlx_test]
    async fn rack_unreachable_alert_cleanup_preserves_skipped_racks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let skipped_rack = RackId::new("skipped-rack");
        let reachable_rack = RackId::new("reachable-rack");
        let unmanaged_rack = RackId::new("unmanaged-rack");
        let cases = [
            (&skipped_rack, true),
            (&reachable_rack, false),
            (&unmanaged_rack, false),
        ];
        let report = crate::rack_alert::nmxc_unreachable_report(
            NVLINK_UNHEALTHY_OVERRIDE_SOURCE,
            Some("https://10.0.0.5:9370"),
            ChassisNmxCUnreachableReason::HelloFailed,
            chrono::Utc::now(),
        );
        let mut txn = pool.begin().await?;
        for (rack_id, _) in cases {
            db::rack::create(
                txn.as_mut(),
                rack_id,
                Some(&RackProfileId::new("NVL72")),
                &RackConfig::default(),
                None,
            )
            .await?;
            db::rack::insert_health_report(
                txn.as_mut(),
                rack_id,
                health_report::HealthReportApplyMode::Merge,
                &report,
            )
            .await?;
        }
        txn.commit().await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();
        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            Arc::new(NmxcSimClient::default()),
            test_meter.meter(),
            NvLinkConfig::default(),
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );
        let managed_rack_ids = HashSet::from([skipped_rack.clone(), reachable_rack.clone()]);
        let outcomes = [(reachable_rack.clone(), None)];
        monitor
            .reconcile_rack_unreachable_alerts(&outcomes, &HashMap::new(), &managed_rack_ids)
            .await;

        for (rack_id, retained) in cases {
            let reports: Option<sqlx::types::Json<model::health::HealthReportSources>> =
                sqlx::query_scalar("SELECT health_reports FROM racks WHERE id = $1")
                    .bind(rack_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(
                reports
                    .as_ref()
                    .and_then(|reports| { reports.0.merges.get(NVLINK_UNHEALTHY_OVERRIDE_SOURCE) }),
                retained.then_some(&report),
                "stored report for {rack_id}"
            );
        }
        join_set.shutdown().await;
        Ok(())
    }

    #[sqlx_test]
    async fn rack_group_publishes_domain_and_queues_endpoint_failures(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rack_id = RackId::new("rack-1");
        let mut txn = pool.begin().await?;

        db::rack::create(
            txn.as_mut(),
            &rack_id,
            Some(&RackProfileId::new("NVL72")),
            &RackConfig::default(),
            None,
        )
        .await?;

        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let endpoint_switch = create_seeded_discovered(txn.as_mut(), 31, "endpoint switch").await?;
        let other_switch = create_seeded_discovered(txn.as_mut(), 32, "other switch").await?;

        for switch_id in [endpoint_switch.id, other_switch.id] {
            sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
                .bind(&rack_id)
                .bind(switch_id)
                .execute(txn.as_mut())
                .await?;
        }

        let power_shelf = create_seeded(txn.as_mut(), 33, "rack power shelf").await?;
        sqlx::query("UPDATE power_shelves SET rack_id = $1 WHERE id = $2")
            .bind(&rack_id)
            .bind(power_shelf.id)
            .execute(txn.as_mut())
            .await?;

        txn.commit().await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

        let test_meter = TestMeter::default();

        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            Arc::new(NmxcSimClient::default()),
            test_meter.meter(),
            NvLinkConfig::default(),
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );

        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;
        let mut managed_snapshot = snapshot(&HostSpec {
            id_byte: 11,
            chassis_serial: Some("CHASSIS-A"),
            rack_id: Some("rack-1"),
        });
        managed_snapshot
            .host_snapshot
            .status
            .nvlink_info
            .as_mut()
            .expect("test snapshot has nvlink info")
            .domain_uuid = domain_uuid;
        let machine_id = managed_snapshot.host_snapshot.id;
        let all_snapshots = HashMap::from([(machine_id, managed_snapshot)]);
        let rack_snapshots = vec![
            all_snapshots
                .get(&machine_id)
                .expect("test snapshot is present"),
        ];
        let machine_nvlink_info = all_snapshots[&machine_id]
            .host_snapshot
            .status
            .nvlink_info
            .clone();

        let cases = [
            (
                "successful endpoint",
                Some("http://nmxc.example:9370"),
                None,
                1,
            ),
            (
                "missing endpoint",
                None,
                Some(ChassisNmxCUnreachableReason::NoEndpoint),
                0,
            ),
            (
                "invalid endpoint",
                Some("not a valid uri"),
                Some(ChassisNmxCUnreachableReason::InvalidEndpointUri),
                0,
            ),
        ];

        for (scenario, endpoint_url, expected_reason, expected_machines_scanned) in cases {
            let machine_nvlink_info = HashMap::from([(machine_id, machine_nvlink_info.clone())]);

            let GroupResult {
                completed_operations,
                null_observations: pending,
                partial_metrics: metrics,
                check_ok: _,
                rack_domain_uuid,
                unreachable_reason: _,
                hello_domain_uuid: _,
            } = monitor
                .process_nmx_c_partition_monitor_group(ProcessMachineGroupInput {
                    group_id: "rack-1".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Rack,
                    snapshots: &rack_snapshots,
                    endpoint_url,
                    rack_id: Some(&rack_id),
                    all_managed_host_snapshots: &all_snapshots,
                    machine_nvlink_info,
                    db_nvl_partitions: &[],
                    db_nvl_logical_partitions: &[],
                })
                .await;

            assert_eq!(completed_operations, 0, "{scenario}");
            assert_eq!(
                metrics.num_machines_scanned, expected_machines_scanned,
                "{scenario}"
            );
            assert_eq!(
                rack_domain_uuid,
                expected_reason.is_none().then_some(domain_uuid),
                "{scenario}"
            );
            match expected_reason {
                Some(reason) => {
                    assert_eq!(pending.len(), 1, "{scenario}");
                    assert_eq!(pending[0].group_id, "rack-1", "{scenario}");
                    assert_eq!(pending[0].group_type.as_str(), "rack", "{scenario}");
                    assert_eq!(pending[0].reason, reason, "{scenario}");
                    assert_eq!(pending[0].machine_ids, vec![machine_id], "{scenario}");
                }
                None => {
                    assert!(pending.is_empty(), "{scenario}");
                    assert_eq!(
                        metrics.nmxc.endpoint,
                        endpoint_url.expect("successful scenario has an endpoint"),
                        "{scenario}"
                    );
                }
            }
        }

        // Nil is a valid protocol UUID representation but not a domain
        // observation. It must not erase the value published by the successful
        // rack-group case above.
        monitor
            .record_rack_switch_domain_uuid(&rack_id, NvLinkDomainId::nil())
            .await;

        let matching_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM switches WHERE rack_id = $1 AND nvlink_domain_uuid = $2),
                (SELECT COUNT(*) FROM power_shelves WHERE rack_id = $1 AND nvlink_domain_uuid = $2)",
        )
        .bind(&rack_id)
        .bind(domain_uuid)
        .fetch_one(&pool)
        .await?;

        assert_eq!(matching_counts, (2, 1));

        sqlx::query("UPDATE switches SET nvlink_domain_uuid = NULL WHERE rack_id = $1")
            .bind(&rack_id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE power_shelves SET nvlink_domain_uuid = NULL WHERE rack_id = $1")
            .bind(&rack_id)
            .execute(&pool)
            .await?;

        monitor
            .observe_and_record_rack_switch_domain_uuid(&rack_id, "http://nmxc.example:9370")
            .await;

        let matching_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM switches WHERE rack_id = $1 AND nvlink_domain_uuid = $2),
                (SELECT COUNT(*) FROM power_shelves WHERE rack_id = $1 AND nvlink_domain_uuid = $2)",
        )
        .bind(&rack_id)
        .bind(domain_uuid)
        .fetch_one(&pool)
        .await?;

        assert_eq!(matching_counts, (2, 1));

        drop(monitor);
        join_set.shutdown().await;

        Ok(())
    }

    /// Every database input that changes what a check would do must change
    /// the fingerprint, and rebuilding the same inputs must not.
    #[test]
    fn group_fingerprint_follows_each_reconcile_input() {
        use carbide_uuid::instance::InstanceId;
        use carbide_uuid::nvlink::{NvLinkLogicalPartitionId, NvLinkPartitionId};
        use config_version::ConfigVersion;
        use model::hardware_info::NvLinkGpu;
        use model::instance::config::InstanceConfig;
        use model::instance::config::nvlink::{InstanceNvLinkConfig, InstanceNvLinkGpuConfig};
        use model::instance::config::tenant_config::TenantConfig;
        use model::instance::snapshot::InstanceSnapshot;
        use model::instance::status::InstanceStatusObservations;
        use model::metadata::Metadata;
        use model::nvl_logical_partition::{LogicalPartition, LogicalPartitionState};
        use model::nvl_partition::{NvlPartition, NvlPartitionName};
        use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};

        struct Spec {
            endpoint: Option<&'static str>,
            host_bytes: Vec<u8>,
            with_instance: bool,
            config_version: u64,
            gpu_logical_partition: Option<NvLinkLogicalPartitionId>,
            admin_network: bool,
            isolated: bool,
            gpu_guid: u64,
            domain: NvLinkDomainId,
            partition_in_domain: bool,
            logical_partition_deleted: bool,
            unrelated_deleted_logical_partition: Option<NvLinkLogicalPartitionId>,
        }

        let domain_a: NvLinkDomainId = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let domain_b: NvLinkDomainId = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        let logical_partition_id = NvLinkLogicalPartitionId::from(uuid::Uuid::from_u128(0x10));
        let other_logical_partition_id =
            NvLinkLogicalPartitionId::from(uuid::Uuid::from_u128(0x11));

        let base = move || Spec {
            endpoint: Some("http://nmxc-a.example:9370"),
            host_bytes: vec![41, 42],
            with_instance: true,
            config_version: 3,
            gpu_logical_partition: Some(logical_partition_id),
            admin_network: false,
            isolated: false,
            gpu_guid: 0xdb48_8cb1_7978_0480,
            domain: domain_a,
            partition_in_domain: true,
            logical_partition_deleted: false,
            unrelated_deleted_logical_partition: None,
        };

        fn instance(spec: &Spec, machine_id: HostMachineId) -> InstanceSnapshot {
            InstanceSnapshot {
                id: InstanceId::from(uuid::Uuid::from_u128(0x20)),
                machine_id,
                instance_type_id: None,
                metadata: Metadata::default(),
                config: InstanceConfig {
                    tenant: TenantConfig {
                        tenant_organization_id: "tenant-a".parse().unwrap(),
                        tenant_keyset_ids: Vec::new(),
                        hostname: None,
                    },
                    os: OperatingSystem {
                        user_data: None,
                        variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                            ipxe_script: "boot".to_string(),
                        }),
                        phone_home_enabled: false,
                        run_provisioning_instructions_on_every_boot: false,
                    },
                    network: Default::default(),
                    infiniband: Default::default(),
                    network_security_group_id: None,
                    extension_services: Default::default(),
                    nvlink: InstanceNvLinkConfig {
                        gpu_configs: vec![InstanceNvLinkGpuConfig {
                            logical_partition_id: spec.gpu_logical_partition,
                            device_instance: 0,
                        }],
                    },
                    spxconfig: Default::default(),
                    power_profile: None,
                },
                nic_firmware_snapshot: None,
                config_version: ConfigVersion::initial(),
                network_config_version: ConfigVersion::initial(),
                ib_config_version: ConfigVersion::initial(),
                nvlink_config_version: ConfigVersion::new(spec.config_version),
                spx_config_version: ConfigVersion::initial(),
                storage_config_version: ConfigVersion::initial(),
                extension_services_config_version: ConfigVersion::initial(),
                observations: InstanceStatusObservations {
                    network: HashMap::new(),
                    extension_services: HashMap::new(),
                    phone_home_last_contact: None,
                },
                use_custom_pxe_on_boot: false,
                custom_pxe_reboot_requested: false,
                deleted: None,
                update_network_config_request: None,
            }
        }

        fn fingerprint(spec: &Spec, logical_partition_id: NvLinkLogicalPartitionId) -> u64 {
            let now = chrono::Utc::now();
            let snapshots: Vec<ManagedHostStateSnapshot> = spec
                .host_bytes
                .iter()
                .map(|&byte| {
                    let mut snapshot = snapshot(&HostSpec {
                        id_byte: byte,
                        chassis_serial: Some("CHASSIS-A"),
                        rack_id: Some("rack-1"),
                    });
                    snapshot
                        .host_snapshot
                        .network_config
                        .value
                        .use_admin_network = Some(spec.admin_network);
                    if spec.isolated {
                        snapshot
                            .aggregate_health
                            .alerts
                            .push(nvlink_isolation_alert());
                    }
                    if spec.with_instance {
                        snapshot.instance = Some(instance(spec, snapshot.host_snapshot.id));
                    }
                    snapshot
                })
                .collect();
            let all_snapshots: HashMap<HostMachineId, ManagedHostStateSnapshot> = snapshots
                .into_iter()
                .map(|snapshot| (snapshot.host_snapshot.id, snapshot))
                .collect();
            let mut group_snapshots: Vec<&ManagedHostStateSnapshot> =
                all_snapshots.values().collect();
            // Hosts arrive in hash-map order; the fingerprint must not depend on it.
            group_snapshots.reverse();
            let machine_nvlink_info = all_snapshots
                .keys()
                .map(|&id| {
                    (
                        id,
                        Some(MachineNvLinkInfo {
                            domain_uuid: spec.domain,
                            chassis_serial: "CHASSIS-A".to_string(),
                            gpus: vec![NvLinkGpu {
                                tray_index: 1,
                                slot_id: 1,
                                device_id: 1,
                                guid: spec.gpu_guid,
                            }],
                        }),
                    )
                })
                .collect();
            let partitions = vec![NvlPartition {
                id: NvLinkPartitionId::from(uuid::Uuid::from_u128(0x30)),
                nmx_c_partition_id: 7,
                domain_uuid: if spec.partition_in_domain {
                    spec.domain
                } else {
                    "33333333-3333-3333-3333-333333333333".parse().unwrap()
                },
                name: NvlPartitionName::try_from("p-7".to_string()).unwrap(),
                created: now,
                updated: now,
                deleted: None,
                logical_partition_id: Some(logical_partition_id),
            }];
            let logical_partition = |id, deleted: bool| LogicalPartition {
                id,
                name: "lp".to_string(),
                description: String::new(),
                tenant_organization_id: "tenant-a".parse().unwrap(),
                config_version: ConfigVersion::initial(),
                partition_state: LogicalPartitionState::Ready,
                created: now,
                updated: now,
                deleted: deleted.then_some(now),
            };
            let mut logical_partitions = vec![logical_partition(
                logical_partition_id,
                spec.logical_partition_deleted,
            )];
            if let Some(id) = spec.unrelated_deleted_logical_partition {
                logical_partitions.push(logical_partition(id, true));
            }
            let rack_id = RackId::new("rack-1");
            group_fingerprint(&ProcessMachineGroupInput {
                group_id: "rack-1".to_string(),
                group_type: nmx_c_endpoint::ManagedHostGroupType::Rack,
                snapshots: &group_snapshots,
                endpoint_url: spec.endpoint,
                rack_id: Some(&rack_id),
                all_managed_host_snapshots: &all_snapshots,
                machine_nvlink_info,
                db_nvl_partitions: &partitions,
                db_nvl_logical_partitions: &logical_partitions,
            })
        }

        let base_fingerprint = fingerprint(&base(), logical_partition_id);
        type Mutation = Box<dyn Fn(&mut Spec)>;
        let cases: Vec<Check<Mutation, bool>> = vec![
            Check {
                scenario: "same inputs rebuilt",
                input: Box::new(|_| {}),
                expect: true,
            },
            Check {
                scenario: "endpoint changed",
                input: Box::new(|spec| spec.endpoint = Some("http://nmxc-b.example:9370")),
                expect: false,
            },
            Check {
                scenario: "host added",
                input: Box::new(|spec| spec.host_bytes.push(43)),
                expect: false,
            },
            Check {
                scenario: "instance released",
                input: Box::new(|spec| spec.with_instance = false),
                expect: false,
            },
            Check {
                scenario: "nvlink config version bumped",
                input: Box::new(|spec| spec.config_version += 1),
                expect: false,
            },
            Check {
                scenario: "gpu moved to another logical partition",
                input: Box::new(move |spec| {
                    spec.gpu_logical_partition = Some(other_logical_partition_id)
                }),
                expect: false,
            },
            Check {
                scenario: "admin network toggled",
                input: Box::new(|spec| spec.admin_network = true),
                expect: false,
            },
            Check {
                scenario: "host marked for nvlink isolation",
                input: Box::new(|spec| spec.isolated = true),
                expect: false,
            },
            Check {
                scenario: "gpu guid changed",
                input: Box::new(|spec| spec.gpu_guid += 1),
                expect: false,
            },
            Check {
                scenario: "domain changed",
                input: Box::new(move |spec| spec.domain = domain_b),
                expect: false,
            },
            Check {
                scenario: "partition row left the domain",
                input: Box::new(|spec| spec.partition_in_domain = false),
                expect: false,
            },
            Check {
                scenario: "logical partition marked deleted",
                input: Box::new(|spec| spec.logical_partition_deleted = true),
                expect: false,
            },
            Check {
                scenario: "logical partition without a partition in the group marked deleted",
                input: Box::new(move |spec| {
                    spec.unrelated_deleted_logical_partition = Some(other_logical_partition_id)
                }),
                expect: true,
            },
        ];
        check_values(cases, |mutate| {
            let mut spec = base();
            mutate(&mut spec);
            fingerprint(&spec, logical_partition_id) == base_fingerprint
        });
    }

    /// The first partition list serves the check; a second one is fetched
    /// only to resolve the NMX-C ids of partitions an operation created.
    #[sqlx_test]
    async fn group_refreshes_the_partition_list_only_after_operations(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();

        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;
        let mut managed_snapshot = snapshot(&HostSpec {
            id_byte: 12,
            chassis_serial: Some("CHASSIS-A"),
            rack_id: None,
        });
        managed_snapshot
            .host_snapshot
            .status
            .nvlink_info
            .as_mut()
            .expect("test snapshot has nvlink info")
            .domain_uuid = domain_uuid;
        let machine_id = managed_snapshot.host_snapshot.id;
        let nvlink_info = managed_snapshot.host_snapshot.status.nvlink_info.clone();
        let all_snapshots = HashMap::from([(machine_id, managed_snapshot)]);
        let chassis_snapshots = vec![&all_snapshots[&machine_id]];

        struct Case {
            scenario: &'static str,
            nmxc: NmxcSimClient,
            expected_completed_operations: usize,
            expected_partition_list_calls: usize,
        }

        for Case {
            scenario,
            nmxc,
            expected_completed_operations,
            expected_partition_list_calls,
        } in [
            Case {
                scenario: "nothing to apply",
                nmxc: NmxcSimClient::default(),
                expected_completed_operations: 0,
                expected_partition_list_calls: 1,
            },
            Case {
                scenario: "default partition removed",
                nmxc: NmxcSimClient::with_default_partition(),
                expected_completed_operations: 1,
                expected_partition_list_calls: 2,
            },
        ] {
            let nmxc = Arc::new(nmxc);
            let monitor = NvlPartitionMonitor::new(
                pool.clone(),
                nmxc.clone(),
                test_meter.meter(),
                NvLinkConfig::default(),
                HostHealthConfig::default(),
                work_lock_manager.clone(),
                Instrumentation::new(test_meter.meter()),
            );

            let result = monitor
                .process_nmx_c_partition_monitor_group(ProcessMachineGroupInput {
                    group_id: "CHASSIS-A".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                    snapshots: &chassis_snapshots,
                    endpoint_url: Some("http://nmxc.example:9370"),
                    rack_id: None,
                    all_managed_host_snapshots: &all_snapshots,
                    machine_nvlink_info: HashMap::from([(machine_id, nvlink_info.clone())]),
                    db_nvl_partitions: &[],
                    db_nvl_logical_partitions: &[],
                })
                .await;

            assert!(result.null_observations.is_empty(), "{scenario}");
            assert_eq!(
                result.completed_operations, expected_completed_operations,
                "{scenario}"
            );
            assert_eq!(
                nmxc.partition_info_list_calls(),
                expected_partition_list_calls,
                "{scenario}"
            );
        }

        join_set.shutdown().await;
        Ok(())
    }

    fn nvlink_isolation_alert() -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert {
            id: "Maintenance".parse().unwrap(),
            target: Some("ComputeTrayDueForSwap".to_string()),
            in_alert_since: None,
            message: String::new(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::isolate_from_nvlink_partitions(),
            ],
        }
    }

    /// A host marked for NVLink isolation leaves every partition, including its
    /// tray default partition, and is not placed back into one. An unmarked
    /// idle host keeps, or gets, its tray default partition.
    #[sqlx_test]
    async fn marked_host_is_removed_from_every_nvlink_partition(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        const GPU_A: u64 = 0x1000;
        const GPU_B: u64 = 0x1001;
        const OTHER_TRAY_GPU: u64 = 0x2000;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();
        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;

        struct Case {
            scenario: &'static str,
            marked: bool,
            nmxc: NmxcSimClient,
            expected_partitions: Vec<(&'static str, Vec<u64>)>,
        }

        for Case {
            scenario,
            marked,
            nmxc,
            expected_partitions,
        } in [
            Case {
                scenario: "idle host keeps its tray partition",
                marked: false,
                nmxc: NmxcSimClient::with_partition(9, "tray_partition_1", vec![GPU_A, GPU_B]),
                expected_partitions: vec![("tray_partition_1", vec![GPU_A, GPU_B])],
            },
            Case {
                scenario: "idle host without a partition gets its tray partition",
                marked: false,
                nmxc: NmxcSimClient::default(),
                expected_partitions: vec![("tray_partition_1", vec![GPU_A, GPU_B])],
            },
            Case {
                scenario: "marked host's tray partition is deleted",
                marked: true,
                nmxc: NmxcSimClient::with_partition(9, "tray_partition_1", vec![GPU_A, GPU_B]),
                expected_partitions: vec![],
            },
            Case {
                scenario: "marked host leaves a partition other GPUs keep",
                marked: true,
                nmxc: NmxcSimClient::with_partition(
                    12345,
                    "unknown-partition",
                    vec![GPU_A, OTHER_TRAY_GPU, GPU_B],
                ),
                expected_partitions: vec![("unknown-partition", vec![OTHER_TRAY_GPU])],
            },
            Case {
                scenario: "marked host without a partition is not placed in one",
                marked: true,
                nmxc: NmxcSimClient::default(),
                expected_partitions: vec![],
            },
        ] {
            let mut managed_snapshot = snapshot(&HostSpec {
                id_byte: 13,
                chassis_serial: Some("CHASSIS-A"),
                rack_id: None,
            });
            managed_snapshot
                .host_snapshot
                .network_config
                .value
                .use_admin_network = Some(true);
            let info = managed_snapshot
                .host_snapshot
                .status
                .nvlink_info
                .as_mut()
                .expect("test snapshot has nvlink info");
            info.domain_uuid = domain_uuid;
            info.gpus = [GPU_A, GPU_B]
                .into_iter()
                .zip(1..)
                .map(|(guid, device_id)| NvLinkGpu {
                    tray_index: 1,
                    slot_id: 1,
                    device_id,
                    guid,
                })
                .collect();
            if marked {
                managed_snapshot
                    .aggregate_health
                    .alerts
                    .push(nvlink_isolation_alert());
            }
            let machine_id = managed_snapshot.host_snapshot.id;
            let nvlink_info = managed_snapshot.host_snapshot.status.nvlink_info.clone();
            let all_snapshots = HashMap::from([(machine_id, managed_snapshot)]);
            let chassis_snapshots = vec![&all_snapshots[&machine_id]];

            let nmxc = Arc::new(nmxc);
            let monitor = NvlPartitionMonitor::new(
                pool.clone(),
                nmxc.clone(),
                test_meter.meter(),
                NvLinkConfig::default(),
                HostHealthConfig::default(),
                work_lock_manager.clone(),
                Instrumentation::new(test_meter.meter()),
            );
            monitor
                .process_nmx_c_partition_monitor_group(ProcessMachineGroupInput {
                    group_id: "CHASSIS-A".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                    snapshots: &chassis_snapshots,
                    endpoint_url: Some("http://nmxc.example:9370"),
                    rack_id: None,
                    all_managed_host_snapshots: &all_snapshots,
                    machine_nvlink_info: HashMap::from([(machine_id, nvlink_info)]),
                    db_nvl_partitions: &[],
                    db_nvl_logical_partitions: &[],
                })
                .await;

            let expected_partitions: Vec<(String, Vec<u64>)> = expected_partitions
                .into_iter()
                .map(|(name, gpu_uids)| (name.to_string(), gpu_uids))
                .collect();
            assert_eq!(nmxc.partitions(), expected_partitions, "{scenario}");
        }

        join_set.shutdown().await;
        Ok(())
    }

    /// A marked host's isolation status is persisted from each pass's NMX-C membership: pending
    /// while its GPUs are partitioned, isolated on the pass after the removals complete, and
    /// removed once the host is no longer marked, which also returns its GPUs to their tray
    /// partition.
    #[sqlx_test]
    async fn monitor_pass_reports_nvlink_isolation_progress(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::ManagedHostState;

        const GPU_A: u64 = 0x1000;
        const GPU_B: u64 = 0x1001;
        const MARK_SOURCE: &str = "compute-tray-due-for-swap";
        let domain_uuid: NvLinkDomainId = "ffffffff-ffff-ffff-ffff-ffffffffffff".parse()?;
        let id = machine_id(42);

        let mut txn = pool.begin().await?;
        db::machine::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        db::machine::update_nvlink_info(
            txn.as_mut(),
            &id,
            MachineNvLinkInfo {
                domain_uuid,
                chassis_serial: "CHASSIS-A".to_string(),
                gpus: [GPU_A, GPU_B]
                    .into_iter()
                    .zip(1..)
                    .map(|(guid, device_id)| NvLinkGpu {
                        tray_index: 1,
                        slot_id: 1,
                        device_id,
                        guid,
                    })
                    .collect(),
            },
        )
        .await?;
        let mut mark = health_report::HealthReport::empty(MARK_SOURCE.to_string());
        mark.alerts.push(nvlink_isolation_alert());
        db::machine::insert_health_report(
            txn.as_mut(),
            &id,
            health_report::HealthReportApplyMode::Merge,
            &mark,
            false,
        )
        .await?;
        txn.commit().await?;

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let nmxc = Arc::new(NmxcSimClient::with_partition(
            9,
            "tray_partition_1",
            vec![GPU_A, GPU_B],
        ));
        let test_meter = TestMeter::default();
        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            nmxc.clone(),
            test_meter.meter(),
            NvLinkConfig::default(),
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );

        // Runs one group pass over the stored machine and returns its reloaded snapshot.
        let pass = || async {
            let snapshot = load_snapshot(&pool, &id).await?;
            let nvlink_info = snapshot.host_snapshot.status.nvlink_info.clone();
            let all_snapshots = HashMap::from([(id, snapshot)]);
            let chassis_snapshots = vec![&all_snapshots[&id]];
            monitor
                .process_nmx_c_partition_monitor_group(ProcessMachineGroupInput {
                    group_id: "CHASSIS-A".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                    snapshots: &chassis_snapshots,
                    endpoint_url: Some("http://nmxc.example:9370"),
                    rack_id: None,
                    all_managed_host_snapshots: &all_snapshots,
                    machine_nvlink_info: HashMap::from([(id, nvlink_info)]),
                    db_nvl_partitions: &[],
                    db_nvl_logical_partitions: &[],
                })
                .await;
            load_snapshot(&pool, &id).await
        };
        let status = |snapshot: &ManagedHostStateSnapshot| {
            snapshot
                .host_snapshot
                .health_reports
                .merges
                .get(NVLINK_ISOLATION_REPORT_SOURCE)
                .map(|report| {
                    (
                        report
                            .successes
                            .iter()
                            .map(|success| success.id.to_string())
                            .collect::<Vec<_>>(),
                        report
                            .alerts
                            .iter()
                            .map(|alert| format!("{}: {}", alert.id, alert.message))
                            .collect::<Vec<_>>(),
                    )
                })
        };

        // Pass 1: both GPUs are still partitioned when the pass starts; it removes them.
        let after_removal = pass().await?;
        assert_eq!(
            status(&after_removal),
            Some((
                vec![],
                vec![format!(
                    "NvlinkIsolationPending: 2 of 2 NVLink GPUs are still in a partition: {GPU_A}, {GPU_B}"
                )],
            )),
        );
        assert_eq!(nmxc.partitions(), vec![], "the pass removed the GPUs");

        // Pass 2: NMX-C lists neither GPU in a partition.
        let isolated = pass().await?;
        assert_eq!(
            status(&isolated),
            Some((vec!["NvlinkIsolated".to_string()], vec![])),
        );

        // Pass 3: once the mark is removed, the status is cleared and the GPUs return.
        let mut txn = pool.begin().await?;
        db::machine::remove_health_report(
            txn.as_mut(),
            &id,
            health_report::HealthReportApplyMode::Merge,
            MARK_SOURCE,
        )
        .await?;
        txn.commit().await?;
        let unmarked = pass().await?;
        assert_eq!(status(&unmarked), None);
        assert_eq!(
            nmxc.partitions(),
            vec![("tray_partition_1".to_string(), vec![GPU_A, GPU_B])],
        );

        join_set.shutdown().await;
        Ok(())
    }

    /// The overrun counter is driven from the iteration path itself: with a
    /// zero run interval every pass is an overrun.
    #[sqlx_test]
    async fn iteration_longer_than_the_run_interval_counts_an_overrun(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_iteration_overruns_total";

        let mut join_set = JoinSet::new();
        let work_lock_manager =
            db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
        let test_meter = TestMeter::default();
        let monitor = NvlPartitionMonitor::new(
            pool.clone(),
            Arc::new(NmxcSimClient::default()),
            test_meter.meter(),
            NvLinkConfig {
                monitor_run_interval: Duration::ZERO,
                ..NvLinkConfig::default()
            },
            HostHealthConfig::default(),
            work_lock_manager,
            Instrumentation::new(test_meter.meter()),
        );

        let metrics = MetricsCapture::for_registry(test_meter.registry());
        monitor.run_single_iteration().await?;
        assert_eq!(metrics.counter_delta(METRIC_NAME, &[]), 1.0);

        join_set.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_group_results_merge_into_iteration_metrics() {
        use futures::future;

        use super::{AppliedChange, NmxcMetricOperation, NmxcMetricOperationStatus, NmxcMetrics};

        let applied_create = AppliedChange {
            operation: NmxcMetricOperation::Create,
            status: NmxcMetricOperationStatus::Completed,
        };
        let domain_a = "domain-a".to_string();
        let domain_b = "domain-b".to_string();

        let mut metrics_a = NvlPartitionMonitorMetrics::new();
        metrics_a.num_machines_scanned = 2;
        metrics_a.num_instances_scanned = 1;
        metrics_a.applied_changes.insert(applied_create.clone(), 1);
        metrics_a
            .nmxc
            .partition_health
            .insert((domain_a.clone(), "healthy"), 3);
        metrics_a
            .nmxc
            .gpu_health
            .insert((domain_a.clone(), "healthy"), 4);
        metrics_a
            .nmxc
            .compute_node_health
            .insert((domain_a.clone(), "healthy"), 1);
        metrics_a.nmxc.endpoint = "http://nmxc-a.example:9370".to_string();

        let mut metrics_b = NvlPartitionMonitorMetrics::new();
        metrics_b.num_machines_scanned = 3;
        metrics_b.num_instances_scanned = 2;
        metrics_b.applied_changes.insert(applied_create.clone(), 2);
        metrics_b
            .nmxc
            .partition_health
            .insert((domain_b.clone(), "healthy"), 5);
        metrics_b
            .nmxc
            .gpu_health
            .insert((domain_b.clone(), "healthy"), 6);
        metrics_b
            .nmxc
            .compute_node_health
            .insert((domain_b.clone(), "healthy"), 2);
        metrics_b.nmxc.endpoint = "http://nmxc-b.example:9370".to_string();

        let machine_c = machine_id(23);
        let domain_d = "domain-d".to_string();
        let mut skipped_health = NmxcMetrics::default();
        skipped_health
            .partition_health
            .insert((domain_d.clone(), "healthy"), 7);
        skipped_health
            .gpu_health
            .insert((domain_d.clone(), "degraded"), 1);
        skipped_health
            .compute_node_health
            .insert((domain_d.clone(), "healthy"), 4);
        let prepared = vec![
            GroupResult {
                completed_operations: 2,
                null_observations: vec![],
                partial_metrics: metrics_a,
                check_ok: true,
                rack_domain_uuid: None,
                unreachable_reason: None,
                hello_domain_uuid: None,
            },
            GroupResult {
                completed_operations: 1,
                null_observations: vec![],
                partial_metrics: metrics_b,
                check_ok: true,
                rack_domain_uuid: None,
                unreachable_reason: None,
                hello_domain_uuid: None,
            },
            GroupResult {
                completed_operations: 0,
                null_observations: vec![super::PendingNullNvlinkObservation {
                    group_id: "CHASSIS-C".to_string(),
                    group_type: nmx_c_endpoint::ManagedHostGroupType::Chassis,
                    reason: ChassisNmxCUnreachableReason::NoEndpoint,
                    machine_ids: vec![machine_c],
                }],
                partial_metrics: NvlPartitionMonitorMetrics::new(),
                check_ok: false,
                rack_domain_uuid: None,
                unreachable_reason: Some(ChassisNmxCUnreachableReason::NoEndpoint),
                hello_domain_uuid: None,
            },
            // A group skipped by change detection carries its last health forward.
            GroupResult::skipped(skipped_health),
        ];
        let group_results =
            future::join_all(prepared.into_iter().map(|result| async move { result })).await;

        // Mirror the fan-in fold in run_single_iteration_inner.
        let mut metrics = NvlPartitionMonitorMetrics::new();
        metrics.num_logical_partitions = 3;
        metrics.num_physical_partitions = 1;
        let mut total_completed_operations = 0;
        let mut pending_null_observations = Vec::new();
        for result in group_results {
            total_completed_operations += result.completed_operations;
            pending_null_observations.extend(result.null_observations);
            metrics.merge_from(result.partial_metrics);
        }
        metrics.num_completed_operations = total_completed_operations;

        assert_eq!(total_completed_operations, 3);
        assert_eq!(metrics.num_completed_operations, 3);
        assert_eq!(metrics.num_machines_scanned, 5);
        assert_eq!(metrics.num_instances_scanned, 3);
        assert_eq!(metrics.applied_changes[&applied_create], 3);
        assert_eq!(
            metrics.nmxc.partition_health,
            HashMap::from([
                ((domain_a.clone(), "healthy"), 3),
                ((domain_b.clone(), "healthy"), 5),
                ((domain_d.clone(), "healthy"), 7),
            ])
        );
        assert_eq!(
            metrics.nmxc.gpu_health,
            HashMap::from([
                ((domain_a.clone(), "healthy"), 4),
                ((domain_b.clone(), "healthy"), 6),
                ((domain_d.clone(), "degraded"), 1),
            ])
        );
        assert_eq!(
            metrics.nmxc.compute_node_health,
            HashMap::from([
                ((domain_a, "healthy"), 1),
                ((domain_b, "healthy"), 2),
                ((domain_d, "healthy"), 4),
            ])
        );
        assert_eq!(metrics.nmxc.endpoint, "http://nmxc-b.example:9370");
        assert_eq!(metrics.num_logical_partitions, 3);
        assert_eq!(metrics.num_physical_partitions, 1);
        assert!(metrics.num_nmx_c_unreachable_chassis.is_empty());

        assert_eq!(pending_null_observations.len(), 1);
        assert_eq!(pending_null_observations[0].group_id, "CHASSIS-C");
        assert_eq!(
            pending_null_observations[0].reason,
            ChassisNmxCUnreachableReason::NoEndpoint
        );
        assert_eq!(pending_null_observations[0].machine_ids, vec![machine_c]);
    }
}
