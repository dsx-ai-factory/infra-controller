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

use std::collections::HashSet;
use std::net::SocketAddr;
use std::str::FromStr;

use carbide_authn::config::{AllowedCertCriteria, TrustConfig};
use carbide_utils::HostPortPair;
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::acl::AclConfig;

#[derive(thiserror::Error, Debug)]
pub(crate) enum ConfigError {
    #[error("{0}")]
    Read(String),
    #[error(transparent)]
    Figment(Box<figment::Error>),
}

impl From<figment::Error> for ConfigError {
    fn from(e: figment::Error) -> Self {
        Self::Figment(Box::new(e))
    }
}

#[derive(Deserialize)]
pub(crate) struct Config {
    #[serde(default = "Defaults::listen")]
    pub(crate) listen: SocketAddr,
    #[serde(default = "Defaults::metrics_endpoint")]
    pub(crate) metrics_endpoint: SocketAddr,
    #[serde(default)]
    pub(crate) allowed_principals: HashSet<String>,
    pub(crate) tls: TlsConfig,
    pub(crate) auth: AuthConfig,
    #[serde(default)]
    pub(crate) carbide_api: CarbideApiConfig,
    pub(crate) bmc_proxy: Option<HostPortPair>,
    #[serde(default)]
    pub(crate) tracing: TracingConfig,
}

/// OpenTelemetry trace export settings for proxied BMC requests.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct TracingConfig {
    /// Whether to record and export OTLP spans. Default: false.
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Collector endpoint for OTLP/gRPC traces. Overridden by the standard
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// variables when either is set.
    #[serde(default)]
    pub(crate) otlp_endpoint: Option<String>,
}

struct Defaults;

impl Defaults {
    fn listen() -> SocketAddr {
        SocketAddr::from_str("[::]:1079").expect("BUG: default listen endpoint doesn't parse")
    }

    fn metrics_endpoint() -> SocketAddr {
        SocketAddr::from_str("[::]:1080").expect("BUG: default metrics endpoint doesn't parse")
    }

