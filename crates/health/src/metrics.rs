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
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use http::Response;
use http::header::CONTENT_TYPE;
use hyper::Request;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use prometheus::core::{Collector, Desc};
use prometheus::proto::LabelPair;
use prometheus::{
    Encoder, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
    proto,
};
use serde::{Deserialize, Serialize};

use crate::HealthError;
use crate::endpoint::{BmcEndpoint, EndpointMetadata, SwitchEndpointRole};

pub type MetricLabel = (Cow<'static, str>, String);
type BoxedErr = Box<dyn std::error::Error + Send + Sync + 'static>;

pub fn operation_duration_buckets_seconds() -> Vec<f64> {
    vec![
        1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 240.0, 300.0,
    ]
}

pub fn bmc_latency_buckets_ms() -> Vec<f64> {
    vec![
        5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0, 2_500.0, 5_000.0, 10_000.0, 30_000.0,
        60_000.0,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BmcLatencyAttribute {
    HttpResponseStatusCode,
    HttpRequestMethod,
    HttpPath,
    ServerAddress,
    UrlScheme,
    BmcVendor,
    BmcModel,
    EntityType,
    MachineId,
    RackId,
}

impl BmcLatencyAttribute {
    pub const ATTRIBUTES: [Self; 10] = [
        Self::HttpResponseStatusCode,
        Self::HttpRequestMethod,
        Self::HttpPath,
        Self::ServerAddress,
        Self::UrlScheme,
        Self::BmcVendor,
        Self::BmcModel,
        Self::EntityType,
        Self::MachineId,
        Self::RackId,
    ];

    pub fn label_name(self) -> &'static str {
        match self {
            Self::HttpResponseStatusCode => "http_response_status_code",
            Self::HttpRequestMethod => "http_request_method",
            Self::HttpPath => "http_path",
            Self::ServerAddress => "server_address",
            Self::UrlScheme => "url_scheme",
            Self::BmcVendor => "bmc_vendor",
            Self::BmcModel => "bmc_model",
            Self::EntityType => "entity_type",
            Self::MachineId => "machine_id",
            Self::RackId => "rack_id",
        }
    }

    pub fn from_label_name(label_name: &str) -> Option<Self> {
        Self::ATTRIBUTES
            .iter()
            .copied()
            .find(|attribute| attribute.label_name() == label_name)
    }
}

#[derive(Clone)]
pub struct BmcLatencyMetrics {
    latency_ms: HistogramVec,
    attributes: Vec<BmcLatencyAttribute>,
}

pub struct BmcLatencyObservation<'a> {
    pub status_code: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub server_address: &'a str,
    pub url_scheme: &'a str,
    pub bmc_vendor: Option<&'a str>,
    pub bmc_model: Option<&'a str>,
    pub entity_type: &'a str,
    pub machine_id: Option<&'a str>,
    pub rack_id: Option<&'a str>,
    pub duration: std::time::Duration,
}

impl BmcLatencyMetrics {
    pub fn new(registry: &Registry, prefix: &str) -> Result<Self, prometheus::Error> {
        Self::new_with_attributes(registry, prefix, &BmcLatencyAttribute::ATTRIBUTES)
    }

    pub fn new_with_attributes(
        registry: &Registry,
        prefix: &str,
        attributes: &[BmcLatencyAttribute],
    ) -> Result<Self, prometheus::Error> {
        let label_names = attributes
            .iter()
            .map(|attribute| attribute.label_name())
            .collect::<Vec<_>>();
        let latency_ms = HistogramVec::new(
            HistogramOpts::new(
                format!("{prefix}_bmc_latency_ms"),
                "Duration of outbound Redfish HTTP requests to BMCs, in milliseconds",
            )
            .buckets(bmc_latency_buckets_ms()),
            &label_names,
        )?;
        registry.register(Box::new(latency_ms.clone()))?;

        Ok(Self {
            latency_ms,
            attributes: attributes.to_vec(),
        })
    }

    pub fn observe(&self, observation: BmcLatencyObservation<'_>) {
        let labels = self
            .attributes
            .iter()
            .map(|attribute| match attribute {
                BmcLatencyAttribute::HttpResponseStatusCode => observation.status_code,
                BmcLatencyAttribute::HttpRequestMethod => observation.method,
                BmcLatencyAttribute::HttpPath => observation.path,
                BmcLatencyAttribute::ServerAddress => observation.server_address,
                BmcLatencyAttribute::UrlScheme => observation.url_scheme,
                BmcLatencyAttribute::BmcVendor => observation.bmc_vendor.unwrap_or("unknown"),
                BmcLatencyAttribute::BmcModel => observation.bmc_model.unwrap_or("unknown"),
                BmcLatencyAttribute::EntityType => observation.entity_type,
                BmcLatencyAttribute::MachineId => observation.machine_id.unwrap_or("unknown"),
                BmcLatencyAttribute::RackId => observation.rack_id.unwrap_or("unknown"),
            })
            .collect::<Vec<_>>();
        self.latency_ms
            .with_label_values(&labels)
            .observe(observation.duration.as_secs_f64() * 1_000.0);
    }
}

/// Current sharded endpoint identities from the configured endpoint source.
/// This is inventory membership, not a BMC reachability or health signal.
pub(crate) struct InventoryEndpointMetrics {
    info: GaugeVec,
}

impl InventoryEndpointMetrics {
    pub(crate) fn new(registry: &Registry, prefix: &str) -> Result<Self, prometheus::Error> {
        let info = GaugeVec::new(
            Opts::new(
                format!("{prefix}_inventory_endpoint_info"),
                "Discovered endpoint identity, independent of reachability",
            ),
            &[
                "endpoint_key",
                "rack_id",
                "domain_id",
                "component_type",
                "endpoint_type",
                "tray",
                "tray_label",
                "switch_serial",
            ],
        )?;
        registry.register(Box::new(info.clone()))?;
        Ok(Self { info })
    }

    /// Replace the previous successful discovery pass so removed or renamed
    /// endpoints cannot linger in the inventory series.
    pub(crate) fn reconcile(&self, endpoints: &[Arc<BmcEndpoint>]) {
        self.info.reset();
        for endpoint in endpoints {
            let metadata = endpoint.metadata.as_ref();
            let (domain_id, tray, tray_label, switch_serial) = match metadata {
                Some(EndpointMetadata::Machine(machine)) => (
                    machine
                        .nvlink_domain_uuid
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    machine
                        .machine_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| endpoint.key()),
                    machine
                        .machine_serial
                        .clone()
                        .filter(|serial| !serial.is_empty())
                        .unwrap_or_else(|| endpoint.addr.ip.to_string()),
                    String::new(),
                ),
                Some(EndpointMetadata::Switch(switch)) => (
                    switch
                        .nvlink_domain_uuid
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    String::new(),
                    String::new(),
                    switch
                        .serial
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .or(switch.log_checkpoint_identity.as_deref())
                        .unwrap_or_default()
                        .to_string(),
                ),
                Some(EndpointMetadata::PowerShelf(power_shelf)) => (
                    power_shelf
                        .nvlink_domain_uuid
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    String::new(),
                    String::new(),
                    String::new(),
                ),
                None => (String::new(), String::new(), String::new(), String::new()),
            };
            let endpoint_key = endpoint.key();
            let component_type = metadata.map_or("unknown", EndpointMetadata::component_type);
            let endpoint_type = match metadata {
                Some(EndpointMetadata::Switch(switch))
                    if switch.endpoint_role == SwitchEndpointRole::Host =>
                {
                    "nvos"
                }
                _ => "bmc",
            };
            let rack_id = endpoint
                .rack_id
                .as_ref()
                .map(ToString::to_string)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| "__unknown_rack__".to_string());
            self.info
                .with_label_values(&[
                    &endpoint_key,
                    &rack_id,
                    &domain_id,
                    component_type,
                    endpoint_type,
                    &tray,
                    &tray_label,
                    &switch_serial,
                ])
                .set(1.0);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentKind {
    Collector,
    Processor,
    Sink,
}

impl ComponentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collector => "collector",
            Self::Processor => "processor",
            Self::Sink => "sink",
        }
    }
}

