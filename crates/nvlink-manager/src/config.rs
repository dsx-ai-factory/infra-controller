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

/// Deserializes a duration and rejects zero.
fn deserialize_positive_duration<'de, D>(deserializer: D) -> Result<std::time::Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let duration: std::time::Duration = deserialize_duration(deserializer)?;
    if duration.is_zero() {
        return Err(serde::de::Error::custom(
            "duration must be greater than zero",
        ));
    }

    Ok(duration)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NvLinkConfig {
    /// Enables NvLink partitioning.
    #[serde(default)]
    pub enabled: bool,

    /// Enables read-only discovery of rack NVLink domains through NMX-C Hello.
    ///
    /// This has no effect when `enabled` is true because the partition monitor
    /// already records the domain observed from each rack's NMX-C endpoint.
    #[serde(default)]
    pub domain_discovery_enabled: bool,

    /// Maximum duration for one read-only domain-discovery database or NMX-C operation.
    /// Must be greater than zero.
    #[serde(
        default = "NvLinkConfig::default_domain_discovery_operation_timeout",
        deserialize_with = "deserialize_positive_duration",
        serialize_with = "as_std_duration"
    )]
    pub domain_discovery_operation_timeout: std::time::Duration,

    /// Defaults to 1 Minute if not specified.
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

    /// Maximum number of NMX-C machine groups (chassis or rack) processed concurrently
    /// during a partition monitor iteration. Bounds DB pool usage and gRPC fan-out.
    /// Defaults to 16. Must be non-zero; deserialization rejects 0.
    #[serde(default = "NvLinkConfig::default_partition_monitor_max_concurrent_groups")]
    pub partition_monitor_max_concurrent_groups: std::num::NonZeroUsize,
}

impl NvLinkConfig {
    pub const fn default_monitor_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn default_domain_discovery_operation_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }

    pub const fn default_partition_monitor_max_concurrent_groups() -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).expect("16 is non-zero")
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
            domain_discovery_enabled: false,
            domain_discovery_operation_timeout: Self::default_domain_discovery_operation_timeout(),
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
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn deserialize_serialize_nvlink_config() {
        let value_json = r#"{
            "enabled": true,
            "domain_discovery_enabled": true,
            "domain_discovery_operation_timeout": "17s",
            "allow_insecure": true,
            "monitor_run_interval": "33"
        }"#;

        let nvlink_config: NvLinkConfig = serde_json::from_str(value_json).unwrap();
        assert_eq!(
            nvlink_config,
            NvLinkConfig {
                enabled: true,
                domain_discovery_enabled: true,
                domain_discovery_operation_timeout: std::time::Duration::from_secs(17),
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
            }
        );
    }

    #[test]
    fn domain_discovery_defaults_to_disabled_when_omitted() {
        let config: NvLinkConfig = serde_json::from_str(r#"{"allow_insecure":false}"#).unwrap();

        assert!(!config.enabled);
        assert!(!config.domain_discovery_enabled);
        assert_eq!(
            config.domain_discovery_operation_timeout,
            NvLinkConfig::default_domain_discovery_operation_timeout()
        );
        assert!(!config.allow_insecure);
    }

    #[test]
    fn deserialize_secure_read_only_domain_discovery() {
        let config: NvLinkConfig = serde_json::from_str(
            r#"{
                "enabled": false,
                "domain_discovery_enabled": true,
                "domain_discovery_operation_timeout": "12s",
                "allow_insecure": false,
                "nmx_c_tls_ca_cert_path": "/tls/ca.crt",
                "nmx_c_tls_client_cert_path": "/tls/client.crt",
                "nmx_c_tls_client_key_path": "/tls/client.key",
                "nmx_c_tls_authority": "nmxc.example.internal",
                "nmx_c_endpoint_port": 9370
            }"#,
        )
        .unwrap();

        assert!(!config.enabled);
        assert!(config.domain_discovery_enabled);
        assert_eq!(
            config.domain_discovery_operation_timeout,
            std::time::Duration::from_secs(12)
        );
        assert!(!config.allow_insecure);
        assert_eq!(
            config.nmx_c_tls_ca_cert_path.as_deref(),
            Some("/tls/ca.crt")
        );
        assert_eq!(
            config.nmx_c_tls_client_cert_path.as_deref(),
            Some("/tls/client.crt")
        );
        assert_eq!(
            config.nmx_c_tls_client_key_path.as_deref(),
            Some("/tls/client.key")
        );
        assert_eq!(
            config.nmx_c_tls_authority.as_deref(),
            Some("nmxc.example.internal")
        );
        assert_eq!(config.nmx_c_endpoint_port, Some(9370));
    }

    #[test]
    fn deserialize_zero_domain_discovery_operation_timeout_is_rejected() {
        let err = serde_json::from_str::<NvLinkConfig>(
            r#"{
                "domain_discovery_operation_timeout": "0s",
                "allow_insecure": false
            }"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("duration must be greater than zero"),
            "unexpected error: {err}"
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
        assert!(err.is_err(), "zero must be rejected by NonZeroUsize");
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
