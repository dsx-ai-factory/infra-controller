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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use hyper::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use tonic::service::AxumBody;
use tower::ServiceBuilder;
use tracing::Span;

pub mod config;
use carbide_instrument::{MetricFamily, Outcome};
use carbide_uuid::machine::MachineId;
pub use config::{get_dpu_agent_meter, get_prometheus_registry};

/// LLDP and OVS expose one enum per restart flow, while the private Event
/// structs keep each existing log level, message, and field set intact. Every
/// variant still updates the shared counter with the same labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum RestartedService {
    Lldpd,
    OvsVswitchd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum ServiceRestartResult {
    Succeeded,
    Retrying,
    Failed,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dpu_agent_service_restart_attempts_total",
    kind = counter,
    component = "forge-dpu-agent",
    describe = "Number of DPU-agent service restart attempts, by service and result."
)]
pub(crate) struct DpuAgentServiceRestartAttempts {
    service: RestartedService,
    result: ServiceRestartResult,
}

/// One lldpd restart attempt. Each variant is an attempt's result, and holds
/// what that result has to say -- a success has no error, and only the final
/// failure counts attempts rather than numbering one.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_lldpd_restart",
    metric_family = DpuAgentServiceRestartAttempts
)]
pub(crate) enum LldpdRestart {
    #[event(
        labels(
            service = RestartedService::Lldpd,
            result = ServiceRestartResult::Succeeded
        ),
        log = info,
        message = "Restarted lldpd service"
    )]
    Succeeded {
        #[context(value)]
        attempt: i64,
    },

    #[event(
        labels(
            service = RestartedService::Lldpd,
            result = ServiceRestartResult::Retrying
        ),
        log = warn,
        message = "Couldn't restart lldpd service, retrying"
    )]
    Retrying {
        #[context]
        error: String,
        #[context(value)]
        attempt: i64,
    },

    #[event(
        labels(
            service = RestartedService::Lldpd,
            result = ServiceRestartResult::Failed
        ),
        log = error,
        message = "Couldn't restart lldpd service"
    )]
    Failed {
        #[context]
        error: String,
        #[context(value)]
        attempt_count: i64,
    },
}

/// One ovs-vswitchd restart attempt, recorded on the same counter.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_ovs_restart",
    metric_family = DpuAgentServiceRestartAttempts
)]
pub(crate) enum OvsRestart {
    #[event(
        labels(
            service = RestartedService::OvsVswitchd,
            result = ServiceRestartResult::Succeeded
        ),
        log = info,
        message = "Successfully restarted ovs-vswitchd.service"
    )]
    Succeeded {},

    #[event(
        labels(
            service = RestartedService::OvsVswitchd,
            result = ServiceRestartResult::Retrying
        ),
        log = error,
        message = "Restarting OVS after admin network change"
    )]
    Retrying {
        #[context(value)]
        error: String,
        #[context(value)]
        managed_host_config_version: String,
    },
}

/// `ReportLoop` labels one full agent reporting iteration rather than one
/// outbound RPC. That boundary also counts pre-RPC build and conversion
/// failures, plus the external FMDS push that generated-client RED metrics do
/// not see.
///
/// The label enum stays private so each caller-facing report variant fixes the
/// only valid `{report_loop, outcome}` pair before emitting its Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum ReportLoop {
    Inventory,
    ConfigFetch,
    FmdsPush,
    NetworkStatus,
}

pub(crate) enum InventoryReport {
    Succeeded,
    Failed,
}

pub(crate) enum ConfigFetch {
    Succeeded,
    Failed {
        error: String,
        retry_interval_seconds: f64,
    },
    NotFound {
        machine_id: String,
    },
}

pub(crate) enum FmdsPush {
    Succeeded,
    Failed { error: String, fmds_address: String },
}

pub(crate) enum NetworkStatus {
    Succeeded,
    ConnectionFailed { forge_api: String, error: String },
    RpcFailed { error: String },
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_dpu_agent_report_total",
    kind = counter,
    component = "forge-dpu-agent",
    describe = "Number of DPU-agent report-loop iterations, by loop and outcome"
)]
struct DpuAgentReport {
    report_loop: ReportLoop,
    outcome: Outcome,
}

/// `InventoryReportSucceeded` records the inventory loop's successful
/// completion and owns its DEBUG diagnostic. The other successful report
/// loops below write concise INFO records for operators.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_inventory_report_succeeded",
    metric_family = DpuAgentReport,
    log = debug,
    message = "Successfully updated machine inventory"
)]
struct InventoryReportSucceeded {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
}

