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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::context::{CollectorKind, DiscoveryLoopContext};
use crate::HealthError;
use crate::bmc::BmcClient;
use crate::collectors::{
    AutoFailureBudget, BackoffConfig, BudgetDecision, Collector, CollectorStartContext,
    EntityDiscoveryCollector, EntityDiscoveryCollectorConfig, FailureKind, FirmwareCollector,
    FirmwareCollectorConfig, GpuInventoryCollector, GpuInventoryCollectorConfig,
    LeakDetectorCollector, LeakDetectorCollectorConfig, LogsCollector, LogsCollectorConfig,
    MetricsCollector, MetricsCollectorConfig, NmxcCollector, NmxcCollectorConfig,
    NmxcSchemaOverrideCollector, NmxcSchemaOverrideCollectorConfig, NmxtCollector,
    NmxtCollectorConfig, NvueRestCollector, NvueRestCollectorConfig, SensorCollector,
    SensorCollectorConfig, SseLogCollector, SseLogCollectorConfig, StreamingCollectorStartContext,
    TelemetryCollector, TelemetryCollectorConfig, spawn_gnmi_collector,
};
use crate::config::{
    Configurable, LogCollectionMode, NmxcCollectorConfig as NmxcCollectorOptions, PeriodicLogConfig,
};
use crate::endpoint::{BmcEndpoint, EndpointMetadata, SwitchEndpointRole};
use crate::metrics::CollectorRegistry;
use crate::sink::DataSink;

fn logs_state_file_path(template: &str, endpoint_id: &str) -> PathBuf {
    PathBuf::from(template.replace("{machine_id}", endpoint_id))
}

fn build_periodic_logs_collector_config(
    config: &PeriodicLogConfig,
    state_file_path: PathBuf,
    data_sink: Option<Arc<dyn DataSink>>,
    include_diagnostics: bool,
) -> LogsCollectorConfig {
    LogsCollectorConfig {
        state_file_path,
        service_refresh_interval: config.state_refresh_interval,
        data_sink,
        include_diagnostics,
        exclude_services: config.exclude_services.clone(),
        skip_initial_history: config.skip_initial_history,
    }
}

/// Returns whether an endpoint is eligible for direct NMX-C Subscribe collection.
pub(super) fn switch_supports_nmxc_subscription(endpoint: &BmcEndpoint) -> bool {
    endpoint.switch_data().is_some_and(|switch| {
        // The NICo API exposes switch host targets through Switch.nvos_info, but
        // NMX-C Subscribe is only valid on the primary switch when desired NMX-C
        // config is enabled. FabricManager readiness is still discovered by
        // attempting Subscribe and retrying with backoff, because API status can
        // lag runtime state.
        matches!(switch.endpoint_role, SwitchEndpointRole::Host)
            && switch.is_primary
            && switch.nmxc_enabled
    })
}

/// Transport services eligible to start for one endpoint.
///
/// Reachability uses this plan so it probes only services selected by the
/// same collector configuration, endpoint-role, capability, and sink gates.
/// A `true` field permits a collector start attempt; it does not mean that the
/// collector started successfully or completed a protocol exchange.
#[derive(Clone, Copy, Default)]
pub(super) struct CollectorEligibility {
    /// At least one eligible collector uses the discovered BMC Redfish socket.
    pub(super) redfish: bool,

    /// The endpoint is eligible for direct NMX-T collection.
    pub(super) nmxt: bool,

    /// The endpoint is eligible for direct NMX-C Subscribe collection.
    pub(super) nmxc: bool,

    /// The endpoint is eligible for direct NVUE REST collection.
    pub(super) nvue_rest: bool,

    /// The endpoint is eligible for direct NVUE gNMI collection.
    pub(super) nvue_gnmi: bool,
}

/// Computes transport eligibility using collector spawn gates.
///
/// `data_sink_present` must describe the same optional sink passed to the spawn
/// path. SSE and NMX-C collectors require that sink, while periodic Redfish
/// collectors may still run without one. Non-host endpoints collapse all
/// Redfish-backed collectors into one BMC transport; switch-host endpoints use
/// only their direct NVUE, gNMI, NMX-T, and NMX-C transports.
pub(super) fn collector_eligibility(
    ctx: &DiscoveryLoopContext,
    endpoint: &BmcEndpoint,
    data_sink_present: bool,
) -> CollectorEligibility {
    let switch_host = endpoint
        .switch_data()
        .is_some_and(|switch| matches!(switch.endpoint_role, SwitchEndpointRole::Host));

    if !switch_host {
        // Periodic logs perform Redfish requests without a sink. SSE requires
        // a sink, while Auto remains periodic-eligible after a downgrade.
        let logs = ctx
            .logs_config
            .as_option()
            .is_some_and(|config| match config.mode {
                LogCollectionMode::Periodic => true,
                LogCollectionMode::Sse => data_sink_present,
                LogCollectionMode::Auto => {
                    data_sink_present || ctx.log_downgrade_registry.is_downgraded(&endpoint.key())
                }
            });

        let gpu_inventory = ctx.gpu_inventory_config.is_enabled()
            && ctx.api_client.is_some()
            && matches!(endpoint.metadata, Some(EndpointMetadata::Machine(_)));

        // Generic collectors share the discovered BMC socket, so one target
        // represents every enabled Redfish-backed collector on this endpoint.
        return CollectorEligibility {
            redfish: ctx.sensors_config.is_enabled()
                || ctx.metrics_config.is_enabled()
                || ctx.telemetry_config.is_enabled()
                || logs
                || ctx.firmware_config.is_enabled()
                || ctx.leak_detector_config.is_enabled()
                || gpu_inventory,
            ..Default::default()
        };
    }

    let nvue = ctx.nvue_config.as_option();

    // NMX-C starts only when an enabled sink consumes its logs or domain reports.
    let nmxc = ctx.nmxc_config.is_enabled()
        && switch_supports_nmxc_subscription(endpoint)
        && (ctx.log_event_sink_enabled || ctx.nvlink_domain_health_report_sink_enabled)
        && data_sink_present;

    CollectorEligibility {
        redfish: false,
        nmxt: ctx.nmxt_config.is_enabled()
            && endpoint
                .switch_data()
                .is_some_and(|switch| switch.nmxt_enabled),
        nmxc,
        nvue_rest: nvue.is_some_and(|config| config.rest.is_enabled()),
        nvue_gnmi: nvue.is_some_and(|config| config.gnmi.is_enabled()),
    }
}