    fn trust_config() -> TrustConfig {
        TrustConfig {
            spiffe_trust_domain: "nico.local".to_string(),
            spiffe_service_base_paths: vec![
                "/forge-system/sa/".to_string(),
                "/default/sa/".to_string(),
            ],
            spiffe_machine_base_path: "/forge-system/machine/".to_string(),
            additional_issuer_cns: vec![],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TlsConfig {
    pub(crate) identity_pemfile_path: String,
    pub(crate) identity_keyfile_path: String,
    pub(crate) root_cafile_path: String,
    pub(crate) admin_root_cafile_path: String,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            identity_pemfile_path: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            identity_keyfile_path: "/var/run/secrets/spiffe.io/tls.key".to_string(),
            root_cafile_path: "/var/run/secrets/spiffe.io/ca.crt".to_string(),
            admin_root_cafile_path: "/etc/forge/carbide-bmc-proxy/site/admin_root_cert_pem"
                .to_string(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CarbideApiConfig {
    pub(crate) root_ca: String,
    pub(crate) client_cert: String,
    pub(crate) client_key: String,
    pub(crate) api_url: Url,
}

impl Default for CarbideApiConfig {
    fn default() -> Self {
        Self {
            root_ca: "/var/run/secrets/spiffe.io/ca.crt".to_string(),
            client_cert: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            client_key: "/var/run/secrets/spiffe.io/tls.key".to_string(),
            api_url: Url::parse("https://carbide-api.forge-system.svc.cluster.local:1079").unwrap(),
        }
    }
}

/// Authentication related configuration
#[derive(Clone, Deserialize)]
pub(crate) struct AuthConfig {
    /// Additional nico-admin-cli certs allowed.  This does not include actually allowing the cert to connect, just that certs that can be verified which match these criteria can do GRPC requests.
    #[serde(default)]
    pub(crate) cli_certs: Option<AllowedCertCriteria>,

    /// Configuration for the root of trust for client cert auth
    #[serde(default = "Defaults::trust_config")]
    pub(crate) trust: TrustConfig,

    #[serde(default)]
    pub(crate) acls: AclConfig,
}

impl Config {
    pub(crate) fn parse(s: &str) -> Result<Config, ConfigError> {
        Figment::new()
            .merge(Toml::string(s))
            .merge(Env::prefixed("CARBIDE_BMC_PROXY_")) // legacy, will be deprecated
            .merge(Env::prefixed("NICO_BMC_PROXY__").split("__"))
            .extract()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    const MINIMAL_TLS: &str = r#"
        [tls]
        identity_pemfile_path = "/tls/cert.pem"
        identity_keyfile_path = "/tls/key.pem"
        root_cafile_path = "/tls/ca.pem"
        admin_root_cafile_path = "/tls/admin-ca.pem"

        [auth]
    "#;

    #[derive(Clone, Copy)]
    enum ConfigCase {
        Minimal,
        ExplicitListeners,
        AllowedPrincipals,
        ProxyHostOnly,
        ProxyPortOnly,
        ProxyHostAndPort,
        ExplicitCarbideApi,
        TracingSection,
    }

    #[derive(Debug, PartialEq)]
    struct ConfigSummary {
        listen: String,
        metrics_endpoint: String,
        allowed_principals: Vec<String>,
        identity_pemfile_path: String,
        root_cafile_path: String,
        trust_domain: String,
        service_base_paths: Vec<String>,
        carbide_api_url: String,
        bmc_proxy: Option<String>,
        tracing_enabled: bool,
        tracing_otlp_endpoint: Option<String>,
    }

    fn config_source(case: ConfigCase) -> String {
        let extra = match case {
            ConfigCase::Minimal => "",
            ConfigCase::ExplicitListeners => {
                r#"
                listen = "127.0.0.1:2079"
                metrics_endpoint = "127.0.0.1:2080"
            "#
            }
            ConfigCase::AllowedPrincipals => {
                r#"
                allowed_principals = ["spiffe-service-id/carbide-api", "trusted-certificate"]
            "#
            }
            ConfigCase::ProxyHostOnly => {
                r#"
                bmc_proxy = "proxy.local"
            "#
            }
            ConfigCase::ProxyPortOnly => {
                r#"
                bmc_proxy = ":8443"
            "#
            }
            ConfigCase::ProxyHostAndPort => {
                r#"
                bmc_proxy = "proxy.local:8443"
            "#
            }
            ConfigCase::ExplicitCarbideApi => {
                r#"
                [carbide_api]
                root_ca = "/api/ca.pem"
                client_cert = "/api/cert.pem"
                client_key = "/api/key.pem"
                api_url = "https://api.example.com:1079"
            "#
            }
            ConfigCase::TracingSection => {
                r#"
                [tracing]
                enabled = true
                otlp_endpoint = "http://collector.example.com:4317"
            "#
            }
        };

        format!("{extra}\n{MINIMAL_TLS}")
    }

    fn summarize_config(case: ConfigCase) -> ConfigSummary {
        let config = Config::parse(&config_source(case)).expect("config parses");
        let mut allowed_principals = config.allowed_principals.into_iter().collect::<Vec<_>>();
        // Config stores this as a HashSet; sort the summary for deterministic
        // table comparisons.
        allowed_principals.sort();

        ConfigSummary {
            listen: config.listen.to_string(),
            metrics_endpoint: config.metrics_endpoint.to_string(),
            allowed_principals,
            identity_pemfile_path: config.tls.identity_pemfile_path,
            root_cafile_path: config.tls.root_cafile_path,
            trust_domain: config.auth.trust.spiffe_trust_domain,
            service_base_paths: config.auth.trust.spiffe_service_base_paths,
            carbide_api_url: config.carbide_api.api_url.to_string(),
            bmc_proxy: config.bmc_proxy.map(|pair| pair.to_string()),
            tracing_enabled: config.tracing.enabled,
            tracing_otlp_endpoint: config.tracing.otlp_endpoint,
        }
    }

    #[test]
    fn parses_config_shapes() {
        value_scenarios!(
            run = summarize_config;
            "minimal config uses defaults" {
                ConfigCase::Minimal => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: None,
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "explicit listeners" {
                ConfigCase::ExplicitListeners => ConfigSummary {
                    listen: "127.0.0.1:2079".to_string(),
                    metrics_endpoint: "127.0.0.1:2080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: None,
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "allowed principals" {
                ConfigCase::AllowedPrincipals => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![
                        "spiffe-service-id/carbide-api".to_string(),
                        "trusted-certificate".to_string(),
                    ],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: None,
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "proxy host only" {
                ConfigCase::ProxyHostOnly => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: Some("proxy.local".to_string()),
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "proxy port only" {
                ConfigCase::ProxyPortOnly => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: Some("8443".to_string()),
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "proxy host and port" {
                ConfigCase::ProxyHostAndPort => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: Some("proxy.local:8443".to_string()),
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "explicit Carbide API" {
                ConfigCase::ExplicitCarbideApi => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://api.example.com:1079/".to_string(),
                    bmc_proxy: None,
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "tracing section" {
                ConfigCase::TracingSection => ConfigSummary {
                    listen: "[::]:1079".to_string(),
                    metrics_endpoint: "[::]:1080".to_string(),
                    allowed_principals: vec![],
                    identity_pemfile_path: "/tls/cert.pem".to_string(),
                    root_cafile_path: "/tls/ca.pem".to_string(),
                    trust_domain: "nico.local".to_string(),
                    service_base_paths: vec![
                        "/forge-system/sa/".to_string(),
                        "/default/sa/".to_string(),
                    ],
                    carbide_api_url: "https://carbide-api.forge-system.svc.cluster.local:1079/"
                        .to_string(),
                    bmc_proxy: None,
                    tracing_enabled: true,
                    tracing_otlp_endpoint: Some("http://collector.example.com:4317".to_string()),
                },
            }
        );
    }
}
