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
use std::num::NonZeroU32;
use std::str::FromStr;
use std::time::Duration;

use carbide_authn::config::{AllowedCertCriteria, TrustConfig};
use carbide_instrument::LabelValue;
use carbide_utils::HostPortPair;
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use http::StatusCode;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use crate::acl::AclConfig;
use crate::class::{ClassTable, ClassTableError};

#[derive(thiserror::Error, Debug)]
pub(crate) enum ConfigError {
    #[error("{0}")]
    Read(String),
    #[error(transparent)]
    Figment(Box<figment::Error>),
    #[error("admission.{0}")]
    AdmissionBreaker(BreakerConfigError),
    #[error(
        "a class with a latency target needs admission.max_in_flight_per_bmc, the limit its slo shares out"
    )]
    SloWithoutLimit,
    #[error("admission.slo min_in_flight must be at most admission.max_in_flight_per_bmc")]
    SloFloorAboveLimit,
    #[error(transparent)]
    Classes(#[from] ClassTableError),
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
    pub(crate) redirects: RedirectConfig,
    #[serde(default)]
    pub(crate) tracing: TracingConfig,
    /// Request classes, written as `[[class]]` tables. Absent keeps every
    /// request in the implicit default class.
    #[serde(rename = "class", default)]
    pub(crate) classes: ClassTable,
    #[serde(default)]
    pub(crate) admission: AdmissionConfig,
}

/// Limits on the requests one proxy replica sends to each BMC, across every
/// class. A class's own limit applies within this one.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionConfig {
    /// Requests one replica sends to one BMC at a time. Absent is unlimited.
    #[serde(default)]
    pub(crate) max_in_flight_per_bmc: Option<NonZeroU32>,
    /// Settings every class's breaker takes where its own `breaker` sets
    /// none. Present turns a breaker on for every class; absent, only for
    /// classes with a `breaker` of their own.
    #[serde(default)]
    pub(crate) breaker: Option<BreakerSettings>,
    /// How the slots classes without a latency target may hold at a BMC
    /// follow the classes with one.
    #[serde(default)]
    pub(crate) slo: SloConfig,
}

/// Most requests of a class with a latency target a decision weighs.
const MAX_SLO_WINDOW: u32 = 1024;

/// Longest a BMC's classes with a latency target may go unmeasured before
/// the other classes get its whole limit back.
const MAX_SLO_RESET_AFTER: Duration = Duration::from_secs(60 * 60);

/// `[admission.slo]`: at each BMC, once a class with a latency target has
/// had `window` requests answered, the proxy weighs them against its target.
/// A miss cuts the slots the classes without a target may hold there to
/// `decrease` of what they were, down to `min_in_flight`; a target met
/// raises them by `increase`, up to `max_in_flight_per_bmc`. With no answer
/// from a class with a target for `reset_after`, they get the whole limit
/// back.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "SloDefinition")]
pub(crate) struct SloConfig {
    pub(crate) window: NonZeroU32,
    pub(crate) increase: NonZeroU32,
    pub(crate) decrease: f32,
    pub(crate) min_in_flight: NonZeroU32,
    pub(crate) reset_after: Duration,
}

impl Default for SloConfig {
    fn default() -> Self {
        Self {
            window: NonZeroU32::new(20).expect("20 is not zero"),
            increase: NonZeroU32::MIN,
            decrease: 0.5,
            min_in_flight: NonZeroU32::MIN,
            reset_after: Duration::from_secs(60),
        }
    }
}

/// `[admission.slo]` as written in the config file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SloDefinition {
    window: Option<NonZeroU32>,
    increase: Option<NonZeroU32>,
    decrease: Option<f32>,
    min_in_flight: Option<NonZeroU32>,
    #[serde(with = "humantime_serde", default)]
    reset_after: Option<Duration>,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum SloConfigError {
    #[error("slo window must be at most {MAX_SLO_WINDOW}")]
    Window,
    #[error("slo decrease must be above 0 and below 1")]
    Decrease,
    #[error("slo reset_after must be above zero and at most {MAX_SLO_RESET_AFTER:?}")]
    ResetAfter,
}

impl TryFrom<SloDefinition> for SloConfig {
    type Error = SloConfigError;

