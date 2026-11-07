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

mod telemetry;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use http::header::{ACCEPT_ENCODING, ALLOW, CONTENT_ENCODING, CONTENT_TYPE, VARY};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use prometheus::core::{Collector, Desc};
use prometheus::proto::LabelPair;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Registry, TextEncoder, proto,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::HealthError;
use crate::config::PrometheusSinkConfig;
use crate::sink::MetricSemantics;

pub type MetricLabel = (Cow<'static, str>, String);
type BoxedErr = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Finite histogram boundaries in seconds, covering operations up to one hour.
pub fn operation_duration_buckets_seconds() -> Vec<f64> {
    vec![
        1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 45.0, 60.0, 90.0, 120.0, 180.0, 240.0, 300.0, 450.0,
        600.0, 900.0, 1_200.0, 1_800.0, 3_600.0,
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
    telemetry_state: Arc<Mutex<telemetry::RegistryState>>,
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
            telemetry_state: Arc::new(Mutex::new(telemetry::RegistryState::default())),
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
        let mut registry = CollectorRegistry::new(id, self.telemetry_registry.clone(), prefix)?;
        registry.telemetry_state = Some(self.telemetry_state.clone());

        Ok(registry)
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

    /// Exports telemetry as uncompressed Prometheus text using the active registry.
    pub fn export_telemetry(&self) -> Result<String, HealthError> {
        let buffer = telemetry::export(self, None, &tokio_util::sync::CancellationToken::new())?;

        String::from_utf8(buffer).map_err(|error| {
            HealthError::GenericError(format!("telemetry text is not UTF-8: {error}"))
        })
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
    telemetry_state: Option<Arc<Mutex<telemetry::RegistryState>>>,
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
            telemetry_state: None,
        })
    }

    pub fn create_gauge_metrics(
        &self,
        id: String,
        help: impl Into<String>,
        static_labels: Vec<MetricLabel>,
    ) -> Result<Arc<GaugeMetrics>, prometheus::Error> {
        let help = help.into();

        let mut state = self
            .telemetry_state
            .as_ref()
            .and_then(|state| telemetry::lock_compact(state));

        let metrics = Arc::new(GaugeMetrics::new(
            id,
            &self.registry.registry,
            self.prefix.clone(),
            help,
            static_labels,
        )?);

        if let Some(state) = &mut state
            && !state.generic
        {
            state
                .gauges
                .insert((self.registry.desc.id, metrics.desc.id), metrics.clone());
        }

        Ok(metrics)
    }

    pub fn unregister_gauge_metrics(
        &self,
        metrics: &GaugeMetrics,
    ) -> Result<(), prometheus::Error> {
        let mut state = self
            .telemetry_state
            .as_ref()
            .and_then(|state| telemetry::lock_compact(state));

        self.registry
            .registry
            .unregister(Box::new(metrics.clone()))?;

        if let Some(state) = &mut state {
            state
                .gauges
                .remove(&(self.registry.desc.id, metrics.desc.id));
        }

        Ok(())
    }

    /// Exposes the registry for custom collectors.
    ///
    /// Accessing a telemetry registry selects generic encoding for that metrics
    /// manager's lifetime because retained registry handles may register collectors.
    pub fn registry(&self) -> &Registry {
        if let Some(state) = &self.telemetry_state {
            let mut state = telemetry::lock(state);
            state.generic = true;
            // Raw unregister operations bypass managed membership; the registry owns those readings.
            state.gauges.clear();
        }

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
        let mut state = self
            .telemetry_state
            .as_ref()
            .and_then(|state| telemetry::lock_compact(state));

        match self.parent.unregister(self.registry.clone()) {
            Ok(_) => {
                if let Some(state) = &mut state {
                    state
                        .gauges
                        .retain(|(owner, _), _| *owner != self.registry.desc.id);
                }
            }
            Err(error) => tracing::error!(
                ?error,
                collector_prefix = self.prefix().as_str(),
                "Could not properly drop registry for collector"
            ),
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
    semantics: MetricSemantics,
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
            semantics: MetricSemantics::Gauge,
            labels: Vec::new(),
        }
    }

    pub fn with_labels(mut self, labels: Vec<MetricLabel>) -> Self {
        self.labels.extend(labels);
        self
    }

    pub(crate) fn with_semantics(mut self, semantics: MetricSemantics) -> Self {
        self.semantics = semantics;
        self
    }
}

