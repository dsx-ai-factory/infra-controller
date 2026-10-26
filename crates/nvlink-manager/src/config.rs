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

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NvLinkConfig {
    /// Enables NvLink partitioning. Defaults to false.
    #[serde(default)]
    pub enabled: bool,

    /// Defaults to 1 Minute if not specified. A site whose passes overrun
    /// the interval can use 2 minutes; partition owners accept that staleness.
    #[serde(
        default = "NvLinkConfig::default_monitor_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub monitor_run_interval: std::time::Duration,

    /// PEM file path: extra CA bundle for verifying the NMX-C server over HTTPS (optional).
    #[serde(default)]
    pub nmx_c_tls_ca_cert_path: Option<String>,
    /// PEM file path: client certificate for mTLS to NMX-C (optional; pair with `nmx_c_tls_client_key_path`).
    #[serde(default)]
    pub nmx_c_tls_client_cert_path: Option<String>,
    /// PEM file path: client private key for mTLS to NMX-C (optional; pair with `nmx_c_tls_client_cert_path`).
    #[serde(default)]
    pub nmx_c_tls_client_key_path: Option<String>,
    /// TLS server name for NMX-C HTTPS: a DNS name or bare IP address.
    /// Defaults to the endpoint URL host if unset.
    #[serde(default)]
    pub nmx_c_tls_authority: Option<String>,
    /// TCP port for NMX-C endpoints derived from switch NVOS IP. Defaults to the production NMX-C port.
    #[serde(default)]
    pub nmx_c_endpoint_port: Option<u16>,
    /// Set to true if NMX-C doesn't adhere to security requirements. Defaults to false.
    #[serde(default)]
    pub allow_insecure: bool,

    /// Optional expiry-driven rotation for NMX-C server certificates.
    #[serde(default)]
    pub nmx_c_certificate_rotation: NmxCCertificateRotationConfig,

    /// Maximum number of NMX-C machine groups (chassis or rack) reconciled
    /// concurrently during a partition monitor iteration. Each group in flight
    /// holds one NMX-C client and at most one database connection at a time,
    /// so this bounds the monitor's gRPC fan-out and its share of the pool.
    /// Defaults to 16. The documented range is 1 to 256; larger values are
    /// accepted with a warning at startup. Deserialization rejects 0 and
    /// values above the Tokio semaphore limit that bounds the fan-out.
    #[serde(
        default = "NvLinkConfig::default_partition_monitor_max_concurrent_groups",
        deserialize_with = "deserialize_partition_monitor_max_concurrent_groups"
    )]
    pub partition_monitor_max_concurrent_groups: std::num::NonZeroUsize,

    /// How often a machine group is fully reconciled against NMX-C even when
    /// nothing changed. Between full checks, a group whose database inputs
    /// are unchanged since its last successful check and whose instances all
    /// have their NVLink config applied is skipped, so its GPU health and
    /// `NvlinkUnhealthy` alert refresh at this interval. Defaults to 10 minutes.
    /// Zero (`"0s"`) disables skipping, so every group is reconciled every iteration.
    #[serde(
        default = "NvLinkConfig::default_partition_monitor_full_check_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub partition_monitor_full_check_interval: std::time::Duration,
}

/// Outcome of [`NvLinkConfig::apply_default_nmx_c_tls`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefaultNmxCTls {
    pub client: DefaultNmxCClientTls,
    pub ca: DefaultNmxCCa,
}

/// Outcome of defaulting the NMX-C client certificate, key, and TLS authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultNmxCClientTls {
    /// The mounted client certificate and `initial_domain_name` authority are now in use.
    Applied,
    /// Neither partitioning nor the certificate monitor is enabled, NMX-C is reached
    /// insecurely, or an `nmx_c_tls_*` key is configured.
    NotNeeded,
    /// Secure NMX-C access has no NMX-C TLS configuration and no mounted client certificate,
    /// so NMX-C connections will present no client certificate.
    CertificateMissing,
}

/// Outcome of defaulting the NMX-C CA bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultNmxCCa {
    /// A default CA bundle is now in use.
    Applied,
    /// Neither partitioning nor the certificate monitor is enabled, NMX-C is reached over
    /// plain HTTP, or a CA is configured.
    NotNeeded,
    /// NMX-C is reached over HTTPS with no configured or default CA bundle, so no NMX-C
    /// server certificate can be verified.
    Missing,
}