#[derive(Clone)]
pub struct ComponentMetrics {
    failures_total: IntCounterVec,
    duration_seconds: HistogramVec,
}

impl ComponentMetrics {
    pub fn new(registry: &Registry, prefix: &str) -> Result<Self, prometheus::Error> {
        let failures_total = IntCounterVec::new(
            prometheus::Opts::new(
                format!("{prefix}_component_failures_total"),
                "Number of component operation failures",
            ),
            &["component_kind", "component_name"],
        )?;
        registry.register(Box::new(failures_total.clone()))?;

        let duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                format!("{prefix}_component_duration_seconds"),
                "Duration of component operations",
            )
            .buckets(operation_duration_buckets_seconds()),
            &["component_kind", "component_name"],
        )?;
        registry.register(Box::new(duration_seconds.clone()))?;

        Ok(Self {
            failures_total,
            duration_seconds,
        })
    }

    pub fn record_operation(
        &self,
        kind: ComponentKind,
        name: &str,
        duration: std::time::Duration,
        success: bool,
    ) {
        let labels = [kind.as_str(), name];
        self.duration_seconds
            .with_label_values(&labels)
            .observe(duration.as_secs_f64());
        if !success {
            self.failures_total.with_label_values(&labels).inc();
        }
    }
}

pub struct MetricsManager {
    global_registry: Registry,
    telemetry_registry: Registry,
    component_metrics: Arc<ComponentMetrics>,
    /// The instrumentation framework's registry, exposed through the same
    /// /metrics response so its events are scrapeable without migrating this
    /// server off its raw prometheus pipeline. Set once at startup.
    framework_registry: std::sync::OnceLock<Registry>,
}

