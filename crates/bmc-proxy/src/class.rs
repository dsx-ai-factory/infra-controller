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
//! share an upstream budget and a place in each BMC's admission queue.
//!
//! A class is a name, the callers whose requests it takes, an ordered list of
//! [`RequestPattern`]s, an upstream timeout, and its admission settings. The
//! table classifies every proxied request by walking the classes in config
//! order and taking the first that takes the caller's requests and whose
//! patterns match; a request no class claims belongs to the implicit `default`
//! class, which carries the proxy's historical upstream budget,
//! [`DEFAULT_UPSTREAM_TIMEOUT`], and no `max_in_flight` of its own. Operators may declare
//! `default` themselves to change those, but it takes no patterns or
//! principals: it exists to catch what nothing else matched.

use std::collections::HashSet;
use std::fmt;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use carbide_authn::middleware::Principal;
use carbide_instrument::LabelValue;
use opentelemetry::StringValue;
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

/// How many of a class's requests may wait for one BMC when the class sets
/// no `max_queued`: several times the few requests a BMC handles at once,
/// while each waiting request also holds its buffered body.
const DEFAULT_MAX_QUEUED: NonZeroUsize = NonZeroUsize::new(16).expect("16 is not zero");

/// Most requests a class may let wait for one BMC. Each BMC in use holds a
/// queue with room for the class's `max_queued`, and for requests on their
/// way to its free slots, for every class that takes slots, allocated when
/// its runtime starts; each waiting request holds its buffered body.
const MAX_QUEUED: usize = 128;

/// The name of a request class: its label on metrics and its name on trace
/// spans. Names come from the proxy's own `[[class]]` tables, so the set of
/// values is fixed at startup and small; nothing a caller sends can create a
/// metric series.
#[derive(Clone, PartialEq)]
pub(crate) struct ClassName(Arc<str>);

impl ClassName {
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
    /// The class's name on the request's trace span. A lowercase letter
    /// followed by lowercase letters, digits, or `_`, at most 32 characters,
    /// unique across the table.
    name: String,
    /// Patterns a request must match to belong to this class. Evaluated in
    /// order across classes; the first class that takes the caller's
    /// requests and has a matching pattern wins.
    #[serde(rename = "match", default)]
    patterns: Vec<RequestPattern>,
    /// Callers whose requests this class takes, by principal identifier as
    /// in `allowed_principals`. Absent takes every caller's. Never
    /// `anonymous`, which every caller holds.
    #[serde(default)]
    principals: Option<Vec<String>>,
    /// Total budget for one upstream exchange in this class.
    #[serde(with = "humantime_serde", default = "default_upstream_timeout")]
    upstream_timeout: Duration,
    /// The order in which this class's waiting requests reach a BMC under
    /// the per-BMC limit: higher first. No effect without that limit.
    #[serde(default)]
    priority: u8,
    /// How many of this class's requests one proxy replica sends to one BMC
    /// at a time. Absent leaves the class unlimited.
    #[serde(default)]
    max_in_flight: Option<NonZeroU32>,
    /// How many of this class's requests may wait for one BMC at a time.
    #[serde(default = "default_max_queued")]
    max_queued: NonZeroUsize,
}

fn default_upstream_timeout() -> Duration {
    DEFAULT_UPSTREAM_TIMEOUT
}

fn default_max_queued() -> NonZeroUsize {
    DEFAULT_MAX_QUEUED
}

/// A validated request class.
pub(crate) struct RequestClass {
    pub(crate) name: ClassName,
    /// Callers whose requests the class takes, by principal identifier;
    /// empty takes every caller's.
    principals: Vec<String>,
    patterns: Vec<RequestPattern>,
    /// Total budget for one upstream exchange: looking up the BMC's
    /// credentials, waiting for a slot there, and from connecting to the BMC
    /// until the last byte of its response body has been streamed to the
    /// caller, redirects included. A request replayed with fresh credentials
    /// gets a budget of its own. A streamed upload looks up credentials and
    /// waits within this budget, then scales its own from its declared size.
    pub(crate) upstream_timeout: Duration,
    /// Higher is admitted first when classes wait for the same BMC under the
    /// per-BMC limit.
    pub(crate) priority: u8,
    /// Requests of this class one replica sends to one BMC at a time; `None`
    /// is unlimited.
    pub(crate) max_in_flight: Option<NonZeroU32>,
    /// Requests of this class that may wait for one BMC at a time.
    pub(crate) max_queued: NonZeroUsize,
}