impl NvLinkConfig {
    /// Mount path of the NMX-C client certificate issued by the nico-api chart's
    /// `nvSwitchTls.nicoClient`.
    pub const DEFAULT_NMX_C_CLIENT_TLS_DIR: &'static str = "/var/run/secrets/nvswitch-client";

    /// CA bundle used when the client certificate Secret has no `ca.crt`.
    pub const DEFAULT_NMX_C_FALLBACK_CA_CERT: &'static str = "/var/run/secrets/nico-roots/ca.crt";

    /// Whether the NMX-C certificate monitor runs: `nmx_c_certificate_rotation.enabled` when
    /// set, otherwise `enabled`.
    pub fn certificate_rotation_enabled(&self) -> bool {
        self.nmx_c_certificate_rotation
            .enabled
            .unwrap_or(self.enabled)
    }

    /// Whether NMX-C is reached over HTTPS rather than plain HTTP.
    pub(crate) fn nmx_c_uses_https(&self) -> bool {
        !self.allow_insecure
            || (self.nmx_c_tls_client_cert_path.is_some()
                && self.nmx_c_tls_client_key_path.is_some())
    }

    /// Fills in NMX-C TLS settings the site configuration leaves unset, when partitioning or
    /// the certificate monitor is enabled.
    ///
    /// The client certificate, key, and authority are defaulted together, and only when none of
    /// the four `nmx_c_tls_*` keys is set: the certificate and key come from `client_tls_dir`
    /// and the authority is `default_authority`. The CA bundle is defaulted on its own whenever
    /// it is unset and NMX-C is reached over HTTPS: `client_tls_dir/ca.crt`, or else
    /// `fallback_ca_cert`.
    pub fn apply_default_nmx_c_tls(
        &mut self,
        client_tls_dir: &std::path::Path,
        fallback_ca_cert: &std::path::Path,
        default_authority: Option<&str>,
    ) -> DefaultNmxCTls {
        if !self.enabled && !self.certificate_rotation_enabled() {
            return DefaultNmxCTls {
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::NotNeeded,
            };
        }

        let tls_configured = self.nmx_c_tls_ca_cert_path.is_some()
            || self.nmx_c_tls_client_cert_path.is_some()
            || self.nmx_c_tls_client_key_path.is_some()
            || self.nmx_c_tls_authority.is_some();
        let client = if self.allow_insecure || tls_configured {
            DefaultNmxCClientTls::NotNeeded
        } else {
            let cert = client_tls_dir.join("tls.crt");
            let key = client_tls_dir.join("tls.key");
            if cert.is_file() && key.is_file() {
                self.nmx_c_tls_client_cert_path = Some(cert.to_string_lossy().into_owned());
                self.nmx_c_tls_client_key_path = Some(key.to_string_lossy().into_owned());
                self.nmx_c_tls_authority = default_authority
                    .filter(|authority| !authority.is_empty())
                    .map(str::to_owned);
                DefaultNmxCClientTls::Applied
            } else {
                DefaultNmxCClientTls::CertificateMissing
            }
        };

        let ca = if self.nmx_c_tls_ca_cert_path.is_some() || !self.nmx_c_uses_https() {
            DefaultNmxCCa::NotNeeded
        } else {
            let default_ca = [
                client_tls_dir.join("ca.crt"),
                fallback_ca_cert.to_path_buf(),
            ]
            .into_iter()
            .find(|path| path.is_file());
            match default_ca {
                Some(path) => {
                    self.nmx_c_tls_ca_cert_path = Some(path.to_string_lossy().into_owned());
                    DefaultNmxCCa::Applied
                }
                None => DefaultNmxCCa::Missing,
            }
        };

        DefaultNmxCTls { client, ca }
    }