pub(super) fn spawn_collectors_for_endpoint(
    ctx: &mut DiscoveryLoopContext,
    endpoint: &Arc<BmcEndpoint>,
    data_sink: Option<Arc<dyn DataSink>>,
    metrics_prefix: &str,
) -> Result<(), HealthError> {
    let endpoint_role = endpoint.switch_data().map(|switch| switch.endpoint_role);

    if matches!(endpoint_role, Some(SwitchEndpointRole::Host)) {
        spawn_switch_host_collectors(ctx, endpoint, data_sink, metrics_prefix)
    } else {
        spawn_generic_redfish_collectors(ctx, endpoint, data_sink, metrics_prefix)
    }
}

fn spawn_generic_redfish_collectors(
    ctx: &mut DiscoveryLoopContext,
    endpoint: &Arc<BmcEndpoint>,
    data_sink: Option<Arc<dyn DataSink>>,
    metrics_prefix: &str,
) -> Result<(), HealthError> {
    let key = endpoint.key();
    let endpoint_arc = endpoint.clone();
    let bmc = endpoint.bmc().clone();

    let sensors_enabled = matches!(ctx.sensors_config, Configurable::Enabled(_));
    let metrics_enabled = matches!(ctx.metrics_config, Configurable::Enabled(_));
    // The GPU inventory collector reads GPU counts from the shared entity
    // inventory, so entity discovery must also run wherever it does — even if
    // sensors/metrics are disabled. Mirror the GPU collector's own spawn gate
    // (enabled + API client present + machine endpoint) so discovery starts for
    // exactly those endpoints and not for switches / power shelves.
    let gpu_inventory_enabled = matches!(ctx.gpu_inventory_config, Configurable::Enabled(_))
        && ctx.api_client.is_some()
        && matches!(endpoint.metadata, Some(EndpointMetadata::Machine(_)));
    // GPU identity attributes on log records are resolved against the shared
    // entity inventory, so discovery must also run for a logs-only deployment.
    let gpu_identity_enabled = ctx.attributes.gpu_identity;

    if (sensors_enabled || metrics_enabled || gpu_inventory_enabled || gpu_identity_enabled)
        && !ctx.collectors.contains(CollectorKind::Discovery, &key)
    {
        let shared = ctx.collectors.inventory_for(&key);
        let collector_registry = Arc::new(ctx.metrics_manager.create_collector_registry(
            format!("entity_discovery_collector_{key}"),
            metrics_prefix,
        )?);
        match Collector::start::<EntityDiscoveryCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            EntityDiscoveryCollectorConfig {
                shared,
                request_concurrency: ctx.bmc_request_concurrency,
                gpu_identity: gpu_identity_enabled,
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: ctx.discovery_config.refresh_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(monitor) => {
                ctx.collectors
                    .insert(CollectorKind::Discovery, key.clone().into(), monitor);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    discovery_collector_count = ctx.collectors.len(CollectorKind::Discovery),
                    "Started entity discovery for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start entity discovery collector"
                );
            }
        }
    }

    if let Configurable::Enabled(sensor_cfg) = &ctx.sensors_config
        && !ctx.collectors.contains(CollectorKind::Sensor, &key)
    {
        let shared = ctx.collectors.inventory_for(&key);
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("sensor_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<SensorCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            SensorCollectorConfig {
                data_sink: data_sink.clone(),
                shared,
                request_concurrency: ctx.bmc_request_concurrency,
                include_sensor_thresholds: sensor_cfg.include_sensor_thresholds,
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: sensor_cfg.sensor_fetch_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(monitor) => {
                ctx.collectors
                    .insert(CollectorKind::Sensor, key.clone().into(), monitor);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    sensor_collector_count = ctx.collectors.len(CollectorKind::Sensor),
                    "Started sensor collection for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start sensor collector"
                );
            }
        }
    }

    if let Configurable::Enabled(metrics_cfg) = &ctx.metrics_config
        && !ctx.collectors.contains(CollectorKind::Metrics, &key)
    {
        let shared = ctx.collectors.inventory_for(&key);
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("metrics_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<MetricsCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            MetricsCollectorConfig {
                data_sink: data_sink.clone(),
                shared,
                request_concurrency: ctx.bmc_request_concurrency,
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: metrics_cfg.fetch_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(monitor) => {
                ctx.collectors
                    .insert(CollectorKind::Metrics, key.clone().into(), monitor);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    entity_metrics_collector_count = ctx.collectors.len(CollectorKind::Metrics),
                    "Started entity metrics collection for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start entity metrics collector"
                );
            }
        }
    }

    if let Configurable::Enabled(telemetry_cfg) = &ctx.telemetry_config
        && !ctx.collectors.contains(CollectorKind::Telemetry, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("telemetry_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<TelemetryCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            TelemetryCollectorConfig {
                data_sink: data_sink.clone(),
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: telemetry_cfg.fetch_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(monitor) => {
                ctx.collectors
                    .insert(CollectorKind::Telemetry, key.clone().into(), monitor);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    telemetry_collector_count = ctx.collectors.len(CollectorKind::Telemetry),
                    "Started telemetry service collection for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start telemetry collector"
                );
            }
        }
    }

    if let Configurable::Enabled(logs_cfg) = &ctx.logs_config
        && !ctx.collectors.contains(CollectorKind::Logs, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("log_collector_{key}"), metrics_prefix)?,
        );

        // Resolved once here because both SSE spawn paths below share the
        // endpoint's inventory handle.
        let sse_gpu_inventory = if ctx.attributes.gpu_identity {
            Some(ctx.collectors.inventory_for(&key))
        } else {
            None
        };

        let sse_cfg = logs_cfg.sse_or_default();
        let sse_backoff_config = || BackoffConfig {
            initial: sse_cfg.initial_backoff,
            max: sse_cfg.max_backoff,
        };

        let spawn_periodic_logs = |pcfg: PeriodicLogConfig,
                                   data_sink: Option<Arc<dyn DataSink>>,
                                   collector_registry: Arc<_>|
         -> Option<Result<Collector, HealthError>> {
            let endpoint_id = endpoint.log_identity().into_owned();
            let state_file_path = logs_state_file_path(&pcfg.logs_state_file, &endpoint_id);
            let collector_config = build_periodic_logs_collector_config(
                &pcfg,
                state_file_path,
                data_sink,
                ctx.logs_include_diagnostics,
            );

            Some(Collector::start::<LogsCollector<BmcClient>>(
                endpoint_arc.clone(),
                bmc.clone(),
                collector_config,
                CollectorStartContext {
                    limiter: ctx.limiter.clone(),
                    iteration_interval: pcfg.logs_collection_interval,
                    collector_registry,
                    metrics_manager: ctx.metrics_manager.clone(),
                },
            ))
        };

        let result = match logs_cfg.mode {
            LogCollectionMode::Sse => {
                if let Some(data_sink) = data_sink.clone() {
                    Some(Collector::start_streaming::<SseLogCollector<BmcClient>, _>(
                        endpoint_arc.clone(),
                        bmc.clone(),
                        SseLogCollectorConfig {
                            include_diagnostics: ctx.logs_include_diagnostics,
                            request_concurrency: ctx.bmc_request_concurrency,
                            gpu_inventory: sse_gpu_inventory,
                        },
                        data_sink,
                        StreamingCollectorStartContext {
                            backoff_config: sse_backoff_config(),
                            collector_registry,
                        },
                        |_| true,
                    ))
                } else {
                    tracing::warn!(
                        endpoint_key = %key,
                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                        "SSE log collector requires a data sink, skipping"
                    );

                    None
                }
            }
            LogCollectionMode::Periodic => spawn_periodic_logs(
                logs_cfg.periodic_or_default(),
                data_sink.clone(),
                collector_registry,
            ),
            LogCollectionMode::Auto => {
                if ctx.log_downgrade_registry.is_downgraded(&key) {
                    spawn_periodic_logs(
                        logs_cfg.auto_periodic_or_default(),
                        data_sink.clone(),
                        collector_registry,
                    )
                } else if let Some(data_sink) = data_sink.clone() {
                    let auto_cfg = logs_cfg.auto.clone().unwrap_or_default();
                    let registry = ctx.log_downgrade_registry.clone();
                    let endpoint_key: std::borrow::Cow<'static, str> = key.clone().into();
                    let endpoint_rack_id = endpoint.rack_id.clone();
                    let mut budget = AutoFailureBudget::new(auto_cfg, Instant::now());

                    Some(Collector::start_streaming::<SseLogCollector<BmcClient>, _>(
                        endpoint_arc.clone(),
                        bmc.clone(),
                        SseLogCollectorConfig {
                            include_diagnostics: ctx.logs_include_diagnostics,
                            request_concurrency: ctx.bmc_request_concurrency,
                            gpu_inventory: sse_gpu_inventory,
                        },
                        data_sink,
                        StreamingCollectorStartContext {
                            backoff_config: sse_backoff_config(),
                            collector_registry,
                        },
                        move |result| match result {
                            Ok(()) => {
                                budget.reset_transient(Instant::now());
                                true
                            }
                            Err(e) => {
                                match budget.record(FailureKind::classify(e), Instant::now()) {
                                    BudgetDecision::Continue => true,
                                    BudgetDecision::Downgrade(reason) => {
                                        registry.mark_downgraded(
                                            endpoint_key.clone(),
                                            endpoint_rack_id.as_ref(),
                                            reason,
                                        );

                                        false
                                    }
                                }
                            }
                        },
                    ))
                } else {
                    tracing::warn!(
                        endpoint_key = %key,
                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                        "auto-mode SSE log collector requires a data sink, skipping"
                    );

                    None
                }
            }
        };

        match result {
            Some(Ok(collector)) => {
                ctx.collectors
                    .insert(CollectorKind::Logs, key.clone().into(), collector);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    mode = ?logs_cfg.mode,
                    log_collector_count = ctx.collectors.len(CollectorKind::Logs),
                    "Started logs collection for BMC endpoint"
                );
            }
            Some(Err(error)) => {
                tracing::error!(
                    ?error,
                    mode = ?logs_cfg.mode,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start logs collector"
                );
            }
            None => {}
        }
    }

    if let Configurable::Enabled(firmware_cfg) = &ctx.firmware_config
        && !ctx.collectors.contains(CollectorKind::Firmware, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("firmware_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<FirmwareCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            FirmwareCollectorConfig {
                data_sink: data_sink.clone(),
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: firmware_cfg.firmware_refresh_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(collector) => {
                ctx.collectors
                    .insert(CollectorKind::Firmware, key.clone().into(), collector);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    firmware_collector_count = ctx.collectors.len(CollectorKind::Firmware),
                    "Started firmware collection for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start firmware collector"
                )
            }
        }
    }

    if let Configurable::Enabled(gpu_cfg) = &ctx.gpu_inventory_config
        && let Some(api_client) = &ctx.api_client
        // GPU inventory validation only applies to machine endpoints (it needs a
        // machine id + assigned SKU). Skip switch / power-shelf endpoints so we
        // don't emit machine-target reports that get dropped for lack of context.
        && matches!(endpoint.metadata, Some(EndpointMetadata::Machine(_)))
        && !ctx.collectors.contains(CollectorKind::GpuInventory, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("gpu_inventory_{key}"), metrics_prefix)?,
        );
        // Reuse the entity-discovery collector's inventory for this endpoint so GPU
        // counting shares its Redfish enumeration instead of re-querying the BMC.
        let shared = ctx.collectors.inventory_for(&key);
        match Collector::start::<GpuInventoryCollector<BmcClient>>(
            endpoint_arc.clone(),
            bmc.clone(),
            GpuInventoryCollectorConfig {
                data_sink: data_sink.clone(),
                api_client: api_client.clone(),
                shared,
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: gpu_cfg.interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(monitor) => {
                ctx.collectors
                    .insert(CollectorKind::GpuInventory, key.clone().into(), monitor);
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start GPU inventory collector"
                );
            }
        }
    }

    if let Configurable::Enabled(leak_detector_cfg) = &ctx.leak_detector_config
        && !ctx.collectors.contains(CollectorKind::LeakDetector, &key)
    {
        let collector_registry =
            Arc::new(ctx.metrics_manager.create_collector_registry(
                format!("leak_detector_collector_{key}"),
                metrics_prefix,
            )?);
        match Collector::start::<LeakDetectorCollector<BmcClient>>(
            endpoint_arc,
            bmc,
            LeakDetectorCollectorConfig {
                data_sink: data_sink.clone(),
                state_refresh_interval: leak_detector_cfg.state_refresh_interval,
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: leak_detector_cfg.poll_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(collector) => {
                ctx.collectors
                    .insert(CollectorKind::LeakDetector, key.clone().into(), collector);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    leak_detector_collector_count =
                        ctx.collectors.len(CollectorKind::LeakDetector),
                    "Started leak detector collection for BMC endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start leak detector collector"
                )
            }
        }
    }

    Ok(())
}

