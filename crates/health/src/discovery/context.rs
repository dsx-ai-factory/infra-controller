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

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use carbide_uuid::nvlink::NvLinkDomainId;
use prometheus::{Histogram, HistogramOpts};

use super::reachability::ReachabilitySpec;
use crate::HealthError;
use crate::api_client::ApiClientWrapper;
use crate::bmc::BmcClient;
use crate::collectors::{Collector, LogDowngradeRegistry, NmxcSchemaOverride, SharedInventory};
use crate::config::{
    Config, Configurable, DiscoveryConfig, FirmwareCollectorConfig as FirmwareCollectorOptions,
    GpuInventoryConfig, LeakDetectorCollectorConfig as LeakDetectorCollectorOptions,
    LogsCollectorConfig as LogsCollectorOptions, MetricsCollectorConfig as MetricsCollectorOptions,
    MtlsProfileConfig, NmxcCollectorConfig as NmxcCollectorOptions,
    NmxtCollectorConfig as NmxtCollectorOptions, NvueCollectorConfig as NvueCollectorOptions,
    ReachabilityCollectorConfig, SensorCollectorConfig as SensorCollectorOptions,
    TelemetryCollectorConfig as TelemetryCollectorOptions,
};
use crate::limiter::RateLimiter;
use crate::metrics::{MetricsManager, operation_duration_buckets_seconds};
use crate::tls::MtlsHttpClientProvider;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) enum CollectorKind {
    Discovery,
    Sensor,
    Metrics,
    Telemetry,
    Logs,
    Firmware,
    LeakDetector,
    Nmxt,
    Nmxc,
    NvueRest,
    NvueGnmi,
    GpuInventory,
    Manager,
    Reachability,
}

impl CollectorKind {
    pub(super) const ALL: [CollectorKind; 13] = [
        CollectorKind::Discovery,
        CollectorKind::Sensor,
        CollectorKind::Metrics,
        CollectorKind::Telemetry,
        CollectorKind::Logs,
        CollectorKind::Firmware,
        CollectorKind::LeakDetector,
        CollectorKind::Nmxt,
        CollectorKind::Nmxc,
        CollectorKind::NvueRest,
        CollectorKind::NvueGnmi,
        CollectorKind::GpuInventory,
        CollectorKind::Manager,
    ];
}

pub(super) struct CollectorState {
    discovery: HashMap<Cow<'static, str>, Collector>,
    sensors: HashMap<Cow<'static, str>, Collector>,
    metrics: HashMap<Cow<'static, str>, Collector>,
    telemetry: HashMap<Cow<'static, str>, Collector>,
    firmware: HashMap<Cow<'static, str>, Collector>,
    leak_detector: HashMap<Cow<'static, str>, Collector>,
    logs: HashMap<Cow<'static, str>, Collector>,
    nmxt: HashMap<Cow<'static, str>, Collector>,
    nmxc: HashMap<Cow<'static, str>, Collector>,
    nvue_rest: HashMap<Cow<'static, str>, Collector>,
    nvue_gnmi: HashMap<Cow<'static, str>, Collector>,
    gpu_inventory: HashMap<Cow<'static, str>, Collector>,
    manager: HashMap<Cow<'static, str>, Collector>,
    reachability: HashMap<Cow<'static, str>, Collector>,
    inventories: HashMap<Cow<'static, str>, SharedInventory<BmcClient>>,
    switch_domain_uuids: HashMap<Cow<'static, str>, Option<NvLinkDomainId>>,
    pub(super) reachability_specs: HashMap<Cow<'static, str>, ReachabilitySpec>,
}

impl CollectorState {
    fn new() -> Self {
        Self {
            discovery: HashMap::new(),
            sensors: HashMap::new(),
            metrics: HashMap::new(),
            telemetry: HashMap::new(),
            firmware: HashMap::new(),
            leak_detector: HashMap::new(),
            logs: HashMap::new(),
            nmxt: HashMap::new(),
            nmxc: HashMap::new(),
            nvue_rest: HashMap::new(),
            nvue_gnmi: HashMap::new(),
            gpu_inventory: HashMap::new(),
            manager: HashMap::new(),
            reachability: HashMap::new(),
            inventories: HashMap::new(),
            switch_domain_uuids: HashMap::new(),
            reachability_specs: HashMap::new(),
        }
    }

