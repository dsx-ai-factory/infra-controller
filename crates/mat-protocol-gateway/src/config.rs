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

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::{Deserialize, Serialize};
pub use ufm_mock::TlsConfig;
use ufm_mock::UfmMockConfig;
use url::Url;

/// Environment prefix for overriding any configuration key, using `__` as the path separator.
///
/// `MAT_PROTOCOL_GATEWAY__CONTROLLER__SOURCES_URL` sets `controller.sources_url`.
const ENV_PREFIX: &str = "MAT_PROTOCOL_GATEWAY__";

/// Default pod-local address where the Go controller publishes its source list.
const DEFAULT_SOURCES_URL: &str = "http://127.0.0.1:8090/v1/sources";

/// Top-level configuration of the protocol gateway process.
///
/// The gateway owns its listener, TLS, and metrics settings. The UFM section reuses the shared
/// [`UfmMockConfig`], except that inventory sources are always taken from the controller rather
/// than from `ufm.inventory.static_sources`, and `ufm.metrics_address` is rejected in favour of
/// the top-level `metrics_address`. The UFM authentication token comes from
/// [`ufm_mock::UFM_MOCK_AUTH_TOKEN_ENV`], never from this file.
///
/// Unknown keys in the gateway-owned tables are a startup error so that a mis-shaped file or a
/// misspelled environment override is noticed before the process serves anything. Keys under
/// `[ufm]` are checked by `ufm-mock`, whose shared config type cannot deny unknown fields because
/// the standalone binary flattens it.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Address serving the UFM and probe routes. Defaults to `0.0.0.0:9888`.
    #[serde(default = "default_listen_address")]
    pub listen_address: SocketAddr,
    /// Serves `listen_address` over TLS when set; plain HTTP otherwise.
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    /// Optional Prometheus endpoint; disabled when omitted.
    #[serde(default)]
    pub metrics_address: Option<SocketAddr>,
    /// How the gateway reaches the controller's source list.
    #[serde(default)]
    pub controller: ControllerConfig,
    /// TLS trust settings for discovered inventory endpoints.
    #[serde(default)]
    pub sources: SourceClientConfig,
    /// Fabric and reconciliation settings shared with `ufm-mock`.
    #[serde(default)]
    pub ufm: UfmMockConfig,
}

impl GatewayConfig {
    /// Loads configuration from an optional TOML file with environment overrides applied.
    ///
    /// Omitting the file means defaults. A file that is named but missing or unparseable is an
    /// error, so a wrong mount or argument stops the process instead of serving defaults.
    pub fn load(path: Option<&Path>) -> eyre::Result<Self> {
        let mut figment = Figment::new();
        if let Some(path) = path {
            figment = figment.merge(Toml::file_exact(path));
        }
        let mut config: Self = figment
            .merge(Env::prefixed(ENV_PREFIX).split("__"))
            .extract()?;
        // The gateway is the only reason this UFM instance exists, so the hosted-mode activation
        // flag has no independent meaning here.
        config.ufm.enabled = true;
        config.validate()?;
        Ok(config)
    }

    /// Rejects settings that cannot become active: zero intervals, no startup attempts, or UFM
    /// options the gateway owns itself.
    pub fn validate(&self) -> eyre::Result<()> {
        eyre::ensure!(
            self.ufm.metrics_address.is_none(),
            "ufm.metrics_address is not available in the gateway; set the top-level metrics_address instead"
        );
        eyre::ensure!(
            self.ufm.inventory.static_sources.is_empty(),
            "ufm.inventory.static_sources is derived from the controller source list and must be empty"
        );
        eyre::ensure!(
            !self.ufm.include_local_inventory,
            "ufm.include_local_inventory is not available in the gateway"
        );
        self.ufm.validate()?;
        eyre::ensure!(
            !self.controller.poll_interval.is_zero(),
            "controller.poll_interval must be nonzero"
        );
        eyre::ensure!(
            !self.controller.request_timeout.is_zero(),
            "controller.request_timeout must be nonzero"
        );
        eyre::ensure!(
            self.controller.startup_attempts > 0,
            "controller.startup_attempts must be at least 1"
        );
        Ok(())
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            listen_address: default_listen_address(),
            tls: None,
            metrics_address: None,
            controller: ControllerConfig::default(),
            sources: SourceClientConfig::default(),
            ufm: UfmMockConfig {
                enabled: true,
                ..UfmMockConfig::default()
            },
        }
    }
}

/// How the gateway reaches the Go controller's pod-local source list.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ControllerConfig {
    /// Endpoint returning the versioned source list.
    #[serde(default = "default_sources_url")]
    pub sources_url: Url,
    /// Interval between generation checks once the gateway is serving.
    #[serde(
        default = "default_controller_poll_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub poll_interval: Duration,
    /// Per-request timeout for `sources_url`.
    #[serde(
        default = "default_controller_request_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub request_timeout: Duration,
    /// Number of attempts to obtain a ready, non-empty source list before the process gives up.
    #[serde(default = "default_startup_attempts")]
    pub startup_attempts: u32,
    /// Delay between startup attempts.
    #[serde(
        default = "default_startup_retry_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub startup_retry_interval: Duration,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            sources_url: default_sources_url(),
            poll_interval: default_controller_poll_interval(),
            request_timeout: default_controller_request_timeout(),
            startup_attempts: default_startup_attempts(),
            startup_retry_interval: default_startup_retry_interval(),
        }
    }
}

