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

//! The `VERB[,VERB...] /path/pattern` request pattern shared by ACL entries
//! and request classes.
//!
//! A pattern matches an HTTP method and a request path. Verb matching is
//! exact unless the pattern omits verbs, in which case any method matches.
//! Path matching is component-wise with the wildcard semantics described by
//! [`PathComponent`]. A single trailing slash on the request path is not a
//! component, because Redfish names the same resource with and without it.
//! Percent-escape hex digits match case-insensitively; escapes stay encoded.

use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use http::uri;
use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

/// A method-and-path pattern in the text form the config files use.
///
/// Examples:
///
/// - `GET /redfish/v1/**`: `GET` for anything under `/redfish/v1`
/// - `POST,PATCH /redfish/v1/Systems/*/SecureBoot/**`: writes below any
///   system's `SecureBoot` subtree
/// - `/redfish/v1/**`: any method under `/redfish/v1`
#[derive(Clone)]
pub(crate) struct RequestPattern {
    verbs: Vec<Verb>,
    path: PathPattern,
}

/// Percent escapes an ACL allow wildcard is permitted to consume.
#[derive(Clone)]
pub(crate) enum WildcardEscapes {
    /// Only these decoded octet values; an empty list keeps wildcards strict.
    Listed(Vec<u8>),
    /// Any percent escape, subject to the proxy's separate path guard.
    All,
}

impl WildcardEscapes {
    /// Checks only the wildcard's portion, leaving literal escapes to the pattern.
    fn matches(&self, fragment: &str) -> bool {
        match self {
            Self::All => true,
            Self::Listed(octets) => fragment.split('%').skip(1).all(|suffix| {
                suffix
                    .get(..2)
                    .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                    .is_some_and(|octet| octets.contains(&octet))
            }),
        }
    }
}

impl<'de> Deserialize<'de> for RequestPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(D::Error::custom)
    }
}

impl Display for RequestPattern {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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

impl FromStr for RequestPattern {
    type Err = PatternParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let s = input.trim();
        if s.is_empty() {
            return Err(PatternParseError {
                orig: input.to_string(),
                err: "pattern cannot be empty".to_string(),
            });
        }

        let parsed = if let Some(pair) = s.split_once(' ') {
            pair.0
                .trim()
                .split(',')
                .map(Verb::from_str)
                .collect::<Result<Vec<_>, _>>()
                .and_then(|verbs| Ok((verbs, pair.1.trim().parse::<PathPattern>()?)))
        } else {
            s.parse::<PathPattern>().map(|path| (Vec::new(), path))
        };
        let (verbs, path) = parsed.map_err(|err| PatternParseError {
            orig: input.to_string(),
            err,
        })?;

        Ok(Self { verbs, path })
    }
}

impl RequestPattern {
    /// Returns whether this pattern matches `method` and `path`.
    pub(crate) fn matches(&self, method: &http::Method, path: &str) -> bool {
        self.matches_with_wildcard_escapes(method, path, &WildcardEscapes::All)
    }

    /// Matches an ACL allow rule, limiting wildcard escapes to its explicit policy.
    pub(crate) fn matches_allow_escapes(
        &self,
        method: &http::Method,
        path: &str,
        allow_escapes: &WildcardEscapes,
    ) -> bool {
        self.matches_with_wildcard_escapes(method, path, allow_escapes)
    }

    /// ACL literals must contain complete `%HH` escapes. This restriction does
    /// not change parsing or wildcard matching for request classes.
    pub(crate) fn validate_percent_escapes(&self) -> Result<(), PatternParseError> {
        for component in &self.path.components {
            let literal = match component {
                PathComponent::PrefixWildcard(s)
                | PathComponent::SuffixWildcard(s)
                | PathComponent::Exact(s) => s,
                PathComponent::SingleWildcard | PathComponent::DoubleWildcard => continue,
            };
            if !literal.split('%').skip(1).all(|suffix| {
                suffix
                    .as_bytes()
                    .get(..2)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            }) {
                return Err(PatternParseError {
                    orig: self.to_string(),
                    err: "percent escapes must use the literal `%HH` spelling".to_string(),
                });
            }
        }
        Ok(())
    }

