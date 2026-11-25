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
pub struct NvLinkConfig {
    /// Enables NvLink partitioning.
    #[serde(default)]
    pub enabled: bool,

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
    /// TLS server name (SNI / cert verification hostname) for NMX-C HTTPS. Defaults to the endpoint URL host if unset.
    #[serde(default)]
    pub nmx_c_tls_authority: Option<String>,
    /// TCP port for NMX-C endpoints derived from switch NVOS IP. Defaults to the production NMX-C port.
    #[serde(default)]
    pub nmx_c_endpoint_port: Option<u16>,
    /// Set to true if NMX-C doesn't adhere to security requirements. Defaults to false.
    pub allow_insecure: bool,

    /// Optional monitoring for NMX-C server certificate propagation.
    #[serde(default)]
    pub nmx_c_certificate_rotation: NmxCCertificateRotationConfig,

    /// Consecutive iterations a rack's NMX-C must be unusable before the
    /// monitor raises the rack's `NmxcUnreachable` health alert. A single
    /// failed pass is common under load and is not reported; the alert is
    /// cleared on the first pass that reaches NMX-C again. Defaults to 3.
    /// Must be non-zero; deserialization rejects 0.
    #[serde(default = "NvLinkConfig::default_partition_monitor_unreachable_alert_after")]
    pub partition_monitor_unreachable_alert_after: std::num::NonZeroU32,
}

impl NvLinkConfig {
    pub const fn default_monitor_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn default_partition_monitor_unreachable_alert_after() -> std::num::NonZeroU32 {
        std::num::NonZeroU32::new(3).expect("3 is non-zero")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NmxCCertificateRotationConfig {
    /// Enables NMX-C server certificate propagation checks.
    #[serde(default)]
    pub enabled: bool,

    /// How often carbide checks whether the desired certificate is running on NMX-C.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,

    /// Warn before the desired mounted certificate reaches expiry.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_expiry_warning_window",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub expiry_warning_window: std::time::Duration,

    /// Per-operation timeout for NMX-C server certificate probes.
    #[serde(
        default = "NmxCCertificateRotationConfig::default_probe_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub probe_timeout: std::time::Duration,

    /// PEM file path: desired NMX-C server certificate.
    #[serde(default)]
    pub server_cert_path: Option<String>,

    /// PEM file path template for per-switch NMX-C server certificates.
    /// Supports the `{rack_id}` placeholder.
    #[serde(default)]
    pub server_cert_path_template: Option<String>,
}

impl NmxCCertificateRotationConfig {
    pub const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60 * 60)
    }

    pub const fn default_expiry_warning_window() -> std::time::Duration {
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
            expiry_warning_window: Self::default_expiry_warning_window(),
            probe_timeout: Self::default_probe_timeout(),
            server_cert_path: None,
            server_cert_path_template: None,
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
            partition_monitor_unreachable_alert_after:
                Self::default_partition_monitor_unreachable_alert_after(),
        }
    }
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
                partition_monitor_unreachable_alert_after:
                    NvLinkConfig::default_partition_monitor_unreachable_alert_after(),
            }
        );
    }

    #[test]
    fn unreachable_alert_after_defaults_to_three_and_rejects_zero() {
        value_scenarios!(run = |json: &str| {
            serde_json::from_str::<NvLinkConfig>(json)
                .map(|config| config.partition_monitor_unreachable_alert_after.get())
                .ok()
        };
            "default" {
                r#"{"allow_insecure":false}"# => Some(3),
            }
            "explicit" {
                r#"{"allow_insecure":false,"partition_monitor_unreachable_alert_after":1}"# => Some(1),
            }
            "rejected" {
                r#"{"allow_insecure":false,"partition_monitor_unreachable_alert_after":0}"# => None,
            }
        );
    }
}
