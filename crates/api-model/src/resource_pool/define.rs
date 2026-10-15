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
use serde::{Deserialize, Serialize};

/// `ResourcePoolConfig` adds allocator selection to a pool definition without
/// changing the definition JSON stored in the database.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(from = "ResourcePoolConfigInput")]
pub struct ResourcePoolConfig {
    /// Ranges and value type, flattened into the configuration table.
    #[serde(flatten)]
    pub definition: ResourcePoolDef,
    /// Backend configuration name, such as `shared`. Omission selects Integrated.
    /// The caller resolves this name before constructing a pool selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Provider lookup name. Omission uses the logical pool name, which
    /// Integrated also requires when this field is supplied explicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_pool: Option<String>,
}

impl From<ResourcePoolDef> for ResourcePoolConfig {
    fn from(definition: ResourcePoolDef) -> Self {
        Self {
            definition,
            backend: None,
            remote_pool: None,
        }
    }
}

/// `ResourcePoolConfigInput` reads the flat pool configuration before grouping
/// its definition fields. Reading these fields directly preserves the unknown
/// field errors and nested paths that the configuration loader uses for warnings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcePoolConfigInput {
    #[serde(default)]
    ranges: Vec<Range>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(rename = "type")]
    pool_type: ResourcePoolType,
    #[serde(default)]
    delegate_prefix_len: Option<u8>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    remote_pool: Option<String>,
}

impl From<ResourcePoolConfigInput> for ResourcePoolConfig {
    fn from(input: ResourcePoolConfigInput) -> Self {
        Self {
            definition: ResourcePoolDef {
                ranges: input.ranges,
                prefix: input.prefix,
                pool_type: input.pool_type,
                delegate_prefix_len: input.delegate_prefix_len,
            },
            backend: input.backend,
            remote_pool: input.remote_pool,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePoolDef {
    #[serde(default)]
    pub ranges: Vec<Range>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(rename = "type")]
    pub pool_type: ResourcePoolType,
    #[serde(default)]
    pub delegate_prefix_len: Option<u8>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: String,
    pub end: String,
    #[serde(default = "default_true")]
    pub auto_assign: bool,
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResourcePoolType {
    Ipv4,
    Ipv6,
    Ipv6Prefix,
    Integer,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn pool_config_accepts_flat_selectors_and_legacy_defaults() {
        scenarios!(
            run = |selectors| {
                let input = format!(
                    "type = \"integer\"\nranges = [{{ start = \"1\", end = \"10\" }}]\n{selectors}"
                );
                let config: ResourcePoolConfig = toml::from_str(&input)?;
                assert_eq!(config.definition, ResourcePoolDef {
                    ranges: vec![Range {
                        start: "1".to_string(),
                        end: "10".to_string(),
                        auto_assign: true,
                    }],
                    prefix: None,
                    pool_type: ResourcePoolType::Integer,
                    delegate_prefix_len: None,
                });
                let encoded = toml::to_string(&config).expect("serialize pool config");
                assert_eq!(toml::from_str::<ResourcePoolConfig>(&encoded)?, config);
                Ok::<_, toml::de::Error>((config.backend, config.remote_pool))
            };
            "legacy defaults" {
                "" => Yields((None, None)),
            }
            "configured selection" {
                "backend = \"shared\"\nremote_pool = \"provider-vnis\"" => Yields((Some("shared".to_string()), Some("provider-vnis".to_string()))),
            }
        );

        let config: ResourcePoolConfig = toml::from_str(
            r#"
type = "ipv6prefix"
prefix = "2001:db8::/48"
delegate_prefix_len = 64
"#,
        )
        .expect("IPv6 prefix configuration");
        assert_eq!(config.definition.pool_type, ResourcePoolType::Ipv6Prefix);
        assert_eq!(config.definition.prefix.as_deref(), Some("2001:db8::/48"));
        assert_eq!(config.definition.delegate_prefix_len, Some(64));
        let encoded = toml::to_string(&config).expect("serialize IPv6 prefix configuration");
        assert_eq!(
            toml::from_str::<ResourcePoolConfig>(&encoded)
                .expect("parse IPv6 prefix configuration"),
            config
        );

        let config: ResourcePoolConfig = serde_json::from_str(
            r#"{"type":"integer","prefix":null,"delegate_prefix_len":null,"backend":null,"remote_pool":null}"#,
        )
        .expect("optional fields accept JSON null");
        assert!(config.definition.ranges.is_empty());
        assert_eq!(config.definition.prefix, None);
        assert_eq!(config.definition.delegate_prefix_len, None);
        assert_eq!(config.backend, None);
        assert_eq!(config.remote_pool, None);

        scenarios!(
            run = |input| serde_json::from_str::<ResourcePoolConfig>(input).map(drop).map_err(drop);
            "required type and unique selectors" {
                r#"{}"# => Fails,
                r#"{"type":"integer","backend":null,"backend":"local"}"# => Fails,
            }
            "unknown fields" {
                r#"{"type":"integer","backed":"shared"}"# => Fails,
            }
        );
    }

    #[test]
    fn pool_selectors_do_not_change_persisted_definition_json() {
        const SNAPSHOT: &str =
            r#"{"ranges":[],"prefix":null,"type":"integer","delegate_prefix_len":null}"#;
        let definition: ResourcePoolDef =
            serde_json::from_str(SNAPSHOT).expect("legacy definition snapshot");
        let mut config = ResourcePoolConfig::from(definition);
        assert_eq!(serde_json::to_string(&config).unwrap(), SNAPSHOT);
        config.backend = Some("shared".to_string());
        config.remote_pool = Some("provider-vnis".to_string());
        assert_eq!(serde_json::to_string(&config.definition).unwrap(), SNAPSHOT);

        scenarios!(
            run = |input| serde_json::from_str::<ResourcePoolDef>(input).map(drop).map_err(drop);
            "snapshot readers reject config selectors" {
                r#"{"type":"integer","backend":"shared"}"# => Fails,
                r#"{"type":"integer","remote_pool":"provider-vnis"}"# => Fails,
            }
        );
    }
}