struct GaugeData {
    name: String,
    metric_type: String,
    unit: String,
    value: f64,
    semantics: MetricSemantics,
    labels: Vec<MetricLabel>,
    generation: u64,
}

#[derive(Clone)]
pub struct GaugeMetrics {
    gauges: Arc<DashMap<GaugeKey, Arc<GaugeData>>>,
    current_generation: Arc<AtomicU64>,
    metric_name_prefix: String,
    metric_help: String,
    static_labels: Arc<[proto::LabelPair]>,
    static_label_names: HashSet<String>,
    desc: Desc,
}

impl GaugeMetrics {
    /// Registers gauge readings with an ID unique to the registry. The registration
    /// identity does not add labels or change the names returned by `collect()`.
    /// `metric_help` supplies gauge `HELP`; counter aliases use the description
    /// in their sample's `MetricSemantics::CumulativeCounter`.
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

    /// Replaces a reading while existing export snapshots retain their captured value.
    pub fn record(&self, reading: GaugeReading) {
        let generation = self.current_generation.load(Ordering::Acquire);

        let next = GaugeData {
            name: reading.name,
            metric_type: reading.metric_type,
            unit: reading.unit,
            value: reading.value,
            semantics: reading.semantics,
            labels: reading.labels,
            generation,
        };

        match self.gauges.entry(reading.key) {
            Entry::Occupied(mut entry) => {
                // Snapshots retain shared readings; otherwise reuse the allocation.
                if let Some(current) = Arc::get_mut(entry.get_mut()) {
                    *current = next;
                } else {
                    entry.insert(Arc::new(next));
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(next));
            }
        }
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
        let mut families = HashMap::new();

        for gauge_ref in self.gauges.iter() {
            let data = gauge_ref.value();
            let mut labels = Vec::with_capacity(self.static_labels.len() + data.labels.len());
            labels.extend_from_slice(&self.static_labels);
            for (name, value) in &data.labels {
                let mut label = proto::LabelPair::new();
                label.set_name(name.as_ref().to_owned());
                label.set_value(value.clone());
                labels.push(label);
            }

            // Both series share one retained reading, so pruning removes both.
            // Replacement uses the latest value; invalid totals omit only the alias.
            let counter = match data.semantics {
                MetricSemantics::CumulativeCounter { description, .. }
                    if data.semantics.exports_counter(data.value) =>
                {
                    Some((proto::MetricType::COUNTER, labels.clone(), description))
                }
                _ => None,
            };
            for (kind, labels, help) in
                std::iter::once((proto::MetricType::GAUGE, labels, self.metric_help.as_str()))
                    .chain(counter)
            {
                let family_key = (
                    data.name.clone(),
                    data.metric_type.clone(),
                    data.unit.clone(),
                    kind,
                );
                let family = families.entry(family_key).or_insert_with(|| {
                    let mut name = format!(
                        "{}_{}_{}_{}",
                        self.metric_name_prefix, data.name, data.metric_type, data.unit
                    );
                    if kind == proto::MetricType::COUNTER {
                        name.push_str("_total");
                    }
                    let mut family = proto::MetricFamily::default();
                    family.set_name(name);
                    family.set_help(help.to_string());
                    family.set_field_type(kind);
                    family
                });

                let mut metric = proto::Metric::new();
                metric.set_label(labels);
                if kind == proto::MetricType::COUNTER {
                    let mut counter = proto::Counter::new();
                    counter.set_value(data.value);
                    metric.set_counter(counter);
                } else {
                    let mut gauge = proto::Gauge::new();
                    gauge.set_value(data.value);
                    metric.set_gauge(gauge);
                }
                family.mut_metric().push(metric);
            }
        }

        families.into_values().collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Identity,
    Gzip,
}

#[derive(Debug, thiserror::Error)]
#[error("telemetry request cancelled")]
struct RequestCancelled;

/// Serves operational metrics and uncompressed telemetry until the listener stops.
///
/// Dropping this future closes accepted connections and cancels their export jobs.
pub async fn run_metrics_server(
    metrics_endpoint: std::net::SocketAddr,
    metrics_manager: Arc<MetricsManager>,
) -> Result<(), BoxedErr> {
    run_metrics_server_with_config(
        metrics_endpoint,
        metrics_manager,
        crate::config::PrometheusSinkConfig::default(),
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}

/// Serve metrics until shutdown. Dropping the server future cancels accepted connections.
pub(crate) async fn run_metrics_server_with_config(
    addr: SocketAddr,
    manager: Arc<MetricsManager>,
    config: PrometheusSinkConfig,
    shutdown: CancellationToken,
) -> Result<(), BoxedErr> {
    let shutdown = shutdown.child_token();
    let _listener_guard = shutdown.clone().drop_guard();

    let listener = metrics_endpoint::bind_tcp_listener(addr).await?;

    tracing::info!(
        metrics_endpoint = %addr,
        "Metrics server listening (paths: /metrics, /telemetry, /livez)"
    );

    loop {
        let (stream, _) = tokio::select! {
            biased;

            () = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted?,
        };

        let manager = manager.clone();
        let config = config.clone();
        let connection = shutdown.child_token();

        tokio::spawn(serve_connection(stream, manager, config, connection));
    }
}

async fn serve_connection(
    stream: TcpStream,
    manager: Arc<MetricsManager>,
    config: PrometheusSinkConfig,
    connection: CancellationToken,
) {
    // Hyper's default half-close policy detects EOF while awaiting a response.
    // Dropping the connection or a pending request cancels its blocking export.
    let _connection_guard = connection.clone().drop_guard();
    let request_connection = connection.clone();

    let service = service_fn(move |req| {
        serve_request(
            req,
            manager.clone(),
            config.clone(),
            request_connection.clone(),
        )
    });

    let io = TokioIo::new(stream);

    tokio::select! {
        biased;

        () = connection.cancelled() => {},
        result = http1::Builder::new().serve_connection(io, service) => {
            if let Err(error) = result {
                tracing::error!(?error, "metrics server connection error");
            }
        }
    }
}

async fn serve_request<B>(
    req: Request<B>,
    manager: Arc<MetricsManager>,
    config: PrometheusSinkConfig,
    connection: CancellationToken,
) -> Result<Response<Full<Bytes>>, RequestCancelled> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/livez") => Ok(text_response(StatusCode::OK, "ok")),
        (&Method::GET, "/metrics") => Ok(serve_prometheus(
            manager.export_metrics().map(Bytes::from),
            "service metrics",
        )),
        (&Method::GET, "/telemetry") => {
            let mut response = serve_telemetry(req.headers(), manager, &config, connection).await?;

            if config.gzip {
                response
                    .headers_mut()
                    .insert(VARY, HeaderValue::from_static("Accept-Encoding"));
            }

            Ok(response)
        }
        (_, "/livez" | "/metrics" | "/telemetry") => {
            let mut response = text_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed; use GET",
            );

            response
                .headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET"));

            Ok(response)
        }
        _ => Ok(text_response(
            StatusCode::NOT_FOUND,
            "not found; use /metrics, /telemetry, or /livez",
        )),
    }
}

