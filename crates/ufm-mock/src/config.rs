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
use std::path::PathBuf;
use std::time::Duration;

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};

/// Configuration shared by the UFM mock library and its standalone binary.
///
/// Machine-a-tron embeds the mock into its control server and may combine its in-process
/// inventory with remote [`InventoryConfig::static_sources`]. The standalone binary owns its
/// listener and has no in-process inventory, so it relies exclusively on the configured static
/// sources.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct UfmMockConfig {
    /// Activates a configured mock when it is embedded into another process.
    ///
    /// Keeping this separate from the presence of the configuration allows deployments to stage
    /// a complete UFM section without starting it. The standalone binary sets this to `true`
    /// because launching that binary is itself the activation decision.
    #[serde(default)]
    pub enabled: bool,
    /// Includes inventory supplied directly by the embedding process.
    ///
    /// Machine-a-tron uses its control state as the local provider. Standalone execution has no
    /// local provider, so this setting has no effect there.
    #[serde(default)]
    pub include_local_inventory: bool,
    #[serde(default)]
    pub metrics_address: Option<SocketAddr>,
    #[serde(default)]
    pub fabric: FabricConfig,
    #[serde(default)]
    pub inventory: InventoryConfig,
}

impl UfmMockConfig {
    pub fn validate(&self) -> eyre::Result<()> {
        eyre::ensure!(
            !self.inventory.poll_interval.is_zero(),
            "ufm_mock.inventory.poll_interval must be nonzero"
        );
        eyre::ensure!(
            !self.inventory.request_timeout.is_zero(),
            "ufm_mock.inventory.request_timeout must be nonzero"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FabricConfig {
    #[serde(default = "default_ufm_version")]
    pub ufm_version: String,
    #[serde(default)]
    pub sm_config: SmConfig,
}

impl Default for FabricConfig {
    fn default() -> Self {
        Self {
            ufm_version: default_ufm_version(),
            sm_config: SmConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SmConfig {
    #[serde(default = "default_subnet_prefix")]
    pub subnet_prefix: String,
    #[serde(default = "default_m_key")]
    pub m_key: String,
    #[serde(default = "default_sm_sa_key")]
    pub sm_key: String,
    #[serde(default = "default_sm_sa_key")]
    pub sa_key: String,
    #[serde(default)]
    pub m_key_per_port: bool,
}

impl Default for SmConfig {
    fn default() -> Self {
        Self {
            subnet_prefix: default_subnet_prefix(),
            m_key: default_m_key(),
            sm_key: default_sm_sa_key(),
            sa_key: default_sm_sa_key(),
            m_key_per_port: false,
        }
    }
}

/// Inventory reconciliation settings used in both hosted and standalone execution.
///
/// Static sources are polled in either mode. A hosted mock may additionally receive an
/// in-process [`crate::InventoryProvider`], while they are the only inventory sources available
/// to the standalone binary.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InventoryConfig {
    #[serde(
        default = "default_poll_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub poll_interval: Duration,
    #[serde(
        default = "default_request_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub request_timeout: Duration,
    #[serde(
        default = "default_failure_grace_period",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub failure_grace_period: Duration,
    #[serde(default)]
    pub failure_action: FailureAction,
    #[serde(default)]
    pub static_sources: Vec<InventorySourceConfig>,
}

impl Default for InventoryConfig {
    fn default() -> Self {
        Self {
            poll_interval: default_poll_interval(),
            request_timeout: default_request_timeout(),
            failure_grace_period: default_failure_grace_period(),
            failure_action: FailureAction::default(),
            static_sources: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailureAction {
    #[default]
    MarkDown,
    Remove,
}

/// A machine-a-tron inventory endpoint parsed and validated while loading configuration.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InventorySourceConfig {
    /// URL returning an [`crate::InventorySnapshot`] as JSON.
    pub url: url::Url,
    #[serde(default)]
    pub ca_cert_path: Option<PathBuf>,
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

/// Top-level configuration for the standalone UFM mock process.
///
/// Listener and TLS settings belong to the standalone process. The flattened [`UfmMockConfig`]
/// is shared with hosted mode, although standalone initialization always activates the mock and
/// cannot provide local machine-a-tron inventory. Its authentication token comes from
/// [`crate::UFM_MOCK_AUTH_TOKEN_ENV`], not this configuration.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct StandaloneConfig {
    #[serde(default = "default_listen_address")]
    pub listen_address: SocketAddr,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(flatten)]
    pub mock: UfmMockConfig,
}

/// PEM certificate chain and private key for a TLS listener.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain served on the listener.
    pub cert_path: PathBuf,
    /// PEM private key matching `cert_path`.
    pub key_path: PathBuf,
}

fn default_ufm_version() -> String {
    "6.18.0".to_string()
}

fn default_subnet_prefix() -> String {
    "0xfe80000000000000".to_string()
}

fn default_m_key() -> String {
    "0x0000000000000000".to_string()
}

fn default_sm_sa_key() -> String {
    "0x0000000000000001".to_string()
}

fn default_poll_interval() -> Duration {
    Duration::from_secs(2)
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_failure_grace_period() -> Duration {
    Duration::from_secs(30)
}

fn default_listen_address() -> SocketAddr {
    "0.0.0.0:9888".parse().unwrap()
}

#[cfg(test)]
mod tests {
    use super::InventorySourceConfig;

    #[test]
    fn rejects_invalid_inventory_source_url_during_deserialization() {
        let result = serde_json::from_value::<InventorySourceConfig>(serde_json::json!({
            "url": "not a URL"
        }));

        assert!(result.is_err());
    }
}