    fn map(&self, kind: CollectorKind) -> &HashMap<Cow<'static, str>, Collector> {
        match kind {
            CollectorKind::Discovery => &self.discovery,
            CollectorKind::Sensor => &self.sensors,
            CollectorKind::Metrics => &self.metrics,
            CollectorKind::Telemetry => &self.telemetry,
            CollectorKind::Logs => &self.logs,
            CollectorKind::Firmware => &self.firmware,
            CollectorKind::LeakDetector => &self.leak_detector,
            CollectorKind::Nmxt => &self.nmxt,
            CollectorKind::Nmxc => &self.nmxc,
            CollectorKind::NvueRest => &self.nvue_rest,
            CollectorKind::NvueGnmi => &self.nvue_gnmi,
            CollectorKind::GpuInventory => &self.gpu_inventory,
            CollectorKind::Manager => &self.manager,
            CollectorKind::Reachability => &self.reachability,
        }
    }

    pub(super) fn map_mut(
        &mut self,
        kind: CollectorKind,
    ) -> &mut HashMap<Cow<'static, str>, Collector> {
        match kind {
            CollectorKind::Discovery => &mut self.discovery,
            CollectorKind::Sensor => &mut self.sensors,
            CollectorKind::Metrics => &mut self.metrics,
            CollectorKind::Telemetry => &mut self.telemetry,
            CollectorKind::Logs => &mut self.logs,
            CollectorKind::Firmware => &mut self.firmware,
            CollectorKind::LeakDetector => &mut self.leak_detector,
            CollectorKind::Nmxt => &mut self.nmxt,
            CollectorKind::Nmxc => &mut self.nmxc,
            CollectorKind::NvueRest => &mut self.nvue_rest,
            CollectorKind::NvueGnmi => &mut self.nvue_gnmi,
            CollectorKind::GpuInventory => &mut self.gpu_inventory,
            CollectorKind::Manager => &mut self.manager,
            CollectorKind::Reachability => &mut self.reachability,
        }
    }

    pub(super) fn inventory_for(&mut self, key: &str) -> SharedInventory<BmcClient> {
        if let Some(shared) = self.inventories.get(key) {
            return shared.clone();
        }
        let shared = Arc::new(ArcSwapOption::empty());
        self.inventories
            .insert(Cow::Owned(key.to_string()), shared.clone());
        shared
    }

    /// Drop the shared inventory handle for a removed endpoint.
    pub(super) fn remove_inventory(&mut self, key: &str) {
        self.inventories.remove(key);
    }

    /// Records the latest switch domain and reports whether it changed.
    ///
    /// The first observation establishes a baseline without forcing a restart.
    /// Later transitions between absent and present values, or between two UUIDs,
    /// require a restart because running collectors retain their startup metadata.
    pub(super) fn observe_switch_domain(
        &mut self,
        key: &str,
        domain_uuid: Option<NvLinkDomainId>,
    ) -> bool {
        match self.switch_domain_uuids.get_mut(key) {
            Some(previous) if *previous != domain_uuid => {
                *previous = domain_uuid;
                true
            }
            Some(_) => false,
            None => {
                self.switch_domain_uuids
                    .insert(Cow::Owned(key.to_string()), domain_uuid);
                false
            }
        }
    }

    pub(super) fn retain_switch_domains(
        &mut self,
        active_switch_endpoints: &HashSet<Cow<'static, str>>,
    ) {
        self.switch_domain_uuids
            .retain(|key, _| active_switch_endpoints.contains(key));
    }

    pub(super) fn contains(&self, kind: CollectorKind, key: &str) -> bool {
        self.map(kind).contains_key(key)
    }

    pub(super) fn insert(
        &mut self,
        kind: CollectorKind,
        key: Cow<'static, str>,
        collector: Collector,
    ) {
        self.map_mut(kind).insert(key, collector);
    }

    pub(super) fn len(&self, kind: CollectorKind) -> usize {
        self.map(kind).len()
    }

    pub(super) fn removed_keys(
        &self,
        active_keys: &HashSet<Cow<'static, str>>,
    ) -> HashSet<Cow<'static, str>> {
        self.discovery
            .keys()
            .chain(self.sensors.keys())
            .chain(self.metrics.keys())
            .chain(self.telemetry.keys())
            .chain(self.logs.keys())
            .chain(self.firmware.keys())
            .chain(self.leak_detector.keys())
            .chain(self.nmxt.keys())
            .chain(self.nmxc.keys())
            .chain(self.nvue_rest.keys())
            .chain(self.nvue_gnmi.keys())
            .chain(self.gpu_inventory.keys())
            .chain(self.manager.keys())
            .filter(|key| !active_keys.contains(*key))
            .cloned()
            .collect()
    }

    pub(super) fn prune_finished_logs(&mut self) {
        self.logs.retain(|key, collector| {
            if collector.is_finished() {
                tracing::info!(
                    endpoint_key = %key,
                    "pruning finished logs collector (task exited); discovery will respawn"
                );
                false
            } else {
                true
            }
        });
    }
}