    fn try_from(definition: SloDefinition) -> Result<Self, Self::Error> {
        let defaults = Self::default();
        let config = Self {
            window: definition.window.unwrap_or(defaults.window),
            increase: definition.increase.unwrap_or(defaults.increase),
            decrease: definition.decrease.unwrap_or(defaults.decrease),
            min_in_flight: definition.min_in_flight.unwrap_or(defaults.min_in_flight),
            reset_after: definition.reset_after.unwrap_or(defaults.reset_after),
        };
        if config.window.get() > MAX_SLO_WINDOW {
            return Err(SloConfigError::Window);
        }
        if !(config.decrease > 0.0 && config.decrease < 1.0) {
            return Err(SloConfigError::Decrease);
        }
        if config.reset_after.is_zero() || config.reset_after > MAX_SLO_RESET_AFTER {
            return Err(SloConfigError::ResetAfter);
        }
        Ok(config)
    }
}

/// A class's latency target: at least `percentile` of its requests to a BMC
/// answered, from their arrival at the proxy to the BMC's response headers,
/// within `latency`.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "SloTargetDefinition")]
pub(crate) struct SloTarget {
    pub(crate) latency: Duration,
    pub(crate) percentile: f64,
}

/// A class's `slo` as written in the config file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SloTargetDefinition {
    #[serde(with = "humantime_serde")]
    latency: Duration,
    #[serde(default = "default_slo_percentile")]
    percentile: f64,
}

fn default_slo_percentile() -> f64 {
    0.9
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum SloTargetError {
    #[error("slo latency must be above zero")]
    Latency,
    #[error("slo percentile must be above 0 and at most 1")]
    Percentile,
}

impl TryFrom<SloTargetDefinition> for SloTarget {
    type Error = SloTargetError;

    fn try_from(definition: SloTargetDefinition) -> Result<Self, Self::Error> {
        if definition.latency.is_zero() {
            return Err(SloTargetError::Latency);
        }
        if !(definition.percentile > 0.0 && definition.percentile <= 1.0) {
            return Err(SloTargetError::Percentile);
        }
        Ok(Self {
            latency: definition.latency,
            percentile: definition.percentile,
        })
    }
}

/// Longest `cool_down` a breaker may set: a BMC back up is served again
/// within this long.
const MAX_BREAKER_COOL_DOWN: Duration = Duration::from_secs(10 * 60);

/// Most exchanges a breaker remembers: each BMC in use keeps this many
/// outcomes for each class with a breaker.
const MAX_BREAKER_WINDOW: u32 = 1024;

/// A breaker's settings where neither its class nor `[admission.breaker]`
/// sets them.
const DEFAULT_FAILURE_THRESHOLD: f32 = 0.5;
const DEFAULT_WINDOW: u32 = 32;
const DEFAULT_MIN_SAMPLES: u32 = 5;
const DEFAULT_COOL_DOWN: Duration = Duration::from_secs(10);
const DEFAULT_TRIP_ON: [Trip; 2] = [Trip::Unreachable, Trip::Timeout];

/// A breaker's settings as written, in `[admission.breaker]` or a class's
/// `breaker`. A class's breaker takes each setting from its own table, then
/// from `[admission.breaker]`, then the default.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BreakerSettings {
    failure_threshold: Option<f32>,
    window: Option<u32>,
    min_samples: Option<u32>,
    #[serde(with = "humantime_serde", default)]
    cool_down: Option<Duration>,
    trip_on: Option<Vec<Trip>>,
}

/// A class's circuit breaker at each BMC: once at least `min_samples` of the
/// class's last `window` exchanges with a BMC were seen, and at least
/// `failure_threshold` of them failed in one of the ways `trip_on` names, the
/// proxy sends that BMC none of the class's requests for `cool_down`, then
/// one whose outcome decides whether to resume.
pub(crate) struct BreakerConfig {
    pub(crate) failure_threshold: f32,
    pub(crate) window: u32,
    pub(crate) min_samples: u32,
    pub(crate) cool_down: Duration,
    trip_on: Vec<Trip>,
}

