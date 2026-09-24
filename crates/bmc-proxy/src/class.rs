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

//! Request classes: operator-defined groups of proxied BMC requests that
//! share an upstream budget and a cache policy.
//!
//! A class is a name, an ordered list of [`RequestPattern`]s, an optional
//! principal filter, an upstream timeout, and an optional cache policy. The
//! table classifies every proxied request by walking the classes in config
//! order and taking the first whose rules match; a request no class claims
//! belongs to the implicit `default` class, which carries the proxy's
//! historical 60 second upstream budget and no cache. Operators may declare
//! `default` themselves to change that budget, but it takes no rules: it
//! exists to catch what nothing else matched.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::LabelValue;
use opentelemetry::StringValue;
use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

use crate::cache::{CachePolicy, CachePolicyConfig, CachePolicyError};
use crate::pattern::RequestPattern;

/// The implicit class for requests no configured class matches.
const DEFAULT_CLASS_NAME: &str = "default";

/// Upstream budget of the implicit default class, and of any class that does
/// not set one: the proxy's historical total-request timeout.
pub(crate) const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest upstream budget a class may set. A fetch the cache runs holds one
/// of a BMC's few fetch slots for its whole budget, so a budget has to end.
/// Streamed firmware uploads scale their own budget and are not bound here.
const MAX_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Longest class name the metric label accepts.
const MAX_CLASS_NAME_LEN: usize = 32;

/// The name of a request class: its identity in the cache and its label on
/// metrics and log lines.
///
/// Names come from the proxy's own `[[class]]` table, so the set of values
/// is fixed at startup and small; nothing a caller sends can create a metric
/// series. Callers never see or choose the class.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ClassName(Arc<str>);

impl ClassName {
    fn new(name: &str) -> Self {
        Self(Arc::from(name))
    }

    /// A name outside any table, for tests of what consumes names.
    #[cfg(test)]
    pub(crate) fn for_test(name: &str) -> Self {
        Self::new(name)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClassName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl LabelValue for ClassName {
    fn label_value(&self) -> StringValue {
        StringValue::from(Arc::clone(&self.0))
    }
}

/// One `[[class]]` table as written in the config file. An unknown field is
/// a configuration error: a misspelled knob must not be silently ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassDefinition {
    /// Label the class carries on metrics and log lines. Lowercase
    /// `snake_case`, at most 32 characters, unique across the table.
    name: String,
    /// Patterns a request must match to belong to this class. Evaluated in
    /// order across classes; the first class with a matching rule wins.
    #[serde(rename = "match", default)]
    rules: Vec<RequestPattern>,
    /// Restricts the class to requests from these principal identifiers.
    /// Empty admits any principal.
    #[serde(default)]
    principals: Vec<String>,
    /// Total budget for one upstream exchange in this class. Streamed
    /// firmware uploads scale their own budget from the declared size and
    /// ignore this value.
    #[serde(with = "humantime_serde", default = "default_upstream_timeout")]
    upstream_timeout: Duration,
    /// Response cache policy for `GET` requests in this class. Absent means
    /// every request is forwarded.
    #[serde(default)]
    cache: Option<CachePolicyConfig>,
}

fn default_upstream_timeout() -> Duration {
    DEFAULT_UPSTREAM_TIMEOUT
}

/// The implicit default class: the historical budget, nothing cached.
fn default_class() -> Arc<RequestClass> {
    Arc::new(RequestClass {
        name: ClassName::new(DEFAULT_CLASS_NAME),
        rules: Vec::new(),
        principals: HashSet::new(),
        upstream_timeout: default_upstream_timeout(),
        cache: None,
    })
}

/// A validated request class.
pub(crate) struct RequestClass {
    pub(crate) name: ClassName,
    rules: Vec<RequestPattern>,
    principals: HashSet<String>,
    pub(crate) upstream_timeout: Duration,
    pub(crate) cache: Option<CachePolicy>,
}

impl RequestClass {
    fn matches(&self, method: &http::Method, path: &str, principals: &[String]) -> bool {
        if !self.principals.is_empty()
            && !principals
                .iter()
                .any(|principal| self.principals.contains(principal))
        {
            return false;
        }
        self.rules.iter().any(|rule| rule.matches(method, path))
    }
}

/// The ordered class table plus the implicit default class.
#[derive(Clone)]
pub(crate) struct ClassTable {
    classes: Arc<[Arc<RequestClass>]>,
    default: Arc<RequestClass>,
}

impl Default for ClassTable {
    fn default() -> Self {
        Self {
            classes: Arc::from([]),
            default: default_class(),
        }
    }
}

impl<'de> Deserialize<'de> for ClassTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let definitions = Vec::<ClassDefinition>::deserialize(deserializer)?;
        Self::from_definitions(definitions).map_err(D::Error::custom)
    }
}

