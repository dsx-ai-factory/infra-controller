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

//! Loads and validates the site-health-probe configuration.
//!
//! The schema is framework-shaped on purpose: probes are named entries with
//! per-probe parameters so future probe kinds slot in without new top-level
//! structure.
//!
//! TODO(#5360-followup): active lifecycle probes — a machine_count parameter
//! (1 = production canary, "all" = scale test) plus blast-radius guards.
//! TODO(#5360-followup): progress reporting — publish p50/p95/p99 of machine
//! progress (e.g. created→ready) during scale tests as gauges from this pod.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use eyre::eyre;
use serde::Deserialize;
use url::Url;

/// Points at the mounted SPIFFE certificate files. The files are re-read on
/// every probe run so cert-manager rotation needs no restart.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsConfig {
    #[serde(default)]
    pub ca: String,
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub key: String,
}

/// Configures the Keycloak client-credentials flow. The client secret is read
/// from `client_secret_path` on every token refresh (rotation-safe) and is
/// never logged.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestAuth {
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret_path: String,
}

/// Probes nico-api's gRPC surface with read-only machine calls.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrpcProbe {
    #[serde(default)]
    pub enabled: bool,
    /// `host:port`, no scheme — the connection is always TLS.
    #[serde(default)]
    pub target: String,
    #[serde(default, deserialize_with = "duration_str::deserialize_duration")]
    pub interval: Duration,
    #[serde(default, deserialize_with = "duration_str::deserialize_duration")]
    pub timeout: Duration,
    #[serde(default)]
    pub page_size: i64,
    #[serde(default)]
    pub tls: TlsConfig,
}

impl GrpcProbe {
    /// The detail-read page bound; absent or non-positive values default
    /// to 50.
    pub(crate) fn effective_page_size(&self) -> usize {
        usize::try_from(self.page_size)
            .ok()
            .filter(|&n| n > 0)
            .unwrap_or(50)
    }
}

/// Probes one nico-rest-api read endpoint.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestProbe {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL including scheme, e.g. `https://nico-rest-api…:8388`.
    #[serde(default)]
    pub target: String,
    #[serde(default, deserialize_with = "duration_str::deserialize_duration")]
    pub interval: Duration,
    #[serde(default, deserialize_with = "duration_str::deserialize_duration")]
    pub timeout: Duration,
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub auth: RestAuth,
    /// Optional extra CA bundle path for the REST endpoint.
    #[serde(default)]
    pub ca: String,
}

/// The probe registry. Adding a probe kind = adding a field here plus its
/// implementation under `probes/`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Probes {
    #[serde(default)]
    pub grpc_machines: GrpcProbe,
    #[serde(default)]
    pub rest_machines: RestProbe,
    #[serde(default)]
    pub rest_instances: RestProbe,
}

/// Caps every per-API call gate applies. At most one call is in flight per
/// API; the rate and deadline are shared by the state readers and the latency
/// probes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Caps {
    pub calls_per_second: f64,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub call_timeout: Duration,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub max_backoff: Duration,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            calls_per_second: 2.0,
            call_timeout: Duration::from_secs(5),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// How host details are read. Only `full` (FindMachinesByIds) exists until
/// nico-api serves a compact host read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HostReadMode {
    #[default]
    Full,
    Compact,
    Auto,
}

/// The nico-api side of the state readers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NicoApiStateConfig {
    /// `host:port`, no scheme — the connection is always TLS.
    pub target: String,
    pub host_read: HostReadMode,
    /// IDs per by-ID read. nico-api refuses more than its `max_find_by_ids`
    /// (default 100).
    pub batch_size: usize,
    pub max_decoding_message_size: usize,
    pub tls: TlsConfig,
}

impl Default for NicoApiStateConfig {
    fn default() -> Self {
        Self {
            target: "nico-api.nico-system.svc.cluster.local:1079".to_string(),
            host_read: HostReadMode::Full,
            batch_size: 100,
            max_decoding_message_size: 64 << 20,
            tls: TlsConfig::default(),
        }
    }
}

/// One REST identity: client credentials, or a mounted token file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RestCredentials {
    pub token_url: String,
    pub client_id: String,
    pub client_secret_path: String,
    /// A bearer token re-read from this file on every call.
    pub token_file: String,
}

/// The provider org identity reads site-wide totals.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderAccount {
    pub org: String,
    pub auth: RestCredentials,
}

/// The nico-rest-api side of the state readers. GET only.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RestStateConfig {
    /// Base URL including scheme, e.g. `https://nico-rest-api…:8388`.
    pub target: String,
    /// Optional extra CA bundle path for the REST endpoint.
    pub ca: String,
    pub provider: ProviderAccount,
}