/// TLS settings applied to every discovered machine-a-tron inventory endpoint.
///
/// Discovered sources share one trust configuration because they are replicas of the same
/// deployment presenting certificates from the same issuer.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceClientConfig {
    /// Additional PEM root trusted for inventory requests.
    #[serde(default)]
    pub ca_cert_path: Option<PathBuf>,
    /// Disables certificate verification for inventory requests.
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

fn default_listen_address() -> SocketAddr {
    "0.0.0.0:9888".parse().unwrap()
}

fn default_sources_url() -> Url {
    Url::parse(DEFAULT_SOURCES_URL).unwrap()
}

fn default_controller_poll_interval() -> Duration {
    Duration::from_secs(15)
}

fn default_controller_request_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_startup_attempts() -> u32 {
    150
}

fn default_startup_retry_interval() -> Duration {
    Duration::from_secs(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_configuration_uses_documented_defaults() {
        let config: GatewayConfig = Figment::new().merge(Toml::string("")).extract().unwrap();

        assert_eq!(config.controller.sources_url.as_str(), DEFAULT_SOURCES_URL);
        assert_eq!(config.listen_address.port(), 9888);
        assert!(config.tls.is_none());
        config.validate().unwrap();
    }

    #[test]
    fn rejects_static_sources_in_the_ufm_section() {
        let config: GatewayConfig = Figment::new()
            .merge(Toml::string(
                r#"
                [[ufm.inventory.static_sources]]
                url = "https://mat.example:1266/machines/status"
                "#,
            ))
            .extract()
            .unwrap();

        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("static_sources"));
    }

    #[test]
    fn rejects_ufm_metrics_address_in_favour_of_the_gateway_setting() {
        let config: GatewayConfig = Figment::new()
            .merge(Toml::string(
                r#"
                metrics_address = "0.0.0.0:9889"

                [ufm]
                metrics_address = "0.0.0.0:9890"
                "#,
            ))
            .extract()
            .unwrap();

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("ufm.metrics_address"), "{error}");
        assert!(error.contains("metrics_address instead"), "{error}");
    }

    #[test]
    fn rejects_unknown_keys_and_names_them() {
        for (toml, key) in [
            ("listen_addres = \"0.0.0.0:1\"", "listen_addres"),
            ("probe_address = \"0.0.0.0:1\"", "probe_address"),
            ("[controller]\npol_interval = \"1s\"", "pol_interval"),
            (
                "[sources]\ninsecure_skip_verif = true",
                "insecure_skip_verif",
            ),
            (
                "[tls]\ncert_path = \"/c\"\nkey_path = \"/k\"\nca_path = \"/ca\"",
                "ca_path",
            ),
        ] {
            let error = Figment::new()
                .merge(Toml::string(toml))
                .extract::<GatewayConfig>()
                .map(|_| ())
                .unwrap_err()
                .to_string();
            assert!(error.contains(key), "{toml}: {error}");
        }
    }

    #[test]
    fn parses_durations_and_tls_paths() {
        let config: GatewayConfig = Figment::new()
            .merge(Toml::string(
                r#"
                listen_address = "0.0.0.0:9443"

                [tls]
                cert_path = "/certs/tls.crt"
                key_path = "/certs/tls.key"

                [controller]
                sources_url = "http://127.0.0.1:9090/v1/sources"
                poll_interval = "30s"
                startup_attempts = 3

                [sources]
                insecure_skip_verify = true
                "#,
            ))
            .extract()
            .unwrap();

        assert_eq!(config.listen_address.port(), 9443);
        assert_eq!(
            config.tls.as_ref().unwrap().cert_path,
            PathBuf::from("/certs/tls.crt")
        );
        assert_eq!(config.controller.poll_interval, Duration::from_secs(30));
        assert_eq!(config.controller.startup_attempts, 3);
        assert!(config.sources.insecure_skip_verify);
        config.validate().unwrap();
    }

    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn load_applies_file_then_environment_and_forces_ufm_enabled() {
        // Jail clears inherited configuration and serializes this test with others that modify
        // process-global environment variables.
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "gateway.toml",
                r#"
                listen_address = "127.0.0.1:9443"

                [controller]
                poll_interval = "20s"
                startup_attempts = 3

                [ufm]
                enabled = false
                "#,
            )?;
            let path = jail.directory().join("gateway.toml");
            jail.set_env("MAT_PROTOCOL_GATEWAY__CONTROLLER__POLL_INTERVAL", "30s");

            let config = GatewayConfig::load(Some(&path)).unwrap();

            assert_eq!(config.listen_address.port(), 9443);
            assert_eq!(config.controller.startup_attempts, 3);
            assert_eq!(
                config.controller.poll_interval,
                Duration::from_secs(30),
                "environment overrides the file"
            );
            assert!(
                config.ufm.enabled,
                "the file cannot disable the embedded UFM"
            );

            jail.set_env("MAT_PROTOCOL_GATEWAY__LISTEN_ADDRES", "127.0.0.1:1");
            let error = GatewayConfig::load(Some(&path)).unwrap_err().to_string();
            assert!(error.contains("listen_addres"), "{error}");
            assert!(error.contains("LISTEN_ADDRES"), "{error}");
            Ok(())
        });
    }

    #[test]
    #[allow(clippy::result_large_err)] // Figment controls the error representation.
    fn load_without_a_file_uses_defaults_but_a_missing_named_file_fails() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            assert_eq!(GatewayConfig::load(None).unwrap(), GatewayConfig::default());

            let path = jail.directory().join("missing.toml");
            let error = GatewayConfig::load(Some(&path)).unwrap_err().to_string();
            assert!(error.contains("missing.toml"), "{error}");
            Ok(())
        });
    }
}