#[derive(thiserror::Error, Debug)]
enum ClassTableError {
    #[error(
        "class name {0:?} must be lowercase snake_case of at most {MAX_CLASS_NAME_LEN} characters"
    )]
    InvalidName(String),
    #[error("class {0:?} is declared more than once")]
    DuplicateName(String),
    #[error("class {0:?} has no match rules; only the default class may omit them")]
    NoRules(String),
    #[error("the default class takes no match rules or principals; it catches unmatched requests")]
    DefaultWithRules,
    #[error(
        "the default class takes no cache policy; it catches resources such as the system and manager roots whose live state must not be served stale"
    )]
    DefaultWithCache,
    #[error("class {name:?} upstream_timeout must be greater than zero")]
    ZeroTimeout { name: String },
    #[error("class {name:?} upstream_timeout exceeds the {MAX_UPSTREAM_TIMEOUT:?} maximum")]
    TimeoutTooLarge { name: String },
    #[error("class {name:?} cache policy: {source}")]
    InvalidCache {
        name: String,
        source: CachePolicyError,
    },
}

impl ClassTable {
    fn from_definitions(definitions: Vec<ClassDefinition>) -> Result<Self, ClassTableError> {
        let mut seen = HashSet::new();
        let mut classes = Vec::with_capacity(definitions.len());
        let mut default = None;

        for definition in definitions {
            if !is_valid_class_name(&definition.name) {
                return Err(ClassTableError::InvalidName(definition.name));
            }
            if !seen.insert(definition.name.clone()) {
                return Err(ClassTableError::DuplicateName(definition.name));
            }
            if definition.upstream_timeout.is_zero() {
                return Err(ClassTableError::ZeroTimeout {
                    name: definition.name,
                });
            }
            if definition.upstream_timeout > MAX_UPSTREAM_TIMEOUT {
                return Err(ClassTableError::TimeoutTooLarge {
                    name: definition.name,
                });
            }
            let cache = definition
                .cache
                .map(CachePolicy::try_from)
                .transpose()
                .map_err(|source| ClassTableError::InvalidCache {
                    name: definition.name.clone(),
                    source,
                })?;

            let class = Arc::new(RequestClass {
                name: ClassName::new(&definition.name),
                rules: definition.rules,
                principals: definition.principals.into_iter().collect(),
                upstream_timeout: definition.upstream_timeout,
                cache,
            });

            if definition.name == DEFAULT_CLASS_NAME {
                if !class.rules.is_empty() || !class.principals.is_empty() {
                    return Err(ClassTableError::DefaultWithRules);
                }
                if class.cache.is_some() {
                    return Err(ClassTableError::DefaultWithCache);
                }
                default = Some(class);
            } else {
                if class.rules.is_empty() {
                    return Err(ClassTableError::NoRules(definition.name));
                }
                classes.push(class);
            }
        }

        Ok(Self {
            classes: classes.into(),
            default: default.unwrap_or_else(default_class),
        })
    }
}