/// `InventoryReportFailed` counts an inventory error without logging it.
/// `machine_inventory_updater::single_run` returns the same error to the
/// main-loop scheduler, which owns the diagnostic.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_inventory_report_failed",
    metric_family = DpuAgentReport,
    log = off
)]
struct InventoryReportFailed {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_config_fetch_succeeded",
    metric_family = DpuAgentReport,
    log = info,
    message = "Successfully fetched the latest configuration"
)]
struct ConfigFetchSucceeded {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_config_fetch_failed",
    metric_family = DpuAgentReport,
    log = error,
    message = "Failed to fetch the latest configuration. Will retry"
)]
struct ConfigFetchFailed {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
    #[context]
    error: String,
    #[context(value)]
    retry_interval_seconds: f64,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_config_not_found",
    metric_family = DpuAgentReport,
    log = warn,
    message = "DPU not found"
)]
struct ConfigNotFound {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
    #[context]
    machine_id: String,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_fmds_push_succeeded",
    metric_family = DpuAgentReport,
    log = info,
    message = "Successfully completed external FMDS update"
)]
struct FmdsPushSucceeded {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_fmds_push_failed",
    metric_family = DpuAgentReport,
    log = error,
    message = "Failed to send config update to external FMDS"
)]
struct FmdsPushFailed {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
    #[context]
    error: String,
    #[context]
    fmds_address: String,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_network_status_succeeded",
    metric_family = DpuAgentReport,
    log = info,
    message = "Successfully recorded DPU network status"
)]
struct NetworkStatusSucceeded {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_network_status_connection_failed",
    metric_family = DpuAgentReport,
    log = error,
    message = "record_network_status: Could not connect to Forge API server. Will retry."
)]
struct NetworkStatusConnectionFailed {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
    #[context]
    forge_api: String,
    #[context]
    error: String,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_network_status_rpc_failed",
    metric_family = DpuAgentReport,
    log = error,
    message = "Error while executing the record_network_status gRPC call"
)]
struct NetworkStatusRpcFailed {
    #[label]
    report_loop: ReportLoop,
    #[label]
    outcome: Outcome,
    #[context]
    error: String,
}

impl InventoryReport {
    pub(crate) fn emit(self) {
        match self {
            Self::Succeeded => carbide_instrument::emit(InventoryReportSucceeded {
                report_loop: ReportLoop::Inventory,
                outcome: Outcome::Ok,
            }),
            Self::Failed => carbide_instrument::emit(InventoryReportFailed {
                report_loop: ReportLoop::Inventory,
                outcome: Outcome::Error,
            }),
        }
    }
}

impl ConfigFetch {
    pub(crate) fn emit(self) {
        match self {
            Self::Succeeded => carbide_instrument::emit(ConfigFetchSucceeded {
                report_loop: ReportLoop::ConfigFetch,
                outcome: Outcome::Ok,
            }),
            Self::Failed {
                error,
                retry_interval_seconds,
            } => carbide_instrument::emit(ConfigFetchFailed {
                report_loop: ReportLoop::ConfigFetch,
                outcome: Outcome::Error,
                error,
                retry_interval_seconds,
            }),
            Self::NotFound { machine_id } => carbide_instrument::emit(ConfigNotFound {
                report_loop: ReportLoop::ConfigFetch,
                outcome: Outcome::Error,
                machine_id,
            }),
        }
    }
}

impl FmdsPush {
    pub(crate) fn emit(self) {
        match self {
            Self::Succeeded => carbide_instrument::emit(FmdsPushSucceeded {
                report_loop: ReportLoop::FmdsPush,
                outcome: Outcome::Ok,
            }),
            Self::Failed {
                error,
                fmds_address,
            } => carbide_instrument::emit(FmdsPushFailed {
                report_loop: ReportLoop::FmdsPush,
                outcome: Outcome::Error,
                error,
                fmds_address,
            }),
        }
    }
}

