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
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use http::uri;
use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

/// Top-level ACL configuration keyed by authenticated principal identifier.
///
/// Each principal maps to an ordered list of [`AclEntry`] values. Entries are
/// evaluated in order, and the first matching entry determines the outcome of
/// the request.
#[derive(Clone, Default)]
pub(crate) struct AclConfig {
    // Keys are "users" (ie. service principals), values are a list of AclEntries for authenticating them.
    config: BTreeMap<String, Vec<AclEntry>>,
}

impl<'de> Deserialize<'de> for AclConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let config = BTreeMap::<String, Vec<AclEntry>>::deserialize(deserializer)?;
        Ok(Self { config })
    }
}

impl AclConfig {
    /// Returns whether `principal` is allowed to perform `method` on `path`.
    ///
    /// The principal's ACL entries are evaluated in order. The first matching
    /// entry wins. If the principal is unknown or no entry matches, this
    /// returns `false`.
    pub(crate) fn allows(&self, principal: &str, method: &http::Method, path: &str) -> bool {
        let Some(entries) = self.config.get(principal) else {
            return false;
        };
        entries
            .iter()
            .find_map(|entry| entry.action_if_matches(method, path))
            .map(|action| action.is_allowed())
            .unwrap_or(false)
    }
}

/// An entry in the access control list for a client to carbide-bmc-proxy.
///
/// The text form for use in the config takes the form of a single string with a leading `!` if the
/// entry is disallowed (otherwise the entry is allowed), a list of HTTP verbs, and a wildcarded HTTP
/// path
///
/// Examples:
///
/// - `GET /redfish/v1/**`: Allow GET for anything that begins with /redfish/v1/
/// - `!POST,PATCH /redfish/v1/Systems/*/SecureBoot/**`: Deny anything in Systems/*/SecureBoot
#[derive(Clone)]
struct AclEntry {
    verbs: Vec<AclVerb>,
    path: AclPath,
    action: AclAction,
}

impl<'de> Deserialize<'de> for AclEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(D::Error::custom)
    }
}

impl Display for AclEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if matches!(self.action, AclAction::Deny) {
            write!(f, "! ")?;
        }

        if !self.verbs.is_empty() {
            write!(
                f,
                "{} ",
                self.verbs
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )?;
        }

        for component in &self.path.components {
            write!(f, "/{component}")?;
        }

        Ok(())
    }
}

impl AclEntry {
    fn action_if_matches(&self, method: &http::Method, path: &str) -> Option<AclAction> {
        if self.matches(method, path) {
            Some(self.action)
        } else {
            None
        }
    }

    /// Returns whether this ACL entry matches `method` and `path`.
    ///
    /// Verb matching is exact unless this entry omits verbs, in which case any
    /// HTTP method matches. Path matching uses the wildcard semantics described
    /// by [`WildcardPathComponent`].
    fn matches(&self, method: &http::Method, path: &str) -> bool {
        if !self.verbs.is_empty() && !self.verbs.iter().any(|verb| verb.0.eq(method)) {
            return false;
        }

        let Some(path) = path.strip_prefix('/') else {
            return false;
        };
        // `/redfish/v1/` and `/redfish/v1` name the same resource (libredfish
        // requests the service root with the trailing slash), so a single
        // trailing slash is not a path component.
        let path = path.strip_suffix('/').unwrap_or(path);
        if path.is_empty() {
            return self.path.components.is_empty()
                || matches!(
                    self.path.components.as_slice(),
                    [WildcardPathComponent::DoubleWildcard]
                );
        }

        let path_components = path.split('/').collect::<Vec<_>>();
        if path_components.iter().any(|component| component.is_empty()) {
            return false;
        }

        let acl_components = &self.path.components;
        let double_wildcard_index = acl_components
            .iter()
            .position(|component| matches!(component, WildcardPathComponent::DoubleWildcard));

        match double_wildcard_index {
            None => {
                acl_components.len() == path_components.len()
                    && acl_components.iter().zip(path_components.iter()).all(
                        |(acl_component, path_component)| acl_component.matches(path_component),
                    )
            }
            Some(double_wildcard_index) => {
                let (prefix, suffix_with_wildcard) = acl_components.split_at(double_wildcard_index);
                let suffix = &suffix_with_wildcard[1..];

                if path_components.len() < prefix.len() + suffix.len() {
                    return false;
                }

                prefix
                    .iter()
                    .zip(path_components.iter())
                    .all(|(acl_component, path_component)| acl_component.matches(path_component))
                    && suffix.iter().rev().zip(path_components.iter().rev()).all(
                        |(acl_component, path_component)| acl_component.matches(path_component),
                    )
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("error parsing ACL path {orig}: {err}")]
struct AclPathParseError {
    orig: String,
    err: String,
}

impl FromStr for AclEntry {
    type Err = AclPathParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(AclPathParseError {
                orig: input.to_string(),
                err: "ACL entry cannot be empty".to_string(),
            });
        }
        let (allowed, s) = if let Some(suffix) = input.strip_prefix('!') {
            (false, suffix.trim())
        } else {
            (true, input.trim())
        };

        let (verbs, path) = if let Some(pair) = s.split_once(' ') {
            let verbs = pair
                .0
                .trim()
                .split(',')
                .map(AclVerb::from_str)
                .collect::<Result<Vec<_>, _>>()?;
            let path = pair.1.trim().parse::<AclPath>()?;
            (verbs, path)
        } else {
            let path = s.parse::<AclPath>()?;
            (Vec::new(), path)
        };

        Ok(Self {
            path,
            verbs,
            action: allowed.into(),
        })
    }
}

/// The authorization decision produced by a matching ACL entry.
#[derive(Copy, Clone)]
enum AclAction {
    /// Permit the request.
    Allow,
    /// Reject the request.
    Deny,
}

impl From<bool> for AclAction {
    fn from(value: bool) -> Self {
        if value { Self::Allow } else { Self::Deny }
    }
}

impl AclAction {
    /// Returns `true` when this action permits the request.
    fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[derive(Clone)]
struct AclPath {
    components: Vec<WildcardPathComponent>,
}

impl FromStr for AclPath {
    type Err = AclPathParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some(s) = s.strip_prefix('/') else {
            return Err(AclPathParseError {
                orig: s.to_string(),
                err: "Path must begin with '/'".to_string(),
            });
        };

