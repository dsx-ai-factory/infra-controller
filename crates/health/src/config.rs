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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Debug;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::providers::{Env, Format, Serialized, Toml};
use figment::value::{Dict, Map, Value};
use figment::{Error as FigmentError, Figment, Metadata, Profile, Provider};
use mac_address::MacAddress;
use rustls_pki_types::DnsName;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Semaphore;
use url::Url;

use crate::metrics::BmcLatencyAttribute;

const DEFAULT_BMC_REQUEST_CONCURRENCY: NonZeroUsize = NonZeroUsize::MIN.saturating_add(3);
const ENDPOINT_SOURCES_CONFIG_KEY: &str = "endpoint_sources";
const NICO_API_CONFIG_KEY: &str = "nico_api";
const CARBIDE_API_CONFIG_ALIAS: &str = "carbide_api";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub endpoint_sources: EndpointSourcesConfig,

    pub tls: TlsConfig,

    pub sinks: SinksConfig,

    pub rate_limit: Configurable<RateLimitConfig>,

    pub collectors: CollectorsConfig,

    /// Opt-in attributes attached to emitted telemetry beyond what a collector
    /// reports on its own.
    pub attributes: AttributesConfig,

    pub processors: ProcessorsConfig,

    pub metrics: MetricsConfig,

    /// Shard ordinal for this instance
    pub shard: usize,

    /// Total number of shards in the StatefulSet
    pub shards_count: usize,

    /// Maximum cache size per BMC, uses etags
    pub cache_size: usize,

    /// Maximum concurrent Redfish operations per BMC, including collector
    /// fan-out. Changes take effect when the service restarts and rebuilds BMC
    /// clients. The value must not exceed [`Semaphore::MAX_PERMITS`], which
    /// bounds SSE event-record buffering.
    pub bmc_request_concurrency: NonZeroUsize,

    /// Interval between BMC endpoint discovery iterations.
    #[serde(with = "humantime_serde")]
    pub endpoint_discovery_interval: Duration,

    /// BMC proxy URL
    pub bmc_proxy_url: Option<Url>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint_sources: EndpointSourcesConfig::default(),
            tls: TlsConfig::default(),
            sinks: SinksConfig::default(),
            rate_limit: Configurable::Enabled(RateLimitConfig::default()),
            collectors: CollectorsConfig::default(),
            attributes: AttributesConfig::default(),
            processors: ProcessorsConfig::default(),
            metrics: MetricsConfig::default(),
            shard: 0,
            shards_count: 1,
            cache_size: 100,
            bmc_request_concurrency: DEFAULT_BMC_REQUEST_CONCURRENCY,
            endpoint_discovery_interval: Duration::from_secs(300),
            bmc_proxy_url: None,
        }
    }
}

/// Opt-in identity attributes attached to emitted telemetry.
///
/// These are attached as log record attributes and metric datapoint attributes
/// rather than resource attributes: a single BMC endpoint fronts many GPUs, so
/// per-GPU identity cannot be a property of the resource without changing how
/// telemetry is grouped.
///
/// Defaults to disabled, so a deployment opts in to the added metric labels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AttributesConfig {
    /// Attach `gpu_uuid`, `gpu_serial`, `gpu_chassis_serial`, and `gpu_model`,
    /// read from the Redfish resource that reports each GPU.
    ///
    /// On metrics these land on the sensor series that already exist for the
    /// GPU's processor and chassis. On SSE log records they are resolved by
    /// matching `origin_of_condition` against the discovered inventory. Both
    /// read resources discovery already fetches, so no Redfish requests are
    /// added.
    pub gpu_identity: bool,
}

/// Configuration for where BMC endpoints are discovered from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointSourcesConfig {
    /// NICo API endpoint discovery settings.
    #[serde(rename = "nico_api", alias = "carbide_api")]
    pub carbide_api: Configurable<CarbideApiConnectionConfig>,

    /// Static BMC endpoints, including switch host endpoints.
    pub static_bmc_endpoints: Vec<StaticBmcEndpoint>,

    /// Static switch host endpoints. Each entry requires `switch`
    /// metadata. The endpoint role defaults to host and must be host when set.
    pub static_switch_host_endpoints: Vec<StaticBmcEndpoint>,

    /// Cluster inventory file source (file or cluster manager JSON RPC)
    pub cluster: Configurable<ClusterEndpointSourceConfig>,
}

impl Default for EndpointSourcesConfig {
    fn default() -> Self {
        Self {
            carbide_api: Configurable::Enabled(CarbideApiConnectionConfig::default()),
            static_bmc_endpoints: Vec::new(),
            static_switch_host_endpoints: Vec::new(),
            cluster: Configurable::Disabled,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterEndpointSourceConfig {
    /// Path to a JSON inventory file containing BMC endpoints and credentials for the cluster.
    /// Used when `cluster_manager_url` is absent.
    #[serde(default)]
    pub inventory_path: PathBuf,

    /// Cluster manager head-node URL (e.g. https://10.x.x.x:8081).
    /// When set, inventory and credentials are fetched live via cluster manager JSON RPC
    /// instead of reading `inventory_path`.
    #[serde(default)]
    pub cluster_manager_url: Option<url::Url>,

    /// Cluster manager partition to read bmcsettings from (default: "base").
    /// The cluster manager stores BMC username/password at partition level; this selects which partition.
    #[serde(default = "default_cluster_manager_partition")]
    pub cluster_manager_partition: String,

    /// Fallback BMC username if cluster manager JSON RPC does not return one.
    /// Cluster manager default is "bright" (set during head-node installation).
    #[serde(default = "default_cluster_manager_username")]
    pub default_username: String,

    /// Fallback BMC password if cluster manager JSON RPC does not return one.
    /// Must be set explicitly — no code-level default.
    #[serde(default)]
    pub default_password: Option<String>,

    /// Optional BMC port override. None uses the BmcClient default (443/HTTPS).
    #[serde(default)]
    pub port: Option<u16>,
}

fn default_cluster_manager_partition() -> String {
    "base".to_string()
}

fn default_cluster_manager_username() -> String {
    "bright".to_string()
}

impl Default for ClusterEndpointSourceConfig {
    fn default() -> Self {
        Self {
            inventory_path: PathBuf::default(),
            cluster_manager_url: None,
            cluster_manager_partition: default_cluster_manager_partition(),
            default_username: default_cluster_manager_username(),
            default_password: None,
            port: None,
        }
    }
}

impl ClusterEndpointSourceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.cluster_manager_url.is_none() && self.inventory_path.as_os_str().is_empty() {
            return Err(
                "cluster endpoint source requires either `inventory_path` or `cluster_manager_url`"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl std::fmt::Debug for ClusterEndpointSourceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterEndpointSourceConfig")
            .field("inventory_path", &self.inventory_path)
            .field("cluster_manager_url", &self.cluster_manager_url)
            .field("cluster_manager_partition", &self.cluster_manager_partition)
            .field("default_username", &self.default_username)
            .field(
                "default_password",
                &self.default_password.as_ref().map(|_| "<redacted>"),
            )
            .field("port", &self.port)
            .finish()
    }
}

/// A single static BMC endpoint configuration.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StaticBmcEndpoint {
    pub ip: IpAddr,

    /// Optional Redfish or NVUE REST HTTPS port, depending on endpoint role.
    #[serde(default)]
    pub port: Option<u16>,
    pub mac: String,
    pub username: String,
    pub password: Option<String>,
    pub machine: Option<StaticMachineEndpoint>,
    pub power_shelf: Option<StaticPowerShelfEndpoint>,
    pub switch: Option<StaticSwitchEndpoint>,
    pub rack_id: Option<String>,
    /// User-defined labels attached to telemetry emitted for this endpoint.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StaticMachineEndpoint {
    /// Stable NICo machine ID for this BMC endpoint. Optional when running without NICo.
    pub id: Option<String>,

    /// Optional chassis serial to emit as machine telemetry metadata.
    pub serial: Option<String>,

    /// Optional uniform GPU driver version to emit for local/static validation.
    pub driver_version: Option<String>,

    #[serde(alias = "physical_slot_number")]
    pub slot_number: Option<i32>,

    #[serde(alias = "compute_tray_index")]
    pub tray_index: Option<i32>,

    /// Optional NVLink domain UUID associated with this machine.
    pub nvlink_domain_uuid: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StaticPowerShelfEndpoint {
    pub id: Option<String>,
    pub serial: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticSwitchEndpointRole {
    Bmc,
    Host,
}

fn default_static_switch_endpoint_role() -> StaticSwitchEndpointRole {
    StaticSwitchEndpointRole::Host
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StaticSwitchEndpoint {
    pub id: Option<String>,
    pub serial: Option<String>,
    #[serde(alias = "physical_slot_number")]
    pub slot_number: Option<i32>,
    #[serde(alias = "compute_tray_index")]
    pub tray_index: Option<i32>,

    /// Optional non-nil NVLink domain UUID associated with this switch.
    /// Invalid or nil values are omitted from telemetry.
    pub nvlink_domain_uuid: Option<String>,

    #[serde(default = "default_static_switch_endpoint_role")]
    pub endpoint_role: StaticSwitchEndpointRole,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub nmxc_enabled: Option<bool>,
    #[serde(default)]
    pub nmxt_enabled: Option<bool>,
}

impl Debug for StaticBmcEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticBmcEndpoint")
            .field("ip", &self.ip)
            .field("port", &self.port)
            .field("mac", &self.mac)
            .field("machine", &self.machine)
            .field("power_shelf", &self.power_shelf)
            .field("switch", &self.switch)
            .field("rack_id", &self.rack_id)
            .field("labels", &self.labels)
            .finish()
    }
}

impl StaticBmcEndpoint {
    fn identity_count(&self) -> usize {
        usize::from(self.machine.is_some())
            + usize::from(self.power_shelf.is_some())
            + usize::from(self.switch.is_some())
    }

    fn validate(&self, config_path: &str, index: usize) -> Result<(), String> {
        if self.identity_count() > 1 {
            return Err(format!(
                "{config_path}[{index}] must specify at most one of machine, power_shelf, or switch"
            ));
        }

        if let Some(power_shelf) = &self.power_shelf
            && power_shelf.id.is_none()
            && power_shelf.serial.is_none()
        {
            return Err(format!(
                "{config_path}[{index}].power_shelf requires id or serial"
            ));
        }

        if let Some(switch) = &self.switch
            && switch.id.is_none()
            && switch.serial.is_none()
        {
            return Err(format!(
                "{config_path}[{index}].switch requires id or serial"
            ));
        }

        const RESERVED_LABELS: &[&str] = &[
            "collector_type",
            "endpoint_ip",
            "endpoint_key",
            "endpoint_mac",
            "machine_id",
            "machine_slot_number",
            "machine_tray_index",
            "nvlink_domain_uuid",
            "power_shelf_id",
            "rack_id",
            "serial_number",
            "switch_id",
            "switch_slot_number",
            "switch_tray_index",
            "system_uuid",
        ];

        if self.labels.len() > 32 {
            return Err(format!(
                "{config_path}[{index}].labels supports at most 32 labels"
            ));
        }

        for (name, value) in &self.labels {
            let mut chars = name.chars();
            let valid_start = chars
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
            let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
            if !valid_start || !valid_rest {
                return Err(format!(
                    "{config_path}[{index}].labels key {name:?} must match [a-zA-Z_][a-zA-Z0-9_]*"
                ));
            }
            if RESERVED_LABELS.contains(&name.as_str()) {
                return Err(format!(
                    "{config_path}[{index}].labels key {name:?} is reserved"
                ));
            }
            if value.len() > 1024 {
                return Err(format!(
                    "{config_path}[{index}].labels value for {name:?} exceeds 1024 bytes"
                ));
            }
        }

        Ok(())
    }
}

impl EndpointSourcesConfig {
    fn validate(&self) -> Result<(), String> {
        const BMC_ENDPOINTS_PATH: &str = "endpoint_sources.static_bmc_endpoints";
        const SWITCH_HOST_ENDPOINTS_PATH: &str = "endpoint_sources.static_switch_host_endpoints";

        let mut static_macs = HashMap::new();

        for (index, endpoint) in self.static_bmc_endpoints.iter().enumerate() {
            endpoint.validate(BMC_ENDPOINTS_PATH, index)?;

            let mac = endpoint.mac.parse::<MacAddress>().map_err(|_| {
                format!("{BMC_ENDPOINTS_PATH}[{index}].mac must be a valid MAC address")
            })?;

            static_macs
                .entry(mac)
                .or_insert((BMC_ENDPOINTS_PATH, index));
        }

        for (index, endpoint) in self.static_switch_host_endpoints.iter().enumerate() {
            endpoint.validate(SWITCH_HOST_ENDPOINTS_PATH, index)?;

            let Some(switch) = endpoint.switch.as_ref() else {
                return Err(format!(
                    "{SWITCH_HOST_ENDPOINTS_PATH}[{index}] requires switch metadata"
                ));
            };

            if switch.endpoint_role != StaticSwitchEndpointRole::Host {
                return Err(format!(
                    "{SWITCH_HOST_ENDPOINTS_PATH}[{index}].switch.endpoint_role must be host"
                ));
            }

            let mac = endpoint.mac.parse::<MacAddress>().map_err(|_| {
                format!("{SWITCH_HOST_ENDPOINTS_PATH}[{index}].mac must be a valid MAC address")
            })?;

            if let Some((existing_path, existing_index)) = static_macs.get(&mac) {
                return Err(format!(
                    "{SWITCH_HOST_ENDPOINTS_PATH}[{index}].mac duplicates {existing_path}[{existing_index}].mac"
                ));
            }

            static_macs.insert(mac, (SWITCH_HOST_ENDPOINTS_PATH, index));
        }

        if let Configurable::Enabled(cluster) = &self.cluster {
            cluster.validate()?;
        }

        Ok(())
    }
}

/// Configuration for output sinks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SinksConfig {
    /// Tracing sink: logs collector events through `tracing`.
    ///
    /// Reachability metrics are excluded because the collector emits explicit
    /// structured records according to its `log_mode`.
    pub tracing: Configurable<TracingSinkConfig>,

    /// Prometheus sink: stores metric events in Prometheus exporter format.
    pub prometheus: Configurable<PrometheusSinkConfig>,

    /// Health report sink: sends health report events to the NICo API.
    #[serde(alias = "carbide_override", alias = "health_override")]
    pub health_report: Configurable<HealthReportSinkConfig>,

    /// Rack health report sink: sends rack-level health reports to the NICo API.
    #[serde(alias = "rack_health_override")]
    pub rack_health_report: Configurable<RackHealthReportSinkConfig>,

    /// Switch health report sink: sends switch-level health reports to the NICo API.
    #[serde(alias = "switch_health_override")]
    pub switch_health_report: Configurable<SwitchHealthReportSinkConfig>,

    /// Sends generated NMX-C domain health reports to the NICo API.
    ///
    /// This sink is disabled by default and cannot be combined with the NMX-C
    /// schema override collector.
    pub nvlink_domain_health_report: Configurable<NvLinkDomainHealthReportSinkConfig>,

    /// Power shelf health report sink: sends power-shelf-level health reports to the NICo API.
    #[serde(alias = "power_shelf_health_override")]
    pub power_shelf_health_report: Configurable<PowerShelfHealthReportSinkConfig>,

    /// Log file sink: writes log events as JSONL to rotating files on disk.
    pub log_file: Configurable<LogFileSinkConfig>,

    /// OTLP log export sink: streams events to an OpenTelemetry collector via gRPC.
    pub otlp: Configurable<OtlpSinkConfig>,
}

impl Default for SinksConfig {
    fn default() -> Self {
        Self {
            tracing: Configurable::Disabled,
            prometheus: Configurable::Enabled(PrometheusSinkConfig::default()),
            health_report: Configurable::Enabled(HealthReportSinkConfig::default()),
            rack_health_report: Configurable::Enabled(RackHealthReportSinkConfig::default()),
            switch_health_report: Configurable::Enabled(SwitchHealthReportSinkConfig::default()),
            nvlink_domain_health_report: Configurable::Disabled,
            power_shelf_health_report: Configurable::Enabled(
                PowerShelfHealthReportSinkConfig::default(),
            ),
            log_file: Configurable::Disabled,
            otlp: Configurable::Disabled,
        }
    }
}

impl SinksConfig {
    /// Returns true when at least one enabled sink consumes log events.
    pub fn includes_log_events(&self) -> bool {
        self.tracing.is_enabled() || self.log_file.is_enabled() || self.otlp.is_enabled()
    }

    /// Returns true when at least one diagnostic-capable sink opts in.
    pub fn includes_log_diagnostics(&self) -> bool {
        self.tracing
            .as_option()
            .is_some_and(|config| config.include_diagnostics)
            || self
                .log_file
                .as_option()
                .is_some_and(|config| config.include_diagnostics)
            || self
                .otlp
                .as_option()
                .is_some_and(OtlpSinkConfig::includes_diagnostics)
    }
}

/// Configuration for the tracing sink.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TracingSinkConfig {
    /// Emit Redfish diagnostic payload fields.
    ///
    /// Disabled by default because payload bodies are opaque and may be large or
    /// sensitive. If no diagnostic-capable sink enables this, collectors do not
    /// attach diagnostic fields.
    pub include_diagnostics: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrometheusSinkConfig {}

/// Configuration for the JSONL log file sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFileSinkConfig {
    /// Directory where rotated health log files are written.
    pub output_dir: String,

    /// Maximum bytes per active log file before rotation.
    pub max_file_size: u64,

    /// Number of rotated backup files to retain.
    pub max_backups: usize,

    /// Write Redfish diagnostic payload fields.
    ///
    /// Disabled by default because payload bodies are opaque and may be large or
    /// sensitive. If no diagnostic-capable sink enables this, collectors do not
    /// attach diagnostic fields.
    pub include_diagnostics: bool,
}

impl Default for LogFileSinkConfig {
    fn default() -> Self {
        Self {
            include_diagnostics: false,
            output_dir: "/tmp/logs".to_string(),
            max_file_size: 104_857_600, // 100MB
            max_backups: 5,
        }
    }
}

/// Configures OTLP/gRPC fan-out to independent targets.
///
/// Each supported log and metric is sent to every target. Targets own separate
/// queues and drain tasks, so a slow or unavailable destination does not block
/// delivery to another destination.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OtlpSinkConfig {
    /// Destinations that receive OTLP logs and metrics.
    ///
    /// At least one target is required when the sink is enabled.
    pub targets: Vec<OtlpTargetConfig>,
}

impl OtlpSinkConfig {
    fn includes_diagnostics(&self) -> bool {
        self.targets.iter().any(|target| target.include_diagnostics)
    }
}