impl NetworkStatus {
    pub(crate) fn emit(self) {
        match self {
            Self::Succeeded => carbide_instrument::emit(NetworkStatusSucceeded {
                report_loop: ReportLoop::NetworkStatus,
                outcome: Outcome::Ok,
            }),
            Self::ConnectionFailed { forge_api, error } => {
                carbide_instrument::emit(NetworkStatusConnectionFailed {
                    report_loop: ReportLoop::NetworkStatus,
                    outcome: Outcome::Error,
                    forge_api,
                    error,
                });
            }
            Self::RpcFailed { error } => carbide_instrument::emit(NetworkStatusRpcFailed {
                report_loop: ReportLoop::NetworkStatus,
                outcome: Outcome::Error,
                error,
            }),
        }
    }
}

pub struct AgentMetricsState {
    meter: Meter,
}

impl AgentMetricsState {
    // Record the boot time of the machine we're running on as a Unix timestamp.
    // This only needs to be called once per lifetime of the Meter (which is
    // probably the same as the process lifetime).
    pub fn record_machine_boot_time(&self, timestamp: u64) {
        self.meter
            .u64_observable_gauge("machine_boot_time_seconds")
            .with_description("Timestamp of this machine's last boot")
            .with_callback(move |machine_boot_time| {
                machine_boot_time.observe(timestamp, &[]);
            })
            .build();
    }

    // Record the agent process's start time as a Unix timestamp. This only
    // needs to be called once per lifetime of the Meter (which is probably the
    // same as the process lifetime).
    pub fn record_agent_start_time(&self, timestamp: u64) {
        self.meter
            .u64_observable_gauge("agent_start_time_seconds")
            .with_description("Timestamp of the agent process's last start")
            .with_callback(move |agent_start_time| {
                agent_start_time.observe(timestamp, &[]);
            })
            .build();
    }

    // Export the expiry of the TLS client certificate the agent presents to
    // the Forge API, as a Unix timestamp. `expiry` runs on every metrics
    // collection, so the exported value follows certificate renewals; a
    // collection that finds no readable certificate observes nothing. This
    // only needs to be called once per lifetime of the Meter (which is
    // probably the same as the process lifetime).
    pub fn record_client_cert_expiry_time(
        &self,
        expiry: impl Fn() -> Option<i64> + Send + Sync + 'static,
    ) {
        self.meter
            .i64_observable_gauge("client_cert_expiry_time_seconds")
            .with_description("Timestamp when the agent's TLS client certificate expires")
            .with_callback(move |cert_expiry_time| {
                if let Some(timestamp) = expiry() {
                    cert_expiry_time.observe(timestamp, &[]);
                }
            })
            .build();
    }
}

pub fn create_metrics(meter: Meter) -> Arc<AgentMetricsState> {
    Arc::new(AgentMetricsState { meter })
}

pub struct NetworkMonitorMetricsState {
    // Metrics for network monitoring
    network_latency: Histogram<f64>,
    network_loss_percent: Histogram<f64>,
    network_monitor_error: Counter<u64>,
    network_communication_error: Counter<u64>,

    // Fields used for network_reachable observations
    network_reachable_map: NetworkReachableMap,
}

type NetworkReachableMap = Arc<Mutex<Option<HashMap<MachineId, bool>>>>;

impl NetworkMonitorMetricsState {
    pub fn initialize(meter: Meter, machine_id: MachineId) -> Arc<Self> {
        let network_reachable_map = NetworkReachableMap::default();

        {
            let network_reachable_map = network_reachable_map.clone();
            meter
                .u64_observable_gauge("forge_dpu_agent_network_reachable")
                .with_description(
                    "Network reachability status (1 for reachable, 0 for unreachable)",
                )
                .with_callback(move |observer| {
                    let network_reachable_map = network_reachable_map.lock().unwrap();
                    if let Some(map) = network_reachable_map.as_ref() {
                        // Export reachability metrics from the map
                        for (dpu_id, reachable) in map.iter() {
                            let reachability = if *reachable { 1 } else { 0 };
                            observer.observe(
                                reachability,
                                &[
                                    KeyValue::new("source_dpu_id", machine_id.to_string()),
                                    KeyValue::new("dest_dpu_id", dpu_id.to_string()),
                                ],
                            );
                        }
                    }
                })
                .build();
        }

        let network_latency = meter
            .f64_histogram("forge_dpu_agent_network_latency")
            .with_unit("ms")
            .build();
        let network_loss_percent = meter
            .f64_histogram("forge_dpu_agent_network_loss_percentage")
            .with_description("Percentage of failed pings out of total 5 pings")
            .build();
        let network_monitor_error = meter
            .u64_counter("forge_dpu_agent_network_monitor_error")
            .with_description("Network monitor errors unrelated to network connectivity")
            .build();
        let network_communication_error = meter
            .u64_counter("forge_dpu_agent_network_communication_error")
            .with_description("Network monitor errors related to ping dpu")
            .build();

        Arc::new(Self {
            network_latency,
            network_loss_percent,
            network_monitor_error,
            network_communication_error,
            network_reachable_map,
        })
    }