/// How an exchange with a BMC can fail, as a breaker's `trip_on` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trip {
    /// `"unreachable"`: the proxy could not connect to the BMC.
    Unreachable,
    /// `"timeout"`: the BMC did not answer in time.
    Timeout,
    /// `"5xx"`: the BMC answered with any 5xx status.
    ServerError,
    /// A status from 400 to 599, such as `"503"`: the BMC answered with it.
    Status(StatusCode),
}

#[derive(thiserror::Error, Debug)]
#[error(
    r#"breaker trip_on value {0:?} must be "unreachable", "timeout", "5xx", or a status from 400 to 599"#
)]
pub(crate) struct UnknownTrip(String);

impl FromStr for Trip {
    type Err = UnknownTrip;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "unreachable" => Ok(Self::Unreachable),
            "timeout" => Ok(Self::Timeout),
            "5xx" => Ok(Self::ServerError),
            status => StatusCode::from_bytes(status.as_bytes())
                .ok()
                .filter(|status| status.is_client_error() || status.is_server_error())
                .map(Self::Status)
                .ok_or_else(|| UnknownTrip(value.to_string())),
        }
    }
}

impl<'de> Deserialize<'de> for Trip {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum BreakerConfigError {
    #[error("breaker failure_threshold must be above 0 and at most 1")]
    FailureThreshold,
    #[error("breaker min_samples must be at least 1")]
    MinSamples,
    #[error("breaker window must be from min_samples to {MAX_BREAKER_WINDOW}")]
    Window,
    #[error("breaker cool_down must be above zero and at most {MAX_BREAKER_COOL_DOWN:?}")]
    CoolDown,
}

impl BreakerConfig {
    /// The breaker of a class whose own settings are `own`, under
    /// `[admission.breaker]`'s `inherited`: none when neither is set, or when
    /// `trip_on` is empty.
    pub(crate) fn resolve(
        own: Option<&BreakerSettings>,
        inherited: Option<&BreakerSettings>,
    ) -> Result<Option<Self>, BreakerConfigError> {
        let unset = BreakerSettings::default();
        let (own, inherited) = match (own, inherited) {
            (None, None) => return Ok(None),
            (own, inherited) => (own.unwrap_or(&unset), inherited.unwrap_or(&unset)),
        };
        let failure_threshold = own
            .failure_threshold
            .or(inherited.failure_threshold)
            .unwrap_or(DEFAULT_FAILURE_THRESHOLD);
        let window = own.window.or(inherited.window).unwrap_or(DEFAULT_WINDOW);
        let min_samples = own
            .min_samples
            .or(inherited.min_samples)
            .unwrap_or(DEFAULT_MIN_SAMPLES);
        let cool_down = own
            .cool_down
            .or(inherited.cool_down)
            .unwrap_or(DEFAULT_COOL_DOWN);
        if !(failure_threshold > 0.0 && failure_threshold <= 1.0) {
            return Err(BreakerConfigError::FailureThreshold);
        }
        if min_samples == 0 {
            return Err(BreakerConfigError::MinSamples);
        }
        if window < min_samples || window > MAX_BREAKER_WINDOW {
            return Err(BreakerConfigError::Window);
        }
        if cool_down.is_zero() || cool_down > MAX_BREAKER_COOL_DOWN {
            return Err(BreakerConfigError::CoolDown);
        }
        let trip_on = own
            .trip_on
            .as_ref()
            .or(inherited.trip_on.as_ref())
            .map_or_else(|| DEFAULT_TRIP_ON.to_vec(), Clone::clone);
        if trip_on.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            failure_threshold,
            window,
            min_samples,
            cool_down,
            trip_on,
        }))
    }

    /// Whether an exchange that ended as `ended`, a `Status` for any answer,
    /// counts as a failure.
    pub(crate) fn trips_on(&self, ended: Trip) -> bool {
        self.trip_on.iter().any(|&trip| {
            trip == ended
                || (trip == Trip::ServerError
                    && matches!(ended, Trip::Status(status) if status.is_server_error()))
        })
    }
}

/// How the proxy handles redirect responses from a BMC.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, LabelValue, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RedirectMode {
    /// Follow redirects only when the destination has the original request's
    /// scheme, host, and effective port.
    #[default]
    FollowSameOrigin,
    /// Return safe same-BMC redirects to the caller for a separately
    /// authorized follow-up request.
    ReturnToClient,
}

