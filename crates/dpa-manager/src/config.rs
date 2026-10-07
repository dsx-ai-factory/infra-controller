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

use std::net::Ipv4Addr;

use carbide_utils::config::{as_duration, as_std_duration};
use duration_str::{deserialize_duration, deserialize_duration_chrono};
use serde::{Deserialize, Serialize};

fn default_mqtt_endpoint() -> String {
    "mqtt.forge".to_string()
}

fn default_mqtt_broker_port() -> u16 {
    1884
}

/// MQTT authentication mode.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MqttAuthMode {
    /// No authentication.
    #[default]
    None,
    /// Username/password basic authentication.
    BasicAuth,
    /// OAuth2 token-based authentication.
    Oauth2,
}

/// OAuth2 configuration for MQTT broker authentication.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MqttOAuth2Config {
    /// OAuth2 token endpoint URL.
    pub token_url: String,

    /// OAuth2 scopes to request when obtaining a token.
    #[serde(default)]
    pub scopes: Vec<String>,

    /// HTTP timeout for token endpoint requests. Default is 30 seconds.
    #[serde(
        default = "MqttOAuth2Config::default_http_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub http_timeout: std::time::Duration,

    /// Username sent with the MQTT CONNECT packet when using OAuth2.
    /// Default is "oauth2token".
    #[serde(default = "MqttOAuth2Config::default_username")]
    pub username: String,
}

impl MqttOAuth2Config {
    fn default_http_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }

    fn default_username() -> String {
        "oauth2token".to_string()
    }
}

/// MQTT authentication configuration shared by DPA and DSX event bus.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MqttAuthConfig {
    /// Authentication mechanism to use for MQTT connections.
    #[serde(default)]
    pub auth_mode: MqttAuthMode,

    /// OAuth2 settings, required when `auth_mode` is `Oauth2`.
    pub oauth2: Option<MqttOAuth2Config>,
}

/// SVPC (Scalable VPC) MQTT connection settings.
///
/// These were previously inlined in [`EwFabricConfig`]; they are grouped here so the
/// SVPC path owns its own MQTT endpoint, port, heartbeat interval, and
/// authentication configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SvpcConfig {
    /// MQTT broker host (name or IP address) used to create client connections.
    #[serde(default = "default_mqtt_endpoint")]
    pub mqtt_endpoint: String,

    /// MQTT broker port to use to establish client connections.
    #[serde(default = "default_mqtt_broker_port")]
    pub mqtt_broker_port: u16,

    /// Interval at which we issue heartbeat requests to the DPA.
    /// Defaults to 120 seconds if not specified.
    #[serde(
        default = "SvpcConfig::default_hb_interval",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub hb_interval: chrono::TimeDelta,

    /// MQTT authentication configuration.
    #[serde(default)]
    pub auth: MqttAuthConfig,
}

impl SvpcConfig {
    pub const fn default_hb_interval() -> chrono::TimeDelta {
        chrono::TimeDelta::minutes(2)
    }
}

impl Default for SvpcConfig {
    fn default() -> Self {
        Self {
            mqtt_endpoint: default_mqtt_endpoint(),
            mqtt_broker_port: default_mqtt_broker_port(),
            hb_interval: Self::default_hb_interval(),
            auth: MqttAuthConfig::default(),
        }
    }
}

/// Astra underlay route and identifier settings.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AstraConfig {
    /// Route prefix length for each rail. Must be less than 32 for IPv4.
    #[serde(default = "AstraConfig::default_underlay_rail_route_prefix_len")]
    pub underlay_rail_route_prefix_len: u8,

    /// Route prefix length for each software plane. Must be less than 32 for IPv4.
    #[serde(default = "AstraConfig::default_underlay_software_plane_route_prefix_len")]
    pub underlay_software_plane_route_prefix_len: u8,

    /// Number of bits used to identify a rail.
    /// Together with the software-plane identifier, must use fewer than 32 bits for IPv4.
    #[serde(default = "AstraConfig::default_underlay_ip_rail_id_bit_len")]
    pub underlay_ip_rail_id_bit_len: u8,

    /// Number of bits used to identify a software plane.
    #[serde(default = "AstraConfig::default_underlay_ip_software_plane_id_bit_len")]
    pub underlay_ip_software_plane_id_bit_len: u8,
}

impl<'de> Deserialize<'de> for AstraConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deserialize with the same fields and defaults, then validate the IPv4 limits.
        #[derive(Deserialize)]
        #[serde(remote = "AstraConfig", deny_unknown_fields)]
        struct UncheckedAstraConfig {
            #[serde(default = "AstraConfig::default_underlay_rail_route_prefix_len")]
            underlay_rail_route_prefix_len: u8,
            #[serde(default = "AstraConfig::default_underlay_software_plane_route_prefix_len")]
            underlay_software_plane_route_prefix_len: u8,
            #[serde(default = "AstraConfig::default_underlay_ip_rail_id_bit_len")]
            underlay_ip_rail_id_bit_len: u8,
            #[serde(default = "AstraConfig::default_underlay_ip_software_plane_id_bit_len")]
            underlay_ip_software_plane_id_bit_len: u8,
        }

        let config = UncheckedAstraConfig::deserialize(deserializer)?;
        for (field, prefix_len) in [
            (
                "underlay_rail_route_prefix_len",
                config.underlay_rail_route_prefix_len,
            ),
            (
                "underlay_software_plane_route_prefix_len",
                config.underlay_software_plane_route_prefix_len,
            ),
        ] {
            if u32::from(prefix_len) >= Ipv4Addr::BITS {
                return Err(serde::de::Error::custom(format!(
                    "{field} must be less than {} for IPv4, got {prefix_len}",
                    Ipv4Addr::BITS
                )));
            }
        }
        // Widen before adding so even invalid u8 values cannot overflow.
        if u32::from(config.underlay_ip_rail_id_bit_len)
            + u32::from(config.underlay_ip_software_plane_id_bit_len)
            >= Ipv4Addr::BITS
        {
            return Err(serde::de::Error::custom(format!(
                "underlay_ip_rail_id_bit_len + underlay_ip_software_plane_id_bit_len must be less than {} for IPv4",
                Ipv4Addr::BITS
            )));
        }
        Ok(config)
    }
}