async fn serve_telemetry(
    headers: &HeaderMap,
    manager: Arc<MetricsManager>,
    config: &PrometheusSinkConfig,
    connection: CancellationToken,
) -> Result<Response<Full<Bytes>>, RequestCancelled> {
    let Some(encoding) = negotiate(headers, config.gzip) else {
        return Ok(text_response(
            StatusCode::NOT_ACCEPTABLE,
            "no acceptable telemetry representation",
        ));
    };

    let gzip_level = (encoding == Encoding::Gzip).then_some(config.gzip_level);
    let cancellation = connection.child_token();
    let _request_guard = cancellation.clone().drop_guard();
    let worker_cancellation = cancellation.clone();

    let job = tokio::task::spawn_blocking(move || {
        telemetry::export(&manager, gzip_level, &worker_cancellation)
    });

    let result = tokio::select! {
        biased;

        () = cancellation.cancelled() => return Err(RequestCancelled),
        result = job => result,
    };

    if cancellation.is_cancelled() {
        return Err(RequestCancelled);
    }

    let export = match result {
        Ok(result) => result.map(Bytes::from).map_err(HealthError::from),
        Err(error) => Err(HealthError::GenericError(format!(
            "telemetry export worker failed: {error}"
        ))),
    };

    let mut response = serve_prometheus(export, "telemetry metrics");

    if response.status().is_success() && encoding == Encoding::Gzip {
        response
            .headers_mut()
            .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }

    Ok(response)
}