impl MetricsManager {
    pub fn new(prefix: &str) -> Result<Self, prometheus::Error> {
        let global_registry = Registry::new();
        let telemetry_registry = Registry::new();
        let component_metrics = Arc::new(ComponentMetrics::new(&global_registry, prefix)?);

        Ok(Self {
            global_registry,
            telemetry_registry,
            component_metrics,
            framework_registry: std::sync::OnceLock::new(),
        })
    }

    pub fn global_registry(&self) -> &Registry {
        &self.global_registry
    }

    pub fn component_metrics(&self) -> Arc<ComponentMetrics> {
        self.component_metrics.clone()
    }

    pub fn create_collector_registry(
        &self,
        id: String,
        prefix: impl Into<String>,
    ) -> Result<CollectorRegistry, HealthError> {
        CollectorRegistry::new(id, self.global_registry.clone(), prefix)
    }

    pub fn create_telemetry_collector_registry(
        &self,
        id: String,
        prefix: impl Into<String>,
    ) -> Result<CollectorRegistry, HealthError> {
        CollectorRegistry::new(id, self.telemetry_registry.clone(), prefix)
    }

    /// Makes the instrumentation framework's registry part of every
    /// subsequent /metrics response. A second call is ignored.
    pub fn expose_framework_registry(&self, registry: Registry) {
        let _ = self.framework_registry.set(registry);
    }

    pub fn export_metrics(&self) -> Result<String, HealthError> {
        let mut exposition = export_registry(&self.global_registry)?;
        if let Some(framework) = self.framework_registry.get() {
            exposition.push_str(&export_registry(framework)?);
        }
        Ok(exposition)
    }

    pub fn export_telemetry(&self) -> Result<String, HealthError> {
        export_registry(&self.telemetry_registry)
    }
}

fn export_registry(registry: &Registry) -> Result<String, HealthError> {
    let encoder = TextEncoder::new();
    let metric_families = registry.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer)?;
    String::from_utf8(buffer).map_err(|e| {
        HealthError::GenericError(format!(
            "MetricManager encoutered IO error while export is called: {e:?}"
        ))
    })
}

pub struct CollectorRegistry {
    prefix: String,
    registry: Box<SubRegistry>,
    parent: Registry,
}

impl CollectorRegistry {
    fn new(id: String, parent: Registry, prefix: impl Into<String>) -> Result<Self, HealthError> {
        let fq_id = id.replace(|c: char| !c.is_ascii_alphanumeric(), "_");

        // Prometheus retains descriptor names after unregistering collectors.
        let desc = Desc::new(
            "health_collector_registry".to_string(),
            "Internal health collector registry identity".to_string(),
            Vec::new(),
            HashMap::from([("collector_id".to_string(), fq_id)]),
        )?;

        let registry = Box::new(SubRegistry {
            registry: Registry::new(),
            desc,
        });

        parent.register(registry.clone())?;

        Ok(Self {
            prefix: prefix.into(),
            registry,
            parent,
        })
    }

    pub fn create_gauge_metrics(
        &self,
        id: String,
        help: impl Into<String>,
        static_labels: Vec<MetricLabel>,
    ) -> Result<Arc<GaugeMetrics>, prometheus::Error> {
        let metrics = Arc::new(GaugeMetrics::new(
            id,
            &self.registry.registry,
            self.prefix.clone(),
            help,
            static_labels,
        )?);

        Ok(metrics)
    }

    pub fn unregister_gauge_metrics(
        &self,
        metrics: &GaugeMetrics,
    ) -> Result<(), prometheus::Error> {
        self.registry
            .registry
            .unregister(Box::new(metrics.clone()))
            .map(|_| ())
    }

    pub fn registry(&self) -> &Registry {
        &self.registry.registry
    }