/// Delivery and batching policy for one OTLP/gRPC destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpTargetConfig {
    /// Endpoint URI that receives both logs and metrics over OTLP/gRPC.
    pub endpoint: String,

    /// Optional TLS or mTLS configuration for this endpoint.
    ///
    /// Omit this table for HTTPS endpoints that use platform trust roots. A
    /// configured profile supplies a private CA, and supplying a client
    /// certificate and key additionally enables mTLS.
    pub tls: Option<OtlpTlsConfig>,

    /// Maximum number of events or samples exported per request. Defaults to
    /// 512.
    #[serde(default = "OtlpTargetConfig::default_batch_size")]
    pub batch_size: usize,

    /// Maximum number of entries waiting in each signal queue for this target.
    ///
    /// Logs and metrics have independent queues of this size. When a queue is
    /// full, inserting a new identity drops its oldest entry. The active export
    /// batch is bounded separately by `batch_size`. Defaults to 32,768 and must
    /// be greater than zero.
    #[serde(default = "OtlpTargetConfig::default_queue_capacity")]
    pub queue_capacity: usize,

    /// Maximum time to wait before flushing a non-empty batch for either
    /// signal. Defaults to two seconds.
    #[serde(
        default = "OtlpTargetConfig::default_flush_interval",
        with = "humantime_serde"
    )]
    pub flush_interval: std::time::Duration,

    /// Export collector diagnostic payload fields to this target.
    ///
    /// Disabled by default because payload bodies are opaque and may be large or
    /// sensitive. If no diagnostic-capable sink enables diagnostics, collectors
    /// do not attach diagnostic fields. OTLP exports parent logs normally and
    /// uses the parent event identity for queue replacement while backed up.
    /// This setting does not control descriptor-decoded NMX-C payloads.
    #[serde(default)]
    pub include_diagnostics: bool,

    /// Emit per-alert detail on health report log records sent to this target.
    ///
    /// Disabled by default because alert messages are free-form and a fully
    /// degraded endpoint can produce many of them. When enabled, health report
    /// records carry a `health_report.alerts` attribute holding a JSON array.
    #[serde(default)]
    pub include_alert_details: bool,
}

impl OtlpTargetConfig {
    pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 32_768;

    fn default_batch_size() -> usize {
        512
    }

    fn default_queue_capacity() -> usize {
        Self::DEFAULT_QUEUE_CAPACITY
    }

    fn default_flush_interval() -> std::time::Duration {
        std::time::Duration::from_secs(2)
    }

    fn validate(&self, index: usize) -> Result<(), String> {
        let path = format!("sinks.otlp.targets[{index}]");

        if self.batch_size == 0 {
            return Err(format!("{path}.batch_size must be greater than 0"));
        }

        if self.queue_capacity == 0 {
            return Err(format!("{path}.queue_capacity must be greater than 0"));
        }

        if self.flush_interval.is_zero() {
            return Err(format!("{path}.flush_interval must be greater than 0"));
        }

        let endpoint = tonic::transport::Channel::from_shared(self.endpoint.clone())
            .map_err(|_| format!("invalid {path}.endpoint: {}", self.endpoint))?;

        if let Some(tls) = &self.tls {
            if endpoint.uri().scheme_str() != Some("https") {
                return Err(format!("{path}.tls requires an https endpoint"));
            }

            tls.validate(&path)?;
        }

        Ok(())
    }
}

/// TLS policy for one OTLP/gRPC target.
///
/// The CA bundle verifies the server certificate. Supplying both client paths
/// adds a client identity and enables mTLS. Each signal drain periodically
/// reloads the certificate files and adopts them only after a replacement
/// connection succeeds. A failed reload leaves the current connection active.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtlpTlsConfig {
    /// Path to the CA bundle used to verify the OTLP server certificate.
    pub ca_cert_path: PathBuf,

    /// Optional path to the client certificate chain for mTLS.
    pub client_cert_path: Option<PathBuf>,

    /// Optional path to the client private key for mTLS.
    pub client_key_path: Option<PathBuf>,

    /// Optional DNS name used for TLS SNI and server certificate verification.
    pub tls_server_name: Option<String>,

    /// Interval between reloads of this target's TLS files. Defaults to five
    /// minutes.
    #[serde(
        default = "OtlpTlsConfig::default_reload_interval",
        with = "humantime_serde"
    )]
    pub reload_interval: Duration,
}

impl OtlpTlsConfig {
    /// Default interval between attempts to reload an OTLP target's TLS files.
    pub(crate) const DEFAULT_RELOAD_INTERVAL: Duration = Duration::from_secs(5 * 60);

    fn default_reload_interval() -> Duration {
        Self::DEFAULT_RELOAD_INTERVAL
    }

    fn validate(&self, target_path: &str) -> Result<(), String> {
        let path = format!("{target_path}.tls");

        if self.ca_cert_path.as_os_str().is_empty() {
            return Err(format!("{path}.ca_cert_path must not be empty"));
        }

        match (&self.client_cert_path, &self.client_key_path) {
            (Some(client_cert_path), Some(client_key_path)) => {
                if client_cert_path.as_os_str().is_empty() {
                    return Err(format!("{path}.client_cert_path must not be empty"));
                }

                if client_key_path.as_os_str().is_empty() {
                    return Err(format!("{path}.client_key_path must not be empty"));
                }
            }
            (Some(_), None) => {
                return Err(format!(
                    "{path}.client_key_path must be set when {path}.client_cert_path is set"
                ));
            }
            (None, Some(_)) => {
                return Err(format!(
                    "{path}.client_cert_path must be set when {path}.client_key_path is set"
                ));
            }
            (None, None) => {}
        }

        if let Some(tls_server_name) = self.tls_server_name.as_deref() {
            if tls_server_name.trim().is_empty() {
                return Err(format!("{path}.tls_server_name must not be empty"));
            }

            if tls_server_name.trim() != tls_server_name {
                return Err(format!(
                    "{path}.tls_server_name must not contain leading or trailing whitespace"
                ));
            }

            DnsName::try_from(tls_server_name)
                .map_err(|_| format!("{path}.tls_server_name must be a valid DNS name"))?;
        }

        if self.reload_interval.is_zero() {
            return Err(format!("{path}.reload_interval must be greater than 0"));
        }

        Ok(())
    }
}

/// Shared NICo API connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CarbideApiConnectionConfig {
    /// Path to the root CA certificate for NICo API connections.
    pub root_ca: String,

    /// Path to the client certificate for NICo API connections.
    pub client_cert: String,

    /// Path to the client key for NICo API connections.
    pub client_key: String,

    /// NICo API server endpoint.
    pub api_url: Url,
}

