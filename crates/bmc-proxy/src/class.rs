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
//! share an upstream budget.
//!
//! A class is a name, an ordered list of [`RequestPattern`]s, and an upstream
//! timeout. The table classifies every proxied request by walking the classes
//! in config order and taking the first whose patterns match; a request no
//! class claims belongs to the implicit `default` class, which carries the
//! proxy's historical upstream budget, [`DEFAULT_UPSTREAM_TIMEOUT`]. Operators
//! may declare `default` themselves to change that budget, but it takes no
//! patterns: it exists to catch what nothing else matched.

use std::collections::HashSet;
use std::time::Duration;

use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

use crate::pattern::RequestPattern;

/// The implicit class for requests no configured class matches.
const DEFAULT_CLASS_NAME: &str = "default";

/// Upstream budget of the default class and of any class that sets none: the
/// proxy's historical total-request timeout. Streamed uploads build their
/// scaled budget on it.
pub(crate) const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest upstream budget a class may set. An exchange holds a proxy task
/// and a BMC connection for its whole budget, so a budget has to end; half an
/// hour is far past any Redfish exchange other than a firmware upload, whose
/// streamed body scales its own budget, so a longer one is a mistake.
const MAX_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Longest class name accepted.
const MAX_CLASS_NAME_LEN: usize = 32;

/// One `[[class]]` table as written in the config file. An unknown field is
/// a configuration error: a misspelled knob must not be silently ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassDefinition {
    /// The class's name on the request's trace span. A lowercase letter
    /// followed by lowercase letters, digits, or `_`, at most 32 characters,
    /// unique across the table.
    name: String,
    /// Patterns a request must match to belong to this class. Evaluated in
    /// order across classes; the first class with a matching pattern wins.
    #[serde(rename = "match", default)]
    patterns: Vec<RequestPattern>,
    /// Total budget for one upstream exchange in this class.
    #[serde(with = "humantime_serde", default = "default_upstream_timeout")]
    upstream_timeout: Duration,
}

fn default_upstream_timeout() -> Duration {
    DEFAULT_UPSTREAM_TIMEOUT
}

/// A validated request class.
pub(crate) struct RequestClass {
    pub(crate) name: String,
    patterns: Vec<RequestPattern>,
    /// Total budget for one upstream exchange: from connecting to the BMC
    /// until the last byte of its response body has been streamed to the
    /// caller, redirects included. A request replayed with fresh credentials
    /// gets a budget of its own. A streamed upload scales its own budget from
    /// its declared size instead.
    pub(crate) upstream_timeout: Duration,
}

impl RequestClass {
    fn default_class() -> Self {
        Self {
            name: DEFAULT_CLASS_NAME.to_string(),
            patterns: Vec::new(),
            upstream_timeout: DEFAULT_UPSTREAM_TIMEOUT,
        }
    }
}

/// The ordered class table plus the implicit default class.
pub(crate) struct ClassTable {
    classes: Vec<RequestClass>,
    default: RequestClass,
}