    pub fn prefix(&self) -> &String {
        &self.prefix
    }
}

#[derive(Clone)]
struct SubRegistry {
    registry: Registry,
    desc: Desc,
}

impl Collector for SubRegistry {
    fn desc(&self) -> Vec<&Desc> {
        vec![&self.desc]
    }

    fn collect(&self) -> Vec<proto::MetricFamily> {
        self.registry.gather()
    }
}

impl Drop for CollectorRegistry {
    fn drop(&mut self) {
        if let Err(e) = self.parent.unregister(self.registry.clone()) {
            tracing::error!(
                error = ?e,
                collector_prefix = self.prefix().as_str(),
                "Could not properly drop registry for collector"
            )
        }
    }
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct GaugeKey(String);

impl From<String> for GaugeKey {
    fn from(s: String) -> Self {
        GaugeKey(s)
    }
}

impl From<&str> for GaugeKey {
    fn from(s: &str) -> Self {
        GaugeKey(s.to_string())
    }
}

pub struct GaugeReading {
    pub key: GaugeKey,
    pub name: String,
    pub metric_type: String,
    pub unit: String,
    pub value: f64,
    pub labels: Vec<MetricLabel>,
}

impl GaugeReading {
    pub fn new(
        key: impl Into<GaugeKey>,
        name: impl Into<String>,
        metric_type: impl Into<String>,
        unit: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            metric_type: metric_type.into(),
            unit: unit.into(),
            value,
            labels: Vec::new(),
        }
    }

    pub fn with_labels(mut self, labels: Vec<MetricLabel>) -> Self {
        self.labels.extend(labels);
        self
    }
}

struct GaugeData {
    name: String,
    metric_type: String,
    unit: String,
    value: f64,
    labels: Vec<MetricLabel>,
    generation: u64,
}

#[derive(Clone)]
pub struct GaugeMetrics {
    gauges: Arc<DashMap<GaugeKey, GaugeData>>,
    current_generation: Arc<AtomicU64>,
    metric_name_prefix: String,
    metric_help: String,
    static_labels: Vec<proto::LabelPair>,
    static_label_names: HashSet<String>,
    desc: Desc,
}

impl GaugeMetrics {
    /// Registers gauge readings with an ID unique to the registry. The registration
    /// identity does not add labels or change the names returned by `collect()`.
    pub fn new(
        id: String,
        registry: &Registry,
        metric_name_prefix: impl Into<String>,
        metric_help: impl Into<String>,
        static_labels: Vec<(impl Into<String>, impl Into<String>)>,
    ) -> Result<Self, prometheus::Error> {
        // Only collect() supplies exported metric names and labels.
        let desc = Desc::new(
            "health_gauge_metrics".to_string(),
            "Internal health gauge collector identity".to_string(),
            Vec::new(),
            HashMap::from([("collector_id".to_string(), id)]),
        )?;

        let mut static_label_names = HashSet::with_capacity(static_labels.len());
        let static_labels = static_labels
            .into_iter()
            .map(|(name, value)| {
                let name = name.into();
                static_label_names.insert(name.clone());

                let mut label = LabelPair::new();
                label.set_name(name);
                label.set_value(value.into());
                label
            })
            .collect();

        let metrics = Self {
            gauges: Arc::new(DashMap::new()),
            current_generation: Arc::new(AtomicU64::new(0)),
            metric_name_prefix: metric_name_prefix.into(),
            metric_help: metric_help.into(),
            static_labels,
            static_label_names,
            desc,
        };

        registry.register(Box::new(metrics.clone()))?;
        Ok(metrics)
    }