        let components = s
            .split('/')
            .map(WildcardPathComponent::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AclPathParseError {
                orig: s.to_string(),
                err: format!("Path contains invalid component: {e}"),
            })?;

        if components
            .iter()
            .filter(|s| matches!(s, WildcardPathComponent::DoubleWildcard))
            .count()
            > 1
        {
            return Err(AclPathParseError {
                orig: s.to_string(),
                err: "Paths may only contain one double-wildcard (**)".to_string(),
            });
        }

        Ok(Self { components })
    }
}

#[derive(Clone)]
enum WildcardPathComponent {
    SingleWildcard,
    DoubleWildcard,
    PrefixWildcard(String),
    SuffixWildcard(String),
    Exact(String),
}

impl FromStr for WildcardPathComponent {
    type Err = AclPathParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(AclPathParseError {
                orig: s.to_string(),
                err: "Empty path component".to_string(),
            });
        }
        if s.eq("*") {
            return Ok(WildcardPathComponent::SingleWildcard);
        } else if s.eq("**") {
            return Ok(WildcardPathComponent::DoubleWildcard);
        }

        if s.contains('*') {
            if s.matches('*').count() > 1 {
                return Err(AclPathParseError {
                    orig: s.to_string(),
                    err: "Path component may contain at most one single-wildcard (`*`) unless it is the double-wildcard (`**`)".to_string(),
                });
            }

            if let Some(suffix) = s.strip_prefix('*') {
                validate_path_component(s, &format!("/x{suffix}"))?;
                return Ok(WildcardPathComponent::SuffixWildcard(suffix.to_string()));
            }

            if let Some(prefix) = s.strip_suffix('*') {
                validate_path_component(s, &format!("/{prefix}x"))?;
                return Ok(WildcardPathComponent::PrefixWildcard(prefix.to_string()));
            }

            return Err(AclPathParseError {
                orig: s.to_string(),
                err: "Path component may only use `*` as the whole component or at the beginning or end".to_string(),
            });
        }

        validate_path_component(s, &format!("/{s}"))?;

        Ok(WildcardPathComponent::Exact(s.to_string()))
    }
}

fn validate_path_component(orig: &str, as_whole_path: &str) -> Result<(), AclPathParseError> {
    let path_and_query =
        uri::PathAndQuery::from_str(as_whole_path).map_err(|e| AclPathParseError {
            orig: orig.to_string(),
            err: format!("Invalid path: {e}"),
        })?;
    if path_and_query.query().is_some() {
        return Err(AclPathParseError {
            orig: orig.to_string(),
            err: "Path must not have query parameters".to_string(),
        });
    }
    if path_and_query.path().ne(as_whole_path) {
        return Err(AclPathParseError {
            orig: orig.to_string(),
            err: "Path must be normalized".to_string(),
        });
    }

    Ok(())
}