impl Default for ClassTable {
    fn default() -> Self {
        Self {
            classes: Vec::new(),
            default: RequestClass::default_class(),
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
    #[error("class {0:?} has no match patterns; only the default class may omit them")]
    NoPatterns(String),
    #[error("the default class takes no match patterns; it catches unmatched requests")]
    DefaultWithPatterns,
    #[error("class {0:?} upstream_timeout must be greater than zero")]
    ZeroTimeout(String),
    #[error("class {0:?} upstream_timeout exceeds the {MAX_UPSTREAM_TIMEOUT:?} maximum")]
    TimeoutTooLarge(String),
}

impl ClassTable {
    fn from_definitions(definitions: Vec<ClassDefinition>) -> Result<Self, ClassTableError> {
        let mut seen = HashSet::new();
        let mut table = Self::default();

        for definition in definitions {
            let name = definition.name;
            if !is_valid_class_name(&name) {
                return Err(ClassTableError::InvalidName(name));
            }
            if !seen.insert(name.clone()) {
                return Err(ClassTableError::DuplicateName(name));
            }
            if definition.upstream_timeout.is_zero() {
                return Err(ClassTableError::ZeroTimeout(name));
            }
            if definition.upstream_timeout > MAX_UPSTREAM_TIMEOUT {
                return Err(ClassTableError::TimeoutTooLarge(name));
            }

            let is_default = name == DEFAULT_CLASS_NAME;
            match (is_default, definition.patterns.is_empty()) {
                (true, false) => return Err(ClassTableError::DefaultWithPatterns),
                (false, true) => return Err(ClassTableError::NoPatterns(name)),
                _ => {}
            }
            let class = RequestClass {
                name,
                patterns: definition.patterns,
                upstream_timeout: definition.upstream_timeout,
            };
            if is_default {
                table.default = class;
            } else {
                table.classes.push(class);
            }
        }

        Ok(table)
    }

    /// The class of a request for `method` on `path`: the first configured
    /// class with a matching pattern, else the default class.
    pub(crate) fn classify(&self, method: &http::Method, path: &str) -> &RequestClass {
        self.classes
            .iter()
            .find(|class| {
                class
                    .patterns
                    .iter()
                    .any(|pattern| pattern.matches(method, path))
            })
            .unwrap_or(&self.default)
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
        name = "inventory"
        match = ["GET /redfish/v1/UpdateService/FirmwareInventory/**"]
        upstream_timeout = "300s"

        [[class]]
        name = "events"
        match = ["GET /redfish/v1/EventService/**"]

        [[class]]
        name = "reads"
        match = ["GET /redfish/v1/**"]
        upstream_timeout = "30s"

        [[class]]
        name = "default"
        upstream_timeout = "90s"
    "#;

    #[derive(Deserialize)]
    struct Tables {
        #[serde(rename = "class", default)]
        classes: ClassTable,
    }

    fn parse_table(source: &str) -> Result<ClassTable, String> {
        figment::Figment::new()
            .merge(Toml::string(source))
            .extract::<Tables>()
            .map(|tables| tables.classes)
            .map_err(|error| error.to_string())
    }

    /// The table a parse produces: class names in order, plus the default
    /// class's budget in seconds.
    fn summarize(source: &str) -> Result<(Vec<String>, u64), String> {
        let table = parse_table(source)?;
        Ok((
            table
                .classes
                .iter()
                .map(|class| class.name.clone())
                .collect(),
            table.default.upstream_timeout.as_secs(),
        ))
    }

    #[test]
    fn class_table_parsing() {
        scenarios!(run = |source| summarize(source).map_err(drop);
            "well formed tables" {
                "" => Yields((vec![], 60)),
                r#"[[class]]
                   name = "a_class_name_of_exactly_32_chars"
                   match = ["/redfish/v1/**"]"# => Yields((
                    vec!["a_class_name_of_exactly_32_chars".to_string()],
                    60,
                )),
                r#"[[class]]
                   name = "default"
                   upstream_timeout = "30m""# => Yields((vec![], 30 * 60)),
            }

            "rejected tables" {
                r#"[[class]]
                   name = "Control"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "1st"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "_control"
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "con-trol"
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
                   name = "control"
                   match = ["/redfish/v1/**"]
                   upstream_timeuot = "30s""# => Fails,
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
            }
        );
    }

    /// What a request for `method` on `path` is classified as: the class
    /// name and its budget in seconds.
    fn classified(method: http::Method, path: &'static str) -> (String, u64) {
        let table = parse_table(TABLE).expect("the test table parses");
        let class = table.classify(&method, path);
        (class.name.clone(), class.upstream_timeout.as_secs())
    }

    #[test]
    fn classification_takes_the_first_matching_class() {
        value_scenarios!(
            run = |(method, path)| classified(method, path);
            "pattern order and verbs" {
                (http::Method::PATCH, "/redfish/v1/Systems/System_0") => ("control".to_string(), 45),
                // `reads` matches too, but comes later.
                (http::Method::GET, "/redfish/v1/UpdateService/FirmwareInventory/FW_BMC_0") => ("inventory".to_string(), 300),
            }

            "a class that sets no budget" {
                (http::Method::GET, "/redfish/v1/EventService/Subscriptions") => ("events".to_string(), 60),
            }

            "unmatched requests fall to the default class" {
                (http::Method::PUT, "/redfish/v1/Systems/System_0") => ("default".to_string(), 90),
            }
        );
    }
}
