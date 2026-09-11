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
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_tls::client_config::ClientCert;
use rpc::forge_tls_client::ForgeClientConfig;
use serde::{Deserialize, Serialize};
use tonic::codegen::http;

const DEFAULT_LISTEN_ADDRESS: &str = "[::]:53";
const DEFAULT_METRICS_ADDRESS: &str = "[::]:8053";
const DEFAULT_NEGATIVE_CACHE_TTL: u64 = 120;
const DEFAULT_NEGATIVE_CACHE_SERVFAIL_TTL: u64 = 5;
pub const NEGATIVE_CACHE_SERVFAIL_TTL_MIN_SECS: u64 = 1;
pub const NEGATIVE_CACHE_SERVFAIL_TTL_MAX_SECS: u64 = 300;
const DEFAULT_NEGATIVE_CACHE_ENTRIES_MAX_COUNT: u64 = 50_000;
const DEFAULT_UPSTREAM_LOOKUP_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    /// Address (host:port) the DNS server listens on for UDP and TCP.
    /// Default: `[::]:53`.
    #[serde(default = "Defaults::listen_addr")]
    pub listen_address: SocketAddr,
    /// URI of the nico-api server queried for DNS records.
    /// Default: the in-cluster nico-api service.
    #[serde(
        default = "Defaults::api_uri",
        serialize_with = "serialize_uri",
        deserialize_with = "deserialize_uri"
    )]
    pub api_uri: http::Uri,
    /// Path to the root CA certificate used to verify the nico-api server.
    /// Default: `/var/run/secrets/spiffe.io/ca.crt`.
    #[serde(default = "Defaults::root_ca_path")]
    pub root_ca_path: PathBuf,
    /// Path to the client certificate presented to nico-api for mTLS.
    /// Default: `/var/run/secrets/spiffe.io/tls.crt`.
    #[serde(default = "Defaults::client_cert_path")]
    pub client_cert_path: PathBuf,
    /// Path to the client private key presented to nico-api for mTLS.
    /// Default: `/var/run/secrets/spiffe.io/tls.key`.
    #[serde(default = "Defaults::client_key_path")]
    pub client_key_path: PathBuf,
    /// OTLP gRPC endpoint that traces are exported to. When unset, trace
    /// export is disabled.
    /// Default: unset (tracing disabled).
    #[serde(
        default,
        serialize_with = "serialize_optional_uri",
        deserialize_with = "deserialize_optional_uri"
    )]
    pub otlp_endpoint: Option<http::Uri>,
    /// How long to cache NXDomain and Refused responses, in seconds.
    /// Default: `120`.
    #[serde(default = "Defaults::negative_cache_ttl_secs")]
    pub negative_cache_ttl_secs: u64,
    /// How long to cache ServFail responses, in seconds. Unlike NXDomain and
    /// Refused, a ServFail reflects a transient
    /// nico-api failure, so it is cached only briefly Clamped to
    /// [1, 300]s (RFC 9520). Default: `5`.
    #[serde(default = "Defaults::negative_cache_servfail_ttl_secs")]
    pub negative_cache_servfail_ttl_secs: u64,
    /// Maximum number of entries the negative cache may hold. Once reached, new
    /// negative responses are not cached, bounding memory under a flood of
    /// distinct non-existent names. Default: `50000`.
    #[serde(default = "Defaults::negative_cache_entries_max_count")]
    pub negative_cache_entries_max_count: u64,
    /// Maximum time to wait for a single upstream `lookup_record` call before
    /// giving up and returning ServFail, in seconds. Default: `5`.
    #[serde(default = "Defaults::upstream_lookup_timeout_secs")]
    pub upstream_lookup_timeout_secs: u64,
    /// Address (host:port) the Prometheus metrics server listens on.
    /// Default: `[::]:8053`.
    #[serde(default = "Defaults::metrics_listen_addr")]
    pub metrics_listen_address: SocketAddr,
}

pub struct Defaults;

impl Defaults {
    pub fn metrics_listen_addr() -> SocketAddr {
        DEFAULT_METRICS_ADDRESS
            .parse()
            .expect("BUG: default metrics address is invalid")
    }
    pub fn negative_cache_ttl_secs() -> u64 {
        DEFAULT_NEGATIVE_CACHE_TTL
    }

    pub fn negative_cache_servfail_ttl_secs() -> u64 {
        DEFAULT_NEGATIVE_CACHE_SERVFAIL_TTL
    }

    pub fn negative_cache_entries_max_count() -> u64 {
        DEFAULT_NEGATIVE_CACHE_ENTRIES_MAX_COUNT
    }

    pub fn upstream_lookup_timeout_secs() -> u64 {
        DEFAULT_UPSTREAM_LOOKUP_TIMEOUT_SECS
    }

    pub fn listen_addr() -> SocketAddr {
        DEFAULT_LISTEN_ADDRESS
            .parse()
            .expect("BUG: default listen address is invalid")
    }

    pub fn api_uri() -> http::Uri {
        "https://carbide-api.forge-system.svc.cluster.local:1079"
            .try_into()
            .expect("BUG: default carbide URI is invalid")
    }