impl Display for WildcardPathComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            WildcardPathComponent::SingleWildcard => write!(f, "*"),
            WildcardPathComponent::DoubleWildcard => write!(f, "**"),
            WildcardPathComponent::PrefixWildcard(s) => write!(f, "{}*", s),
            WildcardPathComponent::SuffixWildcard(s) => write!(f, "*{}", s),
            WildcardPathComponent::Exact(s) => write!(f, "{}", s),
        }
    }
}

impl WildcardPathComponent {
    fn matches(&self, s: &str) -> bool {
        match self {
            WildcardPathComponent::SingleWildcard | WildcardPathComponent::DoubleWildcard => true,
            WildcardPathComponent::PrefixWildcard(prefix) => s.starts_with(prefix),
            WildcardPathComponent::SuffixWildcard(suffix) => s.ends_with(suffix),
            WildcardPathComponent::Exact(expected) => expected == s,
        }
    }
}

#[derive(Clone)]
struct AclVerb(http::Method);

impl FromStr for AclVerb {
    type Err = AclPathParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self(http::Method::GET)),
            "POST" => Ok(Self(http::Method::POST)),
            "PUT" => Ok(Self(http::Method::PUT)),
            "PATCH" => Ok(Self(http::Method::PATCH)),
            "DELETE" => Ok(Self(http::Method::DELETE)),
            "HEAD" => Ok(Self(http::Method::HEAD)),
            _ => Err(AclPathParseError {
                orig: s.to_string(),
                err: "Invalid verb".to_string(),
            }),
        }
    }
}

