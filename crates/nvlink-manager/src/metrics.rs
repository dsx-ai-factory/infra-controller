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
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::NmxcPartitionOperationType;

/// Metrics that are gathered in a single nvl partition monitor run
#[derive(Clone, Debug)]
pub struct NvlPartitionMonitorMetrics {
    /// Start time of metrics gathering
    pub recording_started_at: std::time::Instant,
    pub nmxc: NmxcMetrics,
    pub num_machines_scanned: usize,
    pub num_instances_scanned: usize,
    pub num_gpus_scanned: usize,
    /// Number of machines where NVLink status observation got updated
    pub num_machine_nvl_status_updates: usize,
    /// Number of logical partitions
    pub num_logical_partitions: usize,
    /// Number of physical partitions
    pub num_physical_partitions: usize,
    /// Number of completed operations in this run
    pub num_completed_operations: usize,
    /// Number of NVLink GPU partition ID mismatches between DB and NMX-C
    pub num_nvlink_info_mismatches: usize,
    /// Number of stale partitions deleted from DB (not found in NMX-C)
    pub num_stale_partitions_deleted: usize,
    pub applied_changes: HashMap<AppliedChange, usize>,
    pub operation_latencies: HashMap<AppliedChange, Vec<Duration>>,
    /// Time from nvlink_config_version for instances currently in Pending (time spent in Pending), in milliseconds
    pub nvlink_config_apply_durations_ms: Vec<f64>,
    /// Chassis-level NMX-C connectivity failures that caused null nvlink status observations
    pub num_nmx_c_unreachable_chassis: HashMap<ChassisNmxCUnreachableReason, usize>,
}

/// Why the partition monitor could not use NMX-C for a chassis during an iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChassisNmxCUnreachableReason {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NmxcMetricOperation {
    Create,
    Remove,
    RemoveDefaultPartition,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NmxcMetricOperationStatus {
    Completed,
    Failed,
    Timedout,
    Cancelled,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct AppliedChange {
    /// The operation that has been issued
    pub operation: NmxcMetricOperation,
    /// Whether the operation succeeded or failed
    pub status: NmxcMetricOperationStatus,
}

/// Metrics collected for NMX-C data
#[derive(Clone, Debug, Default)]
pub struct NmxcMetrics {
    /// The endpoint that we use to interact with NMX-C
    pub endpoint: String,
    /// connection errors
    pub connect_error: String,
    /// Version of NMX-C
    pub version: String,
    /// Partition count per (nvlink_domain_uuid, health).
    pub partition_health: HashMap<(String, &'static str), usize>,
    /// GPU count per (nvlink_domain_uuid, health).
    pub gpu_health: HashMap<(String, &'static str), usize>,
    /// Compute-node count per (nvlink_domain_uuid, health).
    pub compute_node_health: HashMap<(String, &'static str), usize>,
}

impl NvlPartitionMonitorMetrics {
    pub fn new() -> Self {
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
            operation_latencies: HashMap::new(),
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

/// Instruments that are used by pub struct NvlPartitionMonitor
pub struct NvlPartitionMonitorInstruments {
    pub iteration_latency: Histogram<f64>,
    pub nmxc_changes_applied: Counter<u64>,
    pub operations_latency: Histogram<f64>,
    pub nvlink_config_apply_latency: Histogram<f64>,
}

impl NvlPartitionMonitorInstruments {
    pub fn new(
        meter: Meter,
        shared_metrics: SharedMetricsHolder<NvlPartitionMonitorMetrics>,
    ) -> Self {
        let iteration_latency = meter
            .f64_histogram("carbide_nvlink_partition_monitor_iteration_latency")
            .with_description("Time consumed for one monitor iteration")
            .with_unit("ms")
            .build();

        let operations_latency = meter
            .f64_histogram("carbide_nvlink_partition_monitor_nmxc_op_latency")
            .with_description("Time consumed for one NMX-C operation")
            .with_unit("ms")
            .build();

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
                .with_description("Number of machines nvlink_status_observation got updated")
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
                .with_description("Number of logical partitions that were monitored")
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
                .with_description("Number of physical partitions that were monitored")
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
                    "Number of chassis where NMX-C was unreachable during partition monitor iteration",
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
            iteration_latency,
            nmxc_changes_applied,
            operations_latency,
            nvlink_config_apply_latency,
        }
    }

    fn emit_counters_and_histograms(&self, metrics: &NvlPartitionMonitorMetrics) {
        self.iteration_latency.record(
            1000.0 * metrics.recording_started_at.elapsed().as_secs_f64(),
            &[],
        );

        for (change, &count) in metrics.applied_changes.iter() {
            self.nmxc_changes_applied.add(
                count as u64,
                &[
                    KeyValue::new("operation", change.operation),
                    KeyValue::new("status", change.status),
                ],
            );
        }

        for (change, latencies) in metrics.operation_latencies.iter() {
            for latency in latencies {
                self.operations_latency.record(
                    1000.0 * latency.as_secs_f64(), // latency in milliseconds
                    &[
                        KeyValue::new("operation", change.operation),
                        KeyValue::new("status", change.status),
                    ],
                );
            }
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
    pub fn values() -> impl Iterator<Item = Self> {
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
        let str_value = match value {
            NmxcMetricOperation::Create => "create",
            NmxcMetricOperation::Update => "update",
            NmxcMetricOperation::Remove => "remove",
            NmxcMetricOperation::RemoveDefaultPartition => "remove_default_partition",
        };

        Self::from(str_value)
    }
}

impl From<NmxcPartitionOperationType> for NmxcMetricOperation {
    fn from(value: NmxcPartitionOperationType) -> NmxcMetricOperation {
        match value {
            NmxcPartitionOperationType::Create => NmxcMetricOperation::Create,
            NmxcPartitionOperationType::Remove(_) => NmxcMetricOperation::Remove,
            NmxcPartitionOperationType::RemoveDefaultPartition(_)
            | NmxcPartitionOperationType::RemoveUnknownPartition(_) => {
                NmxcMetricOperation::RemoveDefaultPartition
            }
            NmxcPartitionOperationType::Update(_) => NmxcMetricOperation::Update,
        }
    }
}

impl NmxcMetricOperationStatus {
    pub fn values() -> impl Iterator<Item = Self> {
        [Self::Completed, Self::Failed, Self::Timedout].into_iter()
    }
}

impl From<NmxcMetricOperationStatus> for opentelemetry::Value {
    fn from(value: NmxcMetricOperationStatus) -> Self {
        let str_value = match value {
            NmxcMetricOperationStatus::Completed => "completed",
            NmxcMetricOperationStatus::Failed => "failed",
            NmxcMetricOperationStatus::Timedout => "timedout",
            NmxcMetricOperationStatus::Cancelled => "cancelled",
        };

        Self::from(str_value)
    }
}

/// Stores Metric data shared between the nvl partition monitor and the OpenTelemetry background task
pub struct MetricHolder {
    instruments: NvlPartitionMonitorInstruments,
    last_iteration_metrics: SharedMetricsHolder<NvlPartitionMonitorMetrics>,
}

impl MetricHolder {
    pub fn new(meter: Meter, hold_period: Duration) -> Self {
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
    pub fn update_metrics(&self, metrics: NvlPartitionMonitorMetrics) {
        // Emit the last recent latency metrics
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