fn serve_prometheus(
    export: Result<Bytes, HealthError>,
    export_name: &'static str,
) -> Response<Full<Bytes>> {
    let bytes = match export {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(?error, export_name, "error exporting prometheus metrics");

            let mut response = Response::new(Full::new(Bytes::from(format!(
                "error exporting {export_name}, see logs"
            ))));

            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;

            return response;
        }
    };

    let mut response = Response::new(Full::new(bytes));

    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(prometheus::TEXT_FORMAT),
    );

    response
}

fn text_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;

    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );

    response
}

fn negotiate(headers: &HeaderMap, gzip_enabled: bool) -> Option<Encoding> {
    let mut gzip = None;
    let mut identity = None;
    let mut wildcard = None;

    for entry in headers
        .get_all(ACCEPT_ENCODING)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .filter_map(|entry| std::str::from_utf8(entry).ok())
    {
        let Some((coding, weight)) = parse_coding(entry) else {
            continue;
        };

        let quality = match coding.to_ascii_lowercase().as_str() {
            "gzip" => &mut gzip,
            "identity" => &mut identity,
            "*" => &mut wildcard,
            _ => continue,
        };

        // A valid exclusion wins over duplicate positive preferences.
        *quality = Some(match *quality {
            Some(0) => 0,
            Some(previous) if weight > 0 => previous.max(weight),
            _ => weight,
        });
    }

    let gzip_weight = if gzip_enabled {
        gzip.or(wildcard).unwrap_or(0)
    } else {
        0
    };

    let identity_weight = identity.unwrap_or(if wildcard == Some(0) { 0 } else { 1000 });

    if gzip_weight > 0 && (identity.is_none() || gzip_weight >= identity_weight) {
        return Some(Encoding::Gzip);
    }

    (identity_weight > 0).then_some(Encoding::Identity)
}

