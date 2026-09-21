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

use std::sync::Arc;

use async_trait::async_trait;
use nv_redfish::bmc_http::reqwest::{
    BmcError, Client as ReqwestClient, ClientParams as ReqwestClientParams,
};
use prometheus::{Gauge, GaugeVec, Opts};

pub mod api_client;
pub mod bmc;
pub mod collectors;
pub mod config;
pub mod discovery;
pub mod endpoint;
pub mod limiter;
pub mod metrics;
pub mod otlp;
pub mod processor;
pub mod sharding;
pub mod sink;

mod tls;

pub use config::Config;
pub use discovery::{DiscoveryIterationStats, DiscoveryLoopContext};

use crate::api_client::{ApiClientWrapper, ApiEndpointSource};
use crate::collectors::BackoffConfig;
use crate::config::Configurable;
use crate::endpoint::{
    ClusterEndpointSource, CompositeEndpointSource, EndpointSource, StaticEndpointSource,
};
use crate::limiter::{BucketLimiter, NoopLimiter, RateLimiter};
use crate::metrics::{BmcLatencyMetrics, MetricsManager, run_metrics_server};
use crate::processor::{
    BmcIntrusionEventProcessor, EventProcessingPipeline, EventProcessor, HealthReportProcessor,
    LeakEventProcessor, NmxcDomainStateProcessor, RackLeakProcessor,
};
use crate::sharding::ShardManager;
use crate::sink::event_mapper::{OpenBmcEventMapper, RedfishEventMapper};
use crate::sink::{
    CompositeDataSink, DataSink, HealthReportSink, LogFileSink, NvLinkDomainHealthReportSink,
    OtlpSink, PowerShelfHealthReportSink, PrometheusSink, RackHealthReportSink,
    SwitchHealthReportSink, TracingSink,
};

#[derive(thiserror::Error, Debug)]
pub enum HealthError {
    #[error("unable to connect to carbide API: {0}")]
    ApiConnectFailed(String),

    #[error("the API call to the carbide API server returned {0}")]
    ApiInvocationError(tonic::Status),

    #[error("generic error: {0}")]
    GenericError(String),

    #[error("logger error: {0}")]
    LoggerError(String),

