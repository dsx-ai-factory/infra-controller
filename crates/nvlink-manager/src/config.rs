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
    /// Enables NvLink partitioning.
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

impl NvLinkConfig {
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
    /// Enables NMX-C server certificate expiry checks and rotation.
    #[serde(default)]
    pub enabled: bool,

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
            enabled: false,
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