impl Default for CarbideApiConnectionConfig {
    fn default() -> Self {
        Self {
            root_ca: "/var/run/secrets/spiffe.io/ca.crt".to_string(),
            client_cert: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            client_key: "/var/run/secrets/spiffe.io/tls.key".to_string(),
            api_url: Url::parse("https://nico-api.nico-system.svc.cluster.local:1079").unwrap(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthReportSinkConfig {
    #[serde(flatten)]
    pub connection: CarbideApiConnectionConfig,

    /// Number of concurrent workers submitting reports to the NICo API.
    pub workers: usize,

    /// Drop reports that contain no successes and no alerts before submitting them.
    pub skip_empty_reports: bool,

    /// Suppress re-sending a success-only health report whose content has not changed
    /// since the last send, until this interval elapses. Reports that contain any alert
    /// are always forwarded immediately regardless of this setting.
    /// Set to null or omit to disable suppression.
    #[serde(with = "humantime_serde::option", default)]
    pub suppress_unchanged_interval: Option<Duration>,
}

impl Default for HealthReportSinkConfig {
    fn default() -> Self {
        Self {
            connection: CarbideApiConnectionConfig::default(),
            workers: 4,
            skip_empty_reports: true,
            suppress_unchanged_interval: Some(Duration::from_secs(300)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RackHealthReportSinkConfig {
    #[serde(flatten)]
    pub connection: CarbideApiConnectionConfig,

    /// Number of concurrent workers submitting rack-level reports to the NICo API.
    pub workers: usize,

    /// Drop reports that contain no successes and no alerts before submitting them.
    pub skip_empty_reports: bool,
}

impl Default for RackHealthReportSinkConfig {
    fn default() -> Self {
        Self {
            connection: CarbideApiConnectionConfig::default(),
            workers: 2,
            skip_empty_reports: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwitchHealthReportSinkConfig {
    #[serde(flatten)]
    pub connection: CarbideApiConnectionConfig,

    /// Number of concurrent workers submitting switch-level reports to the NICo API.
    pub workers: usize,

    /// Drop reports that contain no successes and no alerts before submitting them.
    pub skip_empty_reports: bool,
}

impl Default for SwitchHealthReportSinkConfig {
    fn default() -> Self {
        Self {
            connection: CarbideApiConnectionConfig::default(),
            workers: 2,
            skip_empty_reports: true,
        }
    }
}

/// Configuration for ordered NVLink domain health report submission.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NvLinkDomainHealthReportSinkConfig {
    /// NICo API connection used to submit NVLink domain health reports.
    #[serde(flatten)]
    pub connection: CarbideApiConnectionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerShelfHealthReportSinkConfig {
    #[serde(flatten)]
    pub connection: CarbideApiConnectionConfig,

    /// Number of concurrent workers submitting power-shelf-level reports to the NICo API.
    pub workers: usize,

    /// Drop reports that contain no successes and no alerts before submitting them.
    pub skip_empty_reports: bool,
}

impl Default for PowerShelfHealthReportSinkConfig {
    fn default() -> Self {
        Self {
            connection: CarbideApiConnectionConfig::default(),
            workers: 2,
            skip_empty_reports: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    /// Burst value for explorations, optimal to set to max rate limit.
    pub bucket_burst: usize,

    /// Interval between bucket replenishment.
    /// Default value 30ms will rate limit for 2000 rpm.
    #[serde(with = "humantime_serde")]
    pub bucket_replenish: Duration,

    /// Maximum jitter added to exploration intervals.
    #[serde(with = "humantime_serde")]
    pub max_jitter: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CollectorsConfig {
    /// Entity discovery configuration
    pub discovery: DiscoveryConfig,

    /// Sensor collector configuration (if present, sensor collector is enabled)
    #[serde(alias = "health")]
    pub sensors: Configurable<SensorCollectorConfig>,

    /// Entity metrics collector configuration (if present, metrics collector is enabled)
    pub metrics: Configurable<MetricsCollectorConfig>,

    /// Redfish telemetry service collector configuration (if present, telemetry
    /// collector is enabled)
    pub telemetry: Configurable<TelemetryCollectorConfig>,

    /// Firmware collector configuration (if present, firmware collector is enabled)
    pub firmware: Configurable<FirmwareCollectorConfig>,

    /// Leak detector collector configuration (if present, leak detector collector is enabled)
    pub leak_detector: Configurable<LeakDetectorCollectorConfig>,

    /// Logs collector configuration (if present, logs collector is enabled)
    pub logs: Configurable<LogsCollectorConfig>,

    /// Switch NMX-T collector configuration (if present, nmxt collector is enabled)
    pub nmxt: Configurable<NmxtCollectorConfig>,

    /// NMX-C streaming collector configuration.
    pub nmxc: Configurable<NmxcCollectorConfig>,

    /// NVUE collector configuration for direct NVUE HTTP(s) polling of NVLink switches
    pub nvue: Configurable<NvueCollectorConfig>,

    /// GPU inventory collector: compares OOB GPU count vs the assigned SKU.
    pub gpu_inventory: Configurable<GpuInventoryConfig>,

    /// Direct TCP probes for ports used by enabled collectors.
    ///
    /// The probes report transport reachability only. They do not check TLS,
    /// authentication, or collector protocol readiness.
    pub reachability: Configurable<ReachabilityCollectorConfig>,
}

impl Default for CollectorsConfig {
    fn default() -> Self {
        Self {
            discovery: DiscoveryConfig::default(),
            sensors: Configurable::Enabled(SensorCollectorConfig::default()),
            metrics: Configurable::Disabled,
            telemetry: Configurable::Disabled,
            firmware: Configurable::Disabled,
            leak_detector: Configurable::Enabled(LeakDetectorCollectorConfig::default()),
            logs: Configurable::Disabled,
            nmxt: Configurable::Disabled,
            nmxc: Configurable::Disabled,
            nvue: Configurable::Disabled,
            gpu_inventory: Configurable::Disabled,
            reachability: Configurable::Disabled,
        }
    }
}

/// Controls structured log emission for TCP reachability probes.
///
/// Metrics are emitted for every completed probe regardless of this setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityLogMode {
    /// Emit a structured log for every successful and failed probe.
    All,

    /// Emit a structured log for every failed probe and suppress successful probes.
    #[default]
    Unreachable,
}

/// Global configuration for periodic TCP reachability probes.
///
/// The collector probes the BMC Redfish port when an enabled collector is
/// eligible to use it, plus ports used by enabled eligible switch collectors.
/// One global cadence applies to all targets; each collector's own configuration
/// remains the source of truth for whether its target and port exist. Each
/// completed probe is sent to configured metric sinks; configured log sinks
/// receive policy-selected records.
/// Results never create health reports. It is disabled by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReachabilityCollectorConfig {
    /// Interval between probe cycles for each endpoint.
    ///
    /// Every selected target is probed once per cycle and emits a state metric.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,

    /// Timeout for one TCP connection attempt.
    ///
    /// This bounds the TCP handshake only; it does not cover TLS or any
    /// collector-specific protocol exchange.
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,

    /// Selects which completed probes emit structured log records.
    ///
    /// This policy is stateless: `unreachable` emits every failed probe, including
    /// repeated failures after service restart. Metrics remain continuous in all modes.
    pub log_mode: ReachabilityLogMode,
}

impl Default for ReachabilityCollectorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(3),
            log_mode: ReachabilityLogMode::Unreachable,
        }
    }
}

impl ReachabilityCollectorConfig {
    /// Validates the probe cadence and timeout.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.interval.is_zero() {
            return Err("[collectors.reachability].interval must be greater than 0".to_string());
        }

        if self.timeout.is_zero() {
            return Err("[collectors.reachability].timeout must be greater than 0".to_string());
        }

        Ok(())
    }
}

/// TLS settings owned by hardware-health.
///
/// This section is intentionally outside `[collectors]` because TLS material is
/// connection policy shared by multiple collectors, not a collector itself.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TlsConfig {
    /// Optional mTLS profile used by direct switch collectors.
    pub switch: Option<MtlsProfileConfig>,
}

/// mTLS profile for outbound client TLS connections.
///
/// `[tls.switch]` uses this shape for direct switch collector connections.
/// These paths are independent of the NICo API certificate paths. The
/// files are read and validated when collectors build HTTP clients or gRPC
/// channel TLS configs. The optional TLS server name applies to every direct
/// switch collector that uses this profile. NICo API discovery does not provide
/// switch certificate identities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MtlsProfileConfig {
    /// Path to the CA bundle used to verify switch server certificates.
    pub ca_cert_path: PathBuf,

    /// Path to the client certificate chain sent to switch services.
    pub client_cert_path: PathBuf,

    /// Path to the client private key sent to switch services.
    pub client_key_path: PathBuf,

    /// Optional DNS name used only for TLS SNI and server certificate checks.
    ///
    /// Direct switch collectors still open TCP connections to each discovered
    /// switch endpoint IP. When all switch server certificates carry the same
    /// DNS subjectAltName, set this field so TLS verifies that DNS identity
    /// instead of requiring every switch certificate to include an IP SAN.
    /// This value is never used for endpoint discovery or DNS resolution.
    ///
    /// For HTTP collectors, the request URL and HTTP Host header stay on the
    /// discovered switch IP. Only the TLS server name changes.
    pub tls_server_name: Option<String>,
}

impl MtlsProfileConfig {
    fn validate(&self) -> Result<(), String> {
        if self.ca_cert_path.as_os_str().is_empty() {
            return Err("[tls.switch].ca_cert_path must not be empty".to_string());
        }

        if self.client_cert_path.as_os_str().is_empty() {
            return Err("[tls.switch].client_cert_path must not be empty".to_string());
        }

        if self.client_key_path.as_os_str().is_empty() {
            return Err("[tls.switch].client_key_path must not be empty".to_string());
        }

        if let Some(tls_server_name) = self.tls_server_name.as_deref() {
            if tls_server_name.trim().is_empty() {
                return Err("[tls.switch].tls_server_name must not be empty".to_string());
            }

            if tls_server_name.trim() != tls_server_name {
                return Err(
                    "[tls.switch].tls_server_name must not contain leading or trailing whitespace"
                        .to_string(),
                );
            }

            DnsName::try_from(tls_server_name)
                .map_err(|_| "[tls.switch].tls_server_name must be a valid DNS name".to_string())?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    #[serde(with = "humantime_serde")]
    pub refresh_interval: Duration,

    /// Maximum endpoints whose system identities are resolved concurrently.
    pub discovery_concurrency: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(300),
            discovery_concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsCollectorConfig {
    #[serde(with = "humantime_serde")]
    pub fetch_interval: Duration,
}

impl Default for MetricsCollectorConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryCollectorConfig {
    /// Interval between metric report fetches.
    ///
    /// A report is one GET covering many readings, so this can run
    /// tighter than the per-resource collectors without adding load
    /// proportional to entity count.
    #[serde(with = "humantime_serde")]
    pub fetch_interval: Duration,
}

impl Default for TelemetryCollectorConfig {
    fn default() -> Self {
        Self {
            fetch_interval: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuInventoryConfig {
    /// How often to re-check the GPU count against the assigned SKU. GPU
    /// population is near-static and each iteration re-reads the SKU from the
    /// NICo API, so this defaults to a low frequency (1h, matching the entity
    /// discovery refresh cadence) rather than hammering the API.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
}

impl Default for GpuInventoryConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessorsConfig {
    /// Leak detection processor configuration (if present, leak detection is enabled)
    pub leak_detection: Configurable<LeakDetectionProcessorConfig>,

    /// Rack-level leak processor: aggregates tray leak reports per rack.
    pub rack_leak: Configurable<RackLeakProcessorConfig>,
}

impl Default for ProcessorsConfig {
    fn default() -> Self {
        Self {
            leak_detection: Configurable::Enabled(LeakDetectionProcessorConfig::default()),
            rack_leak: Configurable::Enabled(RackLeakProcessorConfig::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LeakDetectionProcessorConfig {
    /// Minimum number of leak-detector alerts required in one report window
    /// to emit a derived leak health report.
    pub minimum_alerts_per_report: usize,
}

impl Default for LeakDetectionProcessorConfig {
    fn default() -> Self {
        Self {
            minimum_alerts_per_report: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RackLeakProcessorConfig {
    /// Number of leaking trays in a rack required to trigger a rack-level leak override.
    pub leaking_tray_threshold: usize,
}

impl Default for RackLeakProcessorConfig {
    fn default() -> Self {
        Self {
            leaking_tray_threshold: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorCollectorConfig {
    /// Interval between sensor fetch iterations.
    #[serde(with = "humantime_serde")]
    pub sensor_fetch_interval: Duration,

    /// Include sensor thresholds in the metrics attributes.
    pub include_sensor_thresholds: bool,
}

impl Default for SensorCollectorConfig {
    fn default() -> Self {
        Self {
            sensor_fetch_interval: Duration::from_secs(60),
            include_sensor_thresholds: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FirmwareCollectorConfig {
    /// Interval between firmware inventory refresh.
    #[serde(with = "humantime_serde")]
    pub firmware_refresh_interval: Duration,
}

impl Default for FirmwareCollectorConfig {
    fn default() -> Self {
        Self {
            firmware_refresh_interval: Duration::from_secs(60 * 60 * 2), // 2 hours
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LeakDetectorCollectorConfig {
    /// Interval between thermal subsystem leak detector polls.
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,

    /// Interval between thermal subsystem leak detector discovery refreshes.
    #[serde(with = "humantime_serde")]
    pub state_refresh_interval: Duration,
}

impl Default for LeakDetectorCollectorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            state_refresh_interval: Duration::from_secs(60 * 30),
        }
    }
}

/// How log events are collected from each BMC endpoint.
///
/// - `Auto` (default): tries SSE first, downgrades to periodic per-endpoint
///   when SSE is unsupported or keeps failing.
/// - `Sse`: SSE only, retries forever. Use when every BMC has `/EventService`.
/// - `Periodic`: polling only, no SSE attempt.
///
/// Downgrades are in-memory; restart the health service to retry SSE.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogCollectionMode {
    #[default]
    Auto,
    Sse,
    Periodic,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsCollectorConfig {
    pub mode: LogCollectionMode,

    pub sse: Option<SseLogConfig>,
    pub periodic: Option<PeriodicLogConfig>,
    pub auto: Option<AutoModeConfig>,
}

impl LogsCollectorConfig {
    pub fn sse_or_default(&self) -> SseLogConfig {
        self.sse.unwrap_or_default()
    }

    pub fn periodic_or_default(&self) -> PeriodicLogConfig {
        self.periodic.clone().unwrap_or_default()
    }

    pub fn auto_periodic_or_default(&self) -> PeriodicLogConfig {
        self.auto
            .as_ref()
            .map(|auto| auto.periodic.clone())
            .or_else(|| self.periodic.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SseLogConfig {
    /// Initial retry backoff after a streaming connection failure.
    #[serde(with = "humantime_serde")]
    pub initial_backoff: Duration,

    /// Maximum retry backoff after repeated streaming connection failures.
    #[serde(with = "humantime_serde")]
    pub max_backoff: Duration,
}

impl Default for SseLogConfig {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

impl SseLogConfig {
    fn validate(&self) -> Result<(), String> {
        if self.initial_backoff.is_zero() {
            return Err("[collectors.logs.sse].initial_backoff must be greater than 0".to_string());
        }

        if self.max_backoff.is_zero() {
            return Err("[collectors.logs.sse].max_backoff must be greater than 0".to_string());
        }

        if self.max_backoff < self.initial_backoff {
            return Err(
                "[collectors.logs.sse].max_backoff must be greater than or equal to initial_backoff"
                    .to_string(),
            );
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeriodicLogConfig {
    /// Interval between log collection.
    #[serde(with = "humantime_serde")]
    pub logs_collection_interval: Duration,

    /// Interval between log service state refresh.
    #[serde(with = "humantime_serde")]
    pub state_refresh_interval: Duration,

    /// Path to logs collector state file (supports {machine_id} placeholder).
    pub logs_state_file: String,

    /// Substrings matched against each Redfish LogService odata id; any service
    /// whose id contains one of these is skipped during discovery. Defaults to
    /// `["Journal"]` to suppress the bmcweb HTTP-access log, which is
    /// high-volume and self-referential. Set to `[]` to collect from every
    /// discovered LogService.
    #[serde(default = "default_excluded_log_services")]
    pub exclude_services: Vec<String>,

    /// When true, on the first encounter of a LogService with no saved state,
    /// anchor at the current highest entry ID without emitting existing entries.
    /// Subsequent polls collect only new entries, matching SSE real-time
    /// behaviour. Defaults to false (existing entries are collected on first
    /// run). Also applies when auto-mode downgrades to periodic collection.
    #[serde(default)]
    pub skip_initial_history: bool,
}

impl Default for PeriodicLogConfig {
    fn default() -> Self {
        Self {
            logs_collection_interval: Duration::from_secs(300),
            state_refresh_interval: Duration::from_secs(1800),
            logs_state_file: "/tmp/logs_collector_{machine_id}.json".to_string(),
            exclude_services: default_excluded_log_services(),
            skip_initial_history: false,
        }
    }
}

fn default_excluded_log_services() -> Vec<String> {
    vec!["Journal".to_string()]
}

/// downgrade thresholds and periodic fallback for `collectors.logs.mode = "auto"`.
/// sse_not_available is terminal (defaults to 1), everything else goes
/// through a rolling window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoModeConfig {
    pub sse_not_available_threshold: u32,
    #[serde(with = "humantime_serde")]
    pub connect_failure_window: Duration,
    pub connect_failure_threshold: u32,
    #[serde(default, flatten)]
    pub periodic: PeriodicLogConfig,
}

impl Default for AutoModeConfig {
    fn default() -> Self {
        Self {
            sse_not_available_threshold: 1,
            connect_failure_window: Duration::from_secs(300),
            connect_failure_threshold: 5,
            periodic: PeriodicLogConfig::default(),
        }
    }
}

impl AutoModeConfig {
    fn validate(&self) -> Result<(), String> {
        if self.sse_not_available_threshold == 0 {
            return Err(
                "[collectors.logs.auto].sse_not_available_threshold must be greater than 0"
                    .to_string(),
            );
        }
        if self.connect_failure_threshold == 0 {
            return Err(
                "[collectors.logs.auto].connect_failure_threshold must be greater than 0"
                    .to_string(),
            );
        }
        if self.connect_failure_window.is_zero() {
            return Err(
                "[collectors.logs.auto].connect_failure_window must be greater than 0".to_string(),
            );
        }

        Ok(())
    }
}

impl LogsCollectorConfig {
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            LogCollectionMode::Auto => {
                if let Some(auto) = &self.auto {
                    auto.validate()?;
                }
                if let Some(sse) = &self.sse {
                    sse.validate()?;
                }
            }
            LogCollectionMode::Periodic => {
                if self.auto.is_some() {
                    return Err(
                        "[collectors.logs.auto] should not be set when mode = \"periodic\""
                            .to_string(),
                    );
                }
                if self.periodic.is_none() {
                    return Err(
                        "[collectors.logs.periodic] is required when mode = \"periodic\""
                            .to_string(),
                    );
                }
                if self.sse.is_some() {
                    return Err(
                        "[collectors.logs.sse] should not be set when mode = \"periodic\""
                            .to_string(),
                    );
                }
            }
            LogCollectionMode::Sse => {
                if self.auto.is_some() {
                    return Err(
                        "[collectors.logs.auto] should not be set when mode = \"sse\"".to_string(),
                    );
                }
                if self.periodic.is_some() {
                    return Err(
                        "[collectors.logs.periodic] should not be set when mode = \"sse\""
                            .to_string(),
                    );
                }
                if let Some(sse) = &self.sse {
                    sse.validate()?;
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NmxtCollectorConfig {
    /// Interval between switch NMX-T metric scrapes.
    #[serde(with = "humantime_serde")]
    pub scrape_interval: Duration,

    /// Timeout for individual NMX-T HTTP requests.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,

    /// Dangerously disable TLS certificate verification for NMX-T HTTPS requests.
    ///
    /// Defaults to false so strict TLS verification remains the default.
    pub dangerously_skip_tls_verification: bool,
}

impl Default for NmxtCollectorConfig {
    fn default() -> Self {
        Self {
            scrape_interval: Duration::from_secs(60),
            request_timeout: Duration::from_secs(30),
            dangerously_skip_tls_verification: false,
        }
    }
}

const DEFAULT_NMX_C_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_NMX_C_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES: usize = 4 * 1024 * 1024;
const MAX_NMX_C_FRAME_SIZE_BYTES: usize = 64 * 1024 * 1024;

fn default_nmxc_hello_rpc_path() -> String {
    "/nmx_c.NMX_Controller/Hello".to_string()
}

fn default_nmxc_subscribe_rpc_path() -> String {
    "/nmx_c.NMX_Controller/Subscribe".to_string()
}

/// Descriptor-driven replacement configuration for the generated NMX-C collector.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NmxcSchemaOverrideConfig {
    /// Required path to a complete binary protobuf `FileDescriptorSet`.
    ///
    /// The set must contain the configured methods and all imported descriptors.
    /// Relative paths are resolved from the service process working directory.
    /// This field has no default.
    pub descriptor_set_path: PathBuf,

    /// Optional fully qualified gRPC path for the NMX-C Hello method.
    ///
    /// The path must use `/package.Service/Method` form and identify a unary
    /// method in the descriptor. Defaults to `/nmx_c.NMX_Controller/Hello`.
    #[serde(default = "default_nmxc_hello_rpc_path")]
    pub hello_rpc_path: String,

    /// Optional fully qualified gRPC path for the NMX-C Subscribe method.
    ///
    /// The path must use `/package.Service/Method` form and identify a method
    /// that accepts one request and streams responses. Defaults to
    /// `/nmx_c.NMX_Controller/Subscribe`.
    #[serde(default = "default_nmxc_subscribe_rpc_path")]
    pub subscribe_rpc_path: String,

    /// Optional maximum encoded size accepted for one Hello or Subscribe response.
    ///
    /// Defaults to 4 MiB when omitted. Valid values are 1 byte through 64 MiB.
    #[serde(default = "default_nmxc_max_frame_size_bytes")]
    pub max_frame_size_bytes: usize,

    /// Optional additional `SubscribeRequest` fields.
    ///
    /// Keys and values follow the supplied descriptor's protobuf JSON mapping.
    /// The map is empty when omitted. Values are validated when the descriptor
    /// and request are loaded at startup.
    ///
    /// `gateway_id`, `notify_on_self_change`, and `heart_beat_rate` remain owned
    /// by `[collectors.nmxc]` and must not be repeated here.
    #[serde(default)]
    pub subscribe_fields: serde_json::Map<String, serde_json::Value>,
}

const fn default_nmxc_max_frame_size_bytes() -> usize {
    DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES
}

/// Configuration for streaming NMX-C controller notifications from switch hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NmxcCollectorConfig {
    /// NMX-C gRPC port on switch host endpoints.
    pub grpc_port: u16,

    /// `gateway_id` value sent in NMX-C requests.
    pub gateway_id: String,

    /// Whether NMX-C should notify this client about changes caused by this gateway.
    pub notify_on_self_change: bool,

    /// Heartbeat rate value sent to `Subscribe`; NMX-C uses it to send `DomainStateInfo`.
    pub heartbeat_rate: u32,

    /// Optional TCP connect timeout for the switch-host NMX-C gRPC endpoint.
    #[serde(with = "humantime_serde::option", default)]
    pub connect_timeout: Option<Duration>,

    /// Optional timeout for NMX-C Hello, Subscribe, and initial Subscribe acknowledgement.
    #[serde(with = "humantime_serde::option", default)]
    pub rpc_timeout: Option<Duration>,

    /// Initial retry backoff after a streaming connection failure.
    #[serde(with = "humantime_serde")]
    pub initial_backoff: Duration,

    /// Maximum retry backoff after repeated streaming connection failures.
    #[serde(with = "humantime_serde")]
    pub max_backoff: Duration,

    /// Optional descriptor-driven replacement for the generated NMX-C collector.
    ///
    /// Omission uses the generated collector. The descriptor and request are
    /// loaded at service startup, so changes require a restart. Responses are
    /// emitted as generic logs and are not projected through the generated
    /// NMX-C event mapping. Every OTLP target includes canonical protobuf JSON,
    /// independently of its diagnostic payload setting.
    pub schema_override: Option<NmxcSchemaOverrideConfig>,
}

impl Default for NmxcCollectorConfig {
    fn default() -> Self {
        Self {
            grpc_port: 9370,
            gateway_id: "hw-health".to_string(),
            notify_on_self_change: false,
            heartbeat_rate: 30,
            connect_timeout: None,
            rpc_timeout: None,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            schema_override: None,
        }
    }
}

impl NmxcCollectorConfig {
    /// Returns the configured NMX-C connect timeout, or the default when unset.
    pub(crate) fn connect_timeout(&self) -> Duration {
        self.connect_timeout
            .unwrap_or(DEFAULT_NMX_C_CONNECT_TIMEOUT)
    }

    /// Returns the configured NMX-C RPC timeout, or the default when unset.
    pub(crate) fn rpc_timeout(&self) -> Duration {
        self.rpc_timeout.unwrap_or(DEFAULT_NMX_C_RPC_TIMEOUT)
    }

    fn validate(&self) -> Result<(), String> {
        if self.grpc_port == 0 {
            return Err("[collectors.nmxc].grpc_port must be greater than 0".to_string());
        }

        if self.gateway_id.trim().is_empty() {
            return Err("[collectors.nmxc].gateway_id must not be empty".to_string());
        }

        if self.heartbeat_rate == 0 {
            return Err("[collectors.nmxc].heartbeat_rate must be greater than 0".to_string());
        }

        if self
            .connect_timeout
            .is_some_and(|timeout| timeout.is_zero())
        {
            return Err("[collectors.nmxc].connect_timeout must be greater than 0".to_string());
        }

        if self.rpc_timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err("[collectors.nmxc].rpc_timeout must be greater than 0".to_string());
        }

        if self.initial_backoff.is_zero() {
            return Err("[collectors.nmxc].initial_backoff must be greater than 0".to_string());
        }

        if self.max_backoff.is_zero() {
            return Err("[collectors.nmxc].max_backoff must be greater than 0".to_string());
        }

        if self.max_backoff < self.initial_backoff {
            return Err(
                "[collectors.nmxc].max_backoff must be greater than or equal to initial_backoff"
                    .to_string(),
            );
        }

        if let Some(schema_override) = &self.schema_override {
            if schema_override.descriptor_set_path.as_os_str().is_empty() {
                return Err(
                    "[collectors.nmxc.schema_override].descriptor_set_path must not be empty"
                        .to_string(),
                );
            }

            if schema_override.max_frame_size_bytes == 0 {
                return Err(
                    "[collectors.nmxc.schema_override].max_frame_size_bytes must be greater than 0"
                        .to_string(),
                );
            }

            if schema_override.max_frame_size_bytes > MAX_NMX_C_FRAME_SIZE_BYTES {
                return Err(format!(
                    "[collectors.nmxc.schema_override].max_frame_size_bytes must not exceed {MAX_NMX_C_FRAME_SIZE_BYTES}"
                ));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NvueCollectorConfig {
    pub rest: Configurable<NvueRestConfig>,
    pub gnmi: Configurable<NvueGnmiConfig>,
}

impl Default for NvueCollectorConfig {
    fn default() -> Self {
        Self {
            rest: Configurable::Enabled(NvueRestConfig::default()),
            gnmi: Configurable::Disabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NvueGnmiConfig {
    /// gNMI server port on the switch.
    pub gnmi_port: u16,

    /// Interval between SAMPLE mode subscription updates.
    #[serde(with = "humantime_serde")]
    pub sample_interval: Duration,

    /// Timeout for gRPC connection attempts.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,

    /// Dangerously disable TLS certificate and hostname verification for NVUE gNMI.
    ///
    /// Defaults to false so strict TLS verification remains the default.
    pub dangerously_skip_tls_verification: bool,

    /// Enable gNMI ON_CHANGE subscription for live system-event messages.
    #[serde(alias = "system_events_subscription_enabled", alias = "events_enabled")]
    pub system_events_enabled: bool,

    /// gNMI SAMPLE subscription paths.
    pub paths: NvueGnmiPaths,
}

impl Default for NvueGnmiConfig {
    fn default() -> Self {
        Self {
            gnmi_port: 9339,
            sample_interval: Duration::from_secs(300),
            request_timeout: Duration::from_secs(30),
            dangerously_skip_tls_verification: false,
            system_events_enabled: true,
            paths: NvueGnmiPaths::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NvueGnmiPaths {
    pub components_enabled: bool,
    pub interfaces_enabled: bool,
    pub platform_general_enabled: bool,

    /// Collect leak sensor state from the NVOS platform-general gNMI tree.
    /// Disabled by default because path support depends on the NVOS release.
    pub leak_sensors_enabled: bool,
}

impl Default for NvueGnmiPaths {
    fn default() -> Self {
        Self {
            components_enabled: true,
            interfaces_enabled: true,
            platform_general_enabled: true,
            leak_sensors_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NvueRestConfig {
    /// Interval between NVUE REST poll iterations.
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,

    /// Timeout for individual REST requests.
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,

    /// NVUE REST paths to poll.
    pub paths: NvueRestPaths,
}

impl Default for NvueRestConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(300),
            request_timeout: Duration::from_secs(30),
            paths: NvueRestPaths::default(),
        }
    }
}

/// Supported NVUE REST API paths.
/// - system_health_enabled: Poll `/nvue_v1/system/health`.
/// - system_reboot_reason_enabled: Poll `/nvue_v1/system/reboot/reason`.
/// - cluster_apps_enabled: Poll `/nvue_v1/cluster/apps`.
/// - sdn_partitions_enabled: Poll `/nvue_v1/sdn/partition` (including per-partition details)
/// - interfaces_enabled: Poll `/nvue_v1/interface`.
/// - platform_environment_fan_enabled: Poll `/nvue_v1/platform/environment/fan`.
/// - platform_environment_temperature_enabled: Poll `/nvue_v1/platform/environment/temperature`.
/// - platform_environment_leakage_enabled: Poll `/nvue_v1/platform/environment/leakage`.
/// - platform_environment_status_enabled: Poll `/nvue_v1/platform/environment` parent
///   summary for the aggregate `FAN_STATUS` LED state.
///
/// Disabling a flag skips the HTTP request. This is separate from leakage
/// returning top-level JSON `null`, which still means the path was polled and
/// should produce an explicit health-report result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NvueRestPaths {
    pub system_health_enabled: bool,
    pub system_reboot_reason_enabled: bool,
    pub cluster_apps_enabled: bool,
    pub sdn_partitions_enabled: bool,
    pub interfaces_enabled: bool,
    pub platform_environment_fan_enabled: bool,
    pub platform_environment_temperature_enabled: bool,
    pub platform_environment_leakage_enabled: bool,
    pub platform_environment_status_enabled: bool,
}

impl Default for NvueRestPaths {
    fn default() -> Self {
        Self {
            system_health_enabled: true,
            system_reboot_reason_enabled: true,
            cluster_apps_enabled: true,
            sdn_partitions_enabled: true,
            interfaces_enabled: true,
            platform_environment_fan_enabled: true,
            platform_environment_temperature_enabled: true,
            platform_environment_leakage_enabled: true,
            platform_environment_status_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Metrics listener.
    pub endpoint: String,
    /// Prefix for all metrics, defaults to carbide_hardware_health
    pub prefix: String,
    /// Emit per-request BMC latency histograms.
    pub enable_bmc_latency_metrics: bool,
    /// Label attributes emitted on the BMC latency histogram.
    #[serde(
        default = "default_bmc_latency_attributes",
        deserialize_with = "deserialize_bmc_latency_attributes"
    )]
    pub bmc_latency_attributes: Vec<BmcLatencyAttribute>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            bucket_burst: 100,
            bucket_replenish: Duration::from_millis(30),
            max_jitter: Duration::from_millis(50),
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            endpoint: "0.0.0.0:9009".to_string(),
            prefix: "carbide_hardware_health".to_string(),
            enable_bmc_latency_metrics: false,
            bmc_latency_attributes: default_bmc_latency_attributes(),
        }
    }
}

fn default_bmc_latency_attributes() -> Vec<BmcLatencyAttribute> {
    BmcLatencyAttribute::ATTRIBUTES.to_vec()
}

fn deserialize_bmc_latency_attributes<'de, D>(
    deserializer: D,
) -> Result<Vec<BmcLatencyAttribute>, D::Error>
where
    D: Deserializer<'de>,
{
    let labels = Vec::<String>::deserialize(deserializer)?;
    let mut has_all = false;
    let mut unknown = Vec::new();

    for label in &labels {
        if label == "all" {
            has_all = true;
        } else if BmcLatencyAttribute::from_label_name(label).is_none() {
            unknown.push(label.as_str());
        }
    }

    if !unknown.is_empty() {
        return Err(serde::de::Error::custom(format!(
            "unknown BMC latency attribute(s): {}; allowed values are: {}",
            unknown.join(", "),
            bmc_latency_attribute_allowed_values().join(", ")
        )));
    }

    if has_all {
        return Ok(default_bmc_latency_attributes());
    }

    let requested = labels.iter().map(String::as_str).collect::<HashSet<_>>();
    Ok(BmcLatencyAttribute::ATTRIBUTES
        .iter()
        .copied()
        .filter(|attribute| requested.contains(attribute.label_name()))
        .collect())
}

fn bmc_latency_attribute_allowed_values() -> Vec<&'static str> {
    let mut values = vec!["all"];
    values.extend(
        BmcLatencyAttribute::ATTRIBUTES
            .iter()
            .map(|attribute| attribute.label_name()),
    );
    values
}

impl MetricsConfig {
    pub fn bmc_latency_attributes(&self) -> Vec<BmcLatencyAttribute> {
        BmcLatencyAttribute::ATTRIBUTES
            .iter()
            .copied()
            .filter(|attribute| self.bmc_latency_attributes.contains(attribute))
            .collect()
    }

    fn validate(&self) -> Result<(), String> {
        if self.enable_bmc_latency_metrics && self.bmc_latency_attributes().is_empty() {
            return Err(
                "metrics.bmc_latency_attributes must not be empty when metrics.enable_bmc_latency_metrics is true"
                    .to_string(),
            );
        }

        Ok(())
    }
}

/// Normalizes the supported NICo API key alias within one configuration provider.
///
/// This preserves Figment's defaults, TOML, and environment precedence across
/// either key. Within one provider, `nico_api` takes precedence when both keys
/// are set.
struct CanonicalNicoApiProvider<P>(P);

impl<P: Provider> Provider for CanonicalNicoApiProvider<P> {
    fn metadata(&self) -> Metadata {
        self.0.metadata()
    }

    fn data(&self) -> Result<Map<Profile, Dict>, FigmentError> {
        let mut data = self.0.data()?;

        for config in data.values_mut() {
            let Some(Value::Dict(_, endpoint_sources)) =
                config.get_mut(ENDPOINT_SOURCES_CONFIG_KEY)
            else {
                continue;
            };

            if let Some(value) = endpoint_sources.remove(CARBIDE_API_CONFIG_ALIAS) {
                endpoint_sources
                    .entry(NICO_API_CONFIG_KEY.to_string())
                    .or_insert(value);
            }
        }

        Ok(data)
    }

    fn profile(&self) -> Option<Profile> {
        self.0.profile()
    }
}

impl Config {
    /// Load configuration from optional path
    pub fn load(config_path: Option<&Path>) -> Result<Self, String> {
        let mut figment = Figment::new().merge(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            figment = figment.merge(CanonicalNicoApiProvider(Toml::file(path)));
        }

        figment = figment.merge(CanonicalNicoApiProvider(
            Env::prefixed("CARBIDE_HEALTH__").split("__"),
        ));

        let config: Config = figment
            .extract()
            .map_err(|e| format!("Failed to load configuration: {}", e))?;

        config.validate()?;
        Ok(config)
    }

    /// Get the metrics listener address
    pub fn metrics_addr(&self) -> Result<SocketAddr, String> {
        self.metrics
            .endpoint
            .parse()
            .map_err(|_| format!("Invalid metrics endpoint: {}", self.metrics.endpoint))
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.shard >= self.shards_count {
            return Err(format!(
                "shard ({}) must be less than shards_count ({})",
                self.shard, self.shards_count
            ));
        }

        if self.endpoint_discovery_interval.is_zero() {
            return Err("endpoint_discovery_interval must be greater than 0".to_string());
        }

        if self.bmc_request_concurrency.get() > Semaphore::MAX_PERMITS {
            return Err(format!(
                "bmc_request_concurrency must not exceed {}",
                Semaphore::MAX_PERMITS
            ));
        }

        self.metrics.validate()?;

        if let Configurable::Enabled(rate_limit) = &self.rate_limit
            && rate_limit.bucket_replenish.is_zero()
        {
            return Err(
                "bucket_replenish must be greater than 0 when rate limiting is enabled".to_string(),
            );
        }

        if let Configurable::Enabled(leak_detection) = &self.processors.leak_detection
            && leak_detection.minimum_alerts_per_report == 0
        {
            return Err(
                "processors.leak_detection.minimum_alerts_per_report must be greater than 0"
                    .to_string(),
            );
        }

        self.endpoint_sources.validate()?;

        if let Configurable::Enabled(health_report) = &self.sinks.health_report
            && health_report.workers == 0
        {
            return Err("sinks.health_report.workers must be greater than 0".to_string());
        }

        if let Configurable::Enabled(rack_health_report) = &self.sinks.rack_health_report
            && rack_health_report.workers == 0
        {
            return Err("sinks.rack_health_report.workers must be greater than 0".to_string());
        }

        if let Configurable::Enabled(switch_health_report) = &self.sinks.switch_health_report
            && switch_health_report.workers == 0
        {
            return Err("sinks.switch_health_report.workers must be greater than 0".to_string());
        }

        if let Configurable::Enabled(power_shelf_health_report) =
            &self.sinks.power_shelf_health_report
            && power_shelf_health_report.workers == 0
        {
            return Err(
                "sinks.power_shelf_health_report.workers must be greater than 0".to_string(),
            );
        }

        if let Configurable::Enabled(logs) = &self.collectors.logs {
            logs.validate()?;
        }

        if let Some(tls_config) = &self.tls.switch {
            tls_config.validate()?;

            if let Configurable::Enabled(nmxt) = &self.collectors.nmxt
                && nmxt.dangerously_skip_tls_verification
            {
                return Err(
                    "[collectors.nmxt].dangerously_skip_tls_verification must be false when [tls.switch] is configured"
                        .to_string(),
                );
            }

            if let Configurable::Enabled(nvue) = &self.collectors.nvue
                && let Configurable::Enabled(gnmi) = &nvue.gnmi
                && gnmi.dangerously_skip_tls_verification
            {
                return Err(
                    "[collectors.nvue.gnmi].dangerously_skip_tls_verification must be false when [tls.switch] is configured"
                        .to_string(),
                );
            }
        }

        if let Configurable::Enabled(nmxc) = &self.collectors.nmxc {
            nmxc.validate()?;

            if nmxc.schema_override.is_some() && !self.sinks.otlp.is_enabled() {
                return Err(
                    "collectors.nmxc.schema_override requires at least one OTLP target".to_string(),
                );
            }

            if nmxc.schema_override.is_some() && self.sinks.nvlink_domain_health_report.is_enabled()
            {
                return Err(
                    "sinks.nvlink_domain_health_report is not supported with collectors.nmxc.schema_override"
                        .to_string(),
                );
            }
        }

        if let Configurable::Enabled(reachability) = &self.collectors.reachability {
            reachability.validate()?;
        }

        if let Configurable::Enabled(gpu_inventory) = &self.collectors.gpu_inventory {
            if !self.endpoint_sources.carbide_api.is_enabled() {
                return Err(
                    "collectors.gpu_inventory requires endpoint_sources.nico_api to be enabled \
                     (expected GPU counts are resolved from the machine SKU via the NICo API)"
                        .to_string(),
                );
            }
            if !self.sinks.health_report.is_enabled() {
                return Err(
                    "collectors.gpu_inventory requires sinks.health_report to be enabled \
                     (GPU shortage alerts are delivered through the health-report sink)"
                        .to_string(),
                );
            }
            if gpu_inventory.interval.is_zero() {
                // A zero interval would busy-loop the collector and flood the BMC / API.
                return Err("collectors.gpu_inventory.interval must be greater than 0".to_string());
            }
        }

        if let Configurable::Enabled(ref otlp) = self.sinks.otlp {
            if otlp.targets.is_empty() {
                return Err("sinks.otlp.targets must not be empty".to_string());
            }

            let mut endpoints = HashSet::new();

            for (index, target) in otlp.targets.iter().enumerate() {
                target.validate(index)?;

                if !endpoints.insert(target.endpoint.as_str()) {
                    return Err(format!(
                        "sinks.otlp.targets[{index}].endpoint must be unique: {}",
                        target.endpoint
                    ));
                }
            }
        }

        self.metrics_addr()?;

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Configurable<T> {
    Enabled(T),
    Disabled,
}

impl<T> Configurable<T> {
    pub fn as_option(&self) -> Option<&T> {
        match self {
            Self::Enabled(v) => Some(v),
            Self::Disabled => None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled(_))
    }
}

impl<'de, T> Deserialize<'de> for Configurable<T>
where
    T: Deserialize<'de> + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper<T> {
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(flatten)]
            config: Option<T>,
        }

        fn default_true() -> bool {
            true
        }

        let helper_opt = Option::<Helper<T>>::deserialize(deserializer)?;

        match helper_opt {
            None => Ok(Configurable::Disabled),
            Some(helper) => {
                if !helper.enabled {
                    Ok(Configurable::Disabled)
                } else if let Some(cfg) = helper.config {
                    Ok(Configurable::Enabled(cfg))
                } else {
                    Ok(Configurable::Enabled(T::default()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;

    fn config_with(configure: impl FnOnce(&mut Config)) -> Box<Config> {
        let mut config = Config::default();
        configure(&mut config);
        Box::new(config)
    }

    fn static_endpoint() -> StaticBmcEndpoint {
        StaticBmcEndpoint {
            ip: "192.0.2.1".parse().unwrap(),
            port: None,
            mac: "00:11:22:33:44:55".to_string(),
            username: "admin".to_string(),
            password: None,
            machine: None,
            power_shelf: None,
            switch: None,
            rack_id: None,
            labels: BTreeMap::new(),
        }
    }

    fn static_machine() -> StaticMachineEndpoint {
        StaticMachineEndpoint {
            id: Some("machine-id".to_string()),
            serial: None,
            driver_version: None,
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
        }
    }

    fn static_switch() -> StaticSwitchEndpoint {
        StaticSwitchEndpoint {
            id: None,
            serial: Some("switch-serial".to_string()),
            slot_number: None,
            tray_index: None,
            nvlink_domain_uuid: None,
            endpoint_role: StaticSwitchEndpointRole::Host,
            is_primary: false,
            nmxc_enabled: None,
            nmxt_enabled: None,
        }
    }

    fn otlp_target(endpoint: &str) -> OtlpTargetConfig {
        OtlpTargetConfig {
            endpoint: endpoint.to_string(),
            tls: None,
            batch_size: 512,
            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
            flush_interval: Duration::from_secs(2),
            include_diagnostics: false,
            include_alert_details: false,
        }
    }

    fn otlp_tls() -> OtlpTlsConfig {
        OtlpTlsConfig {
            ca_cert_path: PathBuf::from("/site/ca.crt"),
            client_cert_path: None,
            client_key_path: None,
            tls_server_name: None,
            reload_interval: OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL,
        }
    }

    fn switch_tls() -> MtlsProfileConfig {
        MtlsProfileConfig {
            ca_cert_path: PathBuf::from("/switch/ca.crt"),
            client_cert_path: PathBuf::from("/switch/tls.crt"),
            client_key_path: PathBuf::from("/switch/tls.key"),
            tls_server_name: None,
        }
    }

    struct IndexedStaticEndpoint {
        index: usize,
        endpoint: StaticBmcEndpoint,
    }

    struct IndexedOtlpTarget {
        index: usize,
        target: OtlpTargetConfig,
    }

    struct OtlpTlsCase {
        target_path: &'static str,
        tls: OtlpTlsConfig,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct LogsConfigProjection {
        mode: LogCollectionMode,
        validation: Result<(), String>,
        configured_sse: Option<SseLogConfig>,
        configured_periodic: Option<PeriodicLogConfig>,
        configured_auto: Option<AutoModeConfig>,
        effective_sse: SseLogConfig,
        effective_periodic: PeriodicLogConfig,
        effective_auto_periodic: PeriodicLogConfig,
    }

    fn project_logs_config(config: LogsCollectorConfig) -> LogsConfigProjection {
        let validation = config.validate();
        let effective_sse = config.sse_or_default();
        let effective_periodic = config.periodic_or_default();
        let effective_auto_periodic = config.auto_periodic_or_default();
        let LogsCollectorConfig {
            mode,
            sse,
            periodic,
            auto,
        } = config;

        LogsConfigProjection {
            mode,
            validation,
            configured_sse: sse,
            configured_periodic: periodic,
            configured_auto: auto,
            effective_sse,
            effective_periodic,
            effective_auto_periodic,
        }
    }

    fn parsed_periodic_defaults() -> PeriodicLogConfig {
        PeriodicLogConfig::default()
    }

    #[test]
    fn test_parse_example_config() {
        let toml_content = include_str!("../example/config.example.toml");
        let config: Config = Figment::new()
            .merge(Toml::string(toml_content))
            .extract()
            .expect("could not parse config toml file");

        if let Configurable::Enabled(ref carbide_api) = config.endpoint_sources.carbide_api {
            assert_eq!(carbide_api.root_ca, "/var/run/secrets/spiffe.io/ca.crt");
            assert_eq!(
                carbide_api.client_cert,
                "/var/run/secrets/spiffe.io/tls.crt"
            );
            assert_eq!(carbide_api.client_key, "/var/run/secrets/spiffe.io/tls.key");
            assert!(
                carbide_api
                    .api_url
                    .as_str()
                    .starts_with("https://nico-api.nico-system.svc.cluster.local:1079"),
            );
        } else {
            panic!("carbide api empty for sources")
        }

        if let Configurable::Enabled(ref health_report) = config.sinks.health_report {
            assert_eq!(
                health_report.connection.root_ca,
                "/var/run/secrets/spiffe.io/ca.crt"
            );
            assert_eq!(health_report.workers, 8);
            assert!(health_report.skip_empty_reports);
        } else {
            panic!("health report sink is disabled")
        }

        if let Configurable::Enabled(ref rate_limit) = config.rate_limit {
            assert_eq!(rate_limit.bucket_replenish, Duration::from_millis(35));
            assert_eq!(rate_limit.bucket_burst, 200);
            assert_eq!(rate_limit.max_jitter, Duration::from_millis(40));
        } else {
            panic!("rate limit empty")
        }

        assert!(config.collectors.sensors.is_enabled());
        assert!(config.collectors.firmware.is_enabled());
        assert!(config.collectors.leak_detector.is_enabled());
        assert!(config.collectors.logs.is_enabled());
        assert!(config.collectors.nvue.is_enabled());
        assert!(!config.collectors.nmxc.is_enabled());
        assert!(config.collectors.reachability.is_enabled());
        assert!(!config.sinks.tracing.is_enabled());
        assert!(config.sinks.prometheus.is_enabled());

        let reachability = config
            .collectors
            .reachability
            .as_option()
            .expect("example config enables reachability");

        assert_eq!(reachability.interval, Duration::from_secs(30));
        assert_eq!(reachability.timeout, Duration::from_secs(3));
        assert_eq!(reachability.log_mode, ReachabilityLogMode::Unreachable);

        if let Configurable::Enabled(ref logs) = config.collectors.logs {
            assert_eq!(logs.mode, LogCollectionMode::Auto);
            let auto = logs.auto.as_ref().expect("example config sets [auto]");
            assert_eq!(auto.sse_not_available_threshold, 1);
            assert_eq!(auto.connect_failure_window, Duration::from_secs(300));
            assert_eq!(auto.connect_failure_threshold, 5);
            assert_eq!(
                auto.periodic.logs_collection_interval,
                Duration::from_secs(300)
            );
            assert_eq!(
                auto.periodic.state_refresh_interval,
                Duration::from_secs(1800)
            );
            let sse = logs.sse_or_default();
            assert_eq!(sse.initial_backoff, Duration::from_secs(1));
            assert_eq!(sse.max_backoff, Duration::from_secs(30));
            assert!(logs.validate().is_ok());
        } else {
            panic!("logs empty")
        }

        if let Configurable::Enabled(ref leak_detector) = config.collectors.leak_detector {
            assert_eq!(leak_detector.poll_interval, Duration::from_secs(60));
            assert_eq!(
                leak_detector.state_refresh_interval,
                Duration::from_secs(1800)
            );
        } else {
            panic!("leak detector collector is disabled")
        }

        if let Configurable::Enabled(ref leak_detection) = config.processors.leak_detection {
            assert_eq!(leak_detection.minimum_alerts_per_report, 1);
        } else {
            panic!("leak detection processor is disabled")
        }

        assert_eq!(config.metrics.endpoint, "0.0.0.0:9009");

        assert_eq!(config.shard, 0);
        assert_eq!(config.shards_count, 1);

        assert_eq!(config.cache_size, 100);
        assert_eq!(config.bmc_request_concurrency.get(), 4);
        assert_eq!(config.endpoint_discovery_interval, Duration::from_secs(300));

        if let Configurable::Enabled(ref nvue) = config.collectors.nvue {
            if let Configurable::Enabled(ref rest) = nvue.rest {
                assert_eq!(rest.poll_interval, Duration::from_secs(60));
                assert_eq!(rest.request_timeout, Duration::from_secs(30));
            } else {
                panic!("nvue rest config should be enabled in example config");
            }
            if let Configurable::Enabled(ref gnmi) = nvue.gnmi {
                assert_eq!(gnmi.gnmi_port, 9339);
                assert_eq!(gnmi.sample_interval, Duration::from_secs(300));
                assert_eq!(gnmi.request_timeout, Duration::from_secs(30));
                assert!(!gnmi.dangerously_skip_tls_verification);
                assert!(gnmi.system_events_enabled);
            } else {
                panic!("nvue gnmi config should be enabled in example config");
            }
        } else {
            panic!("nvue config should be enabled in example config");
        }
    }

    #[test]
    fn test_static_only_config() {
        let toml_content = r#"
endpoint_discovery_interval = "1m"
bmc_request_concurrency = 2

[[endpoint_sources.static_bmc_endpoints]]
ip = "192.168.1.100"
mac = "00:11:22:33:44:55"
username = "root"
password = "pass"

[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.sensors]
sensor_fetch_interval = "30s"
include_sensor_thresholds = false

[metrics]
endpoint = "127.0.0.1:9009"
prefix = "carbide_hardware_new_health"

shard = 0
shards_count = 1
cache_size = 50
"#;

        let config: Config = Figment::new()
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse");

        assert!(!config.endpoint_sources.carbide_api.is_enabled());
        assert!(!config.sinks.health_report.is_enabled());

        assert_eq!(config.endpoint_sources.static_bmc_endpoints.len(), 1);
        assert_eq!(config.bmc_request_concurrency.get(), 2);

        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[0].ip,
            "192.168.1.100".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[0].mac,
            "00:11:22:33:44:55"
        );

        assert_eq!(config.metrics.prefix, "carbide_hardware_new_health");
        assert_eq!(config.endpoint_discovery_interval, Duration::from_secs(60));

        if let Configurable::Enabled(ref rate_limit) = config.rate_limit {
            assert_eq!(rate_limit.bucket_replenish, Duration::from_millis(30));
            assert_eq!(rate_limit.bucket_burst, 100);
            assert_eq!(rate_limit.max_jitter, Duration::from_millis(50));
        } else {
            panic!("rate limit empty")
        }

        assert!(config.collectors.sensors.is_enabled());
        if let Configurable::Enabled(ref sensors) = config.collectors.sensors {
            assert_eq!(sensors.sensor_fetch_interval, Duration::from_secs(30));
            assert!(!sensors.include_sensor_thresholds);
        } else {
            panic!("sensors empty")
        }

        assert!(!config.collectors.firmware.is_enabled());
        assert!(config.collectors.leak_detector.is_enabled());
        assert!(!config.collectors.logs.is_enabled());
        assert!(!config.collectors.nmxc.is_enabled());
        assert!(config.processors.leak_detection.is_enabled());

        config.validate().expect("config should be valid");
    }

    #[test]
    fn test_static_endpoint_config_rejects_invalid_ip() {
        let toml_content = r#"
[[endpoint_sources.static_bmc_endpoints]]
ip = "not-an-ip"
mac = "00:11:22:33:44:55"
username = "root"
"#;

        let result = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract::<Config>();

        assert!(result.is_err());
    }

    #[test]
    fn cluster_endpoint_source_validation() {
        scenarios!(run = |config: ClusterEndpointSourceConfig| config.validate();
            "missing source" {
                ClusterEndpointSourceConfig::default() => FailsWith(
                    "cluster endpoint source requires either `inventory_path` or `cluster_manager_url`"
                        .to_string()
                ),
            }

            "inventory file" {
                ClusterEndpointSourceConfig {
                    inventory_path: PathBuf::from("/etc/carbide/cluster.json"),
                    ..ClusterEndpointSourceConfig::default()
                } => Yields(()),
            }

            "cluster manager" {
                ClusterEndpointSourceConfig {
                    cluster_manager_url: Some(
                        "https://cluster-manager.example"
                            .parse()
                            .expect("cluster manager URL should parse")
                    ),
                    ..ClusterEndpointSourceConfig::default()
                } => Yields(()),
            }
        );
    }

    #[test]
    fn static_endpoint_validation() {
        scenarios!(run = |IndexedStaticEndpoint { index, endpoint }| endpoint.validate(
            "endpoint_sources.static_bmc_endpoints",
            index,
        );
            "valid endpoint" {
                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: static_endpoint(),
                } => Yields(()),
            }

            "multiple identities" {
                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: StaticBmcEndpoint {
                        machine: Some(static_machine()),
                        switch: Some(static_switch()),
                        ..static_endpoint()
                    },
                } => FailsWith(
                    "endpoint_sources.static_bmc_endpoints[3] must specify at most one of machine, power_shelf, or switch"
                        .to_string()
                ),
            }

            "power shelf identity" {
                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: StaticBmcEndpoint {
                        power_shelf: Some(StaticPowerShelfEndpoint {
                            id: None,
                            serial: None,
                        }),
                        ..static_endpoint()
                    },
                } => FailsWith(
                    "endpoint_sources.static_bmc_endpoints[3].power_shelf requires id or serial"
                        .to_string()
                ),

                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: StaticBmcEndpoint {
                        power_shelf: Some(StaticPowerShelfEndpoint {
                            id: Some("power-shelf-id".to_string()),
                            serial: None,
                        }),
                        ..static_endpoint()
                    },
                } => Yields(()),
            }

            "switch identity" {
                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: StaticBmcEndpoint {
                        switch: Some(StaticSwitchEndpoint {
                            serial: None,
                            ..static_switch()
                        }),
                        ..static_endpoint()
                    },
                } => FailsWith(
                    "endpoint_sources.static_bmc_endpoints[3].switch requires id or serial"
                        .to_string()
                ),

                IndexedStaticEndpoint {
                    index: 3,
                    endpoint: StaticBmcEndpoint {
                        switch: Some(static_switch()),
                        ..static_endpoint()
                    },
                } => Yields(()),
            }
        );
    }

    #[test]
    fn config_validation() {
        scenarios!(run = |config: Box<Config>| config.validate();
            "valid configurations" {
                Box::new(Config::default()) => Yields(()),

                config_with(|config| {
                    config.bmc_request_concurrency =
                        NonZeroUsize::new(Semaphore::MAX_PERMITS)
                            .expect("Tokio supports at least one semaphore permit");
                }) => Yields(()),

                config_with(|config| {
                    config.collectors.logs =
                        Configurable::Enabled(LogsCollectorConfig::default());
                }) => Yields(()),

                config_with(|config| {
                    config.collectors.nmxc =
                        Configurable::Enabled(NmxcCollectorConfig::default());
                }) => Yields(()),

                config_with(|config| {
                    config.collectors.gpu_inventory =
                        Configurable::Enabled(GpuInventoryConfig {
                            interval: Duration::from_secs(300),
                        });
                }) => Yields(()),

                config_with(|config| {
                    config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![otlp_target("http://localhost:4317")],
                    });
                }) => Yields(()),

                config_with(|config| {
                    config.metrics.enable_bmc_latency_metrics = true;
                    config.metrics.bmc_latency_attributes = vec![
                        BmcLatencyAttribute::HttpResponseStatusCode,
                        BmcLatencyAttribute::ServerAddress,
                        BmcLatencyAttribute::UrlScheme,
                    ];
                }) => Yields(()),
            }

            "top-level settings" {
                config_with(|config| {
                    config.shard = 5;
                    config.shards_count = 3;
                }) => FailsWith("shard (5) must be less than shards_count (3)".to_string()),

                config_with(|config| {
                    config.endpoint_discovery_interval = Duration::ZERO;
                }) => FailsWith(
                    "endpoint_discovery_interval must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.bmc_request_concurrency =
                        NonZeroUsize::new(Semaphore::MAX_PERMITS + 1)
                            .expect("the value above Tokio's maximum remains nonzero");
                }) => FailsWith(format!(
                    "bmc_request_concurrency must not exceed {}",
                    Semaphore::MAX_PERMITS
                )),

                config_with(|config| {
                    config.metrics.enable_bmc_latency_metrics = true;
                    config.metrics.bmc_latency_attributes = vec![];
                }) => FailsWith(
                    "metrics.bmc_latency_attributes must not be empty when metrics.enable_bmc_latency_metrics is true".to_string()
                ),

                config_with(|config| {
                    config.rate_limit = Configurable::Enabled(RateLimitConfig {
                        bucket_replenish: Duration::ZERO,
                        ..RateLimitConfig::default()
                    });
                }) => FailsWith(
                    "bucket_replenish must be greater than 0 when rate limiting is enabled"
                        .to_string()
                ),

                config_with(|config| {
                    config.processors.leak_detection =
                        Configurable::Enabled(LeakDetectionProcessorConfig {
                            minimum_alerts_per_report: 0,
                        });
                }) => FailsWith(
                    "processors.leak_detection.minimum_alerts_per_report must be greater than 0"
                        .to_string()
                ),

                config_with(|config| {
                    config.sinks.health_report =
                        Configurable::Enabled(HealthReportSinkConfig {
                            workers: 0,
                            ..HealthReportSinkConfig::default()
                        });
                }) => FailsWith(
                    "sinks.health_report.workers must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.sinks.rack_health_report =
                        Configurable::Enabled(RackHealthReportSinkConfig {
                            workers: 0,
                            ..RackHealthReportSinkConfig::default()
                        });
                }) => FailsWith(
                    "sinks.rack_health_report.workers must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.sinks.switch_health_report =
                        Configurable::Enabled(SwitchHealthReportSinkConfig {
                            workers: 0,
                            ..SwitchHealthReportSinkConfig::default()
                        });
                }) => FailsWith(
                    "sinks.switch_health_report.workers must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.sinks.power_shelf_health_report =
                        Configurable::Enabled(PowerShelfHealthReportSinkConfig {
                            workers: 0,
                            ..PowerShelfHealthReportSinkConfig::default()
                        });
                }) => FailsWith(
                    "sinks.power_shelf_health_report.workers must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.metrics.endpoint = "not an address".to_string();
                }) => FailsWith(
                    "Invalid metrics endpoint: not an address".to_string()
                ),
            }

            "cluster endpoint source" {
                config_with(|config| {
                    config.endpoint_sources.cluster =
                        Configurable::Enabled(ClusterEndpointSourceConfig::default());
                }) => FailsWith(
                    "cluster endpoint source requires either `inventory_path` or `cluster_manager_url`"
                        .to_string()
                ),

                config_with(|config| {
                    config.endpoint_sources.cluster =
                        Configurable::Enabled(ClusterEndpointSourceConfig {
                            inventory_path: PathBuf::from("/etc/carbide/cluster.json"),
                            ..ClusterEndpointSourceConfig::default()
                        });
                }) => Yields(()),
            }

            "static endpoints" {
                config_with(|config| {
                    config.endpoint_sources.static_bmc_endpoints =
                        vec![StaticBmcEndpoint {
                            machine: Some(static_machine()),
                            switch: Some(static_switch()),
                            ..static_endpoint()
                        }];
                }) => FailsWith(
                    "endpoint_sources.static_bmc_endpoints[0] must specify at most one of machine, power_shelf, or switch"
                        .to_string()
                ),
            }

            "log collector" {
                config_with(|config| {
                    config.collectors.logs = Configurable::Enabled(LogsCollectorConfig {
                        mode: LogCollectionMode::Periodic,
                        ..LogsCollectorConfig::default()
                    });
                }) => FailsWith(
                    "[collectors.logs.periodic] is required when mode = \"periodic\"".to_string()
                ),
            }

            "switch TLS" {
                config_with(|config| {
                    config.tls.switch = Some(MtlsProfileConfig {
                        ca_cert_path: PathBuf::new(),
                        ..switch_tls()
                    });
                }) => FailsWith("[tls.switch].ca_cert_path must not be empty".to_string()),

                config_with(|config| {
                    config.tls.switch = Some(switch_tls());
                    config.collectors.nmxt = Configurable::Enabled(NmxtCollectorConfig {
                        dangerously_skip_tls_verification: true,
                        ..NmxtCollectorConfig::default()
                    });
                }) => FailsWith(
                    "[collectors.nmxt].dangerously_skip_tls_verification must be false when [tls.switch] is configured"
                        .to_string()
                ),

                config_with(|config| {
                    config.tls.switch = Some(switch_tls());
                    config.collectors.nvue = Configurable::Enabled(NvueCollectorConfig {
                        gnmi: Configurable::Enabled(NvueGnmiConfig {
                            dangerously_skip_tls_verification: true,
                            ..NvueGnmiConfig::default()
                        }),
                        ..NvueCollectorConfig::default()
                    });
                }) => FailsWith(
                    "[collectors.nvue.gnmi].dangerously_skip_tls_verification must be false when [tls.switch] is configured"
                        .to_string()
                ),
            }

            "NMX-C collector" {
                config_with(|config| {
                    config.collectors.nmxc = Configurable::Enabled(NmxcCollectorConfig {
                        grpc_port: 0,
                        ..NmxcCollectorConfig::default()
                    });
                }) => FailsWith(
                    "[collectors.nmxc].grpc_port must be greater than 0".to_string()
                ),

                config_with(|config| {
                    config.collectors.nmxc = Configurable::Enabled(NmxcCollectorConfig {
                        schema_override: Some(NmxcSchemaOverrideConfig {
                            descriptor_set_path: PathBuf::from("nmx_c.desc"),
                            hello_rpc_path: default_nmxc_hello_rpc_path(),
                            subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                            max_frame_size_bytes: DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES,
                            subscribe_fields: Default::default(),
                        }),
                        ..NmxcCollectorConfig::default()
                    });

                    config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![otlp_target("http://localhost:4317")],
                    });

                    config.sinks.nvlink_domain_health_report = Configurable::Enabled(
                        NvLinkDomainHealthReportSinkConfig::default(),
                    );

                }) => FailsWith(
                    "sinks.nvlink_domain_health_report is not supported with collectors.nmxc.schema_override"
                        .to_string()
                ),
            }

            "GPU inventory" {
                config_with(|config| {
                    config.collectors.gpu_inventory =
                        Configurable::Enabled(GpuInventoryConfig::default());
                    config.endpoint_sources.carbide_api = Configurable::Disabled;
                }) => FailsWith(
                    "collectors.gpu_inventory requires endpoint_sources.nico_api to be enabled (expected GPU counts are resolved from the machine SKU via the NICo API)"
                        .to_string()
                ),

                config_with(|config| {
                    config.collectors.gpu_inventory =
                        Configurable::Enabled(GpuInventoryConfig::default());
                    config.sinks.health_report = Configurable::Disabled;
                }) => FailsWith(
                    "collectors.gpu_inventory requires sinks.health_report to be enabled (GPU shortage alerts are delivered through the health-report sink)"
                        .to_string()
                ),

                config_with(|config| {
                    config.collectors.gpu_inventory =
                        Configurable::Enabled(GpuInventoryConfig {
                            interval: Duration::ZERO,
                        });
                }) => FailsWith(
                    "collectors.gpu_inventory.interval must be greater than 0".to_string()
                ),
            }

            "OTLP sink" {
                config_with(|config| {
                    config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig::default());
                }) => FailsWith("sinks.otlp.targets must not be empty".to_string()),

                config_with(|config| {
                    config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![otlp_target("not a valid uri\n")],
                    });
                }) => FailsWith(
                    "invalid sinks.otlp.targets[0].endpoint: not a valid uri\n".to_string()
                ),

                config_with(|config| {
                    config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![
                            otlp_target("http://site.example:4317"),
                            otlp_target("http://site.example:4317"),
                        ],
                    });
                }) => FailsWith(
                    "sinks.otlp.targets[1].endpoint must be unique: http://site.example:4317"
                        .to_string()
                ),
            }
        );
    }

    /// Verifies each diagnostic-capable sink parses the opt-in flag.
    #[test]
    fn test_sink_include_diagnostics_configs_parse() {
        let tracing: TracingSinkConfig = Figment::new()
            .merge(Toml::string("include_diagnostics = true"))
            .extract()
            .expect("tracing config should parse");
        let log_file: LogFileSinkConfig = Figment::new()
            .merge(Toml::string("include_diagnostics = true"))
            .extract()
            .expect("log file config should parse");
        let otlp: OtlpSinkConfig = Figment::new()
            .merge(Toml::string(
                r#"
[[targets]]
endpoint = "http://localhost:4317"
include_diagnostics = true
"#,
            ))
            .extract()
            .expect("otlp config should parse");

        assert!(tracing.include_diagnostics);
        assert!(log_file.include_diagnostics);
        assert!(otlp.includes_diagnostics());
        assert!(!TracingSinkConfig::default().include_diagnostics);
        assert!(!LogFileSinkConfig::default().include_diagnostics);
        assert!(!OtlpSinkConfig::default().includes_diagnostics());
    }

    #[test]
    fn otlp_target_list_parses_independent_settings() {
        let otlp: OtlpSinkConfig = Figment::new()
            .merge(Toml::string(
                r#"
[[targets]]
endpoint = "https://site.example:4317"

[[targets]]
endpoint = "https://central.example:4317"
batch_size = 1024
queue_capacity = 2048
flush_interval = "5s"
include_diagnostics = true
include_alert_details = true

[targets.tls]
ca_cert_path = "/central/ca.crt"
client_cert_path = "/central/tls.crt"
client_key_path = "/central/tls.key"
tls_server_name = "central.example"
reload_interval = "30s"
"#,
            ))
            .extract()
            .expect("multi-target OTLP config should parse");

        let targets = &otlp.targets;

        assert_eq!(targets.len(), 2);
        assert!(targets[0].tls.is_none());
        assert_eq!(targets[0].batch_size, 512);

        assert_eq!(
            targets[0].queue_capacity,
            OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY
        );

        assert_eq!(targets[0].flush_interval, Duration::from_secs(2));
        assert_eq!(targets[1].batch_size, 1024);
        assert_eq!(targets[1].queue_capacity, 2048);
        assert_eq!(targets[1].flush_interval, Duration::from_secs(5));
        assert!(targets[1].include_diagnostics);
        assert!(!targets[0].include_alert_details);
        assert!(targets[1].include_alert_details);

        let tls = targets[1]
            .tls
            .as_ref()
            .expect("central target should use TLS");

        assert_eq!(tls.ca_cert_path, PathBuf::from("/central/ca.crt"));

        assert_eq!(
            tls.client_cert_path.as_deref(),
            Some(Path::new("/central/tls.crt"))
        );

        assert_eq!(
            tls.client_key_path.as_deref(),
            Some(Path::new("/central/tls.key"))
        );

        assert_eq!(tls.tls_server_name.as_deref(), Some("central.example"));
        assert_eq!(tls.reload_interval, Duration::from_secs(30));

        let mut config = Config::default();

        config.sinks.otlp = Configurable::Enabled(otlp);

        config
            .validate()
            .expect("multi-target OTLP config should validate");
    }

    #[test]
    fn otlp_tls_reload_interval_defaults_to_five_minutes() {
        let tls: OtlpTlsConfig = Figment::new()
            .merge(Toml::string("ca_cert_path = \"/site/ca.crt\""))
            .extract()
            .expect("OTLP TLS config should parse without a reload interval");

        assert_eq!(tls.reload_interval, OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL);
    }

    #[test]
    fn otlp_target_validation() {
        scenarios!(run = |IndexedOtlpTarget { index, target }| target.validate(index);
            "valid target" {
                IndexedOtlpTarget {
                    index: 2,
                    target: otlp_target("http://site.example:4317"),
                } => Yields(()),
            }

            "target settings" {
                IndexedOtlpTarget {
                    index: 2,
                    target: OtlpTargetConfig {
                        batch_size: 0,
                        ..otlp_target("http://site.example:4317")
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].batch_size must be greater than 0".to_string()
                ),

                IndexedOtlpTarget {
                    index: 2,
                    target: OtlpTargetConfig {
                        queue_capacity: 0,
                        ..otlp_target("http://site.example:4317")
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].queue_capacity must be greater than 0".to_string()
                ),

                IndexedOtlpTarget {
                    index: 2,
                    target: OtlpTargetConfig {
                        flush_interval: Duration::ZERO,
                        ..otlp_target("http://site.example:4317")
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].flush_interval must be greater than 0".to_string()
                ),

                IndexedOtlpTarget {
                    index: 2,
                    target: otlp_target("not a valid uri\n"),
                } => FailsWith(
                    "invalid sinks.otlp.targets[2].endpoint: not a valid uri\n".to_string()
                ),
            }

            "TLS settings" {
                IndexedOtlpTarget {
                    index: 2,
                    target: OtlpTargetConfig {
                        tls: Some(otlp_tls()),
                        ..otlp_target("http://site.example:4317")
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls requires an https endpoint".to_string()
                ),

                IndexedOtlpTarget {
                    index: 2,
                    target: OtlpTargetConfig {
                        tls: Some(OtlpTlsConfig {
                            ca_cert_path: PathBuf::new(),
                            ..otlp_tls()
                        }),
                        ..otlp_target("https://site.example:4317")
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.ca_cert_path must not be empty".to_string()
                ),
            }
        );
    }

    #[test]
    fn otlp_tls_validation() {
        scenarios!(run = |OtlpTlsCase { target_path, tls }| tls.validate(target_path);
            "server trust" {
                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: otlp_tls(),
                } => Yields(()),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        ca_cert_path: PathBuf::new(),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.ca_cert_path must not be empty".to_string()
                ),
            }

            "client identity" {
                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        client_cert_path: Some(PathBuf::new()),
                        client_key_path: Some(PathBuf::from("/site/tls.key")),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.client_cert_path must not be empty".to_string()
                ),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        client_cert_path: Some(PathBuf::from("/site/tls.crt")),
                        client_key_path: Some(PathBuf::new()),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.client_key_path must not be empty".to_string()
                ),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        client_cert_path: Some(PathBuf::from("/site/tls.crt")),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.client_key_path must be set when sinks.otlp.targets[2].tls.client_cert_path is set"
                        .to_string()
                ),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        client_key_path: Some(PathBuf::from("/site/tls.key")),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.client_cert_path must be set when sinks.otlp.targets[2].tls.client_key_path is set"
                        .to_string()
                ),
            }

            "TLS server name" {
                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        tls_server_name: Some(String::new()),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.tls_server_name must not be empty".to_string()
                ),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        tls_server_name: Some(" site.example ".to_string()),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.tls_server_name must not contain leading or trailing whitespace"
                        .to_string()
                ),

                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        tls_server_name: Some("not a dns name".to_string()),
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.tls_server_name must be a valid DNS name".to_string()
                ),
            }

            "reload policy" {
                OtlpTlsCase {
                    target_path: "sinks.otlp.targets[2]",
                    tls: OtlpTlsConfig {
                        reload_interval: Duration::ZERO,
                        ..otlp_tls()
                    },
                } => FailsWith(
                    "sinks.otlp.targets[2].tls.reload_interval must be greater than 0".to_string()
                ),
            }
        );
    }

    /// Verifies collectors attach diagnostics only when a capable sink opts in.
    #[test]
    fn test_sinks_config_includes_log_diagnostics() {
        let cases = [
            ("default", SinksConfig::default(), false),
            (
                "diagnostic-capable-sinks-enabled-without-diagnostics",
                SinksConfig {
                    tracing: Configurable::Enabled(TracingSinkConfig::default()),
                    log_file: Configurable::Enabled(LogFileSinkConfig::default()),
                    otlp: Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![OtlpTargetConfig {
                            endpoint: "http://localhost:4317".to_string(),
                            batch_size: 512,
                            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                            flush_interval: Duration::from_secs(2),
                            include_diagnostics: false,
                            include_alert_details: false,
                            tls: None,
                        }],
                    }),
                    ..SinksConfig::default()
                },
                false,
            ),
            (
                "tracing-diagnostics",
                SinksConfig {
                    tracing: Configurable::Enabled(TracingSinkConfig {
                        include_diagnostics: true,
                    }),
                    ..SinksConfig::default()
                },
                true,
            ),
            (
                "log-file-diagnostics",
                SinksConfig {
                    log_file: Configurable::Enabled(LogFileSinkConfig {
                        include_diagnostics: true,
                        ..LogFileSinkConfig::default()
                    }),
                    ..SinksConfig::default()
                },
                true,
            ),
            (
                "otlp-diagnostics",
                SinksConfig {
                    otlp: Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![OtlpTargetConfig {
                            endpoint: "http://localhost:4317".to_string(),
                            batch_size: 512,
                            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                            flush_interval: Duration::from_secs(2),
                            include_diagnostics: true,
                            include_alert_details: false,
                            tls: None,
                        }],
                    }),
                    ..SinksConfig::default()
                },
                true,
            ),
            (
                "one-of-multiple-otlp-targets-enables-diagnostics",
                SinksConfig {
                    otlp: Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![
                            OtlpTargetConfig {
                                endpoint: "http://site.example:4317".to_string(),
                                batch_size: 512,
                                queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                                flush_interval: Duration::from_secs(2),
                                include_diagnostics: false,
                                include_alert_details: false,
                                tls: None,
                            },
                            OtlpTargetConfig {
                                endpoint: "http://central.example:4317".to_string(),
                                batch_size: 512,
                                queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                                flush_interval: Duration::from_secs(2),
                                include_diagnostics: true,
                                include_alert_details: false,
                                tls: None,
                            },
                        ],
                    }),
                    ..SinksConfig::default()
                },
                true,
            ),
        ];

        for (name, sinks, expected) in cases {
            assert_eq!(sinks.includes_log_diagnostics(), expected, "{name}");
        }
    }

    /// Verifies log-only collectors run only when a sink consumes log events.
    #[test]
    fn test_sinks_config_includes_log_events() {
        let cases = [
            ("default", SinksConfig::default(), false),
            (
                "tracing",
                SinksConfig {
                    tracing: Configurable::Enabled(TracingSinkConfig::default()),
                    ..SinksConfig::default()
                },
                true,
            ),
            (
                "log-file",
                SinksConfig {
                    log_file: Configurable::Enabled(LogFileSinkConfig::default()),
                    ..SinksConfig::default()
                },
                true,
            ),
            (
                "otlp",
                SinksConfig {
                    otlp: Configurable::Enabled(OtlpSinkConfig {
                        targets: vec![OtlpTargetConfig {
                            endpoint: "http://localhost:4317".to_string(),
                            batch_size: 512,
                            queue_capacity: OtlpTargetConfig::DEFAULT_QUEUE_CAPACITY,
                            flush_interval: Duration::from_secs(2),
                            include_diagnostics: false,
                            include_alert_details: false,
                            tls: None,
                        }],
                    }),
                    ..SinksConfig::default()
                },
                true,
            ),
        ];

        for (name, sinks, expected) in cases {
            assert_eq!(sinks.includes_log_events(), expected, "{name}");
        }
    }

    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn test_load_defaults() {
        let mut config = None;
        // Jail clears inherited configuration and serializes this load with tests that
        // temporarily modify process-global environment variables.
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            config = Some(Config::load(None).expect("should load defaults"));
            Ok(())
        });

        let config = config.expect("default config should be loaded");
        assert_eq!(config.shard, 0);
        assert_eq!(config.shards_count, 1);
        assert_eq!(config.cache_size, 100);
        assert_eq!(config.bmc_request_concurrency.get(), 4);
        assert_eq!(config.metrics.endpoint, "0.0.0.0:9009");
        assert!(!config.metrics.enable_bmc_latency_metrics);
        assert_eq!(
            config.metrics.bmc_latency_attributes,
            BmcLatencyAttribute::ATTRIBUTES.to_vec()
        );
        assert_eq!(
            config.metrics.bmc_latency_attributes(),
            BmcLatencyAttribute::ATTRIBUTES.to_vec()
        );
        assert!(config.rate_limit.is_enabled());
        assert!(config.processors.leak_detection.is_enabled());
        assert!(config.collectors.leak_detector.is_enabled());
        assert!(!config.collectors.nmxc.is_enabled());
        assert!(!config.collectors.nvue.is_enabled());

        let api = config
            .endpoint_sources
            .carbide_api
            .as_option()
            .expect("NICo API endpoint source should be enabled by default");

        assert_eq!(
            api.api_url.as_str(),
            "https://nico-api.nico-system.svc.cluster.local:1079/"
        );

        if let Configurable::Enabled(ref health_report) = config.sinks.health_report {
            assert!(health_report.skip_empty_reports);
        } else {
            panic!("health report sink should be enabled by default");
        }
    }

    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn nico_api_config_names_and_precedence() {
        value_scenarios!(run = |(file_names, environment_names): (&[&str], &[&str])| {
            let file_config = file_names
                .iter()
                .map(|name| {
                    let host = name.replace('_', "-");

                    format!(
                        "[endpoint_sources.{name}]\napi_url = \"https://{host}.example:1079\"\n"
                    )
                })
                .collect::<String>();

            let mut actual_url = None;

            figment::Jail::expect_with(|jail| {
                jail.clear_env();
                jail.create_file("health.toml", &file_config)?;

                for name in environment_names {
                    let host = name.to_ascii_lowercase().replace('_', "-");

                    jail.set_env(
                        format!("CARBIDE_HEALTH__ENDPOINT_SOURCES__{name}__API_URL"),
                        format!("https://{host}.example:1079"),
                    );
                }

                let config = Config::load(Some(Path::new("health.toml")))
                    .expect("NICo API configuration should load");

                let api = config
                    .endpoint_sources
                    .carbide_api
                    .as_option()
                    .expect("NICo API configuration should remain enabled");

                actual_url = Some(api.api_url.as_str().to_string());

                Ok(())
            });

            actual_url.expect("NICo API precedence case should produce a URL")
        };
            "supported TOML names" {
                (&["nico_api"][..], &[][..]) => "https://nico-api.example:1079/".to_string(),
                (&["carbide_api"][..], &[][..]) => "https://carbide-api.example:1079/".to_string(),
            }

            "environment overrides TOML across names" {
                (&["carbide_api"][..], &["NICO_API"][..]) => "https://nico-api.example:1079/".to_string(),
                (&["nico_api"][..], &["CARBIDE_API"][..]) => "https://carbide-api.example:1079/".to_string(),
            }

            "canonical name wins within one provider" {
                (&["carbide_api", "nico_api"][..], &[][..]) => "https://nico-api.example:1079/".to_string(),
                (&[][..], &["CARBIDE_API", "NICO_API"][..]) => "https://nico-api.example:1079/".to_string(),
            }
        );
    }

    #[test]
    fn zero_bmc_request_concurrency_is_rejected() {
        let result = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string("bmc_request_concurrency = 0"))
            .extract::<Config>();

        assert!(result.is_err());
    }

    #[test]
    fn nvlink_domain_health_report_sink_is_opt_in() {
        assert!(matches!(
            SinksConfig::default().nvlink_domain_health_report,
            Configurable::Disabled
        ));

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(
                r#"
[sinks.nvlink_domain_health_report]
"#,
            ))
            .extract()
            .expect("NVLink domain health report sink config should parse");

        let Configurable::Enabled(_) = config.sinks.nvlink_domain_health_report else {
            panic!("NVLink domain health report sink should be enabled");
        };
    }

    #[test]
    fn bmc_latency_metrics_config_parsing() {
        let toml_content = r#"
[metrics]
enable_bmc_latency_metrics = true
bmc_latency_attributes = ["http_response_status_code", "server_address", "url_scheme", "entity_type", "machine_id", "rack_id"]
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("could not parse config toml file");

        assert!(config.metrics.enable_bmc_latency_metrics);
        assert_eq!(
            config.metrics.bmc_latency_attributes(),
            vec![
                BmcLatencyAttribute::HttpResponseStatusCode,
                BmcLatencyAttribute::ServerAddress,
                BmcLatencyAttribute::UrlScheme,
                BmcLatencyAttribute::EntityType,
                BmcLatencyAttribute::MachineId,
                BmcLatencyAttribute::RackId,
            ]
        );

        let toml_content = r#"
[metrics]
bmc_latency_attributes = ["http_path", "all"]
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("could not parse config toml file");

        assert_eq!(
            config.metrics.bmc_latency_attributes,
            BmcLatencyAttribute::ATTRIBUTES.to_vec()
        );
        assert_eq!(
            config.metrics.bmc_latency_attributes(),
            BmcLatencyAttribute::ATTRIBUTES.to_vec()
        );
    }

    #[test]
    fn test_health_report_sink_can_send_empty_reports_when_configured() {
        let toml_content = r#"
[sinks.health_report]
skip_empty_reports = false
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("could not parse config toml file");

        if let Configurable::Enabled(ref health_report) = config.sinks.health_report {
            assert!(!health_report.skip_empty_reports);
        } else {
            panic!("health report sink is disabled")
        }
    }

    #[test]
    fn test_nvue_config_defaults() {
        let defaults = NvueCollectorConfig::default();
        assert!(defaults.rest.is_enabled());
        assert!(!defaults.gnmi.is_enabled());

        if let Configurable::Enabled(ref rest) = defaults.rest {
            assert_eq!(rest.poll_interval, Duration::from_secs(300));
            assert_eq!(rest.request_timeout, Duration::from_secs(30));
            assert!(rest.paths.system_health_enabled);
            assert!(rest.paths.system_reboot_reason_enabled);
            assert!(rest.paths.cluster_apps_enabled);
            assert!(rest.paths.sdn_partitions_enabled);
            assert!(rest.paths.interfaces_enabled);
            assert!(rest.paths.platform_environment_leakage_enabled);
        }
    }

    #[test]
    fn test_nmxc_config_defaults() {
        let defaults = NmxcCollectorConfig::default();

        assert_eq!(defaults.grpc_port, 9370);
        assert_eq!(defaults.gateway_id, "hw-health");
        assert!(!defaults.notify_on_self_change);
        assert_eq!(defaults.heartbeat_rate, 30);
        assert_eq!(defaults.connect_timeout, None);
        assert_eq!(defaults.rpc_timeout, None);
        assert_eq!(defaults.connect_timeout(), Duration::from_secs(10));
        assert_eq!(defaults.rpc_timeout(), Duration::from_secs(10));
        assert_eq!(defaults.initial_backoff, Duration::from_secs(1));
        assert_eq!(defaults.max_backoff, Duration::from_secs(30));
        assert!(defaults.schema_override.is_none());
    }

    #[test]
    fn test_nmxc_config_parsing() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nmxc]
grpc_port = 9602
gateway_id = "health-service"
notify_on_self_change = true
heartbeat_rate = 15
connect_timeout = "3s"
rpc_timeout = "4s"
initial_backoff = "2s"
max_backoff = "20s"

[collectors.nmxc.schema_override]
descriptor_set_path = "/etc/carbide-health/nmx_c.desc"

[collectors.nmxc.schema_override.subscribe_fields]
testOption = true
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse nmxc config");

        if let Configurable::Enabled(ref nmxc) = config.collectors.nmxc {
            assert_eq!(nmxc.grpc_port, 9602);
            assert_eq!(nmxc.gateway_id, "health-service");
            assert!(nmxc.notify_on_self_change);
            assert_eq!(nmxc.heartbeat_rate, 15);
            assert_eq!(nmxc.connect_timeout, Some(Duration::from_secs(3)));
            assert_eq!(nmxc.rpc_timeout, Some(Duration::from_secs(4)));
            assert_eq!(nmxc.connect_timeout(), Duration::from_secs(3));
            assert_eq!(nmxc.rpc_timeout(), Duration::from_secs(4));
            assert_eq!(nmxc.initial_backoff, Duration::from_secs(2));
            assert_eq!(nmxc.max_backoff, Duration::from_secs(20));

            let schema_override = nmxc
                .schema_override
                .as_ref()
                .expect("schema override config should parse");

            assert_eq!(
                schema_override.descriptor_set_path,
                PathBuf::from("/etc/carbide-health/nmx_c.desc")
            );

            assert_eq!(
                schema_override.hello_rpc_path,
                "/nmx_c.NMX_Controller/Hello"
            );

            assert_eq!(
                schema_override.subscribe_rpc_path,
                "/nmx_c.NMX_Controller/Subscribe"
            );

            assert_eq!(
                schema_override.max_frame_size_bytes,
                DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES
            );

            assert_eq!(
                schema_override.subscribe_fields["testOption"],
                serde_json::Value::Bool(true)
            );
        } else {
            panic!("nmxc config should be enabled");
        }
    }

    #[test]
    fn nmxc_collector_validation() {
        scenarios!(run = |config: NmxcCollectorConfig| config.validate();
            "valid configuration" {
                NmxcCollectorConfig::default() => Yields(()),

                NmxcCollectorConfig {
                    schema_override: Some(NmxcSchemaOverrideConfig {
                        descriptor_set_path: PathBuf::from("nmx_c.desc"),
                        hello_rpc_path: default_nmxc_hello_rpc_path(),
                        subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                        max_frame_size_bytes: MAX_NMX_C_FRAME_SIZE_BYTES,
                        subscribe_fields: Default::default(),
                    }),
                    ..NmxcCollectorConfig::default()
                } => Yields(()),
            }

            "connection settings" {
                NmxcCollectorConfig {
                    grpc_port: 0,
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].grpc_port must be greater than 0".to_string()
                ),

                NmxcCollectorConfig {
                    gateway_id: " ".to_string(),
                    ..NmxcCollectorConfig::default()
                } => FailsWith("[collectors.nmxc].gateway_id must not be empty".to_string()),

                NmxcCollectorConfig {
                    heartbeat_rate: 0,
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].heartbeat_rate must be greater than 0".to_string()
                ),

                NmxcCollectorConfig {
                    connect_timeout: Some(Duration::ZERO),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].connect_timeout must be greater than 0".to_string()
                ),

                NmxcCollectorConfig {
                    rpc_timeout: Some(Duration::ZERO),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].rpc_timeout must be greater than 0".to_string()
                ),
            }

            "backoff settings" {
                NmxcCollectorConfig {
                    initial_backoff: Duration::ZERO,
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].initial_backoff must be greater than 0".to_string()
                ),

                NmxcCollectorConfig {
                    max_backoff: Duration::ZERO,
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].max_backoff must be greater than 0".to_string()
                ),

                NmxcCollectorConfig {
                    initial_backoff: Duration::from_secs(30),
                    max_backoff: Duration::from_secs(1),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc].max_backoff must be greater than or equal to initial_backoff"
                        .to_string()
                ),
            }

            "schema override settings" {
                NmxcCollectorConfig {
                    schema_override: Some(NmxcSchemaOverrideConfig {
                        descriptor_set_path: PathBuf::new(),
                        hello_rpc_path: default_nmxc_hello_rpc_path(),
                        subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                        max_frame_size_bytes: DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES,
                        subscribe_fields: Default::default(),
                    }),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc.schema_override].descriptor_set_path must not be empty"
                        .to_string()
                ),

                NmxcCollectorConfig {
                    schema_override: Some(NmxcSchemaOverrideConfig {
                        descriptor_set_path: PathBuf::from("nmx_c.desc"),
                        hello_rpc_path: default_nmxc_hello_rpc_path(),
                        subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                        max_frame_size_bytes: 0,
                        subscribe_fields: Default::default(),
                    }),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    "[collectors.nmxc.schema_override].max_frame_size_bytes must be greater than 0"
                        .to_string()
                ),

                NmxcCollectorConfig {
                    schema_override: Some(NmxcSchemaOverrideConfig {
                        descriptor_set_path: PathBuf::from("nmx_c.desc"),
                        hello_rpc_path: default_nmxc_hello_rpc_path(),
                        subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                        max_frame_size_bytes: MAX_NMX_C_FRAME_SIZE_BYTES + 1,
                        subscribe_fields: Default::default(),
                    }),
                    ..NmxcCollectorConfig::default()
                } => FailsWith(
                    format!(
                        "[collectors.nmxc.schema_override].max_frame_size_bytes must not exceed {MAX_NMX_C_FRAME_SIZE_BYTES}"
                    )
                ),
            }
        );
    }

    #[test]
    fn nmxc_schema_override_accepts_otlp_target_without_diagnostics() {
        let mut config = Config::default();

        config.collectors.nmxc = Configurable::Enabled(NmxcCollectorConfig {
            schema_override: Some(NmxcSchemaOverrideConfig {
                descriptor_set_path: PathBuf::from("nmx_c.desc"),
                hello_rpc_path: default_nmxc_hello_rpc_path(),
                subscribe_rpc_path: default_nmxc_subscribe_rpc_path(),
                max_frame_size_bytes: DEFAULT_NMX_C_MAX_FRAME_SIZE_BYTES,
                subscribe_fields: Default::default(),
            }),
            ..Default::default()
        });

        assert_eq!(
            config.validate(),
            Err("collectors.nmxc.schema_override requires at least one OTLP target".to_string())
        );

        config.sinks.otlp = Configurable::Enabled(OtlpSinkConfig {
            targets: vec![otlp_target("http://localhost:4317")],
        });

        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn test_nvue_config_parsing() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nvue.rest]