pub struct DiscoveryLoopContext {
    pub(super) collectors: CollectorState,
    pub(crate) discovery_iteration_histogram: Histogram,
    pub(crate) discovery_endpoint_fetch_histogram: Histogram,
    pub(crate) limiter: Arc<dyn RateLimiter>,
    pub(crate) bmc_request_concurrency: NonZeroUsize,
    pub(crate) metrics_manager: Arc<MetricsManager>,
    pub(crate) discovery_config: DiscoveryConfig,
    pub(crate) sensors_config: Configurable<SensorCollectorOptions>,
    pub(crate) metrics_config: Configurable<MetricsCollectorOptions>,
    pub(crate) telemetry_config: Configurable<TelemetryCollectorOptions>,
    pub(crate) logs_config: Configurable<LogsCollectorOptions>,
    pub(crate) firmware_config: Configurable<FirmwareCollectorOptions>,
    pub(crate) leak_detector_config: Configurable<LeakDetectorCollectorOptions>,
    pub(crate) nmxt_config: Configurable<NmxtCollectorOptions>,
    pub(crate) nmxc_config: Configurable<NmxcCollectorOptions>,
    pub(crate) nmxc_schema_override: Option<Arc<NmxcSchemaOverride>>,
    pub(crate) nvue_config: Configurable<NvueCollectorOptions>,
    pub(crate) tls_config: Option<MtlsProfileConfig>,
    pub(crate) tls_http_client_provider: Option<MtlsHttpClientProvider>,

    /// Whether any enabled sink consumes `CollectorEvent::Log` payloads.
    pub(crate) log_event_sink_enabled: bool,

    /// Whether NMX-C domain health reports have an enabled consumer.
    pub(crate) nvlink_domain_health_report_sink_enabled: bool,

    /// Active reachability configuration.
    ///
    /// This is absent when the collector is disabled or no metric or log sink
    /// can consume its observations.
    pub(super) reachability_config: Option<ReachabilityCollectorConfig>,
    pub(crate) gpu_inventory_config: Configurable<GpuInventoryConfig>,
    pub(crate) api_client: Option<Arc<ApiClientWrapper>>,
    pub(crate) log_downgrade_registry: Arc<LogDowngradeRegistry>,

    /// Whether log collectors should attach diagnostic payload carriers.
    pub(crate) logs_include_diagnostics: bool,
}

impl DiscoveryLoopContext {
    pub fn new(
        limiter: Arc<dyn RateLimiter>,
        metrics_manager: Arc<MetricsManager>,
        config: Arc<Config>,
    ) -> Result<Self, HealthError> {
        let nmxc_schema_override = load_nmxc_schema_override(&config)?;
        Self::new_with_tls_config(limiter, metrics_manager, config, None, nmxc_schema_override)
    }

    pub(crate) fn new_with_tls_config(
        limiter: Arc<dyn RateLimiter>,
        metrics_manager: Arc<MetricsManager>,
        config: Arc<Config>,
        tls_config: Option<MtlsProfileConfig>,
        nmxc_schema_override: Option<Arc<NmxcSchemaOverride>>,
    ) -> Result<Self, HealthError> {
        let registry = metrics_manager.global_registry();

        let metrics_prefix = &config.metrics.prefix;

        let discovery_iteration_histogram = Histogram::with_opts(
            HistogramOpts::new(
                format!("{metrics_prefix}_discovery_iteration_seconds"),
                "Duration of full discovery loop iteration",
            )
            .buckets(operation_duration_buckets_seconds()),
        )?;
        registry.register(Box::new(discovery_iteration_histogram.clone()))?;

        let discovery_endpoint_fetch_histogram = Histogram::with_opts(
            HistogramOpts::new(
                format!("{metrics_prefix}_discovery_endpoint_fetch_seconds"),
                "Duration of API call to fetch BMC endpoints",
            )
            .buckets(operation_duration_buckets_seconds()),
        )?;
        registry.register(Box::new(discovery_endpoint_fetch_histogram.clone()))?;

        let tls_config = tls_config.or_else(|| config.tls.switch.clone());

        // Periodic HTTP switch collectors share one provider because
        // `[tls.switch]` is a single profile. The shortest enabled HTTP poll
        // interval bounds cert reload staleness without rebuilding a client per
        // switch target.
        let tls_http_client_provider = tls_config.clone().and_then(|tls_config| {
            switch_http_reload_interval(&config)
                .map(|reload_interval| MtlsHttpClientProvider::new(tls_config, reload_interval))
        });

        let reachability_config =
            if config.sinks.prometheus.is_enabled() || config.sinks.includes_log_events() {
                config.collectors.reachability.as_option().cloned()
            } else {
                None
            };

        Ok(Self {
            collectors: CollectorState::new(),
            discovery_iteration_histogram,
            discovery_endpoint_fetch_histogram,
            limiter,
            bmc_request_concurrency: config.bmc_request_concurrency,
            metrics_manager,
            discovery_config: config.collectors.discovery.clone(),
            sensors_config: config.collectors.sensors.clone(),
            metrics_config: config.collectors.metrics.clone(),
            telemetry_config: config.collectors.telemetry.clone(),
            logs_config: config.collectors.logs.clone(),
            firmware_config: config.collectors.firmware.clone(),
            leak_detector_config: config.collectors.leak_detector.clone(),
            nmxt_config: config.collectors.nmxt.clone(),
            nmxc_config: config.collectors.nmxc.clone(),
            nmxc_schema_override,
            nvue_config: config.collectors.nvue.clone(),
            tls_config,
            tls_http_client_provider,
            log_event_sink_enabled: config.sinks.includes_log_events(),
            nvlink_domain_health_report_sink_enabled: config
                .sinks
                .nvlink_domain_health_report
                .is_enabled(),
            reachability_config,
            gpu_inventory_config: config.collectors.gpu_inventory.clone(),
            api_client: match &config.endpoint_sources.carbide_api {
                Configurable::Enabled(source_cfg) => Some(Arc::new(ApiClientWrapper::new(
                    source_cfg.root_ca.clone(),
                    source_cfg.client_cert.clone(),
                    source_cfg.client_key.clone(),
                    &source_cfg.api_url,
                ))),
                _ => None,
            },
            log_downgrade_registry: Arc::new(LogDowngradeRegistry::new()),
            logs_include_diagnostics: config.sinks.includes_log_diagnostics(),
        })
    }
}