impl RequestClass {
    fn default_class() -> Self {
        Self {
            name: ClassName(Arc::from(DEFAULT_CLASS_NAME)),
            principals: Vec::new(),
            patterns: Vec::new(),
            upstream_timeout: DEFAULT_UPSTREAM_TIMEOUT,
            priority: 0,
            max_in_flight: None,
            max_queued: DEFAULT_MAX_QUEUED,
        }
    }

    /// Whether the class takes the requests of a caller holding
    /// `principals`.
    fn takes_from(&self, principals: &[String]) -> bool {
        self.principals.is_empty()
            || self
                .principals
                .iter()
                .any(|principal| principals.contains(principal))
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
    #[error("class {0:?} lists no principals; omit principals to take every caller's requests")]
    NoPrincipals(String),
    #[error("the default class takes no principals; it catches unmatched requests")]
    DefaultWithPrincipals,
    #[error(
        "class {0:?} lists the anonymous principal, which every caller holds; omit principals to take every caller's requests"
    )]
    AnonymousPrincipal(String),
    #[error("class {0:?} lists principal {1:?}, which is empty or padded with whitespace")]
    MalformedPrincipal(String, String),
    #[error("class {0:?} upstream_timeout must be greater than zero")]
    ZeroTimeout(String),
    #[error("class {0:?} upstream_timeout exceeds the {MAX_UPSTREAM_TIMEOUT:?} maximum")]
    TimeoutTooLarge(String),
    #[error("class {0:?} max_queued exceeds the {MAX_QUEUED} maximum")]
    QueueTooLong(String),
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
            if definition.max_queued.get() > MAX_QUEUED {
                return Err(ClassTableError::QueueTooLong(name));
            }

            let is_default = name == DEFAULT_CLASS_NAME;
            match (is_default, definition.patterns.is_empty()) {
                (true, false) => return Err(ClassTableError::DefaultWithPatterns),
                (false, true) => return Err(ClassTableError::NoPatterns(name)),
                _ => {}
            }
            let principals = match definition.principals {
                Some(_) if is_default => return Err(ClassTableError::DefaultWithPrincipals),
                Some(principals) if principals.is_empty() => {
                    return Err(ClassTableError::NoPrincipals(name));
                }
                principals => principals.unwrap_or_default(),
            };
            if let Some(malformed) = principals
                .iter()
                .find(|principal| principal.is_empty() || principal.trim() != principal.as_str())
            {
                return Err(ClassTableError::MalformedPrincipal(name, malformed.clone()));
            }
            if principals
                .iter()
                .any(|principal| principal == &Principal::Anonymous.as_identifier())
            {
                return Err(ClassTableError::AnonymousPrincipal(name));
            }
            let class = RequestClass {
                name: ClassName(Arc::from(name)),
                principals,
                patterns: definition.patterns,
                upstream_timeout: definition.upstream_timeout,
                priority: definition.priority,
                max_in_flight: definition.max_in_flight,
                max_queued: definition.max_queued,
            };
            if is_default {
                table.default = class;
            } else {
                table.classes.push(class);
            }
        }

        Ok(table)
    }

    /// The class of a request for `method` on `path` from a caller holding
    /// `principals`: the first configured class that takes the caller's
    /// requests and has a matching pattern, else the default class.
    pub(crate) fn classify(
        &self,
        method: &http::Method,
        path: &str,
        principals: &[String],
    ) -> &RequestClass {
        self.classes
            .iter()
            .find(|class| {
                class.takes_from(principals)
                    && class
                        .patterns
                        .iter()
                        .any(|pattern| pattern.matches(method, path))
            })
            .unwrap_or(&self.default)
    }

    /// Every class, the default class last.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &RequestClass> {
        self.classes.iter().chain([&self.default])
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
        name = "dps"
        principals = ["spiffe-service-id/nv-dps-agent", "spiffe-service-id/nv-dps"]
        match = ["/redfish/v1/**"]
        upstream_timeout = "20s"

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
        name = "admins"
        principals = ["external-role/admin"]
        match = ["/redfish/v1/**"]

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
                .map(|class| class.name.to_string())
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
                r#"[[class]]
                   name = "dps"
                   principals = []
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "default"
                   principals = ["spiffe-service-id/nv-dps"]"# => Fails,
                r#"[[class]]
                   name = "dps"
                   principals = ["spiffe-service-id/nv-dps", "anonymous"]
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "dps"
                   principals = [""]
                   match = ["/redfish/v1/**"]"# => Fails,
                r#"[[class]]
                   name = "dps"
                   principals = [" spiffe-service-id/nv-dps"]
                   match = ["/redfish/v1/**"]"# => Fails,
            }
        );
    }

    /// A caller holding no principal of its own.
    const ANYONE: &[&str] = &["anonymous"];
    /// An admin, by its certificate's group.
    const ADMIN: &[&str] = &["external-role/admin", "anonymous"];
    /// DPS, among other principals.
    const DPS: &[&str] = &[
        "trusted-certificate",
        "spiffe-service-id/nv-dps",
        "anonymous",
    ];

    /// What a request for `method` on `path` from a caller holding
    /// `principals` is classified as: the class name and its budget in
    /// seconds.
    fn classified(method: http::Method, path: &'static str, principals: &[&str]) -> (String, u64) {
        let table = parse_table(TABLE).expect("the test table parses");
        let principals: Vec<String> = principals.iter().map(ToString::to_string).collect();
        let class = table.classify(&method, path, &principals);
        (class.name.to_string(), class.upstream_timeout.as_secs())
    }

    #[test]
    fn classification_takes_the_first_matching_class() {
        value_scenarios!(
            run = |(method, path, principals)| classified(method, path, principals);
            "pattern order and verbs" {
                // `dps` matches too, but takes only DPS's requests.
                (http::Method::PATCH, "/redfish/v1/Systems/System_0", ANYONE) => ("control".to_string(), 45),
                // `reads` matches too, but comes later.
                (http::Method::GET, "/redfish/v1/UpdateService/FirmwareInventory/FW_BMC_0", ANYONE) => ("inventory".to_string(), 300),
            }

            "a class that sets no budget" {
                (http::Method::GET, "/redfish/v1/EventService/Subscriptions", ANYONE) => ("events".to_string(), 60),
            }

            "a class for some callers" {
                // `control` matches too, but comes later; the caller holds
                // the second of the class's principals.
                (http::Method::PATCH, "/redfish/v1/Systems/System_0", DPS) => ("dps".to_string(), 20),
                // `admins` takes the caller too, but comes later.
                (http::Method::GET, "/redfish/v1/Systems/System_0", ADMIN) => ("reads".to_string(), 30),
                // The caller is DPS, but no `dps` pattern matches.
                (http::Method::PUT, "/other", DPS) => ("default".to_string(), 90),
            }

            "unmatched requests fall to the default class" {
                (http::Method::PUT, "/redfish/v1/Systems/System_0", ANYONE) => ("default".to_string(), 90),
            }
        );
    }

    /// A class's admission settings: (priority, max_in_flight, max_queued)
    /// of the first class in `source`, or of the default class when it
    /// declares none.
    fn admission_of(source: &str) -> Result<(u8, Option<u32>, usize), String> {
        let table = parse_table(source)?;
        let class = table.iter().next().expect("the table has a class");
        Ok((
            class.priority,
            class.max_in_flight.map(std::num::NonZeroU32::get),
            class.max_queued.get(),
        ))
    }

    #[test]
    fn admission_settings_parse() {
        scenarios!(run = |source| admission_of(source).map_err(drop);
            "loaded" {
                "" => Yields((0, None, 16)),
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]
                   priority = 255
                   max_in_flight = 2
                   max_queued = 128"# => Yields((255, Some(2), 128)),
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]"# => Yields((0, None, 16)),
            }

            "rejected" {
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]
                   priority = 256"# => Fails,
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]
                   max_in_flight = 0"# => Fails,
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]
                   max_queued = 0"# => Fails,
                r#"[[class]]
                   name = "power"
                   match = ["PATCH /redfish/v1/**"]
                   max_queued = 129"# => Fails,
            }
        );
    }
}