fn start_nmxc_collector(
    ctx: &DiscoveryLoopContext,
    endpoint: &Arc<BmcEndpoint>,
    bmc: &Arc<BmcClient>,
    config: &NmxcCollectorOptions,
    data_sink: Arc<dyn DataSink>,
    collector_registry: Arc<CollectorRegistry>,
) -> Result<Collector, HealthError> {
    let start_context = StreamingCollectorStartContext {
        backoff_config: BackoffConfig {
            initial: config.initial_backoff,
            max: config.max_backoff,
        },
        collector_registry,
    };

    if let Some(schema_override) = &ctx.nmxc_schema_override {
        Collector::start_streaming::<NmxcSchemaOverrideCollector, _>(
            endpoint.clone(),
            bmc.clone(),
            NmxcSchemaOverrideCollectorConfig {
                nmxc_config: config.clone(),
                tls_config: ctx.tls_config.clone(),
                schema_override: schema_override.clone(),
            },
            data_sink,
            start_context,
            |_| true,
        )
    } else {
        Collector::start_streaming::<NmxcCollector, _>(
            endpoint.clone(),
            bmc.clone(),
            NmxcCollectorConfig {
                nmxc_config: config.clone(),
                tls_config: ctx.tls_config.clone(),
            },
            data_sink,
            start_context,
            |_| true,
        )
    }
}