    pub const fn default_monitor_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn default_partition_monitor_max_concurrent_groups() -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).expect("16 is non-zero")
    }

    /// Upper end of the documented `partition_monitor_max_concurrent_groups` range.
    pub(crate) const MAX_DOCUMENTED_PARTITION_MONITOR_CONCURRENT_GROUPS: usize = 256;

    pub const fn default_partition_monitor_full_check_interval() -> std::time::Duration {
        std::time::Duration::from_secs(10 * 60)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NmxCCertificateRotationConfig {
    /// Enables NMX-C server certificate expiry checks and rotation. Unset follows
    /// `nvlink_config.enabled`; see [`NvLinkConfig::certificate_rotation_enabled`].
    #[serde(default)]
    pub enabled: Option<bool>,

    /// How often carbide checks the certificate served by NMX-C.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,

    /// Request rotation when the certificate served by NMX-C expires within this duration.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_rotate_before_expiry",
        alias = "expiry_warning_window",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub rotate_before_expiry: std::time::Duration,

    /// Per-operation timeout for NMX-C server certificate probes.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_probe_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub probe_timeout: std::time::Duration,
}

impl NmxCCertificateRotationConfig {
    pub const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60 * 60)
    }

    pub const fn default_rotate_before_expiry() -> std::time::Duration {
        std::time::Duration::from_secs(7 * 24 * 60 * 60)
    }

    pub const fn default_probe_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }
}

impl Default for NmxCCertificateRotationConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            run_interval: Self::default_run_interval(),
            rotate_before_expiry: Self::default_rotate_before_expiry(),
            probe_timeout: Self::default_probe_timeout(),
        }
    }
}

impl Default for NvLinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            monitor_run_interval: Self::default_monitor_run_interval(),
            nmx_c_tls_ca_cert_path: None,
            nmx_c_tls_client_cert_path: None,
            nmx_c_tls_client_key_path: None,
            nmx_c_tls_authority: None,
            nmx_c_endpoint_port: None,
            allow_insecure: false,
            nmx_c_certificate_rotation: NmxCCertificateRotationConfig::default(),
            partition_monitor_max_concurrent_groups:
                Self::default_partition_monitor_max_concurrent_groups(),
            partition_monitor_full_check_interval:
                Self::default_partition_monitor_full_check_interval(),
        }
    }
}

/// Rejects 0 and values the monitor's semaphore cannot represent.
fn deserialize_partition_monitor_max_concurrent_groups<'de, D>(
    deserializer: D,
) -> Result<std::num::NonZeroUsize, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = usize::deserialize(deserializer)?;
    if value > tokio::sync::Semaphore::MAX_PERMITS {
        return Err(serde::de::Error::custom(format!(
            "partition_monitor_max_concurrent_groups must be at most {}, got {value}",
            tokio::sync::Semaphore::MAX_PERMITS
        )));
    }
    std::num::NonZeroUsize::new(value).ok_or_else(|| {
        serde::de::Error::custom("partition_monitor_max_concurrent_groups must be at least 1")
    })
}

#[cfg(test)]
mod test {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn deserialize_serialize_nvlink_config() {
        let value_json =
            r#"{"enabled": true, "allow_insecure": true, "monitor_run_interval": "33" }"#;

        let nvlink_config: NvLinkConfig = serde_json::from_str(value_json).unwrap();
        assert_eq!(
            nvlink_config,
            NvLinkConfig {
                enabled: true,
                monitor_run_interval: std::time::Duration::from_secs(33),
                nmx_c_tls_ca_cert_path: None,
                nmx_c_tls_client_cert_path: None,
                nmx_c_tls_client_key_path: None,
                nmx_c_tls_authority: None,
                nmx_c_endpoint_port: None,
                allow_insecure: true,
                nmx_c_certificate_rotation: NmxCCertificateRotationConfig::default(),
                partition_monitor_max_concurrent_groups:
                    NvLinkConfig::default_partition_monitor_max_concurrent_groups(),
                partition_monitor_full_check_interval:
                    NvLinkConfig::default_partition_monitor_full_check_interval(),
            }
        );
    }

    #[test]
    fn full_check_interval_defaults_to_ten_minutes_and_accepts_zero() {
        value_scenarios!(run = |json: &str| {
            serde_json::from_str::<NvLinkConfig>(json)
                .unwrap()
                .partition_monitor_full_check_interval
        };
            "default" {
                r#"{"allow_insecure":false}"# => std::time::Duration::from_secs(600),
            }
            "explicit" {
                r#"{"allow_insecure":false,"partition_monitor_full_check_interval":"2m"}"# =>
                    std::time::Duration::from_secs(120),
                r#"{"allow_insecure":false,"partition_monitor_full_check_interval":"0s"}"# =>
                    std::time::Duration::ZERO,
            }
        );
    }

