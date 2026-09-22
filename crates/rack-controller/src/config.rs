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
use model::rack_type::RackProfileConfig;
use serde::{Deserialize, Serialize};

/// RMS API used by the ScaleUpFabric Manager configuration workflow.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleUpFabricManagerApiVersion {
    /// Has no effect; accepted for configuration compatibility.
    V1,

    /// Uses the asynchronous V2 workflow.
    #[default]
    V2,
}

pub struct RackConfig {
    pub rms: RmsConfig,
    pub rack_validation_config: RackValidationConfig,
    pub rack_profiles: RackProfileConfig,
}

/// Configuration for rack-level validation (partition-based
/// multi-node tests run after firmware upgrade / maintenance).
///
/// Example:
/// ```toml
/// [rack_validation_config]
/// enabled = true
/// run_interval = "60s"
/// ```
#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RackValidationConfig {
    /// Enables rack validation testing.
    #[serde(default)]
    pub enabled: bool,

    #[serde(
        default = "RackValidationConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,
}

impl RackValidationConfig {
    const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }
}

/// Rack Manager Service (RMS) configuration for API connectivity and mTLS.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RmsConfig {
    /// URL of the RMS API for rack-level firmware upgrades and power sequencing.
    pub api_url: Option<String>,

    /// Path to the root CA certificate for TLS verification when connecting to RMS.
    pub root_ca_path: Option<String>,

    /// Path to the client certificate PEM for mTLS with RMS.
    pub client_cert: Option<String>,

    /// Path to the client private key PEM for mTLS with RMS.
    pub client_key: Option<String>,

    /// Enforce TLS when connecting to RMS. Defaults to true.
    #[serde(default = "default_rms_enforce_tls")]
    pub enforce_tls: bool,

    /// RMS API used to configure ScaleUpFabric Manager. Defaults to `v2`,
    /// which delegates primary selection to RMS and waits for its
    /// asynchronous job. `v1` is accepted but has no effect.
    #[serde(default)]
    pub scale_up_fabric_manager_api_version: ScaleUpFabricManagerApiVersion,
}

fn default_rms_enforce_tls() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{RmsConfig, ScaleUpFabricManagerApiVersion};

    #[test]
    fn rms_config_defaults_scale_up_fabric_manager_to_v2() {
        let config: RmsConfig =
            serde_json::from_value(serde_json::json!({})).expect("RMS config should deserialize");

        assert_eq!(
            config.scale_up_fabric_manager_api_version,
            ScaleUpFabricManagerApiVersion::V2
        );
    }

    #[test]
    fn rms_config_accepts_supported_scale_up_fabric_manager_apis() {
        for (value, expected) in [
            ("v1", ScaleUpFabricManagerApiVersion::V1),
            ("v2", ScaleUpFabricManagerApiVersion::V2),
        ] {
            let config: RmsConfig = serde_json::from_value(serde_json::json!({
                "scale_up_fabric_manager_api_version": value
            }))
            .expect("supported RMS API config should deserialize");

            assert_eq!(config.scale_up_fabric_manager_api_version, expected);
        }
    }

    #[test]
    fn rms_config_rejects_unknown_scale_up_fabric_manager_api() {
        let result = serde_json::from_value::<RmsConfig>(serde_json::json!({
            "scale_up_fabric_manager_api_version": "automatic"
        }));

        assert!(result.is_err());
    }
}
