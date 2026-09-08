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
use std::fmt;
use std::fmt::Display;
use std::time::Duration;

use ::carbide_utils::metrics::SharedMetricsHolder;
use carbide_instrument::{DynamicLog, DynamicMessage, Event, LabelValue, LogAt};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::NmxcPartitionOperationType;

/// Metrics that are gathered in a single nvl partition monitor run
#[derive(Clone, Debug)]
pub(super) struct NvlPartitionMonitorMetrics {
    /// Start time of metrics gathering
    pub(super) recording_started_at: std::time::Instant,
    pub(super) nmxc: NmxcMetrics,
    pub(super) num_machines_scanned: usize,
    pub(super) num_instances_scanned: usize,
    pub(super) num_gpus_scanned: usize,
    /// Number of machines where NVLink status observation got updated
    pub(super) num_machine_nvl_status_updates: usize,
    /// Number of logical partitions
    pub(super) num_logical_partitions: usize,
    /// Number of physical partitions
    pub(super) num_physical_partitions: usize,
    /// Number of completed operations in this run
    pub(super) num_completed_operations: usize,
    /// Number of NVLink GPU partition ID mismatches between DB and NMX-C
    pub(super) num_nvlink_info_mismatches: usize,
    /// Number of stale partitions deleted from DB (not found in NMX-C)
    pub(super) num_stale_partitions_deleted: usize,
    pub(super) applied_changes: HashMap<AppliedChange, usize>,
    /// Time from nvlink_config_version for instances currently in Pending (time spent in Pending), in milliseconds
    pub(super) nvlink_config_apply_durations_ms: Vec<f64>,
    /// Chassis- or rack-level NMX-C connectivity failures that caused null nvlink status observations.
    /// Counted per machine group; the OTEL gauge name remains `..._unreachable_chassis_count` for continuity.
    pub(super) num_nmx_c_unreachable_chassis: HashMap<ChassisNmxCUnreachableReason, usize>,
}

/// Why the partition monitor could not use NMX-C for a machine group during an iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ChassisNmxCUnreachableReason {
    /// No rack-switch NVOS IP or `nvlink_nmxc_endpoints` row resolved an endpoint URL.
    NoEndpoint,
    /// The resolved endpoint URL could not be parsed as a valid NMX-C client URI.
    InvalidEndpointUri,
    /// The NMX-C client pool failed to create a client for the resolved endpoint.
    ClientCreateFailed,
    /// NMX-C `hello` failed after the client was created.
    HelloFailed,
    /// NMX-C `hello` succeeded but the domain UUID in the response could not be parsed.
    DomainUuidParseFailed,
    /// Partition monitor work failed after NMX-C connectivity was established (for example, partition list fetch).
    PartitionMonitorWorkFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, LabelValue)]
pub(super) enum NmxcMetricOperation {
    Create,
    Remove,
    RemoveDefaultPartition,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, LabelValue)]
pub(super) enum NmxcMetricOperationStatus {
    Completed,
    Failed,
    Timedout,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct AppliedChange {
    /// The operation that has been issued
    pub(super) operation: NmxcMetricOperation,
    /// Whether the operation succeeded or failed
    pub(super) status: NmxcMetricOperationStatus,
}

/// The NMX-C call that failed inside one partition operation. This stays in
/// log context rather than becoming another latency label; its only other job
/// is selecting the diagnostic operators already see for that call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NmxcOperationFailureStage {
    None,
    CreatePartitionRetry,
    CreatePartition,
    DeletePartition,
    DeleteDefaultPartition,
    GetPartitionInfo,
    RemoveGpus,
    AddGpus,
}

pub(super) const DELETE_DEFAULT_PARTITION_FAILED_MESSAGE: &str =
    "Failed to delete default partition";

impl Display for NmxcOperationFailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "",
            Self::CreatePartitionRetry => "create_partition_retry",
            Self::CreatePartition => "create_partition",
            Self::DeletePartition => "delete_partition",
            Self::DeleteDefaultPartition => "delete_default_partition",
            Self::GetPartitionInfo => "get_partition_info",
            Self::RemoveGpus => "remove_gpus",
            Self::AddGpus => "add_gpus",
        })
    }
}