    /// Records network latency between two DPUs as milliseconds.
    ///
    /// # Parameters
    /// - `latency`: Network latency between the two DPUs.
    /// - `source_dpu_id`: The ID of source DPU.
    /// - `dest_dpu_id`: The ID of destination DPU.
    pub fn record_network_latency(
        &self,
        latency: Duration,
        source_dpu_id: MachineId,
        dest_dpu_id: MachineId,
    ) {
        let attributes = [
            KeyValue::new("source_dpu_id", source_dpu_id.to_string()),
            KeyValue::new("dest_dpu_id", dest_dpu_id.to_string()),
        ];
        self.network_latency
            .record(latency.as_secs_f64() * 1000.0, &attributes);
    }

    /// Record network loss percent out of total number of pings sent during one network check.
    ///
    /// # Parameters
    /// - `loss_percent`: Percentage of loss out of total pings sent.
    /// - `source_dpu_id`: The ID of source DPU.
    /// - `dest_dpu_id`: The ID of destination DPU.
    pub fn record_network_loss_percent(
        &self,
        loss_percent: f64,
        source_dpu_id: MachineId,
        dest_dpu_id: MachineId,
    ) {
        let attributes = [
            KeyValue::new("source_dpu_id", source_dpu_id.to_string()),
            KeyValue::new("dest_dpu_id", dest_dpu_id.to_string()),
        ];
        self.network_loss_percent.record(loss_percent, &attributes);
    }

    /// Overwrites the network reachable map with a new map.
    ///
    /// # Parameters
    /// - `new_reachable_map`: Records reachability between DPUs where the key is ID of destination DPU
    ///   and value is reachability as bool
    pub fn update_network_reachable_map(&self, new_reachable_map: HashMap<MachineId, bool>) {
        *self.network_reachable_map.lock().unwrap() = Some(new_reachable_map);
    }

    /// Records an error related to network communication with a DPU.
    ///
    /// # Parameters
    /// - `source_dpu_id`: The ID of this DPU, which starts the communication.
    /// - `dest_dpu_id`: The destination DPU id to which communication error happened.
    /// - `error_type`: A string describing the type of communication error.
    pub fn record_communication_error(
        &self,
        source_dpu_id: MachineId,
        dest_dpu_id: MachineId,
        error_type: String,
    ) {
        let attributes = [
            KeyValue::new("source_dpu_id", source_dpu_id.to_string()),
            KeyValue::new("dest_dpu_id", dest_dpu_id.to_string()),
            KeyValue::new("error_type", error_type),
        ];
        self.network_communication_error.add(1, &attributes);
    }

    /// Records an error related to network monitoring that is unrelated to connectivity.
    ///
    /// # Parameters
    /// - `machine_id`: The ID of this machine
    /// - `error_type`: A string describing the type of network monitor error.
    pub fn record_monitor_error(&self, machine_id: MachineId, error_type: String) {
        let attributes = [
            KeyValue::new("dpu_id", machine_id.to_string()),
            KeyValue::new("error_type", error_type),
        ];
        self.network_monitor_error.add(1, &attributes);
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_http_request_started",
    metric_name = "http_requests_total",
    metric_name_unchecked,
    component = "forge-dpu-agent",
    log = info,
    metric = counter,
    message = "HTTP request started",
    describe = "Number of HTTP requests made."
)]
struct DpuAgentHttpRequestStarted {
    #[context]
    method: String,
    #[context]
    request_path: String,
}

impl DpuAgentHttpRequestStarted {
    fn new(request: &Request<AxumBody>) -> Self {
        Self {
            method: request.method().to_string(),
            request_path: request.uri().path().to_string(),
        }
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "dpu_agent_http_response_generated",
    metric_name = "request_latency_milliseconds",
    metric_name_unchecked,
    component = "forge-dpu-agent",
    log = info,
    metric = histogram,
    message = "HTTP response generated",
    describe = "HTTP request latency"
)]
struct DpuAgentHttpResponseGenerated {
    #[context(value)]
    latency_milliseconds: f64,
    #[observation]
    latency: Duration,
}

