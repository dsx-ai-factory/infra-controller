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

/// The root configuration.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    pub metrics_listen: String,
    #[serde(default)]
    pub probes: Probes,
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

        if errs.is_empty() {
            Ok(())
        } else {
            Err(eyre!(errs.join("\n")))
        }
    }

    /// The metrics listen address as a socket address; empty defaults to
    /// `":9009"`, and a bare `":9009"` binds all interfaces, matching
    /// net/http-style listen strings.
    pub(crate) fn listen_addr(&self) -> eyre::Result<SocketAddr> {
        let configured = if self.metrics_listen.is_empty() {
            ":9009"
        } else {
            self.metrics_listen.as_str()
        };
        let listen = if configured.starts_with(':') {
            format!("0.0.0.0{configured}")
        } else {
            configured.to_string()
        };
        listen
            .to_socket_addrs()
            .map_err(|e| eyre!("invalid metrics_listen {:?}: {e}", self.metrics_listen))?
            .next()
            .ok_or_else(|| eyre!("invalid metrics_listen {:?}", self.metrics_listen))
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

    #[test]
    fn disabled_probes_skip_validation() {
        // Disabled probes carry no usable fields, and that must be fine.
        let cfg = load_str("probes:\n  grpc_machines:\n    enabled: false\n").expect("loads");
        assert_eq!(
            cfg.listen_addr().expect("defaults").to_string(),
            "0.0.0.0:9009",
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

    #[test]
    fn listen_addr_parses_go_style_listen_strings() {
        check_values(
            [
                Check {
                    scenario: "bare port binds all interfaces",
                    input: ":9009",
                    expect: "0.0.0.0:9009".to_string(),
                },
                Check {
                    scenario: "host:port passes through",
                    input: "127.0.0.1:8080",
                    expect: "127.0.0.1:8080".to_string(),
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