/// `NmxcOperationFinished` closes one NMX-C partition operation. The Event
/// owns the live latency sample; successful operations keep this Event
/// log-silent, while a terminal failure keeps the diagnostic from the exact
/// RPC stage that failed.
#[derive(Event)]
#[event(
    event_name = "nvlink_nmxc_operation_finished",
    metric_name = "carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds",
    component = "nvlink-manager",
    log = dynamic,
    metric = histogram,
    message = dynamic,
    describe = "Time consumed for one NMX-C operation"
)]
pub(super) struct NmxcOperationFinished {
    #[label]
    pub(super) operation: NmxcMetricOperation,
    #[label]
    pub(super) status: NmxcMetricOperationStatus,
    #[observation]
    pub(super) latency: Duration,
    #[context]
    pub(super) failure_stage: NmxcOperationFailureStage,
    #[context]
    pub(super) nvlink_logical_partition_id: String,
    #[context]
    pub(super) nmx_c_partition_id: String,
    #[context]
    pub(super) create_partition_request: String,
    #[context]
    pub(super) error: String,
}

impl DynamicLog for NmxcOperationFinished {
    fn log_at(&self) -> LogAt {
        match self.status {
            NmxcMetricOperationStatus::Completed => LogAt::Off,
            NmxcMetricOperationStatus::Failed | NmxcMetricOperationStatus::Timedout => {
                LogAt::Level(tracing::Level::WARN)
            }
        }
    }
}

impl DynamicMessage for NmxcOperationFinished {
    fn message(&self) -> &'static str {
        match self.failure_stage {
            NmxcOperationFailureStage::None => "NMX-C operation failed",
            NmxcOperationFailureStage::CreatePartitionRetry => {
                "Failed to retry create partition on NMX-C with multicast_groups_limit=0"
            }
            NmxcOperationFailureStage::CreatePartition => {
                "Failed to issue create partition to NMX-C, continuing with other operations"
            }
            NmxcOperationFailureStage::DeletePartition => {
                "Failed to issue delete partition to NMX-C, continuing with other operations"
            }
            NmxcOperationFailureStage::DeleteDefaultPartition => {
                DELETE_DEFAULT_PARTITION_FAILED_MESSAGE
            }
            NmxcOperationFailureStage::GetPartitionInfo => {
                "Failed to get partition info from NMX-C before update"
            }
            NmxcOperationFailureStage::RemoveGpus => {
                "Failed to remove GPUs from partition on NMX-C"
            }
            NmxcOperationFailureStage::AddGpus => "Failed to add GPUs to partition on NMX-C",
        }
    }
}