fn spawn_switch_host_collectors(
    ctx: &mut DiscoveryLoopContext,
    endpoint: &Arc<BmcEndpoint>,
    data_sink: Option<Arc<dyn DataSink>>,
    metrics_prefix: &str,
) -> Result<(), HealthError> {
    let key = endpoint.key();
    let endpoint_arc = endpoint.clone();
    let bmc = endpoint.bmc().clone();
    let eligibility = collector_eligibility(ctx, endpoint, data_sink.is_some());

    if eligibility.nmxt
        && let Configurable::Enabled(nmxt_cfg) = &ctx.nmxt_config
        && !ctx.collectors.contains(CollectorKind::Nmxt, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("nmxt_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<NmxtCollector>(
            endpoint_arc.clone(),
            bmc.clone(),
            NmxtCollectorConfig {
                nmxt_config: nmxt_cfg.clone(),
                data_sink: data_sink.clone(),
                tls_http_client_provider: ctx.tls_http_client_provider.clone(),
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: nmxt_cfg.scrape_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(handle) => {
                ctx.collectors
                    .insert(CollectorKind::Nmxt, key.clone().into(), handle);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    nmxt_collector_count = ctx.collectors.len(CollectorKind::Nmxt),
                    "Started NMX-T collection for switch host endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start NMX-T collector for switch host"
                )
            }
        }
    }

    if let Configurable::Enabled(nmxc_cfg) = &ctx.nmxc_config
        && !ctx.collectors.contains(CollectorKind::Nmxc, &key)
        && switch_supports_nmxc_subscription(endpoint)
    {
        if !ctx.log_event_sink_enabled && !ctx.nvlink_domain_health_report_sink_enabled {
            tracing::warn!(
                endpoint_key = %key,
                rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                "NMX-C streaming collector requires an enabled log or NVLink domain health report sink, skipping"
            );
        } else if let Some(data_sink) = data_sink.clone() {
            let collector_registry = Arc::new(
                ctx.metrics_manager
                    .create_collector_registry(format!("nmxc_collector_{key}"), metrics_prefix)?,
            );

            match start_nmxc_collector(
                ctx,
                &endpoint_arc,
                &bmc,
                nmxc_cfg,
                data_sink,
                collector_registry,
            ) {
                Ok(handle) => {
                    ctx.collectors
                        .insert(CollectorKind::Nmxc, key.clone().into(), handle);

                    tracing::info!(
                        endpoint_key = %key,
                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                        nmxc_collector_count = ctx.collectors.len(CollectorKind::Nmxc),
                        "Started NMX-C streaming collection for switch endpoint"
                    );
                }

                Err(error) => {
                    tracing::error!(
                        ?error,
                        endpoint_key = %key,
                        rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                        "Could not start NMX-C collector for switch"
                    );
                }
            }
        } else {
            tracing::warn!(
                endpoint_key = %key,
                rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                "NMX-C streaming collector requires a data sink, skipping"
            );
        }
    }

    if eligibility.nvue_rest
        && let Configurable::Enabled(nvue_cfg) = &ctx.nvue_config
        && let Configurable::Enabled(rest_cfg) = &nvue_cfg.rest
        && !ctx.collectors.contains(CollectorKind::NvueRest, &key)
    {
        let credential_provider = bmc.credential_provider();
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("nvue_rest_collector_{key}"), metrics_prefix)?,
        );
        match Collector::start::<NvueRestCollector>(
            endpoint_arc,
            bmc.clone(),
            NvueRestCollectorConfig {
                rest_config: rest_cfg.clone(),
                data_sink: data_sink.clone(),
                log_event_sink_enabled: ctx.log_event_sink_enabled,
                credential_provider,
                tls_http_client_provider: ctx.tls_http_client_provider.clone(),
            },
            CollectorStartContext {
                limiter: ctx.limiter.clone(),
                iteration_interval: rest_cfg.poll_interval,
                collector_registry,
                metrics_manager: ctx.metrics_manager.clone(),
            },
        ) {
            Ok(handle) => {
                ctx.collectors
                    .insert(CollectorKind::NvueRest, key.clone().into(), handle);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    nvue_rest_collector_count = ctx.collectors.len(CollectorKind::NvueRest),
                    "Started NVUE REST collection for switch host endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start NVUE REST collector for switch host"
                )
            }
        }
    }

    if eligibility.nvue_gnmi
        && let Configurable::Enabled(nvue_cfg) = &ctx.nvue_config
        && let Configurable::Enabled(gnmi_cfg) = &nvue_cfg.gnmi
        && !ctx.collectors.contains(CollectorKind::NvueGnmi, &key)
    {
        let collector_registry = Arc::new(
            ctx.metrics_manager
                .create_collector_registry(format!("nvue_gnmi_collector_{key}"), metrics_prefix)?,
        );
        let credential_provider = bmc.credential_provider();
        match spawn_gnmi_collector(
            endpoint,
            gnmi_cfg,
            credential_provider,
            collector_registry,
            data_sink.clone(),
            ctx.tls_config.clone(),
        ) {
            Ok(handle) => {
                ctx.collectors
                    .insert(CollectorKind::NvueGnmi, key.clone().into(), handle);
                tracing::info!(
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    nvue_gnmi_collector_count = ctx.collectors.len(CollectorKind::NvueGnmi),
                    "Started NVUE gNMI streaming collection for switch endpoint"
                );
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    endpoint_key = %key,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Could not start NVUE gNMI collector for switch"
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;

    use super::*;
    use crate::collectors::DowngradeReason;
    use crate::config::{
        AutoModeConfig, CarbideApiConnectionConfig, Config, Configurable, LogsCollectorConfig,
        NvLinkDomainHealthReportSinkConfig, NvueCollectorConfig, NvueGnmiConfig, PeriodicLogConfig,
        TracingSinkConfig,
    };
    use crate::endpoint::test_support::endpoint_with_creds;
    use crate::endpoint::{
        BmcAddr, BmcCredentials, EndpointMetadata, MachineData, SharedSystemUuid, SwitchData,
        SwitchEndpointRole,
    };
    use crate::limiter::{NoopLimiter, RateLimiter};
    use crate::metrics::MetricsManager;
    use crate::sink::{CollectorEvent, EventContext};

    struct NoopSink;

    impl DataSink for NoopSink {
        fn sink_type(&self) -> &'static str {
            "noop"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            _event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            Ok(())
        }
    }

    fn context_with_config(config: Config, metrics_name: &str) -> DiscoveryLoopContext {
        let limiter: Arc<dyn RateLimiter> = Arc::new(NoopLimiter);
        let metrics_manager =
            Arc::new(MetricsManager::new(metrics_name).expect("metrics manager should initialize"));
        DiscoveryLoopContext::new(limiter, metrics_manager, Arc::new(config))
            .expect("context should initialize")
    }

    fn test_endpoint(
        ip: Ipv4Addr,
        mac: &str,
        metadata: Option<EndpointMetadata>,
    ) -> Arc<BmcEndpoint> {
        Arc::new(endpoint_with_creds(
            BmcAddr {
                ip: IpAddr::V4(ip),
                port: Some(443),
                mac: MacAddress::from_str(mac).expect("valid mac address"),
            },
            BmcCredentials::UsernamePassword {
                username: "user".to_string(),
                password: Some("pass".to_string()),
            },
            metadata,
            None,
        ))
    }

    /// Builds switch metadata using primary state as the default NMX-C desired-state flag.
    fn switch_metadata_with_role(
        endpoint_role: SwitchEndpointRole,
        is_primary: bool,
        nmxt_enabled: bool,
        serial: &str,
    ) -> EndpointMetadata {
        switch_metadata_with_nmxc(endpoint_role, is_primary, is_primary, nmxt_enabled, serial)
    }

    /// Builds switch metadata with separate NMX-C and NMX-T desired-state flags.
    fn switch_metadata_with_nmxc(
        endpoint_role: SwitchEndpointRole,
        is_primary: bool,
        nmxc_enabled: bool,
        nmxt_enabled: bool,
        serial: &str,
    ) -> EndpointMetadata {
        EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: serial.to_string(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role,
            is_primary,
            nmxc_enabled,
            nmxt_enabled,
        })
    }

    fn switch_metadata() -> EndpointMetadata {
        switch_metadata_with_role(SwitchEndpointRole::Host, false, false, "switch-serial-1")
    }

    fn machine_metadata() -> EndpointMetadata {
        EndpointMetadata::Machine(MachineData {
            machine_id: Some(
                "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                    .parse()
                    .expect("valid machine id"),
            ),
            machine_serial: None,
            system_uuid: SharedSystemUuid::default(),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            driver_version: None,
        })
    }

    /// Builds config with only the NMX-C collector enabled.
    fn nmxc_only_config(log_event_sink_enabled: bool) -> Config {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Enabled(Default::default());
        config.collectors.nvue = Configurable::Disabled;

        if log_event_sink_enabled {
            config.sinks.tracing = Configurable::Enabled(TracingSinkConfig::default());
        }

        config
    }

    #[test]
    fn test_logs_state_file_path_replaces_endpoint_id() {
        let path = logs_state_file_path("/tmp/logs_{machine_id}.json", "endpoint-42");
        assert_eq!(path, PathBuf::from("/tmp/logs_endpoint-42.json"));
    }

    #[test]
    fn periodic_logs_runtime_config_preserves_excluded_services() {
        check_values(
            [
                Check {
                    scenario: "custom exclusions",
                    input: vec!["Journal".to_string(), "Dump".to_string()],
                    expect: vec!["Journal".to_string(), "Dump".to_string()],
                },
                Check {
                    scenario: "explicit empty exclusions",
                    input: vec![],
                    expect: vec![],
                },
            ],
            |exclude_services| {
                build_periodic_logs_collector_config(
                    &PeriodicLogConfig {
                        exclude_services,
                        ..PeriodicLogConfig::default()
                    },
                    PathBuf::from("/tmp/logs_endpoint-42.json"),
                    None,
                    false,
                )
                .exclude_services
            },
        );
    }

    #[tokio::test]
    async fn test_endpoint_log_identity_falls_back_to_mac_without_metadata() {
        let endpoint = test_endpoint(Ipv4Addr::new(10, 0, 0, 1), "aa:bb:cc:dd:ee:ff", None);

        assert_eq!(endpoint.log_identity().as_ref(), "AA:BB:CC:DD:EE:FF");
    }

    #[tokio::test]
    async fn test_endpoint_log_identity_uses_switch_serial_when_available() {
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 2),
            "11:22:33:44:55:66",
            Some(switch_metadata()),
        );

        assert_eq!(endpoint.log_identity().as_ref(), "switch-serial-1");
    }

    #[tokio::test]
    async fn test_switch_endpoint_does_not_start_generic_redfish_collectors() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Enabled(Default::default());
        config.collectors.logs = Configurable::Enabled(Default::default());
        config.collectors.firmware = Configurable::Enabled(Default::default());
        config.collectors.leak_detector = Configurable::Enabled(Default::default());
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_switch_generic_redfish_gate");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 6),
            "55:66:77:88:99:aa",
            Some(switch_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_generic_redfish_gate",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Logs), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Firmware), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::LeakDetector), 0);
    }

    #[tokio::test]
    async fn test_switch_bmc_endpoint_starts_redfish_but_not_switch_host_collectors() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Enabled(Default::default());
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Enabled(Default::default());

        config.collectors.nmxc = Configurable::Enabled(Default::default());

        config.collectors.nvue = Configurable::Enabled(NvueCollectorConfig {
            rest: Configurable::Enabled(Default::default()),
            gnmi: Configurable::Enabled(NvueGnmiConfig::default()),
        });

        let mut ctx = context_with_config(config, "test_switch_bmc_redfish_only");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 8),
            "55:66:77:88:99:bb",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Bmc,
                true,
                false,
                "switch-bmc",
            )),
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test_switch_bmc_redfish_only")
            .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxt), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueRest), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueGnmi), 0);
    }

    #[tokio::test]
    async fn test_switch_host_primary_starts_nmxt_and_nvue_collectors_when_globally_enabled() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Enabled(Default::default());
        config.collectors.nmxc = Configurable::Disabled;

        config.collectors.nvue = Configurable::Enabled(NvueCollectorConfig {
            rest: Configurable::Enabled(Default::default()),
            gnmi: Configurable::Enabled(NvueGnmiConfig::default()),
        });

        let mut ctx = context_with_config(config, "test_switch_host_nmxt_nvue_enabled");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 9),
            "55:66:77:88:99:cc",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                true,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test")
            .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxt), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueRest), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueGnmi), 1);
    }

    #[tokio::test]
    /// Verifies NMX-C collection starts for a primary switch host when globally enabled.
    async fn test_switch_host_starts_nmxc_collector_when_enabled() {
        let mut ctx = context_with_config(nmxc_only_config(true), "test_switch_host_nmxc_enabled");

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 12),
            "55:66:77:88:99:ef",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                false,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_host_nmxc_enabled",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 1);
    }

    #[tokio::test]
    async fn test_switch_host_starts_nmxc_for_domain_health_report_sink() {
        let mut config = nmxc_only_config(false);
        config.sinks.nvlink_domain_health_report =
            Configurable::Enabled(NvLinkDomainHealthReportSinkConfig::default());

        let mut ctx = context_with_config(config, "test_switch_host_nmxc_domain_health");

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 17),
            "55:66:77:88:99:f4",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                false,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_host_nmxc_domain_health",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 1);
    }

    #[tokio::test]
    /// Verifies NMX-C collection does not start on secondary switch hosts.
    async fn test_switch_host_skips_nmxc_collector_for_secondary_switch() {
        let mut ctx =
            context_with_config(nmxc_only_config(true), "test_switch_host_nmxc_secondary");

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 14),
            "55:66:77:88:99:f1",
            Some(switch_metadata_with_nmxc(
                SwitchEndpointRole::Host,
                false,
                true,
                false,
                "switch-host-secondary",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_host_nmxc_secondary",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
    }

    #[tokio::test]
    /// Verifies NMX-C collection honors the per-switch desired-state flag.
    async fn test_switch_host_skips_nmxc_collector_when_desired_config_disabled() {
        let mut ctx = context_with_config(
            nmxc_only_config(true),
            "test_switch_host_nmxc_config_disabled",
        );

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 15),
            "55:66:77:88:99:f2",
            Some(switch_metadata_with_nmxc(
                SwitchEndpointRole::Host,
                true,
                false,
                false,
                "switch-host-nmxc-disabled",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_host_nmxc_config_disabled",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
    }

    #[tokio::test]
    async fn test_switch_host_skips_nmxc_without_data_sink() {
        let mut ctx = context_with_config(
            nmxc_only_config(true),
            "test_switch_host_nmxc_requires_sink",
        );

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 13),
            "55:66:77:88:99:f0",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                false,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            None,
            "test_switch_host_nmxc_requires_sink",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
    }

    #[tokio::test]
    /// Verifies NMX-C collection skips Prometheus-only or health-report-only sink configs.
    async fn test_switch_host_skips_nmxc_without_log_event_sink() {
        let mut ctx = context_with_config(
            nmxc_only_config(false),
            "test_switch_host_nmxc_requires_log_sink",
        );

        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 16),
            "55:66:77:88:99:f3",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                false,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_switch_host_nmxc_requires_log_sink",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
    }

    #[tokio::test]
    async fn test_switch_host_policy_gates_nmxt_but_not_nvue_rest() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Enabled(Default::default());
        config.collectors.nmxc = Configurable::Disabled;
        config.collectors.nvue = Configurable::Enabled(Default::default());

        let mut ctx = context_with_config(config, "test_switch_host_nmxt_endpoint_disabled");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 10),
            "55:66:77:88:99:dd",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                false,
                false,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test")
            .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxt), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueRest), 1);
    }

    #[tokio::test]
    async fn test_switch_host_does_not_start_host_collectors_when_globally_disabled() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_switch_host_collectors_global_disabled");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 11),
            "55:66:77:88:99:ee",
            Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                true,
                "switch-host",
            )),
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test")
            .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxt), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueRest), 0);
    }

    #[tokio::test]
    async fn test_machine_endpoint_still_starts_sse_logs_collector() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Enabled(Default::default());
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_machine_sse_logs_collector");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 7),
            "66:77:88:99:aa:bb",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_machine_sse_logs_collector",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Logs), 1);
    }

    #[tokio::test]
    async fn test_nvue_collectors_still_spawn_when_credentials_currently_unavailable() {
        use crate::bmc::{BmcClient, BoxFuture, CredentialProvider};
        use crate::endpoint::test_support::reqwest;

        struct FailingProvider;
        impl CredentialProvider for FailingProvider {
            fn fetch_credentials<'a>(
                &'a self,
                _endpoint: &'a BmcAddr,
            ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
                Box::pin(async move {
                    Err(HealthError::GenericError(
                        "simulated credential provider failure".to_string(),
                    ))
                })
            }
        }

        let addr = BmcAddr {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 99)),
            port: Some(443),
            mac: MacAddress::from_str("99:88:77:66:55:44").expect("valid mac"),
        };
        let bmc = Arc::new(
            BmcClient::new(
                reqwest(),
                addr.clone(),
                Arc::new(FailingProvider),
                None,
                10,
                std::num::NonZeroUsize::MIN,
                None,
            )
            .expect("constructor succeeds"),
        );
        let endpoint = Arc::new(BmcEndpoint {
            addr,
            metadata: Some(switch_metadata_with_role(
                SwitchEndpointRole::Host,
                true,
                true,
                "failing-switch-host",
            )),
            rack_id: None,
            labels: Default::default(),
            bmc,
        });

        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Enabled(Default::default());
        config.collectors.nmxc = Configurable::Disabled;

        config.collectors.nvue = Configurable::Enabled(NvueCollectorConfig {
            rest: Configurable::Enabled(Default::default()),
            gnmi: Configurable::Enabled(NvueGnmiConfig::default()),
        });

        let mut ctx = context_with_config(config, "test_nvue_spawn_despite_cred_failure");

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            None,
            "test_nvue_spawn_despite_cred_failure",
        )
        .expect("spawn returns Ok even when the credential provider is failing");

        assert_eq!(
            ctx.collectors.len(CollectorKind::NvueRest),
            1,
            "NVUE REST must still spawn — credential fetch is now per-iteration, \
             not part of the spawn contract"
        );
        assert_eq!(
            ctx.collectors.len(CollectorKind::NvueGnmi),
            1,
            "NVUE gNMI must still spawn — credential fetch is per-stream connection, \
             not part of the spawn contract"
        );
        assert_eq!(
            ctx.collectors.len(CollectorKind::Nmxt),
            1,
            "NMX-T must still start — it doesn't depend on BMC credentials"
        );

        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
    }

    #[tokio::test]
    async fn test_spawn_is_idempotent_when_collectors_are_disabled() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nmxc = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_disabled_collectors");
        let endpoint = test_endpoint(Ipv4Addr::new(10, 0, 0, 1), "aa:bb:cc:dd:ee:ff", None);

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test")
            .expect("first spawn should succeed");
        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test")
            .expect("second spawn should also succeed without duplicate registry errors");

        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Logs), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Firmware), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::LeakDetector), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxt), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Nmxc), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueRest), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::NvueGnmi), 0)
    }

    #[tokio::test]
    async fn machine_endpoint_with_sensors_starts_discovery_and_sensor_only() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Enabled(Default::default());
        config.collectors.metrics = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_discovery_with_sensors");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 20),
            "aa:bb:cc:00:00:20",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_discovery_with_sensors",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Discovery), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Metrics), 0);
    }

    #[tokio::test]
    async fn metrics_only_still_starts_discovery() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.metrics = Configurable::Enabled(Default::default());
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_discovery_with_metrics_only");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 21),
            "aa:bb:cc:00:00:21",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_discovery_with_metrics_only",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Discovery), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Metrics), 1);
    }

    #[tokio::test]
    async fn gpu_inventory_only_starts_discovery() {
        // GpuInventoryCollector reads GPU counts from the shared entity inventory,
        // so enabling it must start entity discovery even with sensors/metrics off,
        // otherwise the collector would read an empty snapshot forever.
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.metrics = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;
        // GPU inventory uses the NICo API client to resolve the machine SKU.
        config.endpoint_sources.carbide_api =
            Configurable::Enabled(CarbideApiConnectionConfig::default());
        config.collectors.gpu_inventory = Configurable::Enabled(Default::default());

        let mut ctx = context_with_config(config, "test_discovery_with_gpu_inventory");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 23),
            "aa:bb:cc:00:00:23",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_discovery_with_gpu_inventory",
        )
        .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Discovery), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::GpuInventory), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Metrics), 0);
    }

    #[tokio::test]
    async fn no_discovery_when_both_readers_disabled() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.metrics = Configurable::Disabled;
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_no_discovery");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 22),
            "aa:bb:cc:00:00:22",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, Some(Arc::new(NoopSink)), "test")
            .expect("spawn should succeed");

        assert_eq!(ctx.collectors.len(CollectorKind::Discovery), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 0);
        assert_eq!(ctx.collectors.len(CollectorKind::Metrics), 0);
    }

    #[tokio::test]
    async fn discovery_sensor_and_metrics_spawn_is_idempotent() {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Enabled(Default::default());
        config.collectors.metrics = Configurable::Enabled(Default::default());
        config.collectors.logs = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;

        let mut ctx = context_with_config(config, "test_discovery_idempotent");
        let endpoint = test_endpoint(
            Ipv4Addr::new(10, 0, 0, 23),
            "aa:bb:cc:00:00:23",
            Some(machine_metadata()),
        );

        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_discovery_idempotent",
        )
        .expect("first spawn should succeed");
        spawn_collectors_for_endpoint(
            &mut ctx,
            &endpoint,
            Some(Arc::new(NoopSink)),
            "test_discovery_idempotent",
        )
        .expect("second spawn should be a no-op without duplicate registry errors");

        assert_eq!(ctx.collectors.len(CollectorKind::Discovery), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Sensor), 1);
        assert_eq!(ctx.collectors.len(CollectorKind::Metrics), 1);
    }

    fn auto_mode_config() -> Config {
        let mut config = Config::default();
        config.collectors.sensors = Configurable::Disabled;
        config.collectors.firmware = Configurable::Disabled;
        config.collectors.leak_detector = Configurable::Disabled;
        config.collectors.nmxt = Configurable::Disabled;
        config.collectors.nvue = Configurable::Disabled;
        config.collectors.logs = Configurable::Enabled(LogsCollectorConfig {
            mode: LogCollectionMode::Auto,
            sse: None,
            periodic: Some(PeriodicLogConfig::default()),
            auto: Some(AutoModeConfig::default()),
        });
        config
    }

    #[tokio::test]
    async fn test_auto_mode_with_downgraded_endpoint_spawns_periodic() {
        let limiter: Arc<dyn RateLimiter> = Arc::new(NoopLimiter);
        let metrics_manager = Arc::new(
            MetricsManager::new("test_auto_downgraded").expect("metrics manager should initialize"),
        );
        let mut ctx =
            DiscoveryLoopContext::new(limiter, metrics_manager, Arc::new(auto_mode_config()))
                .expect("context should initialize");

        let endpoint = test_endpoint(Ipv4Addr::new(10, 0, 0, 1), "aa:bb:cc:dd:ee:01", None);

        ctx.log_downgrade_registry.mark_downgraded(
            endpoint.key().into(),
            endpoint.rack_id.as_ref(),
            DowngradeReason::SseNotAvailable,
        );

        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test_auto_downgraded")
            .expect("spawn should succeed for downgraded auto endpoint");

        assert_eq!(ctx.collectors.len(CollectorKind::Logs), 1);
    }

    #[tokio::test]
    async fn test_auto_mode_without_downgrade_and_no_data_sink_skips_spawn() {
        let limiter: Arc<dyn RateLimiter> = Arc::new(NoopLimiter);
        let metrics_manager = Arc::new(
            MetricsManager::new("test_auto_no_sink").expect("metrics manager should initialize"),
        );
        let mut ctx =
            DiscoveryLoopContext::new(limiter, metrics_manager, Arc::new(auto_mode_config()))
                .expect("context should initialize");

        let endpoint = test_endpoint(Ipv4Addr::new(10, 0, 0, 2), "aa:bb:cc:dd:ee:02", None);
        spawn_collectors_for_endpoint(&mut ctx, &endpoint, None, "test_auto_no_sink")
            .expect("spawn should succeed (gracefully skip) without data sink");

        assert_eq!(ctx.collectors.len(CollectorKind::Logs), 0);
        assert!(!ctx.log_downgrade_registry.is_downgraded(&endpoint.key()));
    }
}
