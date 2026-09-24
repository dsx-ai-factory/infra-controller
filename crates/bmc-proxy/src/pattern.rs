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

//! The `VERB[,VERB...] /path/pattern` request pattern shared by ACL entries,
//! request classes, and cache invalidation rules.
//!
//! A pattern matches an HTTP method and a request path. Verb matching is
//! exact unless the pattern omits verbs, in which case any method matches.
//! Path matching is component-wise with the wildcard semantics described by
//! [`PathComponent`]. A single trailing slash on the request path is not a
//! component, because Redfish names the same resource with and without it.

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

        let (verbs, path) = if let Some(pair) = s.split_once(' ') {
            let verbs = pair
                .0
                .trim()
                .split(',')
                .map(Verb::from_str)
                .collect::<Result<Vec<_>, _>>()?;
            let path = pair.1.trim().parse::<PathPattern>()?;
            (verbs, path)
        } else {
            let path = s.parse::<PathPattern>()?;
            (Vec::new(), path)
        };

        Ok(Self { verbs, path })
    }
}

impl RequestPattern {
    /// Returns whether this pattern matches `method` and `path`.
    pub(crate) fn matches(&self, method: &http::Method, path: &str) -> bool {
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
                            pattern_component.matches(path_component)
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

                prefix.iter().zip(path_components.iter()).all(
                    |(pattern_component, path_component)| pattern_component.matches(path_component),
                ) && suffix.iter().rev().zip(path_components.iter().rev()).all(
                    |(pattern_component, path_component)| pattern_component.matches(path_component),
                )
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("error parsing pattern {orig}: {err}")]
pub(crate) struct PatternParseError {
    pub(crate) orig: String,
    pub(crate) err: String,
}

#[derive(Clone)]
struct PathPattern {
    components: Vec<PathComponent>,
}

impl FromStr for PathPattern {
    type Err = PatternParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some(s) = s.strip_prefix('/') else {
            return Err(PatternParseError {
                orig: s.to_string(),
                err: "Path must begin with '/'".to_string(),
            });
        };

        let components = s
            .split('/')
            .map(PathComponent::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| PatternParseError {
                orig: s.to_string(),
                err: format!("Path contains invalid component: {e}"),
            })?;

        if components
            .iter()
            .filter(|s| matches!(s, PathComponent::DoubleWildcard))
            .count()
            > 1
        {
            return Err(PatternParseError {
                orig: s.to_string(),
                err: "Paths may only contain one double-wildcard (**)".to_string(),
            });
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
    type Err = PatternParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(PatternParseError {
                orig: s.to_string(),
                err: "Empty path component".to_string(),
            });
        }
        if s.eq("*") {
            return Ok(PathComponent::SingleWildcard);
        } else if s.eq("**") {
            return Ok(PathComponent::DoubleWildcard);
        }

        if s.contains('*') {
            if s.matches('*').count() > 1 {
                return Err(PatternParseError {
                    orig: s.to_string(),
                    err: "Path component may contain at most one single-wildcard (`*`) unless it is the double-wildcard (`**`)".to_string(),
                });
            }

            if let Some(suffix) = s.strip_prefix('*') {
                validate_path_component(s, &format!("/x{suffix}"))?;
                return Ok(PathComponent::SuffixWildcard(suffix.to_string()));
            }

            if let Some(prefix) = s.strip_suffix('*') {
                validate_path_component(s, &format!("/{prefix}x"))?;
                return Ok(PathComponent::PrefixWildcard(prefix.to_string()));
            }

            return Err(PatternParseError {
                orig: s.to_string(),
                err: "Path component may only use `*` as the whole component or at the beginning or end".to_string(),
            });
        }

        validate_path_component(s, &format!("/{s}"))?;

        Ok(PathComponent::Exact(s.to_string()))
    }
}

fn validate_path_component(orig: &str, as_whole_path: &str) -> Result<(), PatternParseError> {
    let path_and_query =
        uri::PathAndQuery::from_str(as_whole_path).map_err(|e| PatternParseError {
            orig: orig.to_string(),
            err: format!("Invalid path: {e}"),
        })?;
    if path_and_query.query().is_some() {
        return Err(PatternParseError {
            orig: orig.to_string(),
            err: "Path must not have query parameters".to_string(),
        });
    }
    if path_and_query.path().ne(as_whole_path) {
        return Err(PatternParseError {
            orig: orig.to_string(),
            err: "Path must be normalized".to_string(),
        });
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
    fn matches(&self, s: &str) -> bool {
        match self {
            PathComponent::SingleWildcard | PathComponent::DoubleWildcard => true,
            PathComponent::PrefixWildcard(prefix) => s.starts_with(prefix),
            PathComponent::SuffixWildcard(suffix) => s.ends_with(suffix),
            PathComponent::Exact(expected) => expected == s,
        }
    }
}

#[derive(Clone)]
struct Verb(http::Method);

impl FromStr for Verb {
    type Err = PatternParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self(http::Method::GET)),
            "POST" => Ok(Self(http::Method::POST)),
            "PUT" => Ok(Self(http::Method::PUT)),
            "PATCH" => Ok(Self(http::Method::PATCH)),
            "DELETE" => Ok(Self(http::Method::DELETE)),
            "HEAD" => Ok(Self(http::Method::HEAD)),
            _ => Err(PatternParseError {
                orig: s.to_string(),
                err: "Invalid verb".to_string(),
            }),
        }
    }
}

impl Display for Verb {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