poll_interval = "2m"
request_timeout = "45s"
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse nvue config");

        assert!(config.collectors.nvue.is_enabled());

        if let Configurable::Enabled(ref nvue) = config.collectors.nvue {
            if let Configurable::Enabled(ref rest) = nvue.rest {
                assert_eq!(rest.poll_interval, Duration::from_secs(120));
                assert_eq!(rest.request_timeout, Duration::from_secs(45));
                assert!(rest.paths.system_health_enabled);
                assert!(rest.paths.system_reboot_reason_enabled);
                assert!(rest.paths.platform_environment_leakage_enabled);
            } else {
                panic!("nvue rest config should be enabled");
            }
        } else {
            panic!("nvue config should be enabled");
        }
    }

    #[test]
    fn test_nvue_config_disabled_by_default() {
        let config = Config::default();
        assert!(!config.collectors.nvue.is_enabled());
    }

    #[test]
    fn test_nvue_config_explicit_disable() {
        // nvue is already off by default (test_nvue_config_disabled_by_default), so
        // merging a bare `enabled = false` over the defaults would pass even if the key
        // were ignored outright. Populating `[collectors.nvue.rest]` is what makes this
        // worth running -- on its own that turns nvue *on*
        // (test_nvue_config_rest_only), so this pins the half that can actually break:
        // an explicit `enabled = false` still wins over a populated sub-table.
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nvue]
enabled = false