    #[error("error while handling json: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("tokio task join error {0}")]
    TokioJoinError(#[from] tokio::task::JoinError),

    #[error("prometheus error {0}")]
    PrometheusError(#[from] prometheus::Error),

    /// Nonempty endpoint-keyed spawn failures in discovery processing order.
    ///
    /// Returned after all spawn attempts and reachability reconciliation.
    #[error("collector spawning failed for {} endpoints", .0.len())]
    CollectorSpawnErrors(Vec<(String, HealthError)>),

    #[error("BMC error: {0}")]
    BmcError(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error("HTTP(S) error: {0}")]
    HttpError(String),

    #[error("redfish SSE not available: {0}")]
    SseNotAvailable(String),

    #[error("gNMI error: {0}")]
    GnmiError(String),

    #[error("gNMI RPC failed: {0}")]
    GnmiStatus(tonic::Status),

    #[error("NMX-C RPC failed: {0}")]
    NmxcStatus(tonic::Status),

    /// Descriptor-driven NMX-C configuration is invalid.
    #[error("NMX-C schema override configuration failed: {0}")]
    NmxcSchemaOverride(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Client TLS material could not be read, validated, or applied.
    #[error("TLS profile error: {0}")]
    Tls(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<String> for HealthError {
    fn from(err: String) -> Self {
        HealthError::GenericError(err)
    }
}

impl From<BmcError> for HealthError {
    fn from(err: BmcError) -> Self {
        HealthError::BmcError(Box::new(err))
    }
}

impl From<tls::TlsError> for HealthError {
    fn from(err: tls::TlsError) -> Self {
        HealthError::Tls(Box::new(err))
    }
}

impl<B: nv_redfish::core::Bmc + 'static> From<nv_redfish::Error<B>> for HealthError {
    fn from(err: nv_redfish::Error<B>) -> Self {
        HealthError::BmcError(Box::new(err))
    }
}

struct EndpointWiring {
    source: Arc<dyn EndpointSource>,
}

fn build_endpoint_wiring(
    config: &Config,
    bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
) -> Result<EndpointWiring, HealthError> {
    let reqwest = ReqwestClient::with_params(ReqwestClientParams::new().accept_invalid_certs(true))
        .map_err(BmcError::ReqwestError)?;
    let mut sources: Vec<Arc<dyn EndpointSource>> = Vec::new();

    for endpoint_configs in [
        config.endpoint_sources.static_bmc_endpoints.as_slice(),
        config
            .endpoint_sources
            .static_switch_host_endpoints
            .as_slice(),
    ] {
        if endpoint_configs.is_empty() {
            continue;
        }

        let static_source = StaticEndpointSource::from_config_with_request_concurrency(
            endpoint_configs,
            &reqwest,
            config.bmc_proxy_url.as_ref(),
            config.cache_size,
            config.bmc_request_concurrency,
            bmc_latency_metrics.clone(),
        );
        sources.push(Arc::new(static_source));
    }

    if let Configurable::Enabled(ref source_cfg) = config.endpoint_sources.carbide_api {
        let api_client = Arc::new(ApiClientWrapper::new(
            source_cfg.root_ca.clone(),
            source_cfg.client_cert.clone(),
            source_cfg.client_key.clone(),
            &source_cfg.api_url,
        ));
        let endpoint_source = Arc::new(ApiEndpointSource::new_with_request_concurrency(
            api_client,
            reqwest.clone(),
            config.bmc_proxy_url.clone(),
            config.cache_size,
            config.bmc_request_concurrency,
            bmc_latency_metrics.clone(),
        ));
        sources.push(endpoint_source as Arc<dyn EndpointSource>);
    }

    if let Configurable::Enabled(ref source_cfg) = config.endpoint_sources.cluster {
        let cluster_source = ClusterEndpointSource::from_config_with_request_concurrency(
            source_cfg.clone(),
            &reqwest,
            config.bmc_proxy_url.as_ref(),
            config.cache_size,
            config.bmc_request_concurrency,
            bmc_latency_metrics,
        );
        sources.push(Arc::new(cluster_source));
    }

    let composite_source = CompositeEndpointSource::new(sources);

    if composite_source.is_empty() {
        return Err(HealthError::GenericError(
            "no endpoint sources configured".to_string(),
        ));
    }

    Ok(EndpointWiring {
        source: Arc::new(composite_source),
    })
}

fn build_data_sink(
    config: &Config,
    metrics_manager: Arc<MetricsManager>,
) -> Result<Option<Arc<dyn DataSink>>, HealthError> {
    let mut sinks: Vec<Arc<dyn DataSink>> = Vec::new();
    let mut processors: Vec<Arc<dyn EventProcessor>> = Vec::new();

    if let Configurable::Enabled(sink_cfg) = &config.sinks.tracing {
        sinks.push(Arc::new(TracingSink::new(sink_cfg)));
    }

    if let Configurable::Enabled(_) = &config.sinks.prometheus {
        sinks.push(Arc::new(PrometheusSink::new(
            metrics_manager.clone(),
            &config.metrics.prefix,
        )?));
    }

    if config.sinks.tracing.is_enabled()
        || config.sinks.health_report.is_enabled()
        || config.sinks.power_shelf_health_report.is_enabled()
        || config.sinks.switch_health_report.is_enabled()
        || config.processors.leak_detection.is_enabled()
    {
        processors.push(Arc::new(HealthReportProcessor::new()));
    }

    if config.sinks.health_report.is_enabled() {
        processors.push(Arc::new(BmcIntrusionEventProcessor::new()));
    }

    if config.sinks.nvlink_domain_health_report.is_enabled() {
        processors.push(Arc::new(NmxcDomainStateProcessor::new()));
    }

    if let Configurable::Enabled(ref leak_detection_cfg) = config.processors.leak_detection {
        processors.push(Arc::new(LeakEventProcessor::new(
            leak_detection_cfg.minimum_alerts_per_report,
        )));
    }

    if let Configurable::Enabled(ref rack_leak_cfg) = config.processors.rack_leak {
        processors.push(Arc::new(RackLeakProcessor::new(
            rack_leak_cfg.leaking_tray_threshold,
        )));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.log_file {
        sinks.push(Arc::new(
            LogFileSink::new(sink_cfg).map_err(HealthError::GenericError)?,
        ));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.health_report {
        sinks.push(Arc::new(HealthReportSink::new(sink_cfg)?));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.rack_health_report {
        sinks.push(Arc::new(RackHealthReportSink::new(sink_cfg)?));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.switch_health_report {
        sinks.push(Arc::new(SwitchHealthReportSink::new(sink_cfg)?));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.nvlink_domain_health_report {
        sinks.push(Arc::new(NvLinkDomainHealthReportSink::new(sink_cfg)?));
    }

    if let Configurable::Enabled(ref sink_cfg) = config.sinks.power_shelf_health_report {
        sinks.push(Arc::new(PowerShelfHealthReportSink::new(sink_cfg)?));
    }

    if let Configurable::Enabled(ref otlp_cfg) = config.sinks.otlp {
        let mapper: Arc<dyn RedfishEventMapper> = Arc::new(OpenBmcEventMapper);

        let otlp_sinks = OtlpSink::new_many(
            &otlp_cfg.targets,
            mapper,
            &metrics_manager,
            &config.metrics.prefix,
        )?;

        for sink in otlp_sinks {
            sinks.push(Arc::new(sink));
        }
    }

    if sinks.is_empty() {
        return Ok(None);
    }

    let composite_sink: Arc<dyn DataSink> =
        Arc::new(CompositeDataSink::new(sinks, metrics_manager.clone()));

    if processors.is_empty() {
        return Ok(Some(composite_sink));
    }

    Ok(Some(Arc::new(EventProcessingPipeline::new(
        processors,
        composite_sink,
        metrics_manager,
    ))))
}

/// The per-pass work of the endpoint discovery loop: one discovery iteration
/// plus the gauge updates its stats feed.
struct ServiceDiscoveryIteration {
    endpoint_source: Arc<dyn EndpointSource>,
    shard_manager: ShardManager,
    ctx: DiscoveryLoopContext,
    data_sink: Option<Arc<dyn DataSink>>,
    config: Arc<Config>,
    discovery_endpoints_gauge: GaugeVec,
    active_endpoints_gauge: Gauge,
}

#[async_trait]
impl discovery::DiscoveryIteration for ServiceDiscoveryIteration {
    async fn run_once(&mut self) -> Result<(), HealthError> {
        let stats = discovery::run_discovery_iteration(
            self.endpoint_source.clone(),
            &self.shard_manager,
            &mut self.ctx,
            self.data_sink.clone(),
            &self.config.metrics.prefix,
        )
        .await?;

        self.discovery_endpoints_gauge
            .get_metric_with_label_values(&["discovered"])?
            .set(stats.discovered_endpoints as f64);
        self.discovery_endpoints_gauge
            .get_metric_with_label_values(&["sharded"])?
            .set(stats.sharded_endpoints as f64);
        self.active_endpoints_gauge
            .set(stats.active_monitors as f64);

        Ok(())
    }
}

/// Runs the hardware-health service after validating configured TLS profiles.
///
/// Switch and OTLP TLS material is preflighted before listeners and background
/// tasks start, so invalid certificate configuration fails startup.
///
/// # Errors
///
/// Returns an error when startup validation or initialization fails, or when a
/// long-running service task exits with an error.
pub async fn run_service(config: Config) -> Result<(), HealthError> {
    if let Some(tls_config) = &config.tls.switch {
        tls::preflight(tls_config).await?;
    }

    if let Configurable::Enabled(otlp) = &config.sinks.otlp {
        for target in &otlp.targets {
            if let Some(tls_config) = &target.tls {
                tls::otlp_preflight(tls_config).await?;
            }
        }
    }

    let tls_config = config.tls.switch.clone();
    let nmxc_schema_override = discovery::load_nmxc_schema_override(&config)?;

    let metrics_endpoint = config.metrics_addr()?;
    let metrics_manager = Arc::new(MetricsManager::new(&config.metrics.prefix)?);
    let bmc_latency_metrics = if config.metrics.enable_bmc_latency_metrics {
        let attributes = config.metrics.bmc_latency_attributes();
        Some(Arc::new(BmcLatencyMetrics::new_with_attributes(
            metrics_manager.global_registry(),
            &config.metrics.prefix,
            &attributes,
        )?))
    } else {
        None
    };

    // Back the global OpenTelemetry meter with a prometheus registry and
    // merge that registry into this binary's /metrics exposition, so events
    // emitted through the instrumentation framework -- and the log-event
    // counts accumulating since startup -- are scrapeable alongside the raw
    // prometheus pipeline. The setup must outlive the servers: dropping it
    // would drop the meter provider and stop the exported values.
    let framework_metrics =
        metrics_endpoint::new_metrics_setup("carbide-hw-health", "carbide", true).map_err(|e| {
            HealthError::GenericError(format!("framework metrics setup failed: {e}"))
        })?;
    carbide_instrument::log_events::register(&framework_metrics.meter);
    metrics_manager.expose_framework_registry(framework_metrics.registry.clone());

    let join_listener = tokio::spawn(run_metrics_server(
        metrics_endpoint,
        metrics_manager.clone(),
    ));

    let registry = metrics_manager.global_registry();
    let active_endpoints_gauge = Gauge::new(
        format!(
            "{metrics_prefix}_active_endpoints",
            metrics_prefix = config.metrics.prefix
        ),
        "Number of active endpoints",
    )?;
    registry.register(Box::new(active_endpoints_gauge.clone()))?;

    let discovery_endpoints_gauge = GaugeVec::new(
        Opts::new(
            format!(
                "{metrics_prefix}_discovery_endpoints",
                metrics_prefix = config.metrics.prefix
            ),
            "Number of endpoints at each discovery stage",
        ),
        &["status"],
    )?;
    registry.register(Box::new(discovery_endpoints_gauge.clone()))?;

    let EndpointWiring {
        source: endpoint_source,
    } = build_endpoint_wiring(&config, bmc_latency_metrics)?;

    let data_sink = build_data_sink(&config, metrics_manager.clone())?;

    let config_arc = Arc::new(config);

    let join_discovery: tokio::task::JoinHandle<()> = tokio::spawn({
        let config = config_arc.clone();
        let shard_manager = ShardManager {
            shard: config.shard,
            shards_count: config.shards_count,
        };
        let limiter: Arc<dyn RateLimiter> =
            if let Configurable::Enabled(rate_limit) = &config.rate_limit {
                Arc::new(BucketLimiter::new(
                    rate_limit.bucket_burst,
                    rate_limit.bucket_replenish,
                    rate_limit.max_jitter,
                ))
            } else {
                Arc::new(NoopLimiter)
            };

        let ctx = DiscoveryLoopContext::new_with_tls_config(
            limiter,
            metrics_manager.clone(),
            config.clone(),
            tls_config,
            nmxc_schema_override,
        )?;

        let collector_transition_notify = ctx.collector_transition_notify.clone();

        let interval = config.endpoint_discovery_interval;
        let iteration = ServiceDiscoveryIteration {
            endpoint_source: endpoint_source.clone(),
            shard_manager,
            ctx,
            data_sink: data_sink.clone(),
            config,
            discovery_endpoints_gauge: discovery_endpoints_gauge.clone(),
            active_endpoints_gauge: active_endpoints_gauge.clone(),
        };

        discovery::run_discovery_loop(
            interval,
            BackoffConfig::default(),
            collector_transition_notify,
            iteration,
        )
    });

    tokio::select! {
        res = join_listener => {
            match res {
                Ok(Ok(_)) => {
                    tracing::info!("Metrics listener shutdown");
                }
                 Ok(Err(e)) => {
                    tracing::error!(error=?e, "Metrics listener failed");
                }
                Err(e) => {
                    tracing::error!(error=?e, "Metrics listener join error");
                }
            }
        }
        res = join_discovery => {
            match res {
                Ok(()) => {
                    tracing::error!("Discovery loop ended unexpectedly");
                }
                Err(e) => {
                    tracing::error!(error=?e, "Discovery loop join error");
                }
            }
        }
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};

    use super::*;
    use crate::endpoint::{EndpointMetadata, SwitchEndpointRole};

    #[tokio::test]
    async fn endpoint_wiring_loads_bmc_and_switch_host_static_endpoints() {
        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(
                r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "192.0.2.1"
port = 8443
mac = "00:11:22:33:44:55"
username = "admin"
switch = { serial = "switch-bmc", endpoint_role = "bmc" }

[[endpoint_sources.static_switch_host_endpoints]]
ip = "192.0.2.2"
port = 9443
mac = "00:11:22:33:44:66"
username = "admin"
switch = { serial = "switch-host" }
"#,
            ))
            .extract()
            .expect("static endpoint config should parse");

        config.validate().expect("test config should be valid");

        let wiring = build_endpoint_wiring(&config, None).expect("endpoint wiring should build");

        let endpoints = wiring
            .source
            .fetch_bmc_hosts()
            .await
            .expect("static endpoints should load");

        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].addr.port, Some(8443));
        assert_eq!(endpoints[1].addr.port, Some(9443));

        let roles = endpoints.iter().map(|endpoint| match &endpoint.metadata {
            Some(EndpointMetadata::Switch(switch)) => switch.endpoint_role,
            _ => panic!("static switch endpoint should retain switch metadata"),
        });

        assert_eq!(
            roles.collect::<Vec<_>>(),
            vec![SwitchEndpointRole::Bmc, SwitchEndpointRole::Host]
        );
    }
}