pub(crate) fn load_nmxc_schema_override(
    config: &Config,
) -> Result<Option<Arc<NmxcSchemaOverride>>, HealthError> {
    let Some(nmxc) = config.collectors.nmxc.as_option() else {
        return Ok(None);
    };

    let Some(schema_override) = &nmxc.schema_override else {
        return Ok(None);
    };

    NmxcSchemaOverride::load(schema_override, nmxc)
        .map(Arc::new)
        .map(Some)
        .map_err(|error| HealthError::NmxcSchemaOverride(Box::new(error)))
}

/// Returns the cadence at which periodic HTTP switch collectors reload mTLS material.
fn switch_http_reload_interval(config: &Config) -> Option<Duration> {
    let mut reload_interval = None;

    if let Configurable::Enabled(nmxt_config) = &config.collectors.nmxt {
        reload_interval = Some(nmxt_config.scrape_interval);
    }

    if let Configurable::Enabled(nvue_config) = &config.collectors.nvue
        && let Configurable::Enabled(rest_config) = &nvue_config.rest
    {
        reload_interval = Some(
            reload_interval
                .map(|existing: Duration| existing.min(rest_config.poll_interval))
                .unwrap_or(rest_config.poll_interval),
        );
    }

    reload_interval
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashSet;

    use super::*;
    use crate::collectors::Collector;
    use crate::config::MtlsProfileConfig;

    fn noop_collector() -> Collector {
        Collector::spawn_task(|_| async {})
    }

    #[tokio::test]
    async fn removed_keys_includes_nvue_gnmi_collectors() {
        let mut state = CollectorState::new();
        state.insert(
            CollectorKind::NvueGnmi,
            Cow::Borrowed("removed-gNMI-endpoint"),
            noop_collector(),
        );
        state.insert(
            CollectorKind::NvueRest,
            Cow::Borrowed("active-rest-endpoint"),
            noop_collector(),
        );

        let active = HashSet::from([Cow::Borrowed("active-rest-endpoint")]);
        let removed = state.removed_keys(&active);

        assert!(removed.contains(&Cow::Borrowed("removed-gNMI-endpoint")));
        assert!(!removed.contains(&Cow::Borrowed("active-rest-endpoint")));
    }

    #[test]
    fn context_carries_tls_switch_config() {
        let mut config = Config::default();

        let tls_config = MtlsProfileConfig {
            ca_cert_path: "/switch/ca.crt".into(),
            client_cert_path: "/switch/tls.crt".into(),
            client_key_path: "/switch/tls.key".into(),
            tls_server_name: Some("switches.example.forge".to_string()),
        };

        config.tls.switch = Some(tls_config.clone());

        let context = DiscoveryLoopContext::new(
            Arc::new(crate::limiter::NoopLimiter),
            Arc::new(MetricsManager::new("tls_context").expect("metrics manager")),
            Arc::new(config),
        )
        .expect("context should initialize");

        let actual_tls_config = context
            .tls_config
            .as_ref()
            .expect("[tls.switch] config should be present");

        assert_eq!(actual_tls_config, &tls_config);
    }
}