/// Metrics collected for NMX-C data
#[derive(Clone, Debug, Default)]
pub(super) struct NmxcMetrics {
    /// The endpoint that we use to interact with NMX-C
    pub(super) endpoint: String,
    /// connection errors
    pub(super) connect_error: String,
    /// Version of NMX-C
    pub(super) version: String,
    /// Partition count per (nvlink_domain_uuid, health).
    pub(super) partition_health: HashMap<(String, &'static str), usize>,
    /// GPU count per (nvlink_domain_uuid, health).
    pub(super) gpu_health: HashMap<(String, &'static str), usize>,
    /// Compute-node count per (nvlink_domain_uuid, health).
    pub(super) compute_node_health: HashMap<(String, &'static str), usize>,
}

impl NvlPartitionMonitorMetrics {
    pub(super) fn new() -> Self {
        Self {
            recording_started_at: std::time::Instant::now(),
            num_machines_scanned: 0,
            num_instances_scanned: 0,
            num_machine_nvl_status_updates: 0,
            num_logical_partitions: 0,
            num_physical_partitions: 0,
            num_gpus_scanned: 0,
            num_completed_operations: 0,
            num_nvlink_info_mismatches: 0,
            num_stale_partitions_deleted: 0,
            applied_changes: HashMap::new(),
            nvlink_config_apply_durations_ms: Vec::new(),
            num_nmx_c_unreachable_chassis: HashMap::new(),
            nmxc: NmxcMetrics {
                endpoint: String::new(),
                connect_error: String::new(),
                version: String::new(),
                partition_health: HashMap::new(),
                gpu_health: HashMap::new(),
                compute_node_health: HashMap::new(),
            },
        }
    }

    /// Accumulates per-group metrics collected during concurrent group processing into `self`.
    ///
    /// Fields that are counters or collections are summed/extended. Single-valued NMX-C metadata
    /// (endpoint, version, connect_error) use last-non-empty-wins, matching the previous
    /// sequential behaviour where the last processed group determined those values. Health maps
    /// are keyed by `(domain_uuid, state)` so entries from different groups have distinct keys
    /// and can be merged with `extend`.
    ///
    /// Fields managed by the caller (`recording_started_at`, `num_logical_partitions`,
    /// `num_physical_partitions`, `num_completed_operations`, `num_nmx_c_unreachable_chassis`)
    /// are not touched here.
    ///
    /// Exhaustive destructuring is intentional: adding a field to
    /// [`NvlPartitionMonitorMetrics`] or [`NmxcMetrics`] must force a merge decision here.
    pub(crate) fn merge_from(&mut self, other: Self) {
        let Self {
            nmxc:
                NmxcMetrics {
                    endpoint,
                    connect_error,
                    version,
                    partition_health,
                    gpu_health,
                    compute_node_health,
                },
            num_machines_scanned,
            num_instances_scanned,
            num_gpus_scanned,
            num_machine_nvl_status_updates,
            num_nvlink_info_mismatches,
            num_stale_partitions_deleted,
            applied_changes,
            nvlink_config_apply_durations_ms,
            // Caller-owned — intentionally not merged.
            recording_started_at: _,
            num_logical_partitions: _,
            num_physical_partitions: _,
            num_completed_operations: _,
            num_nmx_c_unreachable_chassis: _,
        } = other;

        if !endpoint.is_empty() {
            self.nmxc.endpoint = endpoint;
        }
        if !version.is_empty() {
            self.nmxc.version = version;
        }
        if !connect_error.is_empty() {
            self.nmxc.connect_error = connect_error;
        }
        self.nmxc.partition_health.extend(partition_health);
        self.nmxc.gpu_health.extend(gpu_health);
        self.nmxc.compute_node_health.extend(compute_node_health);
        self.num_machines_scanned += num_machines_scanned;
        self.num_instances_scanned += num_instances_scanned;
        self.num_gpus_scanned += num_gpus_scanned;
        self.num_machine_nvl_status_updates += num_machine_nvl_status_updates;
        self.num_nvlink_info_mismatches += num_nvlink_info_mismatches;
        self.num_stale_partitions_deleted += num_stale_partitions_deleted;
        for (k, v) in applied_changes {
            *self.applied_changes.entry(k).or_default() += v;
        }
        self.nvlink_config_apply_durations_ms
            .extend(nvlink_config_apply_durations_ms);
    }
}

impl Display for NvlPartitionMonitorMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{ machines_scanned: {}, instances_scanned: {}, nvl_status_updates: {}, num_logical_partitions: {}, num_physical_partitions:{}, num_gpus_scanned: {}, nvlink_info_mismatches: {}, stale_partitions_deleted: {}, nmx_c_unreachable_chassis: {:?}, applied_changes: {}, nmxc_connect_err: {}, nmxc_num_partitions: {}, nmxc_num_gpus: {}, completed_operations: {}, duration: {} }}",
            self.num_machines_scanned,
            self.num_instances_scanned,
            self.num_machine_nvl_status_updates,
            self.num_logical_partitions,
            self.num_physical_partitions,
            self.num_gpus_scanned,
            self.num_nvlink_info_mismatches,
            self.num_stale_partitions_deleted,
            self.num_nmx_c_unreachable_chassis,
            self.applied_changes.len(),
            self.nmxc.connect_error,
            self.nmxc.partition_health.values().sum::<usize>(),
            self.nmxc.gpu_health.values().sum::<usize>(),
            self.num_completed_operations,
            self.recording_started_at.elapsed().as_millis(),
        )
    }
}

/// One NVLink partition monitor pass. Both cases sample the duration; only a
/// failure logs.
#[derive(Event)]
#[event(
    event_name = "nvlink_partition_monitor_iteration_finished",
    metric_name = "carbide_nvlink_partition_monitor_iteration_latency_milliseconds",
    component = "nvlink-manager",
    metric = histogram,
    describe = "Time consumed for one monitor iteration"
)]
pub(super) enum NvlPartitionMonitorIterationFinished {
    /// A clean pass: sampled, never logged.
    #[event(log = off)]
    Succeeded {
        #[observation]
        latency: Duration,
    },

    #[event(log = warn, message = "NVLink partition monitor error")]
    Failed {
        #[observation]
        latency: Duration,
        #[context]
        error: String,
    },
}

/// Instruments that are used by pub struct NvlPartitionMonitor
struct NvlPartitionMonitorInstruments {
    nmxc_changes_applied: Counter<u64>,
    nvlink_config_apply_latency: Histogram<f64>,
}