    /// Matches the method and path, normalizing escape hex case.
    /// `allow_escapes` controls which escapes wildcards may consume.
    fn matches_with_wildcard_escapes(
        &self,
        method: &http::Method,
        path: &str,
        allow_escapes: &WildcardEscapes,
    ) -> bool {
        if !self.verbs.is_empty() && !self.verbs.iter().any(|verb| verb.0.eq(method)) {
            return false;
        }

        let Some(path) = path.strip_prefix('/') else {
            return false;
        };
        let path = normalize_percent_escape_case(path);
        // `/redfish/v1/` and `/redfish/v1` name the same resource (libredfish
        // requests the service root with the trailing slash), so a single
        // trailing slash is not a path component.
        let path = path.strip_suffix('/').unwrap_or(path.as_ref());
        if path.is_empty() {
            return self.path.components.is_empty()
                || matches!(
                    self.path.components.as_slice(),
                    [PathComponent::DoubleWildcard]
                );
        }

        let path_components = path.split('/').collect::<Vec<_>>();
        if path_components.iter().any(|component| component.is_empty()) {
            return false;
        }

        let pattern_components = &self.path.components;
        let double_wildcard_index = pattern_components
            .iter()
            .position(|component| matches!(component, PathComponent::DoubleWildcard));

        match double_wildcard_index {
            None => {
                pattern_components.len() == path_components.len()
                    && pattern_components.iter().zip(path_components.iter()).all(
                        |(pattern_component, path_component)| {
                            pattern_component.matches(path_component, allow_escapes)
                        },
                    )
            }
            Some(double_wildcard_index) => {
                let (prefix, suffix_with_wildcard) =
                    pattern_components.split_at(double_wildcard_index);
                let suffix = &suffix_with_wildcard[1..];

                if path_components.len() < prefix.len() + suffix.len() {
                    return false;
                }

                let wildcard_end = path_components.len() - suffix.len();
                if !path_components[prefix.len()..wildcard_end]
                    .iter()
                    .all(|component| allow_escapes.matches(component))
                {
                    return false;
                }

                prefix.iter().zip(path_components.iter()).all(
                    |(pattern_component, path_component)| {
                        pattern_component.matches(path_component, allow_escapes)
                    },
                ) && suffix.iter().rev().zip(path_components.iter().rev()).all(
                    |(pattern_component, path_component)| {
                        pattern_component.matches(path_component, allow_escapes)
                    },
                )
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("error parsing pattern {orig:?}: {err}")]
pub(crate) struct PatternParseError {
    orig: String,
    err: String,
}

#[derive(Clone)]
struct PathPattern {
    components: Vec<PathComponent>,
}

/// The inner parsers report a bare reason; [`RequestPattern`] names the
/// pattern it came from.
impl FromStr for PathPattern {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some(s) = s.strip_prefix('/') else {
            return Err("path must begin with '/'".to_string());
        };

        let components = s
            .split('/')
            .map(PathComponent::from_str)
            .collect::<Result<Vec<_>, _>>()?;

        if components
            .iter()
            .filter(|s| matches!(s, PathComponent::DoubleWildcard))
            .count()
            > 1
        {
            return Err("path may contain at most one double wildcard (`**`)".to_string());
        }

        Ok(Self { components })
    }
}

/// One component of a path pattern.
///
/// - `*` matches exactly one path component.
/// - `prefix*` matches one component with the given prefix.
/// - `*suffix` matches one component with the given suffix.
/// - `**` matches zero or more components. At most one per pattern.
/// - Anything else matches literally.
#[derive(Clone)]
enum PathComponent {
    SingleWildcard,
    DoubleWildcard,
    PrefixWildcard(String),
    SuffixWildcard(String),
    Exact(String),
}

impl FromStr for PathComponent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("path component cannot be empty".to_string());
        }
        if s.eq("*") {
            return Ok(PathComponent::SingleWildcard);
        } else if s.eq("**") {
            return Ok(PathComponent::DoubleWildcard);
        }

        let normalized = normalize_percent_escape_case(s);
        let s = normalized.as_ref();

        if s.contains('*') {
            if s.matches('*').count() > 1 {
                return Err(format!(
                    "path component {s:?} may contain at most one `*` unless it is `**`"
                ));
            }

            if let Some(suffix) = s.strip_prefix('*') {
                validate_path_component(s, &format!("/x{suffix}"))?;
                return Ok(PathComponent::SuffixWildcard(suffix.to_string()));
            }

            if let Some(prefix) = s.strip_suffix('*') {
                validate_path_component(s, &format!("/{prefix}x"))?;
                return Ok(PathComponent::PrefixWildcard(prefix.to_string()));
            }

            return Err(format!(
                "path component {s:?} may use `*` only as the whole component or at its start or end"
            ));
        }

        validate_path_component(s, &format!("/{s}"))?;

        Ok(PathComponent::Exact(s.to_string()))
    }
}

