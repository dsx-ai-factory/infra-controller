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

mod discovery;
mod entity_metrics;
mod firmware;
mod gpu_inventory;
pub(crate) mod inventory;
mod leak_detector;
mod logs;
mod manager;
mod nmxc;
mod nmxc_schema_override;
mod nmxt;
mod nvue;
#[cfg(test)]
pub(in crate::collectors) mod projection_test_support;
mod reachability;
mod runtime;
mod sensors;
mod telemetry;

pub use discovery::{EntityDiscoveryCollector, EntityDiscoveryCollectorConfig};
pub use entity_metrics::{MetricsCollector, MetricsCollectorConfig};
pub use firmware::{FirmwareCollector, FirmwareCollectorConfig};
pub use gpu_inventory::{GpuInventoryCollector, GpuInventoryCollectorConfig};
pub(crate) use inventory::SharedInventory;
pub use leak_detector::{LeakDetectorCollector, LeakDetectorCollectorConfig};
pub(crate) use logs::auto::{AutoFailureBudget, BudgetDecision, FailureKind};
pub use logs::{
    DowngradeEvent, DowngradeReason, LogDowngradeRegistry, LogsCollector, LogsCollectorConfig,
    SseLogCollector, SseLogCollectorConfig,
};
pub use manager::{ManagerCollector, ManagerCollectorConfig};
pub use nmxc::{NmxcCollector, NmxcCollectorConfig};
pub(crate) use nmxc_schema_override::{
    NmxcSchemaOverride, NmxcSchemaOverrideCollector, NmxcSchemaOverrideCollectorConfig,
};
pub(crate) use nmxt::NMXT_PORT;
pub use nmxt::{NmxtCollector, NmxtCollectorConfig};
pub(crate) use nvue::gnmi::subscriber::spawn_gnmi_collector;
pub use nvue::rest::collector::{NvueRestCollector, NvueRestCollectorConfig};
pub(crate) use reachability::{
    COLLECTOR_TYPE as REACHABILITY_COLLECTOR_TYPE, ReachabilityCollector,
    ReachabilityCollectorStartConfig, ReachabilityService, ReachabilityTarget,
};
pub use runtime::{
    BackoffConfig, Collector, CollectorStartContext, EventStream, ExponentialBackoff,
    IterationResult, PeriodicCollector, StreamMetrics, StreamingCollector,
    StreamingCollectorStartContext, StreamingConnectResult,
};
pub use sensors::{SensorCollector, SensorCollectorConfig};
pub use telemetry::{TelemetryCollector, TelemetryCollectorConfig};