/// One reader's schedule. A sweep may outlast its interval: the next tick is
/// then skipped, never queued.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReaderConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub interval: Duration,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub timeout: Duration,
}

impl ReaderConfig {
    fn new(interval_secs: u64, timeout_secs: u64) -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(interval_secs),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

/// The state readers. A reader given in the file must set all its fields; an
/// absent reader takes its default schedule.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Readers {
    pub devices: ReaderConfig,
    pub rest_inventory: ReaderConfig,
}

impl Default for Readers {
    fn default() -> Self {
        Self {
            devices: ReaderConfig::new(120, 600),
            rest_inventory: ReaderConfig::new(120, 600),
        }
    }
}

impl Readers {
    /// Every reader with its config key, in a fixed order.
    pub(crate) fn all(&self) -> [(&'static str, &ReaderConfig); 2] {
        [
            ("devices", &self.devices),
            ("rest_inventory", &self.rest_inventory),
        ]
    }
}

/// The Prometheus-compatible store `/v1/summary` reads the probe's own
/// series back from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MetricsStoreConfig {
    pub url: String,
    /// Label matchers that pick this site's probe series. Must name `job`
    /// and `namespace`, so a shared store never mixes sites.
    pub selector: BTreeMap<String, String>,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub max_window: Duration,
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub summary_deadline: Duration,
    /// The store's scrape interval of the probe (the ServiceMonitor's).
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub scrape_interval: Duration,
}

impl Default for MetricsStoreConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            selector: BTreeMap::new(),
            max_window: Duration::from_secs(48 * 60 * 60),
            summary_deadline: Duration::from_secs(90),
            scrape_interval: Duration::from_secs(5),
        }
    }
}

/// How long a replaced snapshot that a cursor points into stays readable
/// for its paging client.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Paging {
    pub keep_seconds: u64,
    /// Replaced snapshots kept per paged family, besides the current one.
    pub max_kept_snapshots: usize,
}

impl Default for Paging {
    fn default() -> Self {
        Self {
            keep_seconds: 300,
            max_kept_snapshots: 6,
        }
    }
}

impl Paging {
    /// Replaced snapshots a paged family must hold to keep each one for
    /// `keep_seconds`: the publications that fit in that window, plus one
    /// because a slow sweep can be followed at once by an overdue tick.
    pub(crate) fn needed_for(&self, interval: Duration) -> usize {
        let per_window = (self.keep_seconds as f64 / interval.as_secs_f64()).ceil();
        per_window as usize + 1
    }
}

/// Site state readers and the `/v1` API. Off by default; with it off the
/// probe runs only the latency probes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StateConfig {
    pub enabled: bool,
    pub site_id: String,
    pub caps: Caps,
    pub nico_api: NicoApiStateConfig,
    pub rest: RestStateConfig,
    pub readers: Readers,
    pub metrics_store: MetricsStoreConfig,
    pub paging: Paging,
}

/// The root configuration.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    pub metrics_listen: String,
    /// Listen address of the `/v1` API, defaulting to `:9010`.
    #[serde(default)]
    pub api_listen: String,
    #[serde(default)]
    pub probes: Probes,
    #[serde(default)]
    pub state: StateConfig,
}