[collectors.nvue.rest]
poll_interval = "1m"
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse");

        assert!(!config.collectors.nvue.is_enabled());
    }

    #[test]
    fn test_nvue_config_rest_only() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nvue.rest]
poll_interval = "1m"
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse");

        assert!(config.collectors.nvue.is_enabled());
        if let Configurable::Enabled(ref nvue) = config.collectors.nvue {
            assert!(nvue.rest.is_enabled());
        }
    }

    #[test]
    fn test_nvue_config_selective_endpoints() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nvue.rest]
poll_interval = "1m"

[collectors.nvue.rest.paths]
system_health_enabled = true
system_reboot_reason_enabled = false
cluster_apps_enabled = false
sdn_partitions_enabled = true
interfaces_enabled = false
platform_environment_leakage_enabled = false
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse nvue config with selective endpoints");

        if let Configurable::Enabled(ref nvue) = config.collectors.nvue {
            if let Configurable::Enabled(ref rest) = nvue.rest {
                assert!(rest.paths.system_health_enabled);
                assert!(!rest.paths.system_reboot_reason_enabled);
                assert!(!rest.paths.cluster_apps_enabled);
                assert!(rest.paths.sdn_partitions_enabled);
                assert!(!rest.paths.interfaces_enabled);
                assert!(!rest.paths.platform_environment_leakage_enabled);
            } else {
                panic!("nvue rest config should be enabled");
            }
        } else {
            panic!("nvue config should be enabled");
        }
    }

    #[test]
    fn nvue_gnmi_config_parsing() {
        #[derive(Debug, PartialEq)]
        enum Projection {
            NvueDisabled,
            GnmiDisabled,
            Enabled {
                port: u16,
                sample_interval: Duration,
                request_timeout: Duration,
                dangerously_skip_tls_verification: bool,
                system_events_enabled: bool,
                components_enabled: bool,
                interfaces_enabled: bool,
                platform_general_enabled: bool,
                leak_sensors_enabled: bool,
            },
        }

        check_values(
            [
                Check {
                    scenario: "NVUE disabled",
                    input: "",
                    expect: Projection::NvueDisabled,
                },
                Check {
                    scenario: "gNMI disabled",
                    input: "[collectors.nvue]",
                    expect: Projection::GnmiDisabled,
                },
                Check {
                    scenario: "gNMI defaults",
                    input: "[collectors.nvue.gnmi]",
                    expect: Projection::Enabled {
                        port: 9339,
                        sample_interval: Duration::from_secs(300),
                        request_timeout: Duration::from_secs(30),
                        dangerously_skip_tls_verification: false,
                        system_events_enabled: true,
                        components_enabled: true,
                        interfaces_enabled: true,
                        platform_general_enabled: true,
                        leak_sensors_enabled: false,
                    },
                },
                Check {
                    scenario: "custom gNMI settings and paths",
                    input: r#"
[collectors.nvue.gnmi]
gnmi_port = 19339
sample_interval = "45s"
request_timeout = "7s"
dangerously_skip_tls_verification = true
system_events_enabled = false

[collectors.nvue.gnmi.paths]
components_enabled = false
interfaces_enabled = true
platform_general_enabled = false
leak_sensors_enabled = true
"#,
                    expect: Projection::Enabled {
                        port: 19339,
                        sample_interval: Duration::from_secs(45),
                        request_timeout: Duration::from_secs(7),
                        dangerously_skip_tls_verification: true,
                        system_events_enabled: false,
                        components_enabled: false,
                        interfaces_enabled: true,
                        platform_general_enabled: false,
                        leak_sensors_enabled: true,
                    },
                },
                Check {
                    scenario: "system events subscription alias",
                    input: r#"
[collectors.nvue.gnmi]
system_events_subscription_enabled = false
"#,
                    expect: Projection::Enabled {
                        port: 9339,
                        sample_interval: Duration::from_secs(300),
                        request_timeout: Duration::from_secs(30),
                        dangerously_skip_tls_verification: false,
                        system_events_enabled: false,
                        components_enabled: true,
                        interfaces_enabled: true,
                        platform_general_enabled: true,
                        leak_sensors_enabled: false,
                    },
                },
                Check {
                    scenario: "events enabled alias",
                    input: r#"
[collectors.nvue.gnmi]
events_enabled = false
"#,
                    expect: Projection::Enabled {
                        port: 9339,
                        sample_interval: Duration::from_secs(300),
                        request_timeout: Duration::from_secs(30),
                        dangerously_skip_tls_verification: false,
                        system_events_enabled: false,
                        components_enabled: true,
                        interfaces_enabled: true,
                        platform_general_enabled: true,
                        leak_sensors_enabled: false,
                    },
                },
            ],
            |toml| {
                let config: Config = Figment::new()
                    .merge(Serialized::defaults(Config::default()))
                    .merge(Toml::string(toml))
                    .extract()
                    .expect("failed to parse NVUE gNMI config");
                let Configurable::Enabled(nvue) = config.collectors.nvue else {
                    return Projection::NvueDisabled;
                };
                let Configurable::Enabled(gnmi) = nvue.gnmi else {
                    return Projection::GnmiDisabled;
                };

                Projection::Enabled {
                    port: gnmi.gnmi_port,
                    sample_interval: gnmi.sample_interval,
                    request_timeout: gnmi.request_timeout,
                    dangerously_skip_tls_verification: gnmi.dangerously_skip_tls_verification,
                    system_events_enabled: gnmi.system_events_enabled,
                    components_enabled: gnmi.paths.components_enabled,
                    interfaces_enabled: gnmi.paths.interfaces_enabled,
                    platform_general_enabled: gnmi.paths.platform_general_enabled,
                    leak_sensors_enabled: gnmi.paths.leak_sensors_enabled,
                }
            },
        );
    }

    #[test]
    fn test_nmxt_dangerous_tls_skip_defaults_false_and_parses_true() {
        assert!(!NmxtCollectorConfig::default().dangerously_skip_tls_verification);

        let omitted = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nmxt]