impl AstraConfig {
    const fn default_underlay_ip_rail_id_bit_len() -> u8 {
        4
    }

    const fn default_underlay_ip_software_plane_id_bit_len() -> u8 {
        8
    }

    const fn default_underlay_rail_route_prefix_len() -> u8 {
        16
    }

    const fn default_underlay_software_plane_route_prefix_len() -> u8 {
        13
    }
}

impl Default for AstraConfig {
    fn default() -> Self {
        Self {
            underlay_rail_route_prefix_len: Self::default_underlay_rail_route_prefix_len(),
            underlay_software_plane_route_prefix_len:
                Self::default_underlay_software_plane_route_prefix_len(),
            underlay_ip_rail_id_bit_len: Self::default_underlay_ip_rail_id_bit_len(),
            underlay_ip_software_plane_id_bit_len:
                Self::default_underlay_ip_software_plane_id_bit_len(),
        }
    }
}

/// DPA (aka Cluster Interconnect Network) related configuration.
/// Enables DPA, and specifies basic network settings.
/// The VNI to be used by DPA will be the same as the parent VPC.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EwEthersConfig {
    /// Global enable/disable of Cluster Interconnect Network.
    #[serde(default)]
    pub enabled: bool,

    /// Enable the SVPC (Scalable VPC) path. Disabled by default and not
    /// mutually exclusive with [`Self::astra_enabled`].
    #[serde(default)]
    pub svpc_enabled: bool,

    /// Enable the Astra path. Disabled by default and not mutually exclusive
    /// with [`Self::svpc_enabled`].
    #[serde(default)]
    pub astra_enabled: bool,

    /// Base IPv4 address of the DPA/Cluster Interconnect overlay network.
    #[serde(default = "EwEthersConfig::default_subnet_ip")]
    pub subnet_ip: Ipv4Addr,

    /// IPv4 CIDR prefix length (0–32) for the DPA overlay network.
    #[serde(default = "EwEthersConfig::default_subnet_mask")]
    pub subnet_mask: i32,

    /// Astra specific configuration.
    #[serde(default)]
    pub astra: AstraConfig,

    /// The interval at which we run the DPA monitor.
    #[serde(
        default = "EwEthersConfig::default_monitor_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub monitor_run_interval: std::time::Duration,

    /// SVPC MQTT connection settings.
    #[serde(default)]
    pub svpc: SvpcConfig,
}

impl EwEthersConfig {
    pub const fn default_monitor_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub const fn default_subnet_ip() -> Ipv4Addr {
        Ipv4Addr::UNSPECIFIED
    }

    pub const fn default_subnet_mask() -> i32 {
        11
    }
}

impl Default for EwEthersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            svpc_enabled: false,
            astra_enabled: false,
            subnet_ip: Self::default_subnet_ip(),
            subnet_mask: Self::default_subnet_mask(),
            astra: AstraConfig::default(),
            monitor_run_interval: Self::default_monitor_run_interval(),
            svpc: SvpcConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use serde_json::json;

    use super::AstraConfig;

    #[test]
    fn astra_config_deserialization_checks_ipv4_limits() {
        check_values(
            [
                Check {
                    scenario: "largest valid prefixes and identifier sum",
                    input: json!({
                        "underlay_rail_route_prefix_len": 31,
                        "underlay_software_plane_route_prefix_len": 31,
                        "underlay_ip_rail_id_bit_len": 23,
                        "underlay_ip_software_plane_id_bit_len": 8
                    }),
                    expect: true,
                },
                Check {
                    scenario: "rail prefix reaches 32",
                    input: json!({"underlay_rail_route_prefix_len": 32}),
                    expect: false,
                },
                Check {
                    scenario: "software plane prefix reaches 32",
                    input: json!({"underlay_software_plane_route_prefix_len": 32}),
                    expect: false,
                },
                Check {
                    scenario: "identifier sum reaches 32",
                    input: json!({"underlay_ip_rail_id_bit_len": 24}),
                    expect: false,
                },
                Check {
                    scenario: "identifier sum exceeds u8 without overflow",
                    input: json!({
                        "underlay_ip_rail_id_bit_len": 255,
                        "underlay_ip_software_plane_id_bit_len": 255
                    }),
                    expect: false,
                },
            ],
            |input| serde_json::from_value::<AstraConfig>(input).is_ok(),
        );
    }
}