fn parse_coding(entry: &str) -> Option<(&str, u16)> {
    let mut parts = entry.split(';');
    let coding = parts.next()?.trim();

    if coding.is_empty()
        || !coding
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return None;
    }

    let Some(parameter) = parts.next() else {
        return Some((coding, 1000));
    };

    if parts.next().is_some() {
        return None;
    }

    let (name, value) = parameter.trim().split_once('=')?;

    if !name.trim().eq_ignore_ascii_case("q") {
        return None;
    }

    let value = value.trim();
    let (whole, decimal) = value.split_once('.').unwrap_or((value, ""));

    if decimal.len() > 3 || !decimal.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let weight = match whole {
        "0" => {
            let fraction = if decimal.is_empty() {
                0
            } else {
                decimal.parse::<u16>().ok()?
            };

            let scale = match decimal.len() {
                0 | 3 => 1,
                1 => 100,
                _ => 10,
            };

            fraction * scale
        }
        "1" if decimal.bytes().all(|byte| byte == b'0') => 1000,
        _ => return None,
    };

    Some((coding, weight))
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

    use super::*;

    #[test]
    fn operation_duration_histogram_resolves_slow_collections() {
        check_values(
            [
                Check {
                    scenario: "routine slow log collection",
                    input: 400.0,
                    expect: Some(450.0),
                },
                Check {
                    scenario: "one-hour collection",
                    input: 3_600.0,
                    expect: Some(3_600.0),
                },
            ],
            |duration| {
                let histogram = prometheus::Histogram::with_opts(
                    HistogramOpts::new("test_operation_duration_seconds", "Operation duration")
                        .buckets(operation_duration_buckets_seconds()),
                )
                .expect("operation duration histogram should initialize");
                histogram.observe(duration);

                histogram.collect()[0].get_metric()[0]
                    .get_histogram()
                    .get_bucket()
                    .iter()
                    .find(|bucket| bucket.cumulative_count() == 1)
                    .map(|bucket| bucket.upper_bound())
            },
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
    fn cumulative_readings_preserve_gauges_and_counter_aliases() {
        let exported_families = |replacement| {
            let registry = Registry::new();
            let readings = GaugeMetrics::new(
                "device".to_string(),
                &registry,
                "custom",
                "Device readings",
                vec![("endpoint", "device")],
            )
            .expect("reading collector");
            let reading = |value| {
                GaugeReading::new("device", "hardware", "errors", "count", value)
                    .with_labels(vec![("component".into(), "pcie".to_string())])
                    .with_semantics(MetricSemantics::CumulativeCounter {
                        description: "Number of PCIe errors reported by the device",
                        start_time: Some(std::time::UNIX_EPOCH),
                    })
            };
            readings.record(
                GaugeReading::new("replays", "hardware", "replays", "count", 20.0)
                    .with_labels(vec![("component".into(), "pcie".to_string())])
                    .with_semantics(MetricSemantics::CumulativeCounter {
                        description: "Number of PCIe replays reported by the device",
                        start_time: None,
                    }),
            );
            [10.0, replacement].map(|value| {
                readings.record(reading(value));
                registry
                    .gather()
                    .into_iter()
                    .map(|family| {
                        let points = family
                            .get_metric()
                            .iter()
                            .map(|metric| {
                                let value = if family.get_field_type() == proto::MetricType::GAUGE {
                                    metric.get_gauge().value()
                                } else {
                                    metric.get_counter().value()
                                };
                                let labels = metric
                                    .get_label()
                                    .iter()
                                    .map(|label| {
                                        (label.name().to_string(), label.value().to_string())
                                    })
                                    .collect::<Vec<_>>();
                                // Bits preserve NaN so the unchanged gauge remains comparable.
                                (value.to_bits(), labels)
                            })
                            .collect::<Vec<_>>();
                        (
                            family.name().to_string(),
                            family.get_field_type(),
                            family.help().to_string(),
                            points,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        };

        check_values(
            [
                ("positive absolute total", 10.0, 2),
                ("replacement with zero", 0.0, 2),
                ("negative total", -1.0, 1),
                ("NaN total", f64::NAN, 1),
                ("positive infinity", f64::INFINITY, 1),
            ]
            .map(|(scenario, value, family_count)| Check {
                scenario,
                input: value,
                expect: [(10.0, 2), (value, family_count)].map(|(value, family_count)| {
                    [
                        (
                            "custom_hardware_errors_count",
                            proto::MetricType::GAUGE,
                            "Device readings",
                            value,
                        ),
                        (
                            "custom_hardware_errors_count_total",
                            proto::MetricType::COUNTER,
                            "Number of PCIe errors reported by the device",
                            value,
                        ),
                    ]
                    .into_iter()
                    .take(family_count)
                    .chain([
                        (
                            "custom_hardware_replays_count",
                            proto::MetricType::GAUGE,
                            "Device readings",
                            20.0,
                        ),
                        (
                            "custom_hardware_replays_count_total",
                            proto::MetricType::COUNTER,
                            "Number of PCIe replays reported by the device",
                            20.0,
                        ),
                    ])
                    .map(|(name, kind, help, value)| {
                        (
                            name.to_string(),
                            kind,
                            help.to_string(),
                            vec![(
                                value.to_bits(),
                                vec![
                                    ("endpoint".to_string(), "device".to_string()),
                                    ("component".to_string(), "pcie".to_string()),
                                ],
                            )],
                        )
                    })
                    .collect::<Vec<_>>()
                }),
            }),
            exported_families,
        );
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

#[cfg(test)]
#[path = "metrics/server_tests.rs"]
mod server_tests;