    #[test]
    fn empty_nvlink_config_is_disabled_and_secure() {
        let config: NvLinkConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config, NvLinkConfig::default());
        assert!(!config.enabled);
        assert!(!config.allow_insecure);
        assert!(!config.certificate_rotation_enabled());
    }

    #[test]
    fn certificate_rotation_follows_partitioning_unless_set() {
        for (enabled, rotation, expected) in [
            (false, None, false),
            (true, None, true),
            (false, Some(true), true),
            (true, Some(false), false),
        ] {
            let config = NvLinkConfig {
                enabled,
                nmx_c_certificate_rotation: NmxCCertificateRotationConfig {
                    enabled: rotation,
                    ..NmxCCertificateRotationConfig::default()
                },
                ..NvLinkConfig::default()
            };
            assert_eq!(
                config.certificate_rotation_enabled(),
                expected,
                "enabled={enabled} rotation={rotation:?}"
            );
        }
    }

    #[test]
    fn apply_default_nmx_c_tls() {
        enum Mount {
            ClientWithCa,
            ClientWithoutCa,
            RootsOnly,
            Nothing,
        }

        struct Case {
            name: &'static str,
            config: NvLinkConfig,
            mount: Mount,
            client: DefaultNmxCClientTls,
            ca: DefaultNmxCCa,
            expected_ca: Option<&'static str>,
        }

        let enabled = || NvLinkConfig {
            enabled: true,
            ..NvLinkConfig::default()
        };
        let operator_tls =
            |ca: Option<&str>, cert: Option<&str>, authority: Option<&str>| NvLinkConfig {
                nmx_c_tls_ca_cert_path: ca.map(str::to_owned),
                nmx_c_tls_client_cert_path: cert.map(str::to_owned),
                nmx_c_tls_client_key_path: cert.map(|_| "/operator/tls.key".to_owned()),
                nmx_c_tls_authority: authority.map(str::to_owned),
                ..enabled()
            };

        let cases = [
            Case {
                name: "client secret with ca.crt",
                config: enabled(),
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::Applied,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("client/ca.crt"),
            },
            Case {
                name: "client secret without ca.crt uses fallback CA",
                config: enabled(),
                mount: Mount::ClientWithoutCa,
                client: DefaultNmxCClientTls::Applied,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("roots/ca.crt"),
            },
            Case {
                name: "client certificate not mounted still defaults CA",
                config: enabled(),
                mount: Mount::RootsOnly,
                client: DefaultNmxCClientTls::CertificateMissing,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("roots/ca.crt"),
            },
            Case {
                name: "nothing mounted",
                config: enabled(),
                mount: Mount::Nothing,
                client: DefaultNmxCClientTls::CertificateMissing,
                ca: DefaultNmxCCa::Missing,
                expected_ca: None,
            },
            Case {
                name: "server-only TLS with explicit authority gets only a default CA",
                config: operator_tls(None, None, Some("nmxc.example")),
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("client/ca.crt"),
            },
            Case {
                name: "explicit CA is kept",
                config: operator_tls(Some("/operator/ca.crt"), None, None),
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::NotNeeded,
                expected_ca: None,
            },
            Case {
                name: "insecure plain HTTP",
                config: NvLinkConfig {
                    allow_insecure: true,
                    ..enabled()
                },
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::NotNeeded,
                expected_ca: None,
            },
            Case {
                name: "insecure with operator client certificate uses HTTPS",
                config: NvLinkConfig {
                    allow_insecure: true,
                    ..operator_tls(None, Some("/operator/tls.crt"), None)
                },
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("client/ca.crt"),
            },
            Case {
                name: "certificate monitor alone still gets defaults",
                config: NvLinkConfig {
                    nmx_c_certificate_rotation: NmxCCertificateRotationConfig {
                        enabled: Some(true),
                        ..NmxCCertificateRotationConfig::default()
                    },
                    ..NvLinkConfig::default()
                },
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::Applied,
                ca: DefaultNmxCCa::Applied,
                expected_ca: Some("client/ca.crt"),
            },
            Case {
                name: "partitioning and certificate monitor disabled by default",
                config: NvLinkConfig::default(),
                mount: Mount::ClientWithCa,
                client: DefaultNmxCClientTls::NotNeeded,
                ca: DefaultNmxCCa::NotNeeded,
                expected_ca: None,
            },
        ];

        for case in cases {
            let root = tempfile::tempdir().unwrap();
            let client_dir = root.path().join("client");
            let fallback_ca = root.path().join("roots/ca.crt");
            std::fs::create_dir_all(&client_dir).unwrap();
            std::fs::create_dir_all(fallback_ca.parent().unwrap()).unwrap();
            let (client_files, roots_ca): (&[&str], bool) = match case.mount {
                Mount::ClientWithCa => (&["ca.crt", "tls.crt", "tls.key"], true),
                Mount::ClientWithoutCa => (&["tls.crt", "tls.key"], true),
                Mount::RootsOnly => (&[], true),
                Mount::Nothing => (&[], false),
            };
            for name in client_files {
                std::fs::write(client_dir.join(name), "pem").unwrap();
            }
            if roots_ca {
                std::fs::write(&fallback_ca, "pem").unwrap();
            }
            let path = |relative: &str| root.path().join(relative).to_string_lossy().into_owned();

            let mut config = case.config.clone();
            let outcome =
                config.apply_default_nmx_c_tls(&client_dir, &fallback_ca, Some("site.example"));

            assert_eq!(
                outcome,
                DefaultNmxCTls {
                    client: case.client,
                    ca: case.ca,
                },
                "{}",
                case.name
            );
            let mut expected = case.config.clone();
            if case.client == DefaultNmxCClientTls::Applied {
                expected.nmx_c_tls_client_cert_path = Some(path("client/tls.crt"));
                expected.nmx_c_tls_client_key_path = Some(path("client/tls.key"));
                expected.nmx_c_tls_authority = Some("site.example".to_owned());
            }
            if let Some(expected_ca) = case.expected_ca {
                expected.nmx_c_tls_ca_cert_path = Some(path(expected_ca));
            }
            assert_eq!(config, expected, "{}", case.name);
        }
    }

    #[test]
    fn empty_default_authority_leaves_authority_unset() {
        let root = tempfile::tempdir().unwrap();
        for name in ["ca.crt", "tls.crt", "tls.key"] {
            std::fs::write(root.path().join(name), "pem").unwrap();
        }
        let mut config = NvLinkConfig {
            enabled: true,
            ..NvLinkConfig::default()
        };

        let outcome = config.apply_default_nmx_c_tls(root.path(), root.path(), Some(""));

        assert_eq!(outcome.client, DefaultNmxCClientTls::Applied);
        assert_eq!(config.nmx_c_tls_authority, None);
    }

    #[test]
    fn deserialize_certificate_rotation_window_in_weeks() {
        let config: NmxCCertificateRotationConfig =
            serde_json::from_str(r#"{"rotate_before_expiry":"2w"}"#).unwrap();

        assert_eq!(
            config.rotate_before_expiry,
            std::time::Duration::from_secs(2 * 7 * 24 * 60 * 60)
        );
    }

    #[test]
    fn deserialize_zero_concurrent_groups_is_rejected() {
        let err = serde_json::from_str::<NvLinkConfig>(
            r#"{"allow_insecure":false,"partition_monitor_max_concurrent_groups":0}"#,
        );
        assert!(err.is_err(), "zero must be rejected");
    }

    #[test]
    fn deserialize_concurrent_groups_above_the_semaphore_limit_is_rejected() {
        let limit = tokio::sync::Semaphore::MAX_PERMITS;
        let error = serde_json::from_str::<NvLinkConfig>(&format!(
            r#"{{"allow_insecure":false,"partition_monitor_max_concurrent_groups":{}}}"#,
            limit + 1
        ))
        .expect_err("one above the semaphore limit must be rejected");
        assert!(
            error
                .to_string()
                .contains("partition_monitor_max_concurrent_groups must be at most"),
            "{error}"
        );
        let config: NvLinkConfig = serde_json::from_str(&format!(
            r#"{{"allow_insecure":false,"partition_monitor_max_concurrent_groups":{limit}}}"#
        ))
        .expect("the semaphore limit itself is accepted");
        assert_eq!(config.partition_monitor_max_concurrent_groups.get(), limit);
    }

    #[test]
    fn deserialize_legacy_expiry_warning_window_as_rotation_window() {
        let config: NmxCCertificateRotationConfig =
            serde_json::from_str(r#"{"expiry_warning_window":"3d"}"#).unwrap();

        assert_eq!(
            config.rotate_before_expiry,
            std::time::Duration::from_secs(3 * 24 * 60 * 60)
        );
    }
}
