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
use model::ib::{IBMtu, IBRateLimit, IBServiceLevel};
use serde::{Deserialize, Deserializer, Serialize};

const MAX_IB_PARTITION_PER_TENANT: i32 = 31;

/// InfiniBand fabric manager configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IBFabricConfig {
    /// Maximum InfiniBand partitions per tenant (1-31).
    #[serde(
        default = "IBFabricConfig::default_max_partition_per_tenant",
        deserialize_with = "IBFabricConfig::deserialize_max_partition"
    )]
    pub max_partition_per_tenant: i32,

    /// Enables InfiniBand fabric management.
    #[serde(default)]
    pub enabled: bool,

    /// Whether a fabric configuration that does not
    /// adhere to security requirements for tenant
    /// isolation and infrastructure protection is
    /// allowed.
    #[serde(default)]
    pub allow_insecure: bool,

    /// Maximum transmission unit for InfiniBand fabric
    /// traffic.
    #[serde(
        default = "IBMtu::default",
        deserialize_with = "IBFabricConfig::deserialize_mtu"
    )]
    pub mtu: IBMtu,

    /// Rate limit for InfiniBand fabric traffic.
    #[serde(
        default = "IBRateLimit::default",
        deserialize_with = "IBFabricConfig::deserialize_rate_limit"
    )]
    pub rate_limit: IBRateLimit,

    /// Quality of service level for InfiniBand
    /// packets.
    #[serde(
        default = "IBServiceLevel::default",
        deserialize_with = "IBFabricConfig::deserialize_service_level"
    )]
    pub service_level: IBServiceLevel,

    /// The interval at which ib fabric monitor runs in seconds.
    /// Defaults to 1 Minute if not specified.
    #[serde(
        default = "IBFabricConfig::default_fabric_monitor_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub fabric_monitor_run_interval: std::time::Duration,
}

impl Default for IBFabricConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_partition_per_tenant: Self::default_max_partition_per_tenant(),
            allow_insecure: false,
            mtu: IBMtu::default(),
            rate_limit: IBRateLimit::default(),
            service_level: IBServiceLevel::default(),
            fabric_monitor_run_interval: Self::default_fabric_monitor_run_interval(),
        }
    }
}

impl IBFabricConfig {
    pub const fn default_max_partition_per_tenant() -> i32 {
        MAX_IB_PARTITION_PER_TENANT
    }

    pub const fn default_fabric_monitor_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    pub fn deserialize_max_partition<'de, D>(deserializer: D) -> Result<i32, D::Error>
    where
        D: Deserializer<'de>,
    {
        let max_pkey = i32::deserialize(deserializer)?;

        match max_pkey {
            1..=31 => Ok(max_pkey),
            _ => Err(serde::de::Error::custom("invalid max partition per tenant")),
        }
    }

    pub fn deserialize_mtu<'de, D>(deserializer: D) -> Result<IBMtu, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mtu = i32::deserialize(deserializer)?;

        IBMtu::try_from(mtu).map_err(|e| serde::de::Error::custom(e.to_string()))
    }

    pub fn deserialize_rate_limit<'de, D>(deserializer: D) -> Result<IBRateLimit, D::Error>
    where
        D: Deserializer<'de>,
    {
        let rate_limit = i32::deserialize(deserializer)?;

        IBRateLimit::try_from(rate_limit).map_err(|e| serde::de::Error::custom(e.to_string()))
    }

    pub fn deserialize_service_level<'de, D>(deserializer: D) -> Result<IBServiceLevel, D::Error>
    where
        D: Deserializer<'de>,
    {
        let service_level = i32::deserialize(deserializer)?;

        IBServiceLevel::try_from(service_level).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

/// Settings related to an IB fabric
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IbFabricDefinition {
    /// UFM endpoint address
    /// These need to be fully qualified, e.g. https://1.2.3.4:443
    ///
    /// Note: Currently only a single endpoint is accepted.
    /// This limitation might be lifted in the future
    pub endpoints: Vec<String>,
    /// pkey ranges used for the fabric
    /// Note that editing the pkey ranges will never shrink the currently defined
    /// ranges. It can only be used to expand the range
    pub pkeys: Vec<model::resource_pool::define::Range>,
}

#[cfg(test)]
mod test {
    use figment::Figment;
    use figment::providers::{Format, Toml};

    use super::*;

    #[test]
    fn parse_ib_fabric() {
        let toml = r#"
rate_limit = 300
enabled = true
max_partition_per_tenant = 3
        "#;
        let ib_fabric_config: IBFabricConfig =
            Figment::new().merge(Toml::string(toml)).extract().unwrap();

        println!("{ib_fabric_config:?}");

        assert_eq!(
            <IBMtu as std::convert::Into<i32>>::into(ib_fabric_config.mtu),
            4
        );
        assert_eq!(
            <IBRateLimit as std::convert::Into<i32>>::into(ib_fabric_config.rate_limit),
            300
        );
        assert_eq!(
            <IBServiceLevel as std::convert::Into<i32>>::into(ib_fabric_config.service_level),
            0
        );
        assert!(ib_fabric_config.enabled);
        assert_eq!(ib_fabric_config.max_partition_per_tenant, 3);
    }

    #[test]
    #[allow(clippy::result_large_err)] // complains about figma::Error which we don't control
    fn deserialize_serialize_ib_config() {
        // An empty config matches the default object
        let deserialized_empty: IBFabricConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            IBFabricConfig::default(),
            deserialized_empty,
            "Empty IBFabricConfig does not match default"
        );
        assert!(!deserialized_empty.enabled);

        let value_input = IBFabricConfig {
            enabled: true,
            allow_insecure: false,
            max_partition_per_tenant: 1,
            mtu: IBMtu(2),
            rate_limit: IBRateLimit(10),
            service_level: IBServiceLevel(2),
            fabric_monitor_run_interval: std::time::Duration::from_secs(33),
        };

        let value_json = serde_json::to_string(&value_input).unwrap();
        let value_output: IBFabricConfig = serde_json::from_str(&value_json).unwrap();

        assert_eq!(value_output, value_input);

        let value_json = r#"{"enabled": true, "max_partition_per_tenant": 2, "mtu": 4, "rate_limit": 20, "service_level": 10}"#;
        let value_output: IBFabricConfig = serde_json::from_str(value_json).unwrap();

        assert_eq!(
            value_output,
            IBFabricConfig {
                enabled: true,
                allow_insecure: false,
                max_partition_per_tenant: 2,
                mtu: IBMtu(4),
                rate_limit: IBRateLimit(20),
                service_level: IBServiceLevel(10),
                fabric_monitor_run_interval: std::time::Duration::from_secs(60),
            }
        );

        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "Test.toml",
                r#"
                enabled=true
            "#,
            )?;
            let config: IBFabricConfig = Figment::new()
                .merge(Toml::file("Test.toml"))
                .extract()
                .unwrap();

            assert!(config.enabled);
            assert!(!config.allow_insecure);
            assert_eq!(config.max_partition_per_tenant, MAX_IB_PARTITION_PER_TENANT);
            assert_eq!(config.mtu, IBMtu::default());
            assert_eq!(config.rate_limit, IBRateLimit::default());
            assert_eq!(config.service_level, IBServiceLevel::default());
            assert_eq!(
                config.fabric_monitor_run_interval,
                IBFabricConfig::default_fabric_monitor_run_interval()
            );
            Ok(())
        });
    }
}