impl DpuAgentHttpResponseGenerated {
    fn new(latency: Duration) -> Self {
        Self {
            latency_milliseconds: latency.as_secs_f64() * 1000.0,
            latency,
        }
    }
}

/// `WithTracingLayer` keeps `AgentMetricsState` in its public API for existing
/// callers. The HTTP Events resolve their instruments through the global meter
/// provider, so the implementation does not need to read the handle.
pub trait WithTracingLayer {
    fn with_tracing_layer(self, metrics: Arc<AgentMetricsState>) -> Router;
}

impl WithTracingLayer for Router {
    fn with_tracing_layer(self, _metrics: Arc<AgentMetricsState>) -> Router {
        let layer = tower_http::trace::TraceLayer::new_for_http()
            .on_request(move |request: &Request<AxumBody>, _span: &Span| {
                carbide_instrument::emit(DpuAgentHttpRequestStarted::new(request));
            })
            .on_response(
                move |_response: &Response<AxumBody>, latency: Duration, _span: &Span| {
                    carbide_instrument::emit(DpuAgentHttpResponseGenerated::new(latency));
                },
            );

        self.layer(ServiceBuilder::new().layer(layer))
    }
}

#[cfg(test)]
mod report_loop_tests {
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;

    const REPORT_METRIC: &str = "carbide_dpu_agent_report_total";

    struct EventCase {
        emit: fn(),
        report_loop: &'static str,
        outcome: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct EventObservation {
        metric_delta: f64,
        logs: Vec<LogShape>,
    }

    #[derive(Debug, PartialEq)]
    struct LogShape {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        fields: Vec<(String, String)>,
        retry_interval_kind: Option<CapturedFieldKind>,
    }

    fn expected_log(
        metadata_name: &str,
        level: tracing::Level,
        message: &str,
        report_loop: &str,
        outcome: &str,
        context: &[(&str, &str)],
        retry_interval_kind: Option<CapturedFieldKind>,
    ) -> Vec<LogShape> {
        let mut fields = vec![
            ("event_name".to_string(), metadata_name.to_string()),
            ("metric_name".to_string(), REPORT_METRIC.to_string()),
            ("report_loop".to_string(), report_loop.to_string()),
            ("outcome".to_string(), outcome.to_string()),
        ];
        fields.extend(
            context
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string())),
        );