"#;
        let enabled = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[collectors.nmxt]
dangerously_skip_tls_verification = true
"#;

        for (toml, expected) in [(omitted, false), (enabled, true)] {
            let config: Config = Figment::new()
                .merge(Serialized::defaults(Config::default()))
                .merge(Toml::string(toml))
                .extract()
                .expect("failed to parse NMX-T TLS flag");
            let Configurable::Enabled(nmxt) = config.collectors.nmxt else {
                panic!("nmxt config should be enabled");
            };
            assert_eq!(nmxt.dangerously_skip_tls_verification, expected);
        }
    }

    #[test]
    fn test_tls_switch_profile_absent_by_default_and_does_not_reuse_api_cert_paths() {
        let config = Config::default();

        assert!(config.tls.switch.is_none());

        let Configurable::Enabled(carbide_api) = config.endpoint_sources.carbide_api else {
            panic!("carbide api endpoint source should be enabled by default");
        };

        assert_eq!(carbide_api.root_ca, "/var/run/secrets/spiffe.io/ca.crt");

        assert_eq!(
            carbide_api.client_cert,
            "/var/run/secrets/spiffe.io/tls.crt"
        );

        assert_eq!(carbide_api.client_key, "/var/run/secrets/spiffe.io/tls.key");
    }

    #[test]
    fn test_tls_switch_profile_parses_independent_paths() {
        let toml = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[tls.switch]
ca_cert_path = "/var/run/secrets/switch-mtls/ca.crt"
client_cert_path = "/var/run/secrets/switch-mtls/tls.crt"
client_key_path = "/var/run/secrets/switch-mtls/tls.key"
tls_server_name = "switches.example.forge"
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml))
            .extract()
            .expect("failed to parse mTLS profile config");

        config
            .validate()
            .expect("mTLS profile config should validate");

        let tls_config = config
            .tls
            .switch
            .expect("mTLS profile config should be present");

        assert_eq!(
            tls_config.ca_cert_path,
            PathBuf::from("/var/run/secrets/switch-mtls/ca.crt")
        );

        assert_eq!(
            tls_config.client_cert_path,
            PathBuf::from("/var/run/secrets/switch-mtls/tls.crt")
        );

        assert_eq!(
            tls_config.client_key_path,
            PathBuf::from("/var/run/secrets/switch-mtls/tls.key")
        );

        assert_eq!(
            tls_config.tls_server_name.as_deref(),
            Some("switches.example.forge")
        );
    }

    #[test]
    fn test_tls_switch_profile_rejects_incomplete_or_unknown_fields() {
        value_scenarios!(run = |toml| {
            Figment::new()
                .merge(Serialized::defaults(Config::default()))
                .merge(Toml::string(toml))
                .extract::<Config>()
                .is_err()
        };
            "missing required field" {
                r#"
[tls.switch]
client_cert_path = "/switch/tls.crt"
client_key_path = "/switch/tls.key"
"# => true,

                r#"
[tls.switch]
ca_cert_path = "/switch/ca.crt"
client_key_path = "/switch/tls.key"
"# => true,

                r#"
[tls.switch]
ca_cert_path = "/switch/ca.crt"
client_cert_path = "/switch/tls.crt"
"# => true,
            }

            "unknown field" {
                r#"
[tls.switch]
ca_cert_path = "/switch/ca.crt"
client_cert_path = "/switch/tls.crt"
client_key_path = "/switch/tls.key"
root_ca = "/var/run/secrets/spiffe.io/ca.crt"
"# => true,
            }
        );
    }

    #[test]
    fn switch_tls_validation() {
        scenarios!(run = |tls: MtlsProfileConfig| tls.validate();
            "valid profile" {
                switch_tls() => Yields(()),
            }

            "certificate paths" {
                MtlsProfileConfig {
                    ca_cert_path: PathBuf::new(),
                    ..switch_tls()
                } => FailsWith("[tls.switch].ca_cert_path must not be empty".to_string()),

                MtlsProfileConfig {
                    client_cert_path: PathBuf::new(),
                    ..switch_tls()
                } => FailsWith("[tls.switch].client_cert_path must not be empty".to_string()),

                MtlsProfileConfig {
                    client_key_path: PathBuf::new(),
                    ..switch_tls()
                } => FailsWith("[tls.switch].client_key_path must not be empty".to_string()),
            }

            "TLS server name" {
                MtlsProfileConfig {
                    tls_server_name: Some(" ".to_string()),
                    ..switch_tls()
                } => FailsWith("[tls.switch].tls_server_name must not be empty".to_string()),

                MtlsProfileConfig {
                    tls_server_name: Some(" switches.example.forge ".to_string()),
                    ..switch_tls()
                } => FailsWith(
                    "[tls.switch].tls_server_name must not contain leading or trailing whitespace"
                        .to_string()
                ),

                MtlsProfileConfig {
                    tls_server_name: Some("not a dns name".to_string()),
                    ..switch_tls()
                } => FailsWith(
                    "[tls.switch].tls_server_name must be a valid DNS name".to_string()
                ),
            }
        );
    }

    #[test]
    fn test_example_config_documents_platform_environment_fan_toggle() {
        let toml_content = include_str!("../example/config.example.toml");

        assert!(
            toml_content
                .lines()
                .any(|line| line == "platform_environment_fan_enabled = true")
        );
    }

    #[test]
    fn test_static_endpoint_with_switch_serial() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.0.1"
