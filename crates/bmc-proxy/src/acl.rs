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

//! Per-principal HTTP method and path authorization for proxied requests.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

use crate::pattern::{PatternParseError, RequestPattern, WildcardEscapes};

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
/// The text form for use in the config is a [`RequestPattern`] with a leading
/// `!` if the entry is disallowed (otherwise the entry is allowed).
///
/// Examples:
///
/// - `GET /redfish/v1/**`: Allow GET for anything that begins with /redfish/v1/
/// - `!POST,PATCH /redfish/v1/Systems/*/SecureBoot/**`: Deny anything in Systems/*/SecureBoot
#[derive(Clone)]
struct AclEntry {
    pattern: RequestPattern,
    action: AclAction,
    allow_escapes: WildcardEscapes,
}

impl<'de> Deserialize<'de> for AclEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum ConfigEntry {
            Text(String),
            Options {
                pattern: String,
                #[serde(default)]
                allow_escapes: Vec<String>,
            },
        }

        let (pattern, escapes) = match ConfigEntry::deserialize(deserializer)? {
            ConfigEntry::Text(pattern) => (pattern, Vec::new()),
            ConfigEntry::Options {
                pattern,
                allow_escapes,
            } => (pattern, allow_escapes),
        };
        let mut entry = pattern.parse::<Self>().map_err(D::Error::custom)?;
        if matches!(entry.action, AclAction::Deny) && !escapes.is_empty() {
            return Err(D::Error::custom(
                "allow_escapes is only supported on allow rules",
            ));
        }
        entry.allow_escapes = if escapes.as_slice() == ["*"] {
            WildcardEscapes::All
        } else {
            let octets = escapes
                .iter()
                .map(|escape| {
                    escape
                        .strip_prefix('%')
                        .filter(|digits| {
                            digits.len() == 2 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                        .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                        .ok_or_else(|| {
                            D::Error::custom(
                                "allow_escapes must contain %HH escapes or a single \"*\"",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            WildcardEscapes::Listed(octets)
        };
        Ok(entry)
    }
}

impl Display for AclEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if matches!(self.action, AclAction::Deny) {
            write!(f, "! ")?;
        }
        self.pattern.fmt(f)
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

    /// Allow wildcards consume only opted-in escapes; other escapes must be
    /// literal. Deny wildcards retain their unrestricted escape matching.
    fn matches(&self, method: &http::Method, path: &str) -> bool {
        match self.action {
            AclAction::Allow => {
                self.pattern
                    .matches_allow_escapes(method, path, &self.allow_escapes)
            }
            AclAction::Deny => self.pattern.matches(method, path),
        }
    }
}

impl FromStr for AclEntry {
    type Err = PatternParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (allowed, s) = if let Some(suffix) = input.strip_prefix('!') {
            (false, suffix.trim())
        } else {
            (true, input.trim())
        };

        let pattern: RequestPattern = s.parse()?;
        pattern.validate_percent_escapes()?;
        Ok(Self {
            pattern,
            action: allowed.into(),
            allow_escapes: WildcardEscapes::Listed(Vec::new()),
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

    fn round_trip_as_acl_entry(s: &str) -> Result<String, PatternParseError> {
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

            "literal percent escapes" {
                "GET /redfish/v1/Registries/%23*" => Yields(
                    "GET /redfish/v1/Registries/%23*".to_string()
                ),
                "GET /redfish/v1/Oem/%aBResource" => Yields(
                    "GET /redfish/v1/Oem/%ABResource".to_string()
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
                "GET /foo/%2" => Fails,
                "GET /foo/%GG" => Fails,
                "GET /foo/%+1" => Fails,
                "GET /foo/%2*" => Fails,
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

            "allow entries require literal percent escapes" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%23*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%23*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23Smart%20StorageMessages",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/%23SmartStorageMessages",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/**/SmartStorageMessages",
                    method: http::Method::GET,
                    path: "/redfish/v1/%23Registries/SmartStorageMessages",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%4A*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%4aResource",
                } => true,
            }

            "hex case normalization preserves other literal distinctions" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%4aResource",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%4AResource",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%4AResource",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%4aresource",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/J*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%4aResource",
                } => false,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Registries/%4A*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/JResource",
                } => false,
            }

            "suffix wildcard literals must name every escape" {
                EntryMatchInput {
                    entry: "GET /redfish/v1/Oem/*%3aThing",
                    method: http::Method::GET,
                    path: "/redfish/v1/Oem/Vendor%3AThing",
                } => true,
                EntryMatchInput {
                    entry: "GET /redfish/v1/Oem/*%3aThing",
                    method: http::Method::GET,
                    path: "/redfish/v1/Oem/Vendor%20Name%3aThing",
                } => false,
            }

            "deny entries remain broad for percent escapes" {
                EntryMatchInput {
                    entry: "!GET /redfish/v1/**",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => true,
                EntryMatchInput {
                    entry: "!GET /redfish/v1/Registries/*",
                    method: http::Method::GET,
                    path: "/redfish/v1/Registries/%23SmartStorageMessages",
                } => true,
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
    fn acl_config_requires_an_explicit_allow_for_percent_escapes() {
        let acls = parse_acl_config(
            r#"
        [acls]
        explicit_after_broad_allow = ["/**", "GET /redfish/v1/Registries/%23*"]
        broad_deny_first = ["!GET /redfish/v1/**", "GET /redfish/v1/Registries/%23*"]
        explicit_allow_first = ["GET /redfish/v1/Registries/%23*", "!GET /redfish/v1/**"]
        escaped_deny_first = ["!GET /redfish/v1/Registries/%4aResource", "GET /redfish/v1/Registries/%4A*"]
        "#,
        );

        value_scenarios!(
            run = |(principal, path)| acls.allows(principal, &http::Method::GET, path);
            "a broad allow is skipped but a later literal allow matches" {
                ("explicit_after_broad_allow", "/redfish/v1/Registries/%23SmartStorageMessages") => true,
            }
            "a broad denial still wins" {
                ("broad_deny_first", "/redfish/v1/Registries/%23SmartStorageMessages") => false,
            }
            "a literal allow can precede a broad denial" {
                ("explicit_allow_first", "/redfish/v1/Registries/%23SmartStorageMessages") => true,
            }
            "changing hex case cannot bypass an explicit denial" {
                ("escaped_deny_first", "/redfish/v1/Registries/%4AResource") => false,
            }
        );
    }

    /// Structured escape policies affect only their principal and matching allow.
    #[test]
    fn acl_escape_policies_are_scoped_to_each_allow() {
        let acls = parse_acl_config(
            r#"
        [acls]
        strict = [{ pattern = "GET /nvue_v1/**" }]
        empty = [{ pattern = "GET /nvue_v1/**", allow_escapes = [] }]
        nvue = [
            "!GET /nvue_v1/private/**",
            { pattern = "GET /nvue_v1/**", allow_escapes = ["%2f", "%3A"] },
            "GET /redfish/v1/**",
        ]
        prefix = [{ pattern = "GET /nvue_v1/revision/changeset%23*", allow_escapes = ["%2F"] }]
        suffix = [{ pattern = "GET /nvue_v1/revision/*%23status", allow_escapes = ["%2F"] }]
        literal = ["GET /nvue_v1/revision/changeset%2Fadmin%2F*"]
        admin = [{ pattern = "/nvue_v1/**", allow_escapes = ["*"] }]
        "#,
        );

        value_scenarios!(
            run = |(principal, method, path)| acls.allows(principal, &method, path);
            "omitted and empty policies keep wildcards strict" {
                ("strict", http::Method::GET, "/nvue_v1/revision/1") => true,
                ("strict", http::Method::GET, "/nvue_v1/revision/changeset%2Fadmin%2F1") => false,
                ("empty", http::Method::GET, "/nvue_v1/revision/changeset%2Fadmin%2F1") => false,
            }
            "listed escapes support variable IDs and IPv6 CIDRs" {
                ("nvue", http::Method::GET, "/nvue_v1/revision/changeset%2Fadmin%2f1%2Fextra") => true,
                ("nvue", http::Method::GET, "/nvue_v1/interface/lo/ip/address/2001%3adb8%3A%3A1%2F64") => true,
                ("nvue", http::Method::GET, "/nvue_v1/revision/name%20with%2Fspace") => false,
            }
            "the policy does not widen other entries, methods or denials" {
                ("nvue", http::Method::GET, "/redfish/v1/Systems/a%2Fb") => false,
                ("nvue", http::Method::PATCH, "/nvue_v1/revision/changeset%2Fadmin%2F1") => false,
                ("nvue", http::Method::GET, "/nvue_v1/private/name%2Fsecret") => false,
            }
            "single wildcards check only the portion they consume" {
                ("prefix", http::Method::GET, "/nvue_v1/revision/changeset%23admin%2F1") => true,
                ("prefix", http::Method::GET, "/nvue_v1/revision/changeset%23admin%20name") => false,
                ("suffix", http::Method::GET, "/nvue_v1/revision/admin%2F1%23status") => true,
                ("suffix", http::Method::GET, "/nvue_v1/revision/admin%20name%23status") => false,
            }
            "literal slash exceptions still work without opt-in" {
                ("literal", http::Method::GET, "/nvue_v1/revision/changeset%2fadmin%2F1") => true,
            }
            "the star policy accepts unlisted escapes without changing literal prefixes" {
                ("admin", http::Method::DELETE, "/nvue_v1/revision/name%20with%2Fspace%23suffix") => true,
                ("admin", http::Method::GET, "/redfish/v1/Systems/a%2Fb") => false,
            }
        );
    }

    /// Invalid escape policy syntax and unknown fields fail configuration loading.
    #[test]
    fn acl_escape_policy_configuration_errors() {
        #[derive(Deserialize)]
        struct MockConfig {
            #[serde(rename = "acls")]
            _acls: AclConfig,
        }

        scenarios!(
            run = |entries| figment::Figment::new()
                .merge(Toml::string(&format!("[acls]\nservice = {entries}")))
                .extract::<MockConfig>()
                .map(drop)
                .map_err(drop);
            "invalid escape spellings" {
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = ["%2"] }]"# => Fails,
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = ["%GG"] }]"# => Fails,
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = ["%+1"] }]"# => Fails,
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = ["2F"] }]"# => Fails,
            }
            "star must be the sole list value" {
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = ["*", "%2F"] }]"# => Fails,
                r#"[{ pattern = "/nvue_v1/**", allow_escapes = "*" }]"# => Fails,
            }
            "misconfigured structured entries" {
                r#"[{ pattern = "!GET /nvue_v1/**", allow_escapes = ["%2F"] }]"# => Fails,
                r#"[{ pattern = "/nvue_v1/**", allow_escape = ["%2F"] }]"# => Fails,
                r#"[{ allow_escapes = ["%2F"] }]"# => Fails,
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