impl Config {
    /// Reads, parses, and validates the configuration file. Parsing is strict:
    /// unknown keys are a hard error, so a typoed probe toggle cannot silently
    /// disable monitoring.
    pub(crate) fn load(path: &str) -> eyre::Result<Config> {
        let raw = std::fs::read_to_string(path).map_err(|e| eyre!("read config: {e}"))?;
        let cfg: Config = serde_yaml::from_str(&raw).map_err(|e| eyre!("parse config: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Rejects configurations that could only fail at runtime, reporting every
    /// problem at once rather than one per restart. Purely a check: defaults
    /// live in the accessors ([`Config::listen_addr`],
    /// [`GrpcProbe::effective_page_size`]), so a `Config` built in code
    /// behaves the same as a loaded one.
    pub(crate) fn validate(&self) -> eyre::Result<()> {
        let mut errs: Vec<String> = Vec::new();

        let grpc = &self.probes.grpc_machines;
        if grpc.enabled {
            if grpc.target.is_empty() {
                errs.push("probes.grpc_machines.target is required".to_string());
            } else if !is_host_port(&grpc.target) {
                errs.push(
                    "probes.grpc_machines.target must be host:port with an explicit port and \
                     nothing else (no scheme, userinfo, path, query, or fragment)"
                        .to_string(),
                );
            }
            validate_timing("grpc_machines", grpc.interval, grpc.timeout, &mut errs);
            let tls = &grpc.tls;
            if tls.ca.is_empty() || tls.cert.is_empty() || tls.key.is_empty() {
                errs.push("probes.grpc_machines.tls requires ca, cert, and key paths".to_string());
            }
        }

        for (name, p) in [
            ("rest_machines", &self.probes.rest_machines),
            ("rest_instances", &self.probes.rest_instances),
        ] {
            if !p.enabled {
                continue;
            }
            if p.target.is_empty() {
                errs.push(format!("probes.{name}.target is required"));
            } else if !is_https_url(&p.target) {
                // The GET carries a bearer token; a plaintext or malformed
                // target would hand it to the network or an unintended host.
                errs.push(format!("probes.{name}.target must be an https:// URL"));
            }
            if p.org.is_empty() {
                errs.push(format!("probes.{name}.org is required"));
            }
            if p.auth.token_url.is_empty()
                || p.auth.client_id.is_empty()
                || p.auth.client_secret_path.is_empty()
            {
                errs.push(format!(
                    "probes.{name}.auth requires token_url, client_id, and client_secret_path"
                ));
            }
            if !p.auth.token_url.is_empty() && !is_https_url(&p.auth.token_url) {
                // The token POST carries the client secret in its body.
                errs.push(format!(
                    "probes.{name}.auth.token_url must be an https:// URL"
                ));
            }
            validate_timing(name, p.interval, p.timeout, &mut errs);
        }

        // The gates use the caps whether or not the state readers run: the
        // latency probes call through them too.
        self.state.caps.validate(&mut errs);
        if self.state.enabled {
            self.state.validate(&mut errs);
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(eyre!(errs.join("\n")))
        }
    }

    /// `listen_addr` resolves `metrics_listen`, defaulting to `:9009`.
    /// A bare `:port` selects the IPv6 wildcard so the shared metrics listener
    /// accepts both address families, falling back to IPv4 if IPv6 setup fails.
    /// Explicit addresses retain their configured address family.
    pub(crate) fn listen_addr(&self) -> eyre::Result<SocketAddr> {
        resolve_listen("metrics_listen", &self.metrics_listen, ":9009")
    }

    /// `api_listen_addr` resolves `api_listen` like [`Config::listen_addr`],
    /// defaulting to `:9010`.
    #[cfg_attr(not(test), expect(dead_code, reason = "the /v1 API listens on it"))]
    pub(crate) fn api_listen_addr(&self) -> eyre::Result<SocketAddr> {
        resolve_listen("api_listen", &self.api_listen, ":9010")
    }
}

fn resolve_listen(key: &str, raw: &str, default: &str) -> eyre::Result<SocketAddr> {
    let configured = if raw.is_empty() { default } else { raw };
    let listen = if configured.starts_with(':') {
        format!("[::]{configured}")
    } else {
        configured.to_string()
    };
    listen
        .to_socket_addrs()
        .map_err(|e| eyre!("invalid {key} {raw:?}: {e}"))?
        .next()
        .ok_or_else(|| eyre!("invalid {key} {raw:?}"))
}

impl Caps {
    fn validate(&self, errs: &mut Vec<String>) {
        if !(self.calls_per_second.is_finite() && self.calls_per_second > 0.0) {
            errs.push("state.caps.calls_per_second must be positive".to_string());
        }
        if self.call_timeout.is_zero() {
            errs.push("state.caps.call_timeout must be positive".to_string());
        }
        if self.max_backoff < Duration::from_secs(1) {
            errs.push("state.caps.max_backoff must be at least 1s".to_string());
        }
    }
}

impl StateConfig {
    /// Checks the state section. Only called when it is enabled: a disabled
    /// section may carry unusable defaults.
    fn validate(&self, errs: &mut Vec<String>) {
        if self.site_id.is_empty() {
            errs.push("state.site_id is required".to_string());
        }

        let readers = self.readers.all();
        for (name, reader) in readers {
            if !reader.enabled {
                continue;
            }
            // Unlike the latency probes, a sweep may outlast its interval.
            if reader.interval.is_zero() {
                errs.push(format!("state.readers.{name}.interval must be positive"));
            }
            if reader.timeout.is_zero() {
                errs.push(format!("state.readers.{name}.timeout must be positive"));
            }
        }
        if readers.iter().all(|(_, reader)| !reader.enabled) {
            errs.push("state.readers enables no reader".to_string());
        }

        if self.readers.devices.enabled {
            let nico = &self.nico_api;
            if !is_host_port(&nico.target) {
                errs.push(
                    "state.nico_api.target must be host:port with an explicit port and nothing \
                     else"
                        .to_string(),
                );
            }
            if nico.host_read != HostReadMode::Full {
                errs.push(
                    "state.nico_api.host_read must be full: nico-api has no compact host read yet"
                        .to_string(),
                );
            }
            if !(1..=100).contains(&nico.batch_size) {
                errs.push("state.nico_api.batch_size must be between 1 and 100".to_string());
            }
            if nico.max_decoding_message_size == 0 {
                errs.push("state.nico_api.max_decoding_message_size must be positive".to_string());
            }
            if nico.tls.ca.is_empty() || nico.tls.cert.is_empty() || nico.tls.key.is_empty() {
                errs.push("state.nico_api.tls requires ca, cert, and key paths".to_string());
            }
        }

        if self.readers.rest_inventory.enabled {
            let rest = &self.rest;
            if !is_https_url(&rest.target) {
                errs.push("state.rest.target must be an https:// URL".to_string());
            }
            if rest.provider.org.is_empty() {
                errs.push("state.rest.provider.org is required".to_string());
            }
            validate_credentials("state.rest.provider.auth", &rest.provider.auth, errs);
        }

        let store = &self.metrics_store;
        if !Url::parse(&store.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
            errs.push("state.metrics_store.url must be an http:// or https:// URL".to_string());
        }
        for key in ["job", "namespace"] {
            if store.selector.get(key).is_none_or(String::is_empty) {
                errs.push(format!("state.metrics_store.selector must set {key}"));
            }
        }
        if store.max_window.is_zero() {
            errs.push("state.metrics_store.max_window must be positive".to_string());
        }
        if store.summary_deadline.is_zero() {
            errs.push("state.metrics_store.summary_deadline must be positive".to_string());
        }
        if store.scrape_interval.is_zero() {
            errs.push("state.metrics_store.scrape_interval must be positive".to_string());
        } else if let Some(fastest) = readers
            .iter()
            .filter(|(_, reader)| reader.enabled && !reader.interval.is_zero())
            .map(|(_, reader)| reader.interval)
            .min()
            && store.scrape_interval * 2 > fastest
        {
            // Every sweep must land in at least two scrapes, or the store
            // can miss sweep ends and summaries lose their timing evidence.
            errs.push(format!(
                "state.metrics_store.scrape_interval must be at most half the fastest reader \
                 interval ({fastest:?})"
            ));
        }

        if self.paging.keep_seconds == 0 {
            errs.push("state.paging.keep_seconds must be positive".to_string());
        }
        for (name, reader) in [
            ("devices", &self.readers.devices),
            ("rest_inventory", &self.readers.rest_inventory),
        ] {
            if !reader.enabled || reader.interval.is_zero() || self.paging.keep_seconds == 0 {
                continue;
            }
            let needed = self.paging.needed_for(reader.interval);
            if self.paging.max_kept_snapshots < needed {
                errs.push(format!(
                    "state.paging.max_kept_snapshots must be at least {needed} for the {name} \
                     reader interval"
                ));
            }
        }
    }
}

/// A REST identity is either client credentials or a token file, never both.
fn validate_credentials(key: &str, auth: &RestCredentials, errs: &mut Vec<String>) {
    let client = [&auth.token_url, &auth.client_id, &auth.client_secret_path];
    if !auth.token_file.is_empty() {
        if client.iter().any(|field| !field.is_empty()) {
            errs.push(format!(
                "{key} takes token_file or client credentials, not both"
            ));
        }
        return;
    }
    if client.iter().any(|field| field.is_empty()) {
        errs.push(format!(
            "{key} requires token_url, client_id, and client_secret_path, or token_file"
        ));
    } else if !is_https_url(&auth.token_url) {
        // The token POST carries the client secret in its body.
        errs.push(format!("{key}.token_url must be an https:// URL"));
    }
}

fn validate_timing(name: &str, interval: Duration, timeout: Duration, errs: &mut Vec<String>) {
    if interval.is_zero() {
        errs.push(format!("probes.{name}.interval must be positive"));
    }
    if timeout.is_zero() {
        errs.push(format!("probes.{name}.timeout must be positive"));
    }
    if !timeout.is_zero() && !interval.is_zero() && timeout >= interval {
        errs.push(format!(
            "probes.{name}.timeout must be shorter than its interval"
        ));
    }
}

fn is_https_url(raw: &str) -> bool {
    Url::parse(raw).is_ok_and(|u| u.scheme() == "https")
}

/// The gRPC target is a bare authority — `host:port` with an explicit numeric
/// port and nothing else. The dialer prepends `https://` itself, so a scheme,
/// userinfo, path, query, or fragment here would build a bogus endpoint URL
/// and every run would fail with a dial error that looks like an outage
/// instead of the configuration error it is. A value carrying its own scheme
/// also fails: after prepending, the original scheme ends up in the host,
/// port, or path position and trips those constraints.
fn is_host_port(raw: &str) -> bool {
    // The explicit port is checked on the raw string because `Url::port()`
    // strips a port equal to the scheme default: with the https:// prefix,
    // an explicit `:443` would otherwise read back as "no port" and reject
    // the documented external gRPC port.
    let explicit_port = raw.rsplit_once(':').is_some_and(|(host, port)| {
        !port.is_empty()
            && port.chars().all(|c| c.is_ascii_digit())
            // Port 0 can never reach a listener, and the u16 parse also
            // bounds oversized ports without leaning on the URL parse below.
            && port.parse::<u16>().is_ok_and(|port| port != 0)
            // For a bracketed IPv6 host the port separator must follow `]`;
            // a colon inside the brackets is part of the address.
            && (!host.contains(':') || host.ends_with(']'))
    });
    explicit_port
        && Url::parse(&format!("https://{raw}")).is_ok_and(|u| {
            u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.path() == "/"
                && u.query().is_none()
                && u.fragment().is_none()
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use carbide_test_support::{Check, check_values};

    use super::*;

    fn load_str(yaml: &str) -> eyre::Result<Config> {
        let mut file = tempfile::NamedTempFile::new().expect("temp config file");
        file.write_all(yaml.as_bytes()).expect("write temp config");
        Config::load(file.path().to_str().expect("utf-8 temp path"))
    }

    const VALID: &str = r#"
metrics_listen: ":9100"
probes:
  grpc_machines:
    enabled: true
    target: nico-api.nico-system.svc.cluster.local:1079
    interval: 10s
    timeout: 5s
    page_size: 25
    tls:
      ca: /var/run/secrets/spiffe.io/ca.crt
      cert: /var/run/secrets/spiffe.io/tls.crt
      key: /var/run/secrets/spiffe.io/tls.key
  rest_machines:
    enabled: true
    target: https://nico-rest-api.nico-rest.svc.cluster.local:8388
    interval: 15s
    timeout: 5s
    org: test-org
    auth:
      token_url: https://keycloak.example.com/realms/nico/protocol/openid-connect/token
      client_id: probe
      client_secret_path: /var/secrets/keycloak-machines/secret
"#;

    #[test]
    fn load_valid_config() {
        let cfg = load_str(VALID).expect("valid config loads");
        assert_eq!(cfg.metrics_listen, ":9100");
        let grpc = &cfg.probes.grpc_machines;
        assert!(grpc.enabled);
        assert_eq!(grpc.interval, Duration::from_secs(10));
        assert_eq!(grpc.timeout, Duration::from_secs(5));
        assert_eq!(grpc.page_size, 25);
        assert_eq!(grpc.tls.key, "/var/run/secrets/spiffe.io/tls.key");
        let rest = &cfg.probes.rest_machines;
        assert!(rest.enabled);
        assert_eq!(rest.org, "test-org");
        assert!(!cfg.probes.rest_instances.enabled);
    }

    /// Every rejected configuration names the offending key. The cases share
    /// one operation (load) but assert different message fragments, so a local
    /// case table keeps each expectation next to its input.
    #[test]
    fn validation_errors() {
        struct Case {
            scenario: &'static str,
            yaml: &'static str,
            want: &'static str,
        }
        let cases = [
            Case {
                scenario: "grpc target required",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    interval: 10s\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.target is required",
            },
            Case {
                scenario: "grpc target with scheme rejected",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: https://t:1\n    interval: 10s\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.target must be host:port",
            },
            Case {
                scenario: "grpc target with malformed port rejected",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t:abc\n    interval: 10s\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.target must be host:port",
            },
            Case {
                scenario: "grpc target without port rejected",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t\n    interval: 10s\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.target must be host:port",
            },
            Case {
                scenario: "timeout must be shorter than interval",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    interval: 5s\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.timeout must be shorter than its interval",
            },
            Case {
                scenario: "zero interval",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "probes.grpc_machines.interval must be positive",
            },
            Case {
                scenario: "tls paths required",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    interval: 10s\n    timeout: 5s\n    tls: {ca: a, cert: b}\n",
                want: "probes.grpc_machines.tls requires ca, cert, and key paths",
            },
            Case {
                scenario: "rest org required",
                yaml: "probes:\n  rest_instances:\n    enabled: true\n    target: https://t\n    interval: 10s\n    timeout: 5s\n    auth: {token_url: https://k, client_id: c, client_secret_path: p}\n",
                want: "probes.rest_instances.org is required",
            },
            Case {
                scenario: "rest auth fields required",
                yaml: "probes:\n  rest_machines:\n    enabled: true\n    target: https://t\n    interval: 10s\n    timeout: 5s\n    org: o\n    auth: {token_url: https://k, client_id: c}\n",
                want: "probes.rest_machines.auth requires token_url, client_id, and client_secret_path",
            },
            Case {
                scenario: "plaintext rest target rejected",
                yaml: "probes:\n  rest_machines:\n    enabled: true\n    target: http://t\n    interval: 10s\n    timeout: 5s\n    org: o\n    auth: {token_url: https://k, client_id: c, client_secret_path: p}\n",
                want: "probes.rest_machines.target must be an https:// URL",
            },
            Case {
                scenario: "plaintext token_url rejected",
                yaml: "probes:\n  rest_machines:\n    enabled: true\n    target: https://t\n    interval: 10s\n    timeout: 5s\n    org: o\n    auth: {token_url: http://k, client_id: c, client_secret_path: p}\n",
                want: "probes.rest_machines.auth.token_url must be an https:// URL",
            },
            Case {
                scenario: "unknown field rejected",
                yaml: "metrics_listen: \":9009\"\nsurprise: true\n",
                want: "parse config",
            },
            Case {
                scenario: "bad duration rejected",
                yaml: "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    interval: soon\n    timeout: 5s\n    tls: {ca: a, cert: b, key: c}\n",
                want: "parse config",
            },
        ];
        for case in cases {
            let err = load_str(case.yaml).expect_err(case.scenario).to_string();
            assert!(
                err.contains(case.want),
                "{}: error {err:?} does not contain {:?}",
                case.scenario,
                case.want
            );
        }
    }

    /// The latency probe's timeout is its own budget for the whole run, so it
    /// may exceed the gate's call timeout: the gate then cuts a slow call at
    /// 5 s and the probe records a timeout well before its own 8 s.
    #[test]
    fn probe_timeout_may_exceed_call_timeout() {
        let cfg = load_str(
            "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    \
             interval: 10s\n    timeout: 8s\n    tls: {ca: a, cert: b, key: c}\n\
             state:\n  enabled: false\n  caps: {call_timeout: 5s}\n",
        )
        .expect("probe timeout above the call timeout loads");
        assert_eq!(cfg.probes.grpc_machines.timeout, Duration::from_secs(8));
        assert_eq!(cfg.probes.grpc_machines.interval, Duration::from_secs(10));
        assert_eq!(cfg.state.caps.call_timeout, Duration::from_secs(5));
        assert!(!cfg.state.enabled, "no state readers");
    }

    #[test]
    fn disabled_probes_skip_validation() {
        // Disabled probes carry no usable fields, and that must be fine.
        let cfg = load_str("probes:\n  grpc_machines:\n    enabled: false\n").expect("loads");
        assert_eq!(
            cfg.listen_addr().expect("defaults").to_string(),
            "[::]:9009",
            "listen address defaults"
        );
    }

    /// The gRPC target accepts exactly a bare authority with an explicit
    /// numeric port — including `:443`, which `Url::port()` alone would strip
    /// as the https default (the regression this table pins), and bracketed
    /// IPv6 hosts.
    #[test]
    fn host_port_accepts_only_bare_authorities() {
        check_values(
            [
                Check {
                    scenario: "documented external port 443",
                    input: "nico-api.example.com:443",
                    expect: true,
                },
                Check {
                    scenario: "cluster-internal port",
                    input: "nico-api.nico-system.svc.cluster.local:1079",
                    expect: true,
                },
                Check {
                    scenario: "bracketed ipv6 with port",
                    input: "[2001:db8::1]:443",
                    expect: true,
                },
                Check {
                    scenario: "missing port",
                    input: "nico-api.example.com",
                    expect: false,
                },
                Check {
                    scenario: "non-numeric port",
                    input: "nico-api:abc",
                    expect: false,
                },
                Check {
                    scenario: "scheme",
                    input: "https://nico-api:1079",
                    expect: false,
                },
                Check {
                    scenario: "userinfo",
                    input: "user@nico-api:1079",
                    expect: false,
                },
                Check {
                    scenario: "path",
                    input: "nico-api:1079/v1",
                    expect: false,
                },
                Check {
                    scenario: "unbracketed ipv6",
                    input: "2001:db8::1",
                    expect: false,
                },
                Check {
                    scenario: "port above 65535",
                    input: "nico-api:99999",
                    expect: false,
                },
                Check {
                    scenario: "port zero",
                    input: "nico-api:0",
                    expect: false,
                },
                Check {
                    scenario: "bracketed ipv6 with port zero",
                    input: "[2001:db8::1]:0",
                    expect: false,
                },
            ],
            is_host_port,
        );
    }

    /// Absent, zero, and negative page sizes all resolve to the default; an
    /// explicit positive value is honored.
    #[test]
    fn page_size_defaults_when_not_positive() {
        check_values(
            [
                Check {
                    scenario: "absent defaults",
                    input: 0,
                    expect: 50,
                },
                Check {
                    scenario: "negative defaults",
                    input: -3,
                    expect: 50,
                },
                Check {
                    scenario: "positive honored",
                    input: 25,
                    expect: 25,
                },
            ],
            |page_size| {
                GrpcProbe {
                    page_size,
                    ..GrpcProbe::default()
                }
                .effective_page_size()
            },
        );
    }

    /// A minimal valid state section; tests append or override keys.
    const STATE: &str = r#"
state:
  enabled: true
  site_id: site-1
  nico_api:
    tls: {ca: /certs/ca.crt, cert: /certs/tls.crt, key: /certs/tls.key}
  rest:
    target: https://nico-rest-api.nico-rest.svc.cluster.local:8388
    provider:
      org: provider-org
      auth: {token_url: https://k/token, client_id: probe, client_secret_path: /s/provider}
  metrics_store:
    url: http://prometheus.monitoring.svc:9090
    selector: {job: nico-site-health-probe-metrics, namespace: nico-system}
"#;

    fn state_with(extra: &str) -> eyre::Result<Config> {
        load_str(&format!("{STATE}{extra}"))
    }

    #[test]
    fn state_config_defaults() {
        let off = load_str("metrics_listen: \":9009\"\n").expect("loads");
        assert!(!off.state.enabled, "state is off by default");
        assert_eq!(
            off.api_listen_addr().expect("defaults").to_string(),
            "[::]:9010"
        );

        let cfg = state_with("").expect("minimal state section loads");
        let state = &cfg.state;
        assert_eq!(state.caps, Caps::default());
        assert_eq!(state.caps.calls_per_second, 2.0);
        assert_eq!(state.caps.call_timeout, Duration::from_secs(5));
        assert_eq!(state.caps.max_backoff, Duration::from_secs(30));
        assert_eq!(
            state.nico_api.target,
            "nico-api.nico-system.svc.cluster.local:1079"
        );
        assert_eq!(state.nico_api.host_read, HostReadMode::Full);
        assert_eq!(state.nico_api.batch_size, 100);
        assert_eq!(state.nico_api.max_decoding_message_size, 67_108_864);
        let schedule = |r: &ReaderConfig| (r.enabled, r.interval.as_secs(), r.timeout.as_secs());
        let readers = &state.readers;
        assert_eq!(schedule(&readers.devices), (true, 120, 600));
        assert_eq!(schedule(&readers.rest_inventory), (true, 120, 600));
        let store = &state.metrics_store;
        assert_eq!(store.max_window, Duration::from_secs(48 * 3600));
        assert_eq!(store.summary_deadline, Duration::from_secs(90));
        assert_eq!(store.scrape_interval, Duration::from_secs(5));
        assert_eq!(state.paging, Paging::default());
        assert_eq!(
            (state.paging.keep_seconds, state.paging.max_kept_snapshots),
            (300, 6)
        );
    }

    #[test]
    fn state_config_rejects_slow_scrape() {
        // Every sweep of the fastest reader (devices, 20 s here) must land
        // in two scrapes.
        let fast = "  readers:\n    devices: {enabled: true, interval: 20s, timeout: 600s}\n  \
                    paging: {max_kept_snapshots: 16}\n";
        let err = state_with(&format!("    scrape_interval: 11s\n{fast}"))
            .expect_err("11 s scrapes miss 20 s sweeps")
            .to_string();
        assert!(
            err.contains("state.metrics_store.scrape_interval must be at most half"),
            "{err}"
        );
        state_with(&format!("    scrape_interval: 10s\n{fast}")).expect("exactly half is enough");
        let off = "  readers:\n    devices: {enabled: false, interval: 20s, timeout: 600s}\n";
        state_with(&format!("    scrape_interval: 30s\n{off}"))
            .expect("only enabled readers bound the scrape interval");
    }

    #[test]
    fn state_config_allows_sweep_timeout_over_interval() {
        let cfg = load_str(&format!(
            "{STATE}  readers:\n    devices: {{enabled: true, interval: 20s, timeout: 600s}}\n  \
             paging: {{max_kept_snapshots: 16}}\n"
        ))
        .expect("a sweep may outlast its interval");
        assert_eq!(cfg.state.readers.devices.timeout, Duration::from_secs(600));

        // The latency probes keep their rule.
        let err = load_str(
            "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    interval: 5s\n    \
             timeout: 10s\n    tls: {ca: a, cert: b, key: c}\n",
        )
        .expect_err("latency probe timeout over interval")
        .to_string();
        assert!(
            err.contains("timeout must be shorter than its interval"),
            "{err}"
        );
    }

    #[test]
    fn state_config_rejects_undersized_paging() {
        check_values(
            [
                Check {
                    scenario: "default 6 covers the 120 s device sweep (needs 4)",
                    input: (120, 6),
                    expect: None,
                },
                Check {
                    scenario: "20 s sweeps need 16",
                    input: (20, 16),
                    expect: None,
                },
                Check {
                    scenario: "20 s sweeps with the default 6",
                    input: (20, 6),
                    expect: Some(
                        "state.paging.max_kept_snapshots must be at least 16 for the devices \
                         reader interval"
                            .to_string(),
                    ),
                },
                Check {
                    scenario: "120 s sweeps with 3, both paged readers",
                    input: (120, 3),
                    expect: Some(
                        "state.paging.max_kept_snapshots must be at least 4 for the devices \
                         reader interval\nstate.paging.max_kept_snapshots must be at least 4 \
                         for the rest_inventory reader interval"
                            .to_string(),
                    ),
                },
            ],
            |(interval, kept)| {
                let yaml = format!(
                    "{STATE}  readers:\n    devices: {{enabled: true, interval: {interval}s, \
                     timeout: 600s}}\n  paging: {{max_kept_snapshots: {kept}}}\n"
                );
                load_str(&yaml).err().map(|err| err.to_string())
            },
        );
    }

    #[test]
    fn caps_validated_without_state_readers() {
        for (caps, want) in [
            (
                "calls_per_second: 0",
                "state.caps.calls_per_second must be positive",
            ),
            (
                "call_timeout: 0s",
                "state.caps.call_timeout must be positive",
            ),
            (
                "max_backoff: 500ms",
                "state.caps.max_backoff must be at least 1s",
            ),
        ] {
            let err = load_str(&format!(
                "probes:\n  grpc_machines:\n    enabled: true\n    target: t:1\n    \
                 interval: 10s\n    timeout: 5s\n    tls: {{ca: a, cert: b, key: c}}\n\
                 state:\n  enabled: false\n  caps: {{{caps}}}\n"
            ))
            .expect_err(caps)
            .to_string();
            assert!(err.contains(want), "{caps}: {err}");
        }
    }

    #[test]
    fn state_config_validation_errors() {
        for (scenario, yaml, want) in [
            (
                "site id required",
                "state:\n  enabled: true\n",
                "state.site_id is required",
            ),
            (
                "selector needs job and namespace",
                "state:\n  enabled: true\n  metrics_store: {url: http://p:9090, selector: {job: j}}\n",
                "state.metrics_store.selector must set namespace",
            ),
            (
                "nico-api reader needs tls",
                "state:\n  enabled: true\n",
                "state.nico_api.tls requires ca, cert, and key paths",
            ),
            (
                "compact host read not available",
                "state:\n  enabled: true\n  nico_api: {host_read: auto}\n",
                "state.nico_api.host_read must be full",
            ),
            (
                "batch above max_find_by_ids",
                "state:\n  enabled: true\n  nico_api: {batch_size: 101}\n",
                "state.nico_api.batch_size must be between 1 and 100",
            ),
            (
                "rest credentials are one kind",
                "state:\n  enabled: true\n  rest: {provider: {auth: {token_file: f, client_id: c}}}\n",
                "state.rest.provider.auth takes token_file or client credentials, not both",
            ),
            (
                "partial reader rejected",
                "state:\n  readers: {devices: {enabled: true}}\n",
                "parse config",
            ),
        ] {
            let err = load_str(yaml).expect_err(scenario).to_string();
            assert!(err.contains(want), "{scenario}: {err:?} lacks {want:?}");
        }
    }

    #[test]
    fn listen_addr_parses_go_style_listen_strings() {
        check_values(
            [
                Check {
                    scenario: "bare port binds all interfaces",
                    input: ":9100",
                    expect: "[::]:9100".to_string(),
                },
                Check {
                    scenario: "host:port passes through",
                    input: "127.0.0.1:8080",
                    expect: "127.0.0.1:8080".to_string(),
                },
                Check {
                    scenario: "explicit IPv6 address passes through",
                    input: "[::1]:8080",
                    expect: "[::1]:8080".to_string(),
                },
            ],
            |listen| {
                Config {
                    metrics_listen: listen.to_string(),
                    ..Config::default()
                }
                .listen_addr()
                .expect("parses")
                .to_string()
            },
        );
    }
}