fn is_valid_class_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_CLASS_NAME_LEN
        && first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl ClassTable {
    /// The class of a request from `principals` for `method` on `path`: the
    /// first configured class whose rules match, else the default class.
    pub(crate) fn classify(
        &self,
        method: &http::Method,
        path: &str,
        principals: &[String],
    ) -> Arc<RequestClass> {
        self.classes
            .iter()
            .find(|class| class.matches(method, path, principals))
            .unwrap_or(&self.default)
            .clone()
    }

    /// Every class with a cache policy, in table order.
    pub(crate) fn cached_classes(&self) -> impl Iterator<Item = &Arc<RequestClass>> {
        self.classes.iter().filter(|class| class.cache.is_some())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};
    use figment::providers::{Format, Toml};

    use super::*;

    const TABLE: &str = r#"
        [[class]]
        name = "control"
        match = ["POST,PATCH,DELETE /redfish/v1/**"]
        upstream_timeout = "45s"

        [[class]]
        name = "dps_metrics"
        principals = ["spiffe-service-id/nv-dps"]
        match = ["GET /redfish/v1/**/EnvironmentMetrics"]
        upstream_timeout = "20s"

        [[class]]
        name = "inventory"
        match = ["GET /redfish/v1/UpdateService/FirmwareInventory/**"]
        upstream_timeout = "300s"
        cache = { ttl = "1h", stale_while_revalidate = "6h", stale_if_error = "24h" }

        [[class]]
        name = "default"
        upstream_timeout = "90s"
    "#;

    #[derive(Deserialize)]
    struct MockConfig {
        #[serde(rename = "class", default)]
        classes: ClassTable,
    }

    fn parse_table(source: &str) -> Result<ClassTable, String> {
        figment::Figment::new()
            .merge(Toml::string(source))
            .extract::<MockConfig>()
            .map(|config| config.classes)
            .map_err(|error| error.to_string())
    }

    /// The table shape a parse produces: class names in order, plus the
    /// default class's budget.
    fn summarize(source: &str) -> Result<(Vec<String>, u64), String> {
        let table = parse_table(source)?;
        Ok((
            table
                .classes
                .iter()
                .map(|class| class.name.as_str().to_string())
                .collect(),
            table.default.upstream_timeout.as_secs(),
        ))
    }

    struct ClassifyInput {
        method: http::Method,
        path: &'static str,
        principals: &'static [&'static str],
    }

    /// What a caller observes from classification: the class name and the
    /// budget applied to the forward.
    fn classify(input: ClassifyInput) -> (String, u64) {
        let table = parse_table(TABLE).expect("the test table parses");
        let principals = input
            .principals
            .iter()
            .map(|principal| principal.to_string())
            .collect::<Vec<_>>();
        let class = table.classify(&input.method, input.path, &principals);
        (
            class.name.as_str().to_string(),
            class.upstream_timeout.as_secs(),
        )
    }

    #[test]
    fn class_table_parsing() {
        scenarios!(run = |source| summarize(source).map_err(drop);
            "well formed tables" {
                "" => Yields((vec![], 60)),
                TABLE => Yields((
                    vec![
                        "control".to_string(),
                        "dps_metrics".to_string(),
                        "inventory".to_string(),
                    ],
                    90,
                )),
            }

            "rejected tables" {
                r#"[[class]]
                   name = "Control"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "a_very_long_class_name_that_exceeds_the_limit"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "control"
                   match = ["/redfish/v1/**"]
                   [[class]]
                   name = "control"
                   match = ["/redfish/v1/Systems/**"]"# => Fails,
                r#"[[class]]
                   name = "control""# => Fails,
                r#"[[class]]
                   name = "default"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "default"
                   cache = { ttl = "1h" }"# => Fails,
                r#"[[class]]
                   name = "control"
                   match = ["/redfish/v1/**"]
                   upstream_timeuot = "30s""# => Fails,
                r#"[[class]]
                   name = "inventory"
                   match = ["GET /redfish/v1/**"]
                   cache = { ttl = "1h", stale_if_erorr = "24h" }"# => Fails,
                r#"[[class]]
                   name = "control"
                   match = ["/redfish/v1/**"]
                   upstream_timeout = "0s""# => Fails,
                r#"[[class]]
                   name = "control"
                   match = ["/redfish/v1/**"]
                   upstream_timeout = "31m""# => Fails,
                r#"[[class]]
                   name = "control"
                   match = ["BOGUS /redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "inventory"
                   match = ["GET /redfish/v1/**"]
                   cache = { ttl = "0s" }"# => Fails,
            }
        );
    }

    #[test]
    fn classification_takes_the_first_matching_class() {
        value_scenarios!(
            run = classify;
            "rule order and verbs" {
                ClassifyInput {
                    method: http::Method::PATCH,
                    path: "/redfish/v1/Chassis/HGX_Chassis_0/EnvironmentMetrics",
                    principals: &["spiffe-service-id/nv-dps"],
                } => ("control".to_string(), 45),
                ClassifyInput {
                    method: http::Method::GET,
                    path: "/redfish/v1/UpdateService/FirmwareInventory/FW_BMC_0",
                    principals: &["spiffe-service-id/nico-api"],
                } => ("inventory".to_string(), 300),
            }

            "principal filter" {
                ClassifyInput {
                    method: http::Method::GET,
                    path: "/redfish/v1/Chassis/HGX_Chassis_0/EnvironmentMetrics",
                    principals: &["spiffe-service-id/nv-dps", "anonymous"],
                } => ("dps_metrics".to_string(), 20),
                ClassifyInput {
                    method: http::Method::GET,
                    path: "/redfish/v1/Chassis/HGX_Chassis_0/EnvironmentMetrics",
                    principals: &["spiffe-service-id/nico-api", "anonymous"],
                } => ("default".to_string(), 90),
            }

            "unmatched requests fall to the default class" {
                ClassifyInput {
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/System_0",
                    principals: &["spiffe-service-id/nico-api"],
                } => ("default".to_string(), 90),
            }
        );
    }

    /// Without a `[[class]]` table the proxy keeps its historical behavior:
    /// one class, the 60 second budget, nothing cached.
    #[test]
    fn empty_table_is_the_historical_default() {
        let table = ClassTable::default();
        let class = table.classify(&http::Method::GET, "/redfish/v1", &[]);
        assert_eq!(class.name.as_str(), DEFAULT_CLASS_NAME);
        assert_eq!(class.upstream_timeout, Duration::from_secs(60));
        assert!(class.cache.is_none());
        assert_eq!(table.cached_classes().count(), 0);
    }
}