        vec![LogShape {
            metadata_name: metadata_name.to_string(),
            level,
            message: message.to_string(),
            fields,
            retry_interval_kind,
        }]
    }

    fn observe_event(case: EventCase) -> EventObservation {
        let EventCase {
            emit,
            report_loop,
            outcome,
        } = case;
        let metrics = MetricsCapture::start();
        let logs = capture_logs(emit)
            .into_iter()
            .map(|log| {
                let retry_interval_kind = log.field_kind("retry_interval_seconds");
                LogShape {
                    metadata_name: log.metadata_name,
                    level: log.level,
                    message: log.message,
                    fields: log.fields,
                    retry_interval_kind,
                }
            })
            .collect();

        EventObservation {
            metric_delta: metrics.counter_delta(
                REPORT_METRIC,
                &[("report_loop", report_loop), ("outcome", outcome)],
            ),
            logs,
        }
    }

    #[test]
    fn semantic_events_preserve_the_loop_outcome_matrix_and_log_shapes() {
        const MACHINE_ID: &str = "fm100000000000000000000000000000000000000000000000000000000000";

        check_values(
            [
                Check {
                    scenario: "inventory success logs at debug",
                    input: EventCase {
                        emit: || InventoryReport::Succeeded.emit(),
                        report_loop: "inventory",
                        outcome: "ok",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_inventory_report_succeeded",
                            tracing::Level::DEBUG,
                            "Successfully updated machine inventory",
                            "inventory",
                            "ok",
                            &[],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "inventory failure remains metric-only",
                    input: EventCase {
                        emit: || InventoryReport::Failed.emit(),
                        report_loop: "inventory",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
                Check {
                    scenario: "config fetch success logs at info",
                    input: EventCase {
                        emit: || ConfigFetch::Succeeded.emit(),
                        report_loop: "config_fetch",
                        outcome: "ok",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_config_fetch_succeeded",
                            tracing::Level::INFO,
                            "Successfully fetched the latest configuration",
                            "config_fetch",
                            "ok",
                            &[],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "config fetch failure retains retry context",
                    input: EventCase {
                        emit: || {
                            ConfigFetch::Failed {
                                error: "config failed".to_string(),
                                retry_interval_seconds: 30.5,
                            }
                            .emit()
                        },
                        report_loop: "config_fetch",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_config_fetch_failed",
                            tracing::Level::ERROR,
                            "Failed to fetch the latest configuration. Will retry",
                            "config_fetch",
                            "error",
                            &[
                                ("error", "config failed"),
                                ("retry_interval_seconds", "30.5"),
                            ],
                            Some(CapturedFieldKind::F64),
                        ),
                    },
                },
                Check {
                    scenario: "FMDS success logs at info",
                    input: EventCase {
                        emit: || FmdsPush::Succeeded.emit(),
                        report_loop: "fmds_push",
                        outcome: "ok",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_fmds_push_succeeded",
                            tracing::Level::INFO,
                            "Successfully completed external FMDS update",
                            "fmds_push",
                            "ok",
                            &[],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "FMDS failure retains address context",
                    input: EventCase {
                        emit: || {
                            FmdsPush::Failed {
                                error: "FMDS failed".to_string(),
                                fmds_address: "http://fmds:50051".to_string(),
                            }
                            .emit()
                        },
                        report_loop: "fmds_push",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_fmds_push_failed",
                            tracing::Level::ERROR,
                            "Failed to send config update to external FMDS",
                            "fmds_push",
                            "error",
                            &[
                                ("error", "FMDS failed"),
                                ("fmds_address", "http://fmds:50051"),
                            ],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "network status success logs at info",
                    input: EventCase {
                        emit: || NetworkStatus::Succeeded.emit(),
                        report_loop: "network_status",
                        outcome: "ok",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_network_status_succeeded",
                            tracing::Level::INFO,
                            "Successfully recorded DPU network status",
                            "network_status",
                            "ok",
                            &[],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "network status RPC failure retains error context",
                    input: EventCase {
                        emit: || {
                            NetworkStatus::RpcFailed {
                                error: "RPC failed".to_string(),
                            }
                            .emit()
                        },
                        report_loop: "network_status",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_network_status_rpc_failed",
                            tracing::Level::ERROR,
                            "Error while executing the record_network_status gRPC call",
                            "network_status",
                            "error",
                            &[("error", "RPC failed")],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "missing config retains machine context",
                    input: EventCase {
                        emit: || {
                            ConfigFetch::NotFound {
                                machine_id: MACHINE_ID.to_string(),
                            }
                            .emit()
                        },
                        report_loop: "config_fetch",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_config_not_found",
                            tracing::Level::WARN,
                            "DPU not found",
                            "config_fetch",
                            "error",
                            &[("machine_id", MACHINE_ID)],
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "network connection failure retains endpoint context",
                    input: EventCase {
                        emit: || {
                            NetworkStatus::ConnectionFailed {
                                forge_api: "https://forge:50051".to_string(),
                                error: "connection refused".to_string(),
                            }
                            .emit()
                        },
                        report_loop: "network_status",
                        outcome: "error",
                    },
                    expect: EventObservation {
                        metric_delta: 1.0,
                        logs: expected_log(
                            "dpu_agent_network_status_connection_failed",
                            tracing::Level::ERROR,
                            "record_network_status: Could not connect to Forge API server. Will retry.",
                            "network_status",
                            "error",
                            &[
                                ("forge_api", "https://forge:50051"),
                                ("error", "connection refused"),
                            ],
                            None,
                        ),
                    },
                },
            ],
            observe_event,
        );
    }
}

#[cfg(test)]
mod http_request_tests {
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use carbide_instrument::emit;
    use carbide_instrument::testing::{
        ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
    };
    use carbide_test_support::{Check, check_values};
    use tower::ServiceExt;

    use super::*;

    const REQUEST_METRIC: &str = "http_requests_total";
    const LATENCY_METRIC: &str = "request_latency_milliseconds";

    enum EventCase {
        Request,
        Response,
    }

    #[derive(Debug, PartialEq)]
    struct EventObservation {
        request_delta: f64,
        latency_count_delta: u64,
        latency_sum_delta: ApproxHistogramSum,
        logs: Vec<LogObservation>,
    }

    #[derive(Debug, PartialEq)]
    struct LogObservation {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        fields: Vec<(String, String)>,
        method_kind: Option<CapturedFieldKind>,
        request_path_kind: Option<CapturedFieldKind>,
        latency_kind: Option<CapturedFieldKind>,
    }

    fn observe_event(case: EventCase) -> EventObservation {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| match case {
            EventCase::Request => emit(DpuAgentHttpRequestStarted {
                method: "GET".to_string(),
                request_path: "/latest/meta-data".to_string(),
            }),
            EventCase::Response => emit(DpuAgentHttpResponseGenerated::new(Duration::from_micros(
                12_500,
            ))),
        })
        .into_iter()
        .map(|log| LogObservation {
            method_kind: log.field_kind("method"),
            request_path_kind: log.field_kind("request_path"),
            latency_kind: log.field_kind("latency_milliseconds"),
            metadata_name: log.metadata_name,
            level: log.level,
            message: log.message,
            fields: log.fields,
        })
        .collect();

        EventObservation {
            request_delta: metrics.counter_delta(REQUEST_METRIC, &[]),
            latency_count_delta: metrics.histogram_count_delta(LATENCY_METRIC, &[]),
            latency_sum_delta: metrics.histogram_sum_delta(LATENCY_METRIC, &[]),
            logs,
        }
    }

    #[test]
    fn http_events_preserve_metrics_and_structured_logs() {
        check_values(
            [
                Check {
                    scenario: "request start increments the legacy counter and logs request context",
                    input: EventCase::Request,
                    expect: EventObservation {
                        request_delta: 1.0,
                        latency_count_delta: 0,
                        latency_sum_delta: ApproxHistogramSum(0.0),
                        logs: vec![LogObservation {
                            metadata_name: "dpu_agent_http_request_started".to_string(),
                            level: tracing::Level::INFO,
                            message: "HTTP request started".to_string(),
                            fields: vec![
                                (
                                    "event_name".to_string(),
                                    "dpu_agent_http_request_started".to_string(),
                                ),
                                ("metric_name".to_string(), REQUEST_METRIC.to_string()),
                                ("method".to_string(), "GET".to_string()),
                                ("request_path".to_string(), "/latest/meta-data".to_string()),
                            ],
                            method_kind: Some(CapturedFieldKind::Debug),
                            request_path_kind: Some(CapturedFieldKind::Debug),
                            latency_kind: None,
                        }],
                    },
                },
                Check {
                    scenario: "response completion records milliseconds and logs native latency",
                    input: EventCase::Response,
                    expect: EventObservation {
                        request_delta: 0.0,
                        latency_count_delta: 1,
                        latency_sum_delta: ApproxHistogramSum(12.5),
                        logs: vec![LogObservation {
                            metadata_name: "dpu_agent_http_response_generated".to_string(),
                            level: tracing::Level::INFO,
                            message: "HTTP response generated".to_string(),
                            fields: vec![
                                (
                                    "event_name".to_string(),
                                    "dpu_agent_http_response_generated".to_string(),
                                ),
                                ("metric_name".to_string(), LATENCY_METRIC.to_string()),
                                ("latency_milliseconds".to_string(), "12.5".to_string()),
                            ],
                            method_kind: None,
                            request_path_kind: None,
                            latency_kind: Some(CapturedFieldKind::F64),
                        }],
                    },
                },
            ],
            observe_event,
        );
    }

    #[test]
    fn tracing_layer_emits_one_start_and_completion_event_per_request() {
        let metrics = MetricsCapture::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        let logs = capture_logs(|| {
            runtime.block_on(async {
                let metrics_state = create_metrics(opentelemetry::global::meter("test"));
                let router = Router::new()
                    .route("/health", get(|| async { StatusCode::NO_CONTENT }))
                    .with_tracing_layer(metrics_state);
                let response = router
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/health")
                            .body(Body::empty())
                            .expect("test request should build"),
                    )
                    .await
                    .expect("test request should complete");
                assert_eq!(response.status(), StatusCode::NO_CONTENT);
            });
        });

        assert_eq!(metrics.counter_delta(REQUEST_METRIC, &[]), 1.0);
        assert_eq!(metrics.histogram_count_delta(LATENCY_METRIC, &[]), 1);
        assert_eq!(
            logs.iter()
                .map(|log| log.metadata_name.as_str())
                .collect::<Vec<_>>(),
            [
                "dpu_agent_http_request_started",
                "dpu_agent_http_response_generated",
            ]
        );
    }
}