impl NvlPartitionMonitorInstruments {
    fn new(meter: Meter, shared_metrics: SharedMetricsHolder<NvlPartitionMonitorMetrics>) -> Self {
        let nvlink_config_apply_latency = meter
            .f64_histogram("carbide_nvlink_partition_monitor_nvlink_config_apply_latency")
            .with_description("Time since nvlink config was requested for this instance")
            .with_unit("ms")
            .build();

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_nvlink_partition_monitor_machine_status_updates_count",
                )
                .with_description("Number of machines whose NVLink status observation was updated")
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        o.observe(metrics.num_machine_nvl_status_updates as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_num_logical_partitions")
                .with_description("Number of monitored logical partitions")
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        o.observe(metrics.num_logical_partitions as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_num_physical_partitions")
                .with_description("Number of monitored physical partitions")
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        o.observe(metrics.num_physical_partitions as u64, attrs);
                    })
                })
                .build();
        }

        let nmxc_changes_applied = meter
            .u64_counter("carbide_nvlink_partition_monitor_nmxc_changes_applied")
            .with_description("Number of changes requested to NMX-C")
            .build();

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_nmxc_connect_error_count")
                .with_description("The errors encountered while checking NMX-C")
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        if !metrics.nmxc.connect_error.is_empty() {
                            o.observe(
                                1,
                                &[
                                    attrs,
                                    &[KeyValue::new(
                                        "error",
                                        truncate_error_for_metric_label(
                                            metrics.nmxc.connect_error.clone(),
                                        ),
                                    )],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_nmxc_partition_count")
                .with_description(
                    "Number of partitions NMX-C is reporting, by NVLink domain and health state",
                )
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        for ((domain, health), &count) in &metrics.nmxc.partition_health {
                            o.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("nvlink_domain_uuid", domain.clone()),
                                        KeyValue::new("health", *health),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_nmxc_gpu_count")
                .with_description(
                    "Number of GPUs NMX-C is reporting, by NVLink domain and health state",
                )
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        for ((domain, health), &count) in &metrics.nmxc.gpu_health {
                            o.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("nvlink_domain_uuid", domain.clone()),
                                        KeyValue::new("health", *health),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_nmxc_compute_node_count")
                .with_description(
                    "Number of compute nodes NMX-C reports, by NVLink domain and health state",
                )
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        for ((domain, health), &count) in &metrics.nmxc.compute_node_health {
                            o.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("nvlink_domain_uuid", domain.clone()),
                                        KeyValue::new("health", *health),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_nvlink_info_mismatches")
                .with_description(
                    "Number of NVLink GPU partition ID mismatches between DB and NMX-C",
                )
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        o.observe(metrics.num_nvlink_info_mismatches as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_nvlink_partition_monitor_nmx_c_unreachable_chassis_count",
                )
                .with_description(
                    "Number of machine groups (chassis or rack) where NMX-C was unreachable during partition monitor iteration",
                )
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        for (reason, &count) in &metrics.num_nmx_c_unreachable_chassis {
                            o.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("reason", *reason)]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics;
            meter
                .u64_observable_gauge("carbide_nvlink_partition_monitor_stale_partitions_deleted")
                .with_description("Number of stale partitions deleted from DB (not found in NMX-C)")
                .with_callback(move |o| {
                    metrics.if_available(|metrics, attrs| {
                        o.observe(metrics.num_stale_partitions_deleted as u64, attrs);
                    })
                })
                .build();
        }

        Self {
            nmxc_changes_applied,
            nvlink_config_apply_latency,
        }
    }

    fn emit_counters_and_histograms(&self, metrics: &NvlPartitionMonitorMetrics) {
        for (change, &count) in metrics.applied_changes.iter() {
            self.nmxc_changes_applied.add(
                count as u64,
                &[
                    KeyValue::new("operation", change.operation),
                    KeyValue::new("status", change.status),
                ],
            );
        }

        for &duration_ms in &metrics.nvlink_config_apply_durations_ms {
            self.nvlink_config_apply_latency.record(duration_ms, &[]);
        }
    }

    fn init_counters_and_histograms(&self) {
        for status in NmxcMetricOperationStatus::values() {
            for operation in NmxcMetricOperation::values() {
                self.nmxc_changes_applied.add(
                    0u64,
                    &[
                        KeyValue::new("operation", operation),
                        KeyValue::new("status", status),
                    ],
                );
            }
        }
    }
}

impl NmxcMetricOperation {
    fn values() -> impl Iterator<Item = Self> {
        [
            Self::Create,
            Self::Update,
            Self::Remove,
            Self::RemoveDefaultPartition,
        ]
        .into_iter()
    }
}

impl From<ChassisNmxCUnreachableReason> for opentelemetry::Value {
    fn from(value: ChassisNmxCUnreachableReason) -> Self {
        let str_value = match value {
            ChassisNmxCUnreachableReason::NoEndpoint => "no_endpoint",
            ChassisNmxCUnreachableReason::InvalidEndpointUri => "invalid_endpoint_uri",
            ChassisNmxCUnreachableReason::ClientCreateFailed => "client_create_failed",
            ChassisNmxCUnreachableReason::HelloFailed => "hello_failed",
            ChassisNmxCUnreachableReason::DomainUuidParseFailed => "domain_uuid_parse_failed",
            ChassisNmxCUnreachableReason::PartitionMonitorWorkFailed => {
                "partition_monitor_work_failed"
            }
        };

        Self::from(str_value)
    }
}

impl From<NmxcMetricOperation> for opentelemetry::Value {
    fn from(value: NmxcMetricOperation) -> Self {
        Self::from(value.label_value())
    }
}

impl From<NmxcPartitionOperationType> for NmxcMetricOperation {
    fn from(value: NmxcPartitionOperationType) -> NmxcMetricOperation {
        match value {
            NmxcPartitionOperationType::Create => NmxcMetricOperation::Create,
            NmxcPartitionOperationType::Remove(_) => NmxcMetricOperation::Remove,
            NmxcPartitionOperationType::RemoveUnknownPartition(_) => {
                NmxcMetricOperation::RemoveDefaultPartition
            }
            NmxcPartitionOperationType::Update(_) => NmxcMetricOperation::Update,
        }
    }
}

impl NmxcMetricOperationStatus {
    fn values() -> impl Iterator<Item = Self> {
        [Self::Completed, Self::Failed, Self::Timedout].into_iter()
    }
}

impl From<NmxcMetricOperationStatus> for opentelemetry::Value {
    fn from(value: NmxcMetricOperationStatus) -> Self {
        Self::from(value.label_value())
    }
}

/// Stores Metric data shared between the nvl partition monitor and the OpenTelemetry background task
pub(super) struct MetricHolder {
    instruments: NvlPartitionMonitorInstruments,
    last_iteration_metrics: SharedMetricsHolder<NvlPartitionMonitorMetrics>,
}

impl MetricHolder {
    pub(super) fn new(meter: Meter, hold_period: Duration) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        let instruments =
            NvlPartitionMonitorInstruments::new(meter, last_iteration_metrics.clone());
        instruments.init_counters_and_histograms();
        Self {
            instruments,
            last_iteration_metrics,
        }
    }

    /// Updates the most recent metrics
    pub(super) fn update_metrics(&self, metrics: NvlPartitionMonitorMetrics) {
        self.instruments.emit_counters_and_histograms(&metrics);
        self.last_iteration_metrics.update(metrics);
    }
}

/// Truncates an error message in order to use it as label
/// Borrowed this from IbFabricMonitor code
fn truncate_error_for_metric_label(mut error: String) -> String {
    const MAX_LEN: usize = 32;

    let upto = error
        .char_indices()
        .map(|(i, _)| i)
        .nth(MAX_LEN)
        .unwrap_or(error.len());
    error.truncate(upto);
    error
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn merge_from_sums_group_fields_and_preserves_caller_owned() {
        let applied_create = AppliedChange {
            operation: NmxcMetricOperation::Create,
            status: NmxcMetricOperationStatus::Completed,
        };
        let applied_remove = AppliedChange {
            operation: NmxcMetricOperation::Remove,
            status: NmxcMetricOperationStatus::Failed,
        };

        let mut base = NvlPartitionMonitorMetrics::new();
        base.recording_started_at = std::time::Instant::now();
        let started_at = base.recording_started_at;
        base.num_logical_partitions = 4;
        base.num_physical_partitions = 2;
        base.num_completed_operations = 7;
        base.num_nmx_c_unreachable_chassis
            .insert(ChassisNmxCUnreachableReason::NoEndpoint, 1);
        base.num_machines_scanned = 1;
        base.num_instances_scanned = 2;
        base.num_gpus_scanned = 3;
        base.num_machine_nvl_status_updates = 1;
        base.num_nvlink_info_mismatches = 1;
        base.num_stale_partitions_deleted = 1;
        base.applied_changes.insert(applied_create.clone(), 2);
        base.nvlink_config_apply_durations_ms.push(10.0);
        base.nmxc.endpoint = "https://first.example:9370".to_string();
        base.nmxc.version = "first=1".to_string();
        base.nmxc.connect_error = "first-error".to_string();
        base.nmxc
            .partition_health
            .insert(("domain-a".to_string(), "healthy"), 1);
        base.nmxc
            .gpu_health
            .insert(("domain-a".to_string(), "healthy"), 2);
        base.nmxc
            .compute_node_health
            .insert(("domain-a".to_string(), "healthy"), 3);

        let mut other = NvlPartitionMonitorMetrics::new();
        other.num_logical_partitions = 99;
        other.num_physical_partitions = 99;
        other.num_completed_operations = 99;
        other
            .num_nmx_c_unreachable_chassis
            .insert(ChassisNmxCUnreachableReason::HelloFailed, 5);
        other.num_machines_scanned = 10;
        other.num_instances_scanned = 20;
        other.num_gpus_scanned = 30;
        other.num_machine_nvl_status_updates = 4;
        other.num_nvlink_info_mismatches = 5;
        other.num_stale_partitions_deleted = 6;
        other.applied_changes.insert(applied_create.clone(), 3);
        other.applied_changes.insert(applied_remove.clone(), 1);
        other.nvlink_config_apply_durations_ms.push(20.0);
        other.nmxc.endpoint = "https://second.example:9370".to_string();
        other.nmxc.version = "second=2".to_string();
        other.nmxc.connect_error = "second-error".to_string();
        other
            .nmxc
            .partition_health
            .insert(("domain-b".to_string(), "healthy"), 4);
        other
            .nmxc
            .gpu_health
            .insert(("domain-b".to_string(), "healthy"), 5);
        other
            .nmxc
            .compute_node_health
            .insert(("domain-b".to_string(), "healthy"), 6);

        base.merge_from(other);

        assert_eq!(base.num_machines_scanned, 11);
        assert_eq!(base.num_instances_scanned, 22);
        assert_eq!(base.num_gpus_scanned, 33);
        assert_eq!(base.num_machine_nvl_status_updates, 5);
        assert_eq!(base.num_nvlink_info_mismatches, 6);
        assert_eq!(base.num_stale_partitions_deleted, 7);
        assert_eq!(base.applied_changes[&applied_create], 5);
        assert_eq!(base.applied_changes[&applied_remove], 1);
        assert_eq!(base.nvlink_config_apply_durations_ms, vec![10.0, 20.0]);
        assert_eq!(base.nmxc.endpoint, "https://second.example:9370");
        assert_eq!(base.nmxc.version, "second=2");
        assert_eq!(base.nmxc.connect_error, "second-error");
        assert_eq!(
            base.nmxc.partition_health,
            HashMap::from([
                (("domain-a".to_string(), "healthy"), 1),
                (("domain-b".to_string(), "healthy"), 4),
            ])
        );
        assert_eq!(
            base.nmxc.gpu_health,
            HashMap::from([
                (("domain-a".to_string(), "healthy"), 2),
                (("domain-b".to_string(), "healthy"), 5),
            ])
        );
        assert_eq!(
            base.nmxc.compute_node_health,
            HashMap::from([
                (("domain-a".to_string(), "healthy"), 3),
                (("domain-b".to_string(), "healthy"), 6),
            ])
        );

        // Caller-owned fields must not change during merge.
        assert_eq!(base.recording_started_at, started_at);
        assert_eq!(base.num_logical_partitions, 4);
        assert_eq!(base.num_physical_partitions, 2);
        assert_eq!(base.num_completed_operations, 7);
        assert_eq!(
            base.num_nmx_c_unreachable_chassis,
            HashMap::from([(ChassisNmxCUnreachableReason::NoEndpoint, 1)])
        );

        // Empty metadata from `other` must not overwrite existing values.
        let mut keep = NvlPartitionMonitorMetrics::new();
        keep.nmxc.endpoint = "https://keep.example:9370".to_string();
        keep.nmxc.version = "keep=1".to_string();
        keep.nmxc.connect_error = "keep-error".to_string();
        keep.merge_from(NvlPartitionMonitorMetrics::new());
        assert_eq!(keep.nmxc.endpoint, "https://keep.example:9370");
        assert_eq!(keep.nmxc.version, "keep=1");
        assert_eq!(keep.nmxc.connect_error, "keep-error");
    }

    #[test]
    fn partition_monitor_iteration_records_latency_and_warns_only_on_failure() {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_iteration_latency_milliseconds";

        struct IterationCase {
            latency: Duration,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        check_values(
            [
                Check {
                    scenario: "successful iteration",
                    input: IterationCase {
                        latency: Duration::from_millis(125),
                        error: "",
                    },
                    expect: Observation {
                        log_count: 0,
                        log: None,
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(125.0),
                    },
                },
                Check {
                    scenario: "failed iteration",
                    input: IterationCase {
                        latency: Duration::from_millis(375),
                        error: "database unavailable",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            level: tracing::Level::WARN,
                            metadata_name: "nvlink_partition_monitor_iteration_finished"
                                .to_string(),
                            message: "NVLink partition monitor error".to_string(),
                            event_name: Some(
                                "nvlink_partition_monitor_iteration_finished".to_string(),
                            ),
                            metric_name: Some(METRIC_NAME.to_string()),
                            error: Some("database unavailable".to_string()),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(375.0),
                    },
                },
            ],
            |IterationCase { latency, error }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(if error.is_empty() {
                        NvlPartitionMonitorIterationFinished::Succeeded { latency }
                    } else {
                        NvlPartitionMonitorIterationFinished::Failed {
                            latency,
                            error: error.to_string(),
                        }
                    });
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics.histogram_count_delta(METRIC_NAME, &[]),
                    histogram_sum_delta: metrics.histogram_sum_delta(METRIC_NAME, &[]),
                }
            },
        );
    }

    #[test]
    fn nmxc_operation_records_latency_and_keeps_each_failure_message() {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds";

        struct OperationCase {
            scenario: &'static str,
            operation: NmxcMetricOperation,
            operation_label: &'static str,
            status: NmxcMetricOperationStatus,
            status_label: &'static str,
            failure_stage: NmxcOperationFailureStage,
            failure_stage_label: &'static str,
            nmx_c_partition_id: &'static str,
            create_partition_request: &'static str,
            message: Option<&'static str>,
        }

        #[derive(Debug, PartialEq)]
        struct OperationLog {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            operation: Option<String>,
            status: Option<String>,
            failure_stage: Option<String>,
            nvlink_logical_partition_id: Option<String>,
            nmx_c_partition_id: Option<String>,
            create_partition_request: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct OperationObservation {
            log: Option<OperationLog>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        fn expected_log(case: &OperationCase) -> Option<OperationLog> {
            case.message.map(|message| OperationLog {
                level: tracing::Level::WARN,
                metadata_name: "nvlink_nmxc_operation_finished".to_string(),
                message: message.to_string(),
                event_name: Some("nvlink_nmxc_operation_finished".to_string()),
                metric_name: Some(METRIC_NAME.to_string()),
                operation: Some(case.operation_label.to_string()),
                status: Some(case.status_label.to_string()),
                failure_stage: Some(case.failure_stage_label.to_string()),
                nvlink_logical_partition_id: Some("logical-partition-1".to_string()),
                nmx_c_partition_id: Some(case.nmx_c_partition_id.to_string()),
                create_partition_request: Some(case.create_partition_request.to_string()),
                error: Some("rpc failed".to_string()),
            })
        }

        let cases = [
            OperationCase {
                scenario: "completed create",
                operation: NmxcMetricOperation::Create,
                operation_label: "create",
                status: NmxcMetricOperationStatus::Completed,
                status_label: "completed",
                failure_stage: NmxcOperationFailureStage::None,
                failure_stage_label: "",
                nmx_c_partition_id: "",
                create_partition_request: "",
                message: None,
            },
            OperationCase {
                scenario: "create retry failed",
                operation: NmxcMetricOperation::Create,
                operation_label: "create",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::CreatePartitionRetry,
                failure_stage_label: "create_partition_retry",
                nmx_c_partition_id: "",
                create_partition_request: "retry request",
                message: Some(
                    "Failed to retry create partition on NMX-C with multicast_groups_limit=0",
                ),
            },
            OperationCase {
                scenario: "create failed",
                operation: NmxcMetricOperation::Create,
                operation_label: "create",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::CreatePartition,
                failure_stage_label: "create_partition",
                nmx_c_partition_id: "",
                create_partition_request: "create request",
                message: Some(
                    "Failed to issue create partition to NMX-C, continuing with other operations",
                ),
            },
            OperationCase {
                scenario: "delete failed",
                operation: NmxcMetricOperation::Remove,
                operation_label: "remove",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::DeletePartition,
                failure_stage_label: "delete_partition",
                nmx_c_partition_id: "42",
                create_partition_request: "",
                message: Some(
                    "Failed to issue delete partition to NMX-C, continuing with other operations",
                ),
            },
            OperationCase {
                scenario: "default partition delete failed",
                operation: NmxcMetricOperation::RemoveDefaultPartition,
                operation_label: "remove_default_partition",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::DeleteDefaultPartition,
                failure_stage_label: "delete_default_partition",
                nmx_c_partition_id: "42",
                create_partition_request: "",
                message: Some("Failed to delete default partition"),
            },
            OperationCase {
                scenario: "partition lookup failed",
                operation: NmxcMetricOperation::Update,
                operation_label: "update",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::GetPartitionInfo,
                failure_stage_label: "get_partition_info",
                nmx_c_partition_id: "42",
                create_partition_request: "",
                message: Some("Failed to get partition info from NMX-C before update"),
            },
            OperationCase {
                scenario: "GPU removal failed",
                operation: NmxcMetricOperation::Update,
                operation_label: "update",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::RemoveGpus,
                failure_stage_label: "remove_gpus",
                nmx_c_partition_id: "42",
                create_partition_request: "",
                message: Some("Failed to remove GPUs from partition on NMX-C"),
            },
            OperationCase {
                scenario: "GPU addition failed",
                operation: NmxcMetricOperation::Update,
                operation_label: "update",
                status: NmxcMetricOperationStatus::Failed,
                status_label: "failed",
                failure_stage: NmxcOperationFailureStage::AddGpus,
                failure_stage_label: "add_gpus",
                nmx_c_partition_id: "42",
                create_partition_request: "",
                message: Some("Failed to add GPUs to partition on NMX-C"),
            },
        ];

        let checks = cases.into_iter().map(|case| {
            let expect = OperationObservation {
                log: expected_log(&case),
                histogram_count_delta: 1,
                histogram_sum_delta: ApproxHistogramSum(1.25),
            };
            Check {
                scenario: case.scenario,
                input: case,
                expect,
            }
        });

        check_values(checks, |case| {
            let metrics = MetricsCapture::start();
            let logs = capture_logs(|| {
                emit(NmxcOperationFinished {
                    operation: case.operation,
                    status: case.status,
                    latency: Duration::from_micros(1_250),
                    failure_stage: case.failure_stage,
                    nvlink_logical_partition_id: "logical-partition-1".to_string(),
                    nmx_c_partition_id: case.nmx_c_partition_id.to_string(),
                    create_partition_request: case.create_partition_request.to_string(),
                    error: "rpc failed".to_string(),
                });
            });
            let log = logs.first().map(|log| OperationLog {
                level: log.level,
                metadata_name: log.metadata_name.clone(),
                message: log.message.clone(),
                event_name: log.field("event_name").map(str::to_string),
                metric_name: log.field("metric_name").map(str::to_string),
                operation: log.field("operation").map(str::to_string),
                status: log.field("status").map(str::to_string),
                failure_stage: log.field("failure_stage").map(str::to_string),
                nvlink_logical_partition_id: log
                    .field("nvlink_logical_partition_id")
                    .map(str::to_string),
                nmx_c_partition_id: log.field("nmx_c_partition_id").map(str::to_string),
                create_partition_request: log.field("create_partition_request").map(str::to_string),
                error: log.field("error").map(str::to_string),
            });

            OperationObservation {
                log,
                histogram_count_delta: metrics.histogram_count_delta(
                    METRIC_NAME,
                    &[
                        ("operation", case.operation_label),
                        ("status", case.status_label),
                    ],
                ),
                histogram_sum_delta: metrics.histogram_sum_delta(
                    METRIC_NAME,
                    &[
                        ("operation", case.operation_label),
                        ("status", case.status_label),
                    ],
                ),
            }
        });
    }

    #[test]
    fn nmxc_operation_histogram_exposition_stays_stable() {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_nmxc_op_latency_milliseconds";

        let metrics = MetricsCapture::start();
        emit(NmxcOperationFinished {
            operation: NmxcMetricOperation::Update,
            status: NmxcMetricOperationStatus::Completed,
            latency: Duration::from_millis(125),
            failure_stage: NmxcOperationFailureStage::None,
            nvlink_logical_partition_id: String::new(),
            nmx_c_partition_id: String::new(),
            create_partition_request: String::new(),
            error: String::new(),
        });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {METRIC_NAME} Time consumed for one NMX-C operation\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {METRIC_NAME} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            encoded.contains("operation=\"update\",status=\"completed\""),
            "expected the historical operation/status labels:\n{encoded}"
        );
        assert!(
            !encoded.contains("_milliseconds_milliseconds"),
            "the unit suffix must be applied exactly once:\n{encoded}"
        );
    }

    #[test]
    fn partition_monitor_iteration_histogram_exposition_stays_stable() {
        const METRIC_NAME: &str = "carbide_nvlink_partition_monitor_iteration_latency_milliseconds";

        let metrics = MetricsCapture::start();
        emit(NvlPartitionMonitorIterationFinished::Succeeded {
            latency: Duration::from_millis(125),
        });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {METRIC_NAME} Time consumed for one monitor iteration\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {METRIC_NAME} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            !encoded.contains(
                "carbide_nvlink_partition_monitor_iteration_latency_milliseconds_milliseconds"
            ),
            "the unit suffix must be applied exactly once:\n{encoded}"
        );
        for suffix in ["count", "sum"] {
            let prefix = format!("{METRIC_NAME}_{suffix} ");
            let sample = encoded
                .lines()
                .find(|line| line.starts_with(&prefix))
                .unwrap_or_else(|| panic!("missing {prefix} sample:\n{encoded}"));
            assert!(
                !sample.contains('{'),
                "iteration latency must remain label-free: {sample}"
            );
        }
    }
}