impl Display for AclVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use figment::providers::{Format, Toml};

    use super::*;

    struct EntryMatchInput {
        entry: &'static str,
        method: http::Method,
        path: &'static str,
    }

    struct ConfigRequestInput {
        principal: &'static str,
        method: http::Method,
        path: &'static str,
    }

    fn parse_acl_config(config_str: &str) -> AclConfig {
        #[derive(Deserialize)]
        struct MockConfig {
            acls: AclConfig,
        }

        let config: MockConfig = figment::Figment::new()
            .merge(Toml::string(config_str))
            .extract()
            .expect("Mock config didn't parse");
        config.acls
    }

    fn round_trip_as_acl_entry(s: &str) -> Result<String, AclPathParseError> {
        s.parse::<AclEntry>().map(|entry| entry.to_string())
    }

    #[test]
    fn acl_entry_parsing_cases() {
        scenarios!(run = |input| round_trip_as_acl_entry(input).map_err(drop);
            "canonical forms" {
                "GET /redfish/v1/**" => Yields("GET /redfish/v1/**".to_string()),
                "! GET,PUT,POST,PATCH /redfish/v1/**" => Yields(
                    "! GET,PUT,POST,PATCH /redfish/v1/**".to_string()
                ),
                "! GET,PUT,POST,PATCH /redfish/v1/Systems/*/SecureBoot/**" => Yields(
                    "! GET,PUT,POST,PATCH /redfish/v1/Systems/*/SecureBoot/**".to_string()
                ),
                "GET /redfish/v1/Systems/system*/SecureBoot" => Yields(
                    "GET /redfish/v1/Systems/system*/SecureBoot".to_string()
                ),
                "GET /redfish/v1/Systems/*Boot/SecureBoot" => Yields(
                    "GET /redfish/v1/Systems/*Boot/SecureBoot".to_string()
                ),
            }

            "canonical spacing" {
                "!/redfish/v1/**" => Yields("! /redfish/v1/**".to_string()),
                "!GET,PUT,POST /redfish/v1/**" => Yields(
                    "! GET,PUT,POST /redfish/v1/**".to_string()
                ),
            }

            "canonical method normalization" {
                "delete,head /redfish/v1/**" => Yields(
                    "DELETE,HEAD /redfish/v1/**".to_string()
                ),
            }

            "invalid entries" {
                "" => Fails,
                "/" => Fails,
                "GET /foo//bar" => Fails,
                "GET foo" => Fails,
                "BOGUS /foo" => Fails,
                "GET /foo?query_not_supported" => Fails,
                "GET /foo/bar*baz" => Fails,
                "GET /foo/**bar" => Fails,
                "GET /redfish/v1/**/SecureBoot/**" => Fails,
                "GET /foo/ba r" => Fails,
                "GET /foo/ba r*" => Fails,
                "GET /foo/*ba r" => Fails,
                "GET /foo#fragment" => Fails,
            }
        );
    }

    #[test]
    fn acl_entry_matching_cases() {
        value_scenarios!(
            run = |EntryMatchInput {
                       entry,
                       method,
                       path,
                   }| {
                entry
                    .parse::<AclEntry>()
                    .expect("table contains a valid ACL entry")
                    .matches(&method, path)
            };
            "fixed paths" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-1/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-1/SecureBoot/extra",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-1",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*/SecureBoot",
                    method: http::Method::POST,
                    path: "/redfish/v1/Systems/node-1/SecureBoot",
                } => false,
            }

            "middle double wildcard" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-1/Bios/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-1/Bios",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v2/SecureBoot",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/**/redfish",
                    method: http::Method::GET,
                    path: "/redfish",
                } => false,
            }

            "invalid request paths" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "redfish/v1/Systems",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish//v1/Systems",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/",
                } => false,
            }

            // Redfish treats a trailing slash as insignificant, and libredfish
            // requests the service root as `/redfish/v1/`; the slash must not
            // count as an (empty) path component.
            "trailing slash is not a component" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/",
                } => true,
                EntryMatchInput {
                    entry: "GET /**",
                    method: http::Method::GET,
                    path: "/redfish/v1/",
                } => true,
                EntryMatchInput {
                    entry: "GET /**",
                    method: http::Method::GET,
                    path: "/",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/",
                } => true,
            }

            "trailing double wildcard" {
                EntryMatchInput {
                    entry: "GET /redfish/**",
                    method: http::Method::GET,
                    path: "/redfish",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/**",
                    method: http::Method::GET,
                    path: "/other/v1/Systems",
                } => false,
            }

            "leading double wildcard" {
                EntryMatchInput {
                    entry: "GET /**/SecureBoot",
                    method: http::Method::GET,
                    path: "/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /**/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/Bios",
                } => false,
            }

            "prefix wildcard" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/system*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/system-1/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/system*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/system/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/system*/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/node-system/SecureBoot",
                } => false,
            }

            "suffix wildcard" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*Boot/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/FastBoot/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*Boot/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/Boot/SecureBoot",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Systems/*Boot/SecureBoot",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/BootOrder/SecureBoot",
                } => false,
            }

            "verbless entries" {
                EntryMatchInput {
                    entry: "/redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems",
                } => true,
                EntryMatchInput {
                    entry: "/redfish/v1/**",
                    method: http::Method::DELETE,
                    path: "/redfish/v1/Systems",
                } => true,
            }
        );
    }

    #[test]
    fn acl_config_matching() {
        let acls = parse_acl_config(
            r#"
        [acls]
        service_a = ["/redfish/v1/**"]
        service_b = ["!POST /redfish/v1/Systems/*/SecureBoot/**", "/redfish/v1/**"]
        "#,
        );

        value_scenarios!(
            run = |ConfigRequestInput {
                       principal,
                       method,
                       path,
                   }| acls.allows(principal, &method, path);
            "matching allow entries" {
                ConfigRequestInput {
                    principal: "service_a",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems",
                } => true,
                ConfigRequestInput {
                    principal: "service_b",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems",
                } => true,
            }

            "matching deny entry" {
                ConfigRequestInput {
                    principal: "service_b",
                    method: http::Method::POST,
                    path: "/redfish/v1/Systems/System1/SecureBoot/Bad",
                } => false,
            }
        );
    }

    #[test]
    fn acl_config_first_match_wins() {
        let acls = parse_acl_config(
            r#"
        [acls]
        deny_first = ["! /redfish/v1/Systems/**", "/redfish/v1/**"]
        allow_first = ["/redfish/v1/**", "! /redfish/v1/Systems/**"]
        "#,
        );

        value_scenarios!(
            run = |ConfigRequestInput {
                       principal,
                       method,
                       path,
                   }| acls.allows(principal, &method, path);
            "first matching entry decides" {
                ConfigRequestInput {
                    principal: "deny_first",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/System1",
                } => false,
                ConfigRequestInput {
                    principal: "allow_first",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems/System1",
                } => true,
            }
        );
    }

    #[test]
    fn acl_config_defaults_to_deny() {
        let acls = parse_acl_config(
            r#"
        [acls]
        service_a = ["/redfish/v1/**"]
        "#,
        );

        value_scenarios!(
            run = |ConfigRequestInput {
                       principal,
                       method,
                       path,
                   }| acls.allows(principal, &method, path);
            "requests without an allow match" {
                ConfigRequestInput {
                    principal: "unknown_service",
                    method: http::Method::GET,
                    path: "/redfish/v1/Systems",
                } => false,
                ConfigRequestInput {
                    principal: "service_a",
                    method: http::Method::GET,
                    path: "/other/stuff",
                } => false,
            }
        );
    }
}