/// Redirect handling settings.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RedirectConfig {
    /// Redirect behavior. Defaults to [`RedirectMode::FollowSameOrigin`].
    pub(crate) mode: RedirectMode,
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
        let mut config: Config = Figment::new()
            .merge(Toml::string(s))
            .merge(Env::prefixed("CARBIDE_BMC_PROXY_")) // legacy, will be deprecated
            .merge(Env::prefixed("NICO_BMC_PROXY__").split("__"))
            .extract()?;
        // Checked alone first, so that its errors name it rather than a class
        // inheriting them, and are found even when every class overrides it.
        BreakerConfig::resolve(None, config.admission.breaker.as_ref())
            .map_err(ConfigError::AdmissionBreaker)?;
        config
            .classes
            .resolve_breakers(config.admission.breaker.as_ref())?;
        match config.admission.max_in_flight_per_bmc {
            None if config.classes.iter().any(|class| class.slo.is_some()) => {
                return Err(ConfigError::SloWithoutLimit);
            }
            Some(limit) if config.admission.slo.min_in_flight > limit => {
                return Err(ConfigError::SloFloorAboveLimit);
            }
            _ => {}
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};

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
        RedirectsSection,
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
        redirect_mode: RedirectMode,
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
            ConfigCase::RedirectsSection => {
                r#"
                [redirects]
                mode = "return_to_client"
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
            redirect_mode: config.redirects.mode,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
                    tracing_enabled: false,
                    tracing_otlp_endpoint: None,
                },
            }

            "redirect handling mode" {
                ConfigCase::RedirectsSection => ConfigSummary {
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
                    redirect_mode: RedirectMode::ReturnToClient,
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
                    redirect_mode: RedirectMode::FollowSameOrigin,
                    tracing_enabled: true,
                    tracing_otlp_endpoint: Some("http://collector.example.com:4317".to_string()),
                },
            }
        );
    }

    #[test]
    fn rejects_unknown_redirect_mode() {
        let source = format!(
            r#"
            [redirects]
            mode = "follow_anywhere"

            {MINIMAL_TLS}
            "#
        );

        let error = Config::parse(&source)
            .err()
            .expect("unknown redirect mode must fail");
        let message = error.to_string();
        assert!(message.contains("follow_anywhere"));
        assert!(message.contains("follow_same_origin"));
        assert!(message.contains("return_to_client"));
    }

    /// A breaker's (failure_threshold, window, min_samples, cool_down in
    /// milliseconds, trip_on).
    type BreakerSummary = (f32, u32, u32, u128, Vec<Trip>);

    /// The config with `admission`, and a `power` class with `power` as its
    /// own breaker settings.
    fn with_power_class((admission, power): (&str, &str)) -> Result<Config, ConfigError> {
        Config::parse(&format!(
            r#"
            {admission}

            [[class]]
            name = "power"
            match = ["PATCH /redfish/v1/**"]
            {power}
            {MINIMAL_TLS}
            "#
        ))
    }

    /// The breakers of the `power` class and of the default class in
    /// [`with_power_class`].
    fn breakers_of(settings: (&str, &str)) -> Result<[Option<BreakerSummary>; 2], ()> {
        let config = with_power_class(settings).map_err(drop)?;
        let breaker_of = |method| {
            config
                .classes
                .classify(&method, "/redfish/v1/Chassis", &[])
                .breaker
                .as_ref()
                .map(|breaker| {
                    (
                        breaker.failure_threshold,
                        breaker.window,
                        breaker.min_samples,
                        breaker.cool_down.as_millis(),
                        breaker.trip_on.clone(),
                    )
                })
        };
        Ok([
            breaker_of(http::Method::PATCH),
            breaker_of(http::Method::GET),
        ])
    }

    /// `[admission.breaker]` turns a breaker on for every class, and a
    /// class's `breaker` for that class. A class's breaker takes each setting
    /// from its own table, then `[admission.breaker]`, then the default, and
    /// an empty `trip_on` turns it off. Settings out of bounds, alone or
    /// together, or unknown, do not load.
    #[test]
    fn breaker_settings_parse() {
        let defaults = || Some((0.5, 32, 5, 10_000, vec![Trip::Unreachable, Trip::Timeout]));
        scenarios!(
            run = breakers_of;
            "loaded" {
                ("", "") => Yields([None, None]),
                ("[admission.breaker]", "") => Yields([defaults(), defaults()]),
                (
                    "[admission.breaker]\nfailure_threshold = 1.0\nwindow = 1024\nmin_samples = 1\ncool_down = \"10m\"\ntrip_on = [\"5xx\", \"429\"]",
                    "",
                ) => Yields(
                    [(); 2].map(|()| {
                        Some((1.0, 1024, 1, 600_000, vec![Trip::ServerError, Trip::Status(StatusCode::TOO_MANY_REQUESTS)]))
                    }),
                ),
                ("[admission.breaker]\nwindow = 5\nmin_samples = 5", "") => Yields(
                    [(); 2].map(|()| Some((0.5, 5, 5, 10_000, vec![Trip::Unreachable, Trip::Timeout]))),
                ),
                ("", r#"breaker = { trip_on = ["503"] }"#) => Yields([
                    Some((0.5, 32, 5, 10_000, vec![Trip::Status(StatusCode::SERVICE_UNAVAILABLE)])),
                    None,
                ]),
                ("[admission.breaker]\nwindow = 8\nmin_samples = 2", "breaker = { min_samples = 8 }") => Yields([
                    Some((0.5, 8, 8, 10_000, vec![Trip::Unreachable, Trip::Timeout])),
                    Some((0.5, 8, 2, 10_000, vec![Trip::Unreachable, Trip::Timeout])),
                ]),
                ("[admission.breaker]", "breaker = { trip_on = [] }") => Yields([None, defaults()]),
            }

            "rejected" {
                ("[admission.breaker]\nfailure_threshold = 0.0", "") => Fails,
                ("[admission.breaker]\nfailure_threshold = 1.5", "") => Fails,
                ("[admission.breaker]\nmin_samples = 0", "") => Fails,
                ("[admission.breaker]\nwindow = 4\nmin_samples = 5", "") => Fails,
                ("[admission.breaker]\nfailure_threshold = nan", "") => Fails,
                ("[admission.breaker]\nwindow = 1025", "") => Fails,
                ("[admission.breaker]\ncool_down = \"0s\"", "") => Fails,
                ("[admission.breaker]\ncool_down = \"11m\"", "") => Fails,
                ("[admission.breaker]\ncooldown = \"10s\"", "") => Fails,
                ("[admission.breaker]\ntrip_on = [\"4xx\"]", "") => Fails,
                ("[admission.breaker]\ntrip_on = [\"200\"]", "") => Fails,
                ("[admission.breaker]\nwindow = 4\nmin_samples = 2", "breaker = { min_samples = 5 }") => Fails,
            }
        );
    }

    /// A breaker setting out of bounds is reported where it was written: in
    /// `[admission.breaker]`, or in a class, alone or with what it inherits.
    #[test]
    fn breaker_errors_name_where_they_were_written() {
        value_scenarios!(
            run = |settings| with_power_class(settings).err().map(|error| error.to_string());
            "named" {
                ("[admission.breaker]\nfailure_threshold = 1.5", "") => Some(
                    "admission.breaker failure_threshold must be above 0 and at most 1".to_string(),
                ),
                ("[admission.breaker]\nwindow = 4\nmin_samples = 2", "breaker = { min_samples = 5 }") => Some(
                    r#"class "power" breaker window must be from min_samples to 1024"#.to_string(),
                ),
            }
        );
    }

    /// A breaker counts an answer as a failure when its `trip_on` names the
    /// answer's status, or names `5xx` and the status is one.
    #[test]
    fn trip_on_names_the_answers_that_fail() {
        value_scenarios!(
            run = |(trip, status): (&str, u16)| {
                let settings = BreakerSettings {
                    trip_on: Some(vec![trip.parse().expect("a trip")]),
                    ..BreakerSettings::default()
                };
                BreakerConfig::resolve(Some(&settings), None)
                    .expect("valid settings")
                    .expect("a breaker")
                    .trips_on(Trip::Status(StatusCode::from_u16(status).expect("a status")))
            };
            "counted" {
                ("5xx", 503) => true,
                ("429", 429) => true,
            }

            "not counted" {
                ("5xx", 429) => false,
                ("503", 500) => false,
            }
        );
    }

    /// The `power` class's latency target, (latency in milliseconds,
    /// percentile), and the `[admission.slo]` tuning, (window, increase,
    /// decrease, min_in_flight, reset_after in seconds), in
    /// [`with_power_class`].
    #[allow(clippy::type_complexity)]
    fn slo_of(
        settings: (&str, &str),
    ) -> Result<(Option<(u128, f64)>, (u32, u32, f32, u32, u64)), ()> {
        let config = with_power_class(settings).map_err(drop)?;
        let target = config
            .classes
            .classify(&http::Method::PATCH, "/redfish/v1/Chassis", &[])
            .slo
            .map(|slo| (slo.latency.as_millis(), slo.percentile));
        let slo = config.admission.slo;
        Ok((
            target,
            (
                slo.window.get(),
                slo.increase.get(),
                slo.decrease,
                slo.min_in_flight.get(),
                slo.reset_after.as_secs(),
            ),
        ))
    }

    /// A class's `slo` sets its latency target, its percentile defaulted, and
    /// `[admission.slo]` the tuning, each setting defaulted when left out. A
    /// target needs the per-BMC limit it shares out, and fits in the class's
    /// budget; settings out of bounds, or unknown, do not load.
    #[test]
    fn slo_settings_parse() {
        const LIMIT: &str = "[admission]\nmax_in_flight_per_bmc = 4";
        scenarios!(
            run = slo_of;
            "loaded" {
                ("", "") => Yields((None, (20, 1, 0.5, 1, 60))),
                (LIMIT, r#"slo = { latency = "2s" }"#) => Yields((Some((2000, 0.9)), (20, 1, 0.5, 1, 60))),
                (
                    "[admission]\nmax_in_flight_per_bmc = 4\n[admission.slo]\nwindow = 1024\nincrease = 2\ndecrease = 0.25\nmin_in_flight = 4\nreset_after = \"1h\"",
                    r#"slo = { latency = "60s", percentile = 1.0 }"#,
                ) => Yields((Some((60_000, 1.0)), (1024, 2, 0.25, 4, 3600))),
            }

            "rejected" {
                ("", r#"slo = { latency = "2s" }"#) => Fails,
                (LIMIT, r#"slo = { latency = "61s" }"#) => Fails,
                (LIMIT, r#"slo = { latency = "0s" }"#) => Fails,
                (LIMIT, r#"slo = { latency = "2s", percentile = 0.0 }"#) => Fails,
                (LIMIT, r#"slo = { latency = "2s", percentile = 1.5 }"#) => Fails,
                (LIMIT, "slo = { percentile = 0.9 }") => Fails,
                (LIMIT, r#"slo = { latency = "2s", target = "1s" }"#) => Fails,
                ("[admission]\nmax_in_flight_per_bmc = 4\n[admission.slo]\nmin_in_flight = 5", "") => Fails,
                ("[admission.slo]\nwindow = 1025", "") => Fails,
                ("[admission.slo]\ndecrease = 1.0", "") => Fails,
                ("[admission.slo]\ndecrease = 0.0", "") => Fails,
                ("[admission.slo]\nreset_after = \"0s\"", "") => Fails,
                ("[admission.slo]\nreset_after = \"61m\"", "") => Fails,
                ("[admission.slo]\nwindows = 2", "") => Fails,
            }
        );
    }

    /// `[admission]` sets the per-BMC limit; without it there is none. A
    /// zero limit or an unknown key does not load.
    #[test]
    fn admission_limits_parse() {
        scenarios!(
            run = |admission: &str| {
                Config::parse(&format!("{admission}\n{MINIMAL_TLS}"))
                    .map(|config| config.admission.max_in_flight_per_bmc.map(NonZeroU32::get))
                    .map_err(drop)
            };
            "loaded" {
                "" => Yields(None),
                "[admission]\nmax_in_flight_per_bmc = 4" => Yields(Some(4)),
            }

            "rejected" {
                "[admission]\nmax_in_flight_per_bmc = 0" => Fails,
                "[admission]\nmax_in_flight = 4" => Fails,
            }
        );
    }
}