mac = "aa:bb:cc:dd:ee:ff"
username = "admin"
password = "pass"

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:11"
username = "cumulus"
password = "pass"
machine = { id = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "MN-001" }

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.1"
mac = "11:22:33:44:55:66"
username = "cumulus"
password = "pass"
switch = { id = "fsw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "SN-SW-001", slot_number = 7, tray_index = 3 }

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.2.1"
mac = "22:33:44:55:66:77"
username = "admin"
password = "pass"
power_shelf = { id = "fps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "SN-PS-001" }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse static switch endpoint config");

        assert_eq!(config.endpoint_sources.static_bmc_endpoints.len(), 4);
        assert!(
            config.endpoint_sources.static_bmc_endpoints[0]
                .machine
                .is_none()
                && config.endpoint_sources.static_bmc_endpoints[0]
                    .switch
                    .is_none()
                && config.endpoint_sources.static_bmc_endpoints[0]
                    .power_shelf
                    .is_none()
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[1]
                .machine
                .as_ref()
                .and_then(|machine| machine.id.as_deref()),
            Some("fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[1]
                .machine
                .as_ref()
                .and_then(|machine| machine.serial.as_deref()),
            Some("MN-001")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .switch
                .as_ref()
                .and_then(|switch| switch.id.as_deref()),
            Some("fsw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .switch
                .as_ref()
                .and_then(|switch| switch.serial.as_deref()),
            Some("SN-SW-001")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .switch
                .as_ref()
                .and_then(|switch| switch.slot_number),
            Some(7)
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .switch
                .as_ref()
                .and_then(|switch| switch.tray_index),
            Some(3)
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .switch
                .as_ref()
                .map(|switch| switch.endpoint_role),
            Some(StaticSwitchEndpointRole::Host)
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[3]
                .power_shelf
                .as_ref()
                .and_then(|power_shelf| power_shelf.id.as_deref()),
            Some("fps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[3]
                .power_shelf
                .as_ref()
                .and_then(|power_shelf| power_shelf.serial.as_deref()),
            Some("SN-PS-001")
        );
    }

    #[test]
    fn test_static_machine_endpoint_without_id() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[sinks.health_report]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:11"
username = "admin"
password = "pass"
machine = { serial = "MN-001" }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse static machine endpoint config without id");

        assert_eq!(config.endpoint_sources.static_bmc_endpoints.len(), 1);
        let machine = config.endpoint_sources.static_bmc_endpoints[0]
            .machine
            .as_ref()
            .expect("machine metadata should be present");
        assert_eq!(machine.id, None);
        assert_eq!(machine.serial.as_deref(), Some("MN-001"));
    }

    #[test]
    fn test_static_switch_host_accepts_primary_without_nmxt_override() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.1"
mac = "11:22:33:44:55:66"
username = "admin"
password = "pass"
switch = { id = "fsw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "SN-SW-001", endpoint_role = "host", is_primary = true }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("static switch host config should parse");

        let switch = config.endpoint_sources.static_bmc_endpoints[0]
            .switch
            .as_ref()
            .expect("switch metadata");

        assert_eq!(switch.endpoint_role, StaticSwitchEndpointRole::Host);
        assert!(switch.is_primary);
        assert_eq!(switch.nmxc_enabled, None);
        assert_eq!(switch.nmxt_enabled, None);
    }

    #[test]
    fn test_static_switch_host_accepts_nmx_collector_overrides() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:77"
username = "admin"
password = "pass"
switch = { id = "fsw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "SN-SW-002", endpoint_role = "host", is_primary = false, nmxc_enabled = true, nmxt_enabled = true, nvlink_domain_uuid = "9f4b45ec-705a-4af4-89f7-a112bc9c8f4e" }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("static switch host config should parse");

        let switch = config.endpoint_sources.static_bmc_endpoints[0]
            .switch
            .as_ref()
            .expect("switch metadata");

        assert_eq!(switch.endpoint_role, StaticSwitchEndpointRole::Host);
        assert!(!switch.is_primary);
        assert_eq!(switch.nmxc_enabled, Some(true));
        assert_eq!(switch.nmxt_enabled, Some(true));

        assert_eq!(
            switch.nvlink_domain_uuid.as_deref(),
            Some("9f4b45ec-705a-4af4-89f7-a112bc9c8f4e")
        );
    }

    #[test]
    fn static_switch_host_endpoint_validation() {
        scenarios!(run = |config: Box<Config>| config.validate();
            "host" {
                config_with(|config| {
                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![StaticBmcEndpoint {
                            switch: Some(static_switch()),
                            ..static_endpoint()
                        }];
                }) => Yields(()),
            }

            "missing switch metadata" {
                config_with(|config| {
                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![static_endpoint()];
                }) => FailsWith(
                    "endpoint_sources.static_switch_host_endpoints[0] requires switch metadata"
                        .to_string()
                ),
            }

            "BMC role" {
                config_with(|config| {
                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![StaticBmcEndpoint {
                            switch: Some(StaticSwitchEndpoint {
                                endpoint_role: StaticSwitchEndpointRole::Bmc,
                                ..static_switch()
                            }),
                            ..static_endpoint()
                        }];
                }) => FailsWith(
                    "endpoint_sources.static_switch_host_endpoints[0].switch.endpoint_role must be host"
                        .to_string()
                ),
            }

            "malformed BMC endpoint MAC" {
                config_with(|config| {
                    config.endpoint_sources.static_bmc_endpoints =
                        vec![StaticBmcEndpoint {
                            mac: "not-a-mac".to_string(),
                            ..static_endpoint()
                        }];
                }) => FailsWith(
                    "endpoint_sources.static_bmc_endpoints[0].mac must be a valid MAC address"
                        .to_string()
                ),
            }

            "malformed switch host endpoint MAC" {
                config_with(|config| {
                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![StaticBmcEndpoint {
                            mac: "not-a-mac".to_string(),
                            switch: Some(static_switch()),
                            ..static_endpoint()
                        }];
                }) => FailsWith(
                    "endpoint_sources.static_switch_host_endpoints[0].mac must be a valid MAC address"
                        .to_string()
                ),
            }

            "duplicate within BMC endpoint list is accepted" {
                config_with(|config| {
                    let endpoint = StaticBmcEndpoint {
                        mac: "aa:bb:cc:dd:ee:ff".to_string(),
                        ..static_endpoint()
                    };

                    config.endpoint_sources.static_bmc_endpoints = vec![
                        endpoint,
                        StaticBmcEndpoint {
                            mac: "AA:BB:CC:DD:EE:FF".to_string(),
                            ..static_endpoint()
                        },
                    ];
                }) => Yields(()),
            }

            "duplicate within host list" {
                config_with(|config| {
                    let endpoint = StaticBmcEndpoint {
                        switch: Some(static_switch()),
                        mac: "aa:bb:cc:dd:ee:ff".to_string(),
                        ..static_endpoint()
                    };

                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![endpoint.clone(), endpoint];
                }) => FailsWith(
                    "endpoint_sources.static_switch_host_endpoints[1].mac duplicates endpoint_sources.static_switch_host_endpoints[0].mac"
                        .to_string()
                ),
            }

            "duplicate across endpoint lists" {
                config_with(|config| {
                    config.endpoint_sources.static_bmc_endpoints =
                        vec![StaticBmcEndpoint {
                            mac: "AA:BB:CC:DD:EE:FF".to_string(),
                            ..static_endpoint()
                        }];

                    config.endpoint_sources.static_switch_host_endpoints =
                        vec![StaticBmcEndpoint {
                            mac: "aa:bb:cc:dd:ee:ff".to_string(),
                            switch: Some(static_switch()),
                            ..static_endpoint()
                        }];
                }) => FailsWith(
                    "endpoint_sources.static_switch_host_endpoints[0].mac duplicates endpoint_sources.static_bmc_endpoints[0].mac"
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn test_static_machine_endpoint_accepts_placement_and_nvlink_metadata() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:11"
username = "admin"
password = "pass"
labels = { site = "rno-dev7", environment = "development" }
machine = { id = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", serial = "MN-001", driver_version = "570.82", slot_number = 15, tray_index = 5, nvlink_domain_uuid = "00000000-0000-0000-0000-000000000000" }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse static machine endpoint config");

        let machine = config.endpoint_sources.static_bmc_endpoints[0]
            .machine
            .as_ref()
            .expect("machine metadata");

        config.validate().expect("valid custom labels");
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[0]
                .labels
                .get("site")
                .map(String::as_str),
            Some("rno-dev7")
        );

        assert_eq!(machine.slot_number, Some(15));
        assert_eq!(machine.tray_index, Some(5));
        assert_eq!(machine.driver_version.as_deref(), Some("570.82"));
        assert_eq!(
            machine.nvlink_domain_uuid.as_deref(),
            Some("00000000-0000-0000-0000-000000000000")
        );
    }

    #[test]
    fn test_static_endpoint_rejects_invalid_or_reserved_label_names() {
        for (name, expected) in [
            ("bad-label", "must match"),
            ("power_shelf_id", "is reserved"),
            ("system_uuid", "is reserved"),
        ] {
            let toml_content = format!(
                r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:11"
username = "admin"
password = "pass"
labels = {{ {name} = "value" }}
machine = {{ id = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0" }}
"#
            );
            let config: Config = Figment::new()
                .merge(Serialized::defaults(Config::default()))
                .merge(Toml::string(&toml_content))
                .extract()
                .expect("label syntax should parse");

            let error = config.validate().expect_err("label should be rejected");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn test_static_endpoints_accept_position_field_aliases() {
        let toml_content = r#"
[endpoint_sources.nico_api]
enabled = false

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.2"
mac = "11:22:33:44:55:11"
username = "admin"
password = "pass"
machine = { id = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0", physical_slot_number = 15, compute_tray_index = 5 }

[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.1.1"
mac = "11:22:33:44:55:66"
username = "cumulus"
password = "pass"
switch = { serial = "SN-SW-001", physical_slot_number = 7, compute_tray_index = 3 }
"#;

        let config: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml_content))
            .extract()
            .expect("failed to parse static endpoint config");

        let machine = config.endpoint_sources.static_bmc_endpoints[0]
            .machine
            .as_ref()
            .expect("machine metadata");
        assert_eq!(machine.slot_number, Some(15));
        assert_eq!(machine.tray_index, Some(5));

        let switch = config.endpoint_sources.static_bmc_endpoints[1]
            .switch
            .as_ref()
            .expect("switch metadata");
        assert_eq!(switch.slot_number, Some(7));
        assert_eq!(switch.tray_index, Some(3));
    }

    #[test]
    fn test_example_config_static_endpoint_has_switch_serial() {
        let toml_content = include_str!("../example/config.example.toml");
        let config: Config = Figment::new()
            .merge(Toml::string(toml_content))
            .extract()
            .expect("could not parse config toml file");

        config.validate().expect("example config should be valid");

        assert_eq!(config.endpoint_sources.static_bmc_endpoints.len(), 3);
        assert_eq!(
            config.endpoint_sources.static_switch_host_endpoints.len(),
            1
        );
        assert!(
            config.endpoint_sources.static_bmc_endpoints[0]
                .switch
                .is_none()
        );
        let machine = config.endpoint_sources.static_bmc_endpoints[0]
            .machine
            .as_ref()
            .expect("machine metadata");
        assert_eq!(machine.serial.as_deref(), Some("MN-001"));
        assert_eq!(machine.slot_number, Some(15));
        assert_eq!(machine.tray_index, Some(5));
        assert_eq!(
            machine.nvlink_domain_uuid.as_deref(),
            Some("00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[1]
                .switch
                .as_ref()
                .and_then(|switch| switch.id.as_deref()),
            Some("fsw100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[1]
                .switch
                .as_ref()
                .and_then(|switch| switch.serial.as_deref()),
            Some("SN-SWITCH-BMC-001")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[1]
                .switch
                .as_ref()
                .map(|switch| switch.endpoint_role),
            Some(StaticSwitchEndpointRole::Bmc)
        );
        assert_eq!(
            config.endpoint_sources.static_switch_host_endpoints[0]
                .switch
                .as_ref()
                .and_then(|switch| switch.serial.as_deref()),
            Some("SN-SWITCH-HOST-001")
        );
        assert_eq!(
            config.endpoint_sources.static_switch_host_endpoints[0]
                .switch
                .as_ref()
                .map(|switch| switch.endpoint_role),
            Some(StaticSwitchEndpointRole::Host)
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .power_shelf
                .as_ref()
                .and_then(|power_shelf| power_shelf.id.as_deref()),
            Some("fps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
        );
        assert_eq!(
            config.endpoint_sources.static_bmc_endpoints[2]
                .power_shelf
                .as_ref()
                .and_then(|power_shelf| power_shelf.serial.as_deref()),
            Some("SN-POWER-SHELF-001")
        );
        if let Configurable::Enabled(ref health_report) = config.sinks.health_report {
            assert_eq!(health_report.workers, 8);
        } else {
            panic!("health report sink is disabled");
        }
    }

    #[test]
    fn sse_log_config_validation() {
        scenarios!(run = |config: SseLogConfig| config.validate();
            "valid backoff" {
                SseLogConfig::default() => Yields(()),
            }

            "zero backoff" {
                SseLogConfig {
                    initial_backoff: Duration::ZERO,
                    ..SseLogConfig::default()
                } => FailsWith(
                    "[collectors.logs.sse].initial_backoff must be greater than 0".to_string()
                ),

                SseLogConfig {
                    max_backoff: Duration::ZERO,
                    ..SseLogConfig::default()
                } => FailsWith(
                    "[collectors.logs.sse].max_backoff must be greater than 0".to_string()
                ),
            }

            "backoff ordering" {
                SseLogConfig {
                    initial_backoff: Duration::from_secs(30),
                    max_backoff: Duration::from_secs(1),
                } => FailsWith(
                    "[collectors.logs.sse].max_backoff must be greater than or equal to initial_backoff"
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn auto_mode_config_validation() {
        scenarios!(run = |config: AutoModeConfig| config.validate();
            "valid thresholds" {
                AutoModeConfig::default() => Yields(()),
            }

            "invalid thresholds" {
                AutoModeConfig {
                    sse_not_available_threshold: 0,
                    ..AutoModeConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto].sse_not_available_threshold must be greater than 0"
                        .to_string()
                ),

                AutoModeConfig {
                    connect_failure_threshold: 0,
                    ..AutoModeConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto].connect_failure_threshold must be greater than 0"
                        .to_string()
                ),

                AutoModeConfig {
                    connect_failure_window: Duration::ZERO,
                    ..AutoModeConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto].connect_failure_window must be greater than 0"
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn logs_collector_validation() {
        scenarios!(run = |config: LogsCollectorConfig| config.validate();
            "auto mode" {
                LogsCollectorConfig::default() => Yields(()),

                LogsCollectorConfig {
                    auto: Some(AutoModeConfig::default()),
                    sse: Some(SseLogConfig::default()),
                    ..LogsCollectorConfig::default()
                } => Yields(()),

                LogsCollectorConfig {
                    auto: Some(AutoModeConfig {
                        sse_not_available_threshold: 0,
                        ..AutoModeConfig::default()
                    }),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto].sse_not_available_threshold must be greater than 0"
                        .to_string()
                ),

                LogsCollectorConfig {
                    sse: Some(SseLogConfig {
                        initial_backoff: Duration::ZERO,
                        ..SseLogConfig::default()
                    }),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.sse].initial_backoff must be greater than 0".to_string()
                ),
            }

            "periodic mode" {
                LogsCollectorConfig {
                    mode: LogCollectionMode::Periodic,
                    periodic: Some(PeriodicLogConfig::default()),
                    ..LogsCollectorConfig::default()
                } => Yields(()),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Periodic,
                    periodic: Some(PeriodicLogConfig::default()),
                    auto: Some(AutoModeConfig::default()),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto] should not be set when mode = \"periodic\""
                        .to_string()
                ),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Periodic,
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.periodic] is required when mode = \"periodic\"".to_string()
                ),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Periodic,
                    periodic: Some(PeriodicLogConfig::default()),
                    sse: Some(SseLogConfig::default()),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.sse] should not be set when mode = \"periodic\"".to_string()
                ),
            }

            "SSE mode" {
                LogsCollectorConfig {
                    mode: LogCollectionMode::Sse,
                    ..LogsCollectorConfig::default()
                } => Yields(()),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Sse,
                    sse: Some(SseLogConfig::default()),
                    ..LogsCollectorConfig::default()
                } => Yields(()),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Sse,
                    auto: Some(AutoModeConfig::default()),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.auto] should not be set when mode = \"sse\"".to_string()
                ),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Sse,
                    periodic: Some(PeriodicLogConfig::default()),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.periodic] should not be set when mode = \"sse\"".to_string()
                ),

                LogsCollectorConfig {
                    mode: LogCollectionMode::Sse,
                    sse: Some(SseLogConfig {
                        initial_backoff: Duration::from_secs(30),
                        max_backoff: Duration::from_secs(1),
                    }),
                    ..LogsCollectorConfig::default()
                } => FailsWith(
                    "[collectors.logs.sse].max_backoff must be greater than or equal to initial_backoff"
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn excluded_log_services_config_surface() {
        scenarios!(run = |toml| {
            Figment::new()
                .merge(Serialized::defaults(Config::default()))
                .merge(Toml::string(toml))
                .extract::<Config>()
                .map_err(|_| ())
                .and_then(|config| {
                    config.validate().map_err(|_| ())?;
                    let logs = config.collectors.logs.as_option().ok_or(())?;

                    Ok(match logs.mode {
                        LogCollectionMode::Auto => {
                            logs.auto_periodic_or_default().exclude_services
                        }
                        LogCollectionMode::Periodic => {
                            logs.periodic_or_default().exclude_services
                        }
                        LogCollectionMode::Sse => Vec::new(),
                    })
                })
        };
            "periodic mode" {
                r#"
[collectors.logs]
mode = "periodic"
[collectors.logs.periodic]
"# => Yields(vec!["Journal".to_string()]),

                r#"
[collectors.logs]
mode = "periodic"
[collectors.logs.periodic]
exclude_services = ["Journal", "Dump"]
"# => Yields(vec!["Journal".to_string(), "Dump".to_string()]),

                r#"
[collectors.logs]
mode = "periodic"
[collectors.logs.periodic]
exclude_services = []
"# => Yields(vec![]),
            }

            "auto fallback" {
                r#"
[collectors.logs]
mode = "auto"
[collectors.logs.auto]
"# => Yields(vec!["Journal".to_string()]),

                r#"
[collectors.logs]
mode = "auto"
[collectors.logs.auto]
exclude_services = ["Journal", "Dump"]
"# => Yields(vec!["Journal".to_string(), "Dump".to_string()]),

                r#"
[collectors.logs]
mode = "auto"
[collectors.logs.auto]
exclude_services = []
"# => Yields(vec![]),
            }
        );
    }

    #[test]
    fn log_config_parsing() {
        scenarios!(run = |toml| {
            Figment::new()
                .merge(Toml::string(toml))
                .extract::<LogsCollectorConfig>()
                .map(project_logs_config)
                .map_err(|_| ())
        };
            "default auto mode" {
                r#"mode = "auto""# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Auto,
                    validation: Ok(()),
                    configured_sse: None,
                    configured_periodic: None,
                    configured_auto: None,
                    effective_sse: SseLogConfig::default(),
                    effective_periodic: PeriodicLogConfig::default(),
                    effective_auto_periodic: PeriodicLogConfig::default(),
                }),
            }

            "periodic mode" {
                r#"
mode = "periodic"
[periodic]
logs_collection_interval = "5m"
"# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Periodic,
                    validation: Ok(()),
                    configured_sse: None,
                    configured_periodic: Some(parsed_periodic_defaults()),
                    configured_auto: None,
                    effective_sse: SseLogConfig::default(),
                    effective_periodic: parsed_periodic_defaults(),
                    effective_auto_periodic: parsed_periodic_defaults(),
                }),
            }

            "SSE mode" {
                r#"
mode = "sse"
[sse]
initial_backoff = "2s"
max_backoff = "1m"
"# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Sse,
                    validation: Ok(()),
                    configured_sse: Some(SseLogConfig {
                        initial_backoff: Duration::from_secs(2),
                        max_backoff: Duration::from_secs(60),
                    }),
                    configured_periodic: None,
                    configured_auto: None,
                    effective_sse: SseLogConfig {
                        initial_backoff: Duration::from_secs(2),
                        max_backoff: Duration::from_secs(60),
                    },
                    effective_periodic: PeriodicLogConfig::default(),
                    effective_auto_periodic: PeriodicLogConfig::default(),
                }),
            }

            "auto mode with periodic fallback" {
                r#"
mode = "auto"
[periodic]
logs_collection_interval = "5m"
[auto]
sse_not_available_threshold = 2
connect_failure_window = "10m"
connect_failure_threshold = 8
"# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Auto,
                    validation: Ok(()),
                    configured_sse: None,
                    configured_periodic: Some(parsed_periodic_defaults()),
                    configured_auto: Some(AutoModeConfig {
                        sse_not_available_threshold: 2,
                        connect_failure_window: Duration::from_secs(600),
                        connect_failure_threshold: 8,
                        periodic: parsed_periodic_defaults(),
                    }),
                    effective_sse: SseLogConfig::default(),
                    effective_periodic: parsed_periodic_defaults(),
                    effective_auto_periodic: parsed_periodic_defaults(),
                }),

                r#"
mode = "auto"
[auto]
sse_not_available_threshold = 2
connect_failure_window = "10m"
connect_failure_threshold = 8
logs_collection_interval = "2m"
state_refresh_interval = "20m"
logs_state_file = "/tmp/auto_{machine_id}.json"
"# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Auto,
                    validation: Ok(()),
                    configured_sse: None,
                    configured_periodic: None,
                    configured_auto: Some(AutoModeConfig {
                        sse_not_available_threshold: 2,
                        connect_failure_window: Duration::from_secs(600),
                        connect_failure_threshold: 8,
                        periodic: PeriodicLogConfig {
                            logs_collection_interval: Duration::from_secs(120),
                            state_refresh_interval: Duration::from_secs(1200),
                            logs_state_file: "/tmp/auto_{machine_id}.json".to_string(),
                            ..parsed_periodic_defaults()
                        },
                    }),
                    effective_sse: SseLogConfig::default(),
                    effective_periodic: PeriodicLogConfig::default(),
                    effective_auto_periodic: PeriodicLogConfig {
                        logs_collection_interval: Duration::from_secs(120),
                        state_refresh_interval: Duration::from_secs(1200),
                        logs_state_file: "/tmp/auto_{machine_id}.json".to_string(),
                        ..parsed_periodic_defaults()
                    },
                }),
            }

            "auto mode with SSE settings" {
                r#"
mode = "auto"
[sse]
initial_backoff = "3s"
max_backoff = "45s"
"# => Yields(LogsConfigProjection {
                    mode: LogCollectionMode::Auto,
                    validation: Ok(()),
                    configured_sse: Some(SseLogConfig {
                        initial_backoff: Duration::from_secs(3),
                        max_backoff: Duration::from_secs(45),
                    }),
                    configured_periodic: None,
                    configured_auto: None,
                    effective_sse: SseLogConfig {
                        initial_backoff: Duration::from_secs(3),
                        max_backoff: Duration::from_secs(45),
                    },
                    effective_periodic: PeriodicLogConfig::default(),
                    effective_auto_periodic: PeriodicLogConfig::default(),
                }),
            }
        );
    }

    #[test]
    fn test_auto_mode_config_defaults() {
        let defaults = AutoModeConfig::default();
        assert_eq!(defaults.sse_not_available_threshold, 1);
        assert_eq!(defaults.connect_failure_window, Duration::from_secs(300));
        assert_eq!(defaults.connect_failure_threshold, 5);
        assert_eq!(
            defaults.periodic.logs_collection_interval,
            Duration::from_secs(300)
        );
    }

    #[test]
    fn test_sse_log_config_defaults() {
        let defaults = SseLogConfig::default();

        assert_eq!(defaults.initial_backoff, Duration::from_secs(1));
        assert_eq!(defaults.max_backoff, Duration::from_secs(30));
    }

    #[test]
    fn reachability_validation_rejects_non_positive_runtime_values() {
        let mut config = ReachabilityCollectorConfig {
            interval: Duration::ZERO,
            ..ReachabilityCollectorConfig::default()
        };

        assert_eq!(
            config.validate().expect_err("zero interval must fail"),
            "[collectors.reachability].interval must be greater than 0"
        );

        config.interval = Duration::from_secs(1);
        config.timeout = Duration::ZERO;

        assert_eq!(
            config.validate().expect_err("zero timeout must fail"),
            "[collectors.reachability].timeout must be greater than 0"
        );
    }
}