    pub fn begin_update(&self) {
        self.current_generation.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn has_static_label(&self, name: &str) -> bool {
        self.static_label_names.contains(name)
    }

    pub fn record(&self, reading: GaugeReading) {
        let generation = self.current_generation.load(Ordering::Acquire);

        self.gauges.insert(
            reading.key,
            GaugeData {
                name: reading.name,
                metric_type: reading.metric_type,
                unit: reading.unit,
                value: reading.value,
                labels: reading.labels,
                generation,
            },
        );
    }

    /// Drops readings with the selected type, unit, and label values. When given,
    /// label names must match the complete dynamic label set.
    pub(crate) fn prune(
        &self,
        metric_type: Option<&str>,
        labels: &[(Cow<'static, str>, String)],
        unit: Option<&str>,
        label_names: Option<&[Cow<'static, str>]>,
    ) {
        self.gauges.retain(|_, data| {
            let matches = metric_type.is_none_or(|kind| data.metric_type == kind)
                && labels.iter().all(|label| data.labels.contains(label))
                && unit.is_none_or(|unit| data.unit == unit)
                && label_names.is_none_or(|names| {
                    data.labels.len() == names.len()
                        && names
                            .iter()
                            .all(|name| data.labels.iter().any(|(actual, _)| actual == name))
                });

            !matches
        });
    }

    /// Removes one reading by the key used when it was recorded.
    pub(crate) fn prune_key(&self, key: &str) {
        self.gauges.remove(&GaugeKey::from(key));
    }

    pub fn sweep_stale(&self) {
        let current_gen = self.current_generation.load(Ordering::Acquire);
        self.gauges.retain(|_, data| data.generation == current_gen);
    }

    pub fn clear(&self) {
        self.gauges.clear();
    }
}

impl Collector for GaugeMetrics {
    fn desc(&self) -> Vec<&Desc> {
        vec![&self.desc]
    }

    fn collect(&self) -> Vec<proto::MetricFamily> {
        let mut families: HashMap<(String, String, String), proto::MetricFamily> = HashMap::new();

        for gauge_ref in self.gauges.iter() {
            let data = gauge_ref.value();
            let family_key = (
                data.name.clone(),
                data.metric_type.clone(),
                data.unit.clone(),
            );

            let family = families.entry(family_key.clone()).or_insert_with(|| {
                let metric_name = format!(
                    "{}_{}_{}_{}",
                    self.metric_name_prefix, family_key.0, family_key.1, family_key.2
                );
                let mut mf = proto::MetricFamily::default();
                mf.set_name(metric_name);
                mf.set_help(self.metric_help.clone());
                mf.set_field_type(proto::MetricType::GAUGE);
                mf
            });

            let mut labels: Vec<proto::LabelPair> = self.static_labels.clone();

            for (name, value) in &data.labels {
                let mut label = proto::LabelPair::new();
                label.set_name(name.as_ref().to_owned());
                label.set_value(value.clone());
                labels.push(label);
            }

            let mut gauge = proto::Gauge::new();
            gauge.set_value(data.value);

            let mut metric = proto::Metric::new();
            metric.set_label(labels);
            metric.set_gauge(gauge);

            family.mut_metric().push(metric);
        }

        families.into_values().collect()
    }
}

pub async fn run_metrics_server(
    metrics_endpoint: std::net::SocketAddr,
    metrics_manager: Arc<MetricsManager>,
) -> Result<(), BoxedErr> {
    let listener = metrics_endpoint::bind_tcp_listener(metrics_endpoint)
        .await
        .map_err(|e| Box::new(e) as BoxedErr)?;

    tracing::info!(
        %metrics_endpoint,
        "Metrics server listening (paths: /metrics, /telemetry, /livez)"
    );

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|e| Box::new(e) as BoxedErr)?;

        let io = TokioIo::new(stream);
        let metrics_manager = metrics_manager.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let metrics_manager = metrics_manager.clone();
                async move { serve_request(req, metrics_manager) }
            });

            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                tracing::error!(error=?e, "metrics server connection error");
            }
        });
    }
}

fn serve_request<B>(
    req: Request<B>,
    metrics_manager: Arc<MetricsManager>,
) -> Result<Response<String>, hyper::Error> {
    match req.uri().path() {
        "/livez" => Ok(Response::builder()
            .status(http::StatusCode::OK)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body("ok".to_string())
            .expect("BUG: Response::builder error")),
        "/metrics" => serve_prometheus(metrics_manager.export_metrics(), "service metrics"),
        "/telemetry" => serve_prometheus(metrics_manager.export_telemetry(), "telemetry metrics"),
        _ => Ok(Response::builder()
            .status(http::StatusCode::NOT_FOUND)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body("not found; use /metrics, /telemetry, or /livez".to_string())
            .expect("BUG: Response::builder error")),
    }
}

fn serve_prometheus(
    export_result: Result<String, HealthError>,
    export_name: &'static str,
) -> Result<Response<String>, hyper::Error> {
    let encoder = TextEncoder::new();
    let body = match export_result {
        Ok(body) => body,
        Err(e) => {
            tracing::error!(error=?e, export_name, "error exporting prometheus metrics");
            return Ok(Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(format!("error exporting {export_name}, see logs"))
                .expect("BUG: Response::builder error"));
        }
    };

    Ok(Response::builder()
        .status(200)
        .header(CONTENT_TYPE, encoder.format_type())
        .body(body)
        .expect("BUG: Response::builder error"))
}