/// RFC 3986 section 6.2.2.1 makes hex digits in `%HH` case-insensitive.
/// Keep escapes encoded so ACL allow wildcards can enforce their escape policy.
fn normalize_percent_escape_case(path: &str) -> Cow<'_, str> {
    let mut normalized = Cow::Borrowed(path);
    for (index, _) in path.match_indices('%') {
        let Some(digits) = path.get(index + 1..index + 3) else {
            continue;
        };
        if digits.bytes().all(|digit| digit.is_ascii_hexdigit())
            && digits.bytes().any(|digit| digit.is_ascii_lowercase())
        {
            normalized
                .to_mut()
                .replace_range(index + 1..index + 3, &digits.to_ascii_uppercase());
        }
    }
    normalized
}

fn validate_path_component(orig: &str, as_whole_path: &str) -> Result<(), String> {
    let path_and_query = uri::PathAndQuery::from_str(as_whole_path)
        .map_err(|e| format!("path component {orig:?} is not a valid path: {e}"))?;
    if path_and_query.query().is_some() {
        return Err(format!(
            "path component {orig:?} must not have query parameters"
        ));
    }
    if path_and_query.path().ne(as_whole_path) {
        return Err(format!("path component {orig:?} must be normalized"));
    }

    Ok(())
}

impl Display for PathComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            PathComponent::SingleWildcard => write!(f, "*"),
            PathComponent::DoubleWildcard => write!(f, "**"),
            PathComponent::PrefixWildcard(s) => write!(f, "{}*", s),
            PathComponent::SuffixWildcard(s) => write!(f, "*{}", s),
            PathComponent::Exact(s) => write!(f, "{}", s),
        }
    }
}

impl PathComponent {
    /// Matches a component whose escape hex case is already normalized.
    /// Literal portions match independently of the wildcard's escape policy.
    fn matches(&self, s: &str, allow_escapes: &WildcardEscapes) -> bool {
        match self {
            PathComponent::SingleWildcard | PathComponent::DoubleWildcard => {
                allow_escapes.matches(s)
            }
            PathComponent::PrefixWildcard(prefix) => s
                .strip_prefix(prefix)
                .is_some_and(|rest| allow_escapes.matches(rest)),
            PathComponent::SuffixWildcard(suffix) => s
                .strip_suffix(suffix)
                .is_some_and(|rest| allow_escapes.matches(rest)),
            PathComponent::Exact(expected) => expected == s,
        }
    }
}

#[derive(Clone)]
struct Verb(http::Method);

impl FromStr for Verb {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self(http::Method::GET)),
            "POST" => Ok(Self(http::Method::POST)),
            "PUT" => Ok(Self(http::Method::PUT)),
            "PATCH" => Ok(Self(http::Method::PATCH)),
            "DELETE" => Ok(Self(http::Method::DELETE)),
            "HEAD" => Ok(Self(http::Method::HEAD)),
            _ => Err(format!("invalid verb {s:?}")),
        }
    }
}

impl Display for Verb {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// A rejected pattern is named once, whole, with the reason. Whitespace
    /// around a pattern or between its methods and path is not part of it.
    #[test]
    fn patterns_parse_or_name_the_reason_they_do_not() {
        value_scenarios!(
            run = |input: &str| input.parse::<RequestPattern>().err().map(|error| error.to_string());
            "rejected patterns" {
                "" => Some(r#"error parsing pattern "": pattern cannot be empty"#.to_string()),
                "GET,,POST /x" => Some(r#"error parsing pattern "GET,,POST /x": invalid verb """#.to_string()),
                "BREW /x" => Some(r#"error parsing pattern "BREW /x": invalid verb "BREW""#.to_string()),
                "GET x" => Some(r#"error parsing pattern "GET x": path must begin with '/'"#.to_string()),
                "/a//b" => Some(r#"error parsing pattern "/a//b": path component cannot be empty"#.to_string()),
                "/a/**/b/**" => Some(r#"error parsing pattern "/a/**/b/**": path may contain at most one double wildcard (`**`)"#.to_string()),
                "/a*b*" => Some(r#"error parsing pattern "/a*b*": path component "a*b*" may contain at most one `*` unless it is `**`"#.to_string()),
                "/a*b" => Some(r#"error parsing pattern "/a*b": path component "a*b" may use `*` only as the whole component or at its start or end"#.to_string()),
                "/a?b" => Some(r#"error parsing pattern "/a?b": path component "a?b" must not have query parameters"#.to_string()),
            }

            "surrounding whitespace" {
                " GET  /x " => None,
            }
        );
    }
}