    pub fn root_ca_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/ca.crt".into()
    }

    pub fn client_cert_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/tls.crt".into()
    }

    pub fn client_key_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/tls.key".into()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("could not read config file: {path}: {error}")]
    CouldNotRead { path: String, error: std::io::Error },
    #[error("invalid TOML in config file: {path}: {error}")]
    InvalidToml {
        path: String,
        error: toml::de::Error,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: Defaults::listen_addr(),
            api_uri: Defaults::api_uri(),
            root_ca_path: Defaults::root_ca_path(),
            client_cert_path: Defaults::client_cert_path(),
            client_key_path: Defaults::client_key_path(),
            otlp_endpoint: None,
            negative_cache_ttl_secs: Defaults::negative_cache_ttl_secs(),
            negative_cache_servfail_ttl_secs: Defaults::negative_cache_servfail_ttl_secs(),
            negative_cache_entries_max_count: Defaults::negative_cache_entries_max_count(),
            upstream_lookup_timeout_secs: Defaults::upstream_lookup_timeout_secs(),
            metrics_listen_address: Defaults::metrics_listen_addr(),
        }
    }
}

fn serialize_uri<S>(uri: &http::Uri, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&format!("{uri}"))
}

fn deserialize_uri<'de, D>(deserializer: D) -> Result<http::Uri, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let uri_str: String = Deserialize::deserialize(deserializer)?;
    uri_str.parse().map_err(serde::de::Error::custom)
}

fn serialize_optional_uri<S>(uri: &Option<http::Uri>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match uri {
        Some(uri) => serializer.serialize_some(&format!("{uri}")),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_uri<'de, D>(deserializer: D) -> Result<Option<http::Uri>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let uri_str: Option<String> = Deserialize::deserialize(deserializer)?;
    uri_str
        .map(|uri_str| uri_str.parse().map_err(serde::de::Error::custom))
        .transpose()
}

impl Config {
    pub fn forge_client_config(&self) -> ForgeClientConfig {
        let forge_root_ca = self
            .root_ca_path
            .to_str()
            .expect("forge root CA path is not valid UTF-8")
            .to_string();
        let client_cert = ClientCert {
            cert_path: self
                .client_cert_path
                .to_str()
                .expect("client cert path is not valid UTF-8")
                .to_string(),
            key_path: self
                .client_key_path
                .to_str()
                .expect("client key path is not valid UTF-8")
                .to_string(),
        };
        ForgeClientConfig::new(forge_root_ca, Some(client_cert))
    }

    /// The ServFail cache TTL as a `Duration`, clamped to the supported
    /// [1, 300]s range (RFC 9520).
    pub fn servfail_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.negative_cache_servfail_ttl_secs.clamp(
            NEGATIVE_CACHE_SERVFAIL_TTL_MIN_SECS,
            NEGATIVE_CACHE_SERVFAIL_TTL_MAX_SECS,
        ))
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let cfg = std::fs::read_to_string(path).map_err(|error| ConfigError::CouldNotRead {
            path: path.to_string_lossy().to_string(),
            error,
        })?;
        toml::from_str::<Self>(&cfg).map_err(|error| ConfigError::InvalidToml {
            path: path.to_string_lossy().to_string(),
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_to_defaults() {
        let toml = include_str!("test/carbide-dns.toml");
        let config: Config = toml::from_str(toml).expect("example config should parse");
        assert_eq!(config, Config::default());
    }

    // `Config::default()` runs the `expect`-based parses in `Defaults`.
    #[test]
    fn defaults_are_sane() {
        let config = Config::default();
        assert_eq!(config.listen_address, "[::]:53".parse().unwrap());
        assert_eq!(config.metrics_listen_address, "[::]:8053".parse().unwrap());
        assert_eq!(config.negative_cache_ttl_secs, 120);
        assert_eq!(config.negative_cache_servfail_ttl_secs, 5);
        assert_eq!(config.upstream_lookup_timeout_secs, 5);
        assert_eq!(config.negative_cache_entries_max_count, 50_000);
    }

    #[test]
    fn servfail_ttl_is_clamped_to_supported_range() {
        let with_ttl = |secs| Config {
            negative_cache_servfail_ttl_secs: secs,
            ..Config::default()
        };

        assert_eq!(
            with_ttl(0).servfail_cache_ttl(),
            Duration::from_secs(NEGATIVE_CACHE_SERVFAIL_TTL_MIN_SECS)
        );
        assert_eq!(
            with_ttl(u64::MAX).servfail_cache_ttl(),
            Duration::from_secs(NEGATIVE_CACHE_SERVFAIL_TTL_MAX_SECS)
        );
        assert_eq!(with_ttl(5).servfail_cache_ttl(), Duration::from_secs(5));
    }

    // Serializing the defaults and reading them back must complete successfully
    #[test]
    fn config_round_trips_through_toml() {
        let original = Config::default();
        let serialized = toml::to_string(&original).expect("serialize config");
        let parsed: Config = toml::from_str(&serialized).expect("deserialize config");
        assert_eq!(original, parsed);
    }

    // Omitted keys fall back to their defaults; a present key overrides only
    // itself.
    #[test]
    fn omitted_keys_fall_back_to_defaults() {
        let config: Config =
            toml::from_str("negative_cache_ttl_secs = 300").expect("partial config should parse");
        assert_eq!(config.negative_cache_ttl_secs, 300);
        assert_eq!(
            config.negative_cache_entries_max_count,
            Defaults::negative_cache_entries_max_count()
        );
        assert_eq!(config.api_uri, Defaults::api_uri());
    }
}