pub fn sanitize_unit(unit: &str) -> String {
    match unit.to_lowercase().as_str() {
        "%" => "percent".to_string(),
        "°c" | "c" | "cel" => "celsius".to_string(),
        "°f" | "f" => "fahrenheit".to_string(),
        "v" => "volts".to_string(),
        "a" | "amps" => "amperes".to_string(),
        "w" => "watts".to_string(),
        "hz" => "hertz".to_string(),
        _ => unit
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::rack::RackId;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{
        EndpointMetadata, MachineData, PowerShelfData, SharedSystemUuid, SwitchData,
    };

    #[test]
    fn inventory_info_tracks_discovered_trays_without_bmc_access() {
        let manager = MetricsManager::new("carbide_hardware_health").unwrap();
        let inventory =
            InventoryEndpointMetrics::new(manager.global_registry(), "carbide_hardware_health")
                .unwrap();
        let mut tray = test_endpoint(mac("02:00:00:00:00:01"));
        tray.rack_id = Some(RackId::new("vr-nvl72-test"));
        tray.metadata = Some(EndpointMetadata::Machine(MachineData {
            machine_id: Some(
                "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                    .parse()
                    .unwrap(),
            ),
            machine_serial: Some("TRAY-01".to_string()),
            system_uuid: SharedSystemUuid::default(),
            slot_number: Some(1),
            tray_index: Some(1),
            nvlink_domain_uuid: None,
            driver_version: None,
        }));

        inventory.reconcile(&[Arc::new(tray)]);
        let first = manager.export_metrics().unwrap();
        assert!(first.contains("carbide_hardware_health_inventory_endpoint_info{"));
        assert!(first.contains("rack_id=\"vr-nvl72-test\""));
        assert!(first.contains("endpoint_type=\"bmc\""));
        assert!(first.contains("tray_label=\"TRAY-01\""));

        inventory.reconcile(&[]);
        let second = manager.export_metrics().unwrap();
        assert!(!second.contains("carbide_hardware_health_inventory_endpoint_info{"));
    }

    #[test]
    fn inventory_info_keeps_endpoints_with_missing_identity_visible() {
        let manager = MetricsManager::new("inventory_unknown").unwrap();
        let inventory =
            InventoryEndpointMetrics::new(manager.global_registry(), "inventory_unknown").unwrap();
        inventory.reconcile(&[Arc::new(test_endpoint(mac("02:00:00:00:00:02")))]);

        let body = manager.export_metrics().unwrap();
        assert!(body.contains("inventory_unknown_inventory_endpoint_info{"));
        assert!(body.contains("rack_id=\"__unknown_rack__\""));
        assert!(body.contains("component_type=\"unknown\""));
    }

    #[test]
    fn inventory_info_distinguishes_switch_bmc_and_nvos_endpoints() {
        let manager = MetricsManager::new("inventory_components").unwrap();
        let inventory =
            InventoryEndpointMetrics::new(manager.global_registry(), "inventory_components")
                .unwrap();
        let mut switch_bmc = test_endpoint(mac("02:00:00:00:00:03"));
        switch_bmc.rack_id = Some(RackId::new("rack-1"));
        switch_bmc.metadata = Some(EndpointMetadata::Switch(SwitchData {
            id: None,
            serial: Some("SWITCH-01".to_string()),
            log_checkpoint_identity: None,
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role: SwitchEndpointRole::Bmc,
            is_primary: true,
            nmxc_enabled: true,
            nmxt_enabled: true,
        }));
        let mut switch_host = switch_bmc.clone();
        switch_host.addr.mac = Some(mac("02:00:00:00:00:04"));
        if let Some(EndpointMetadata::Switch(switch)) = switch_host.metadata.as_mut() {
            switch.endpoint_role = SwitchEndpointRole::Host;
        }
        let mut power_shelf = test_endpoint(mac("02:00:00:00:00:05"));
        power_shelf.rack_id = Some(RackId::new("rack-1"));
        power_shelf.metadata = Some(EndpointMetadata::PowerShelf(PowerShelfData {
            id: None,
            serial: Some("SHELF-01".to_string()),
            nvlink_domain_uuid: None,
        }));

        inventory.reconcile(&[
            Arc::new(switch_bmc),
            Arc::new(switch_host.clone()),
            Arc::new(power_shelf),
        ]);
        let body = manager.export_metrics().unwrap();
        assert_eq!(
            body.matches("inventory_components_inventory_endpoint_info{")
                .count(),
            3
        );
        let switch_lines: Vec<_> = body
            .lines()
            .filter(|line| line.contains("switch_serial=\"SWITCH-01\""))
            .collect();
        assert_eq!(switch_lines.len(), 2);
        assert!(
            switch_lines
                .iter()
                .any(|line| line.contains("endpoint_type=\"bmc\""))
        );
        assert!(
            switch_lines
                .iter()
                .any(|line| line.contains("endpoint_type=\"nvos\""))
        );
        assert!(
            body.lines()
                .any(|line| line.contains("component_type=\"power_shelf\"")
                    && line.contains("endpoint_type=\"bmc\""))
        );

        // A switch with NVOS inventory but no BMC inventory must remain visible.
        inventory.reconcile(&[Arc::new(switch_host.clone())]);
        let host_only = manager.export_metrics().unwrap();
        assert_eq!(
            host_only
                .matches("inventory_components_inventory_endpoint_info{")
                .count(),
            1
        );
        assert!(host_only.contains("endpoint_type=\"nvos\""));

        if let Some(EndpointMetadata::Switch(switch)) = switch_host.metadata.as_mut() {
            switch.serial = None;
        }
        inventory.reconcile(&[Arc::new(switch_host.clone())]);
        let no_serial = manager.export_metrics().unwrap();
        assert!(no_serial.contains("endpoint_type=\"nvos\""));
        assert!(no_serial.contains("switch_serial=\"\""));

        if let Some(EndpointMetadata::Switch(switch)) = switch_host.metadata.as_mut() {
            switch.log_checkpoint_identity = Some("gb-nvl-050-switch01".to_string());
        }
        inventory.reconcile(&[Arc::new(switch_host)]);
        let configured_name = manager.export_metrics().unwrap();
        assert!(configured_name.contains("switch_serial=\"gb-nvl-050-switch01\""));
    }

    #[test]
    fn inventory_info_preserves_power_shelf_domain_id() {
        let manager = MetricsManager::new("inventory_shelf_domain").unwrap();
        let inventory =
            InventoryEndpointMetrics::new(manager.global_registry(), "inventory_shelf_domain")
                .unwrap();
        let mut power_shelf = test_endpoint(mac("02:00:00:00:00:06"));
        power_shelf.rack_id = Some(RackId::new("rack-1"));
        power_shelf.metadata = Some(EndpointMetadata::PowerShelf(PowerShelfData {
            id: None,
            serial: Some("SHELF-01".to_string()),
            nvlink_domain_uuid: Some("ec000000-0000-0000-0000-000000000001".parse().unwrap()),
        }));

        inventory.reconcile(&[Arc::new(power_shelf)]);
        let body = manager.export_metrics().unwrap();
        let shelf_line = body
            .lines()
            .find(|line| line.contains("inventory_shelf_domain_inventory_endpoint_info{"))
            .expect("power shelf inventory series should be exported");
        assert!(shelf_line.contains("component_type=\"power_shelf\""));
        assert!(shelf_line.contains("domain_id=\"ec000000-0000-0000-0000-000000000001\""));
    }

    #[test]
    fn unknown_metrics_route_returns_not_found() {
        let request = Request::builder()
            .uri("/definitely-not-a-route")
            .body(())
            .expect("test request should be valid");
        let metrics_manager = Arc::new(
            MetricsManager::new("test").expect("metrics manager should initialize for test"),
        );

        let response =
            serve_request(request, metrics_manager).expect("request should produce a response");

        assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8"),
        );
        assert_eq!(
            response.body(),
            "not found; use /metrics, /telemetry, or /livez",
        );
    }

    #[test]
    fn collector_registry_metadata_is_fixed_across_identities() {
        let parent = Registry::new();
        let ids = ["sensor_10.0.0.1:443", "log_bmc-01.example.com"];

        let collectors: Vec<_> = ids
            .iter()
            .map(|id| CollectorRegistry::new(id.to_string(), parent.clone(), "test").unwrap())
            .collect();

        for collector in &collectors {
            assert_eq!(collector.registry.desc.fq_name, "health_collector_registry");

            assert_eq!(
                collector.registry.desc.dim_hash,
                collectors[0].registry.desc.dim_hash
            );
        }

        assert_ne!(
            collectors[0].registry.desc.id,
            collectors[1].registry.desc.id
        );

        assert!(matches!(
            CollectorRegistry::new(ids[0].to_string(), parent.clone(), "test"),
            Err(HealthError::PrometheusError(prometheus::Error::AlreadyReg))
        ));

        assert!(matches!(
            CollectorRegistry::new("sensor_10_0_0_1_443".to_string(), parent.clone(), "test"),
            Err(HealthError::PrometheusError(prometheus::Error::AlreadyReg))
        ));

        drop(collectors);

        CollectorRegistry::new(ids[0].to_string(), parent, "test")
            .expect("dropped collector identity must register again");
    }

    #[test]
    fn gauge_registration_identity_preserves_export_and_unregister() {
        let collector = CollectorRegistry::new("owner".to_string(), Registry::new(), "test")
            .expect("collector registry");

        let gauges: Vec<_> = ["endpoint_a", "endpoint_b"]
            .into_iter()
            .map(|id| {
                collector
                    .create_gauge_metrics(
                        id.to_string(),
                        "Sensor readings",
                        vec![("endpoint".into(), id.to_string())],
                    )
                    .unwrap()
            })
            .collect();

        for gauge in &gauges {
            gauge.record(
                GaugeReading::new("reading", "temperature", "sensor", "celsius", 42.0)
                    .with_labels(vec![("channel".into(), "inlet".to_string())]),
            );
        }

        assert_eq!(gauges[0].desc.fq_name, "health_gauge_metrics");
        assert_eq!(gauges[0].desc.dim_hash, gauges[1].desc.dim_hash);
        assert_ne!(gauges[0].desc.id, gauges[1].desc.id);

        assert!(matches!(
            collector.create_gauge_metrics("endpoint_a".to_string(), "Sensor readings", Vec::new()),
            Err(prometheus::Error::AlreadyReg)
        ));

        let families = collector.registry().gather();

        assert_eq!(families.len(), 1);
        assert_eq!(families[0].name(), "test_temperature_sensor_celsius");
        assert_eq!(families[0].help(), "Sensor readings");
        assert_eq!(families[0].get_field_type(), proto::MetricType::GAUGE);
        assert_eq!(families[0].get_metric().len(), 2);

        for (metric, endpoint) in families[0]
            .get_metric()
            .iter()
            .zip(["endpoint_a", "endpoint_b"])
        {
            assert_eq!(metric.get_gauge().value(), 42.0);
            assert_eq!(metric.get_label().len(), 2);
            assert_eq!(metric.get_label()[0].name(), "endpoint");
            assert_eq!(metric.get_label()[0].value(), endpoint);
            assert_eq!(metric.get_label()[1].name(), "channel");
            assert_eq!(metric.get_label()[1].value(), "inlet");
        }

        collector
            .unregister_gauge_metrics(&gauges[0])
            .expect("unregister first endpoint");

        assert_eq!(collector.registry().gather()[0].get_metric().len(), 1);
        assert!(collector.unregister_gauge_metrics(&gauges[0]).is_err());

        collector
            .create_gauge_metrics("endpoint_a".to_string(), "Sensor readings", Vec::new())
            .expect("removed gauge identity must register again");
    }

    #[test]
    fn sanitize_unit_cases() {
        check_values(
            [
                Check {
                    scenario: "percent symbol",
                    input: "%",
                    expect: "percent".to_string(),
                },
                Check {
                    scenario: "degree Celsius",
                    input: "°C",
                    expect: "celsius".to_string(),
                },
                Check {
                    scenario: "Celsius abbreviation",
                    input: "c",
                    expect: "celsius".to_string(),
                },
                Check {
                    scenario: "Redfish Celsius unit",
                    input: "CEL",
                    expect: "celsius".to_string(),
                },
                Check {
                    scenario: "degree Fahrenheit",
                    input: "°F",
                    expect: "fahrenheit".to_string(),
                },
                Check {
                    scenario: "Fahrenheit abbreviation",
                    input: "f",
                    expect: "fahrenheit".to_string(),
                },
                Check {
                    scenario: "volts",
                    input: "V",
                    expect: "volts".to_string(),
                },
                Check {
                    scenario: "amperes",
                    input: "A",
                    expect: "amperes".to_string(),
                },
                Check {
                    scenario: "amps alias",
                    input: "Amps",
                    expect: "amperes".to_string(),
                },
                Check {
                    scenario: "watts",
                    input: "W",
                    expect: "watts".to_string(),
                },
                Check {
                    scenario: "hertz",
                    input: "Hz",
                    expect: "hertz".to_string(),
                },
                Check {
                    scenario: "revolutions per minute",
                    input: "RPM",
                    expect: "rpm".to_string(),
                },
                Check {
                    scenario: "punctuation is normalized",
                    input: "J/s",
                    expect: "j_s".to_string(),
                },
            ],
            sanitize_unit,
        );
    }
}
