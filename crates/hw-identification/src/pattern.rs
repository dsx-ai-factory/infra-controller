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

//! Text patterns used by hardware identification rules.

use std::borrow::Cow::{self, Borrowed};
use std::fmt;

use Pattern::{Contains, Exact, Prefix};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// An exact, prefix, or substring match expressed as a single-entry table.
/// Comparisons ignore ASCII case and trim surrounding whitespace from both values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Match the entire value.
    Exact(Cow<'static, str>),
    /// Match the beginning of the value.
    Prefix(Cow<'static, str>),
    /// Match text anywhere in the value.
    Contains(Cow<'static, str>),
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PatternVisitor;

        impl<'de> Visitor<'de> for PatternVisitor {
            type Value = Pattern;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a pattern table with exactly one of exact, prefix, or contains")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pattern, A::Error> {
                let Some((operator, text)) = map.next_entry::<String, Cow<'static, str>>()? else {
                    return Err(de::Error::invalid_length(0, &self));
                };
                if let Some(extra) = map.next_key::<String>()? {
                    return Err(de::Error::custom(format_args!(
                        "pattern must contain exactly one operator, found {operator:?} and {extra:?}"
                    )));
                }
                match operator.as_str() {
                    "exact" => Ok(Exact(text)),
                    "prefix" => Ok(Prefix(text)),
                    "contains" => Ok(Contains(text)),
                    other => Err(de::Error::unknown_variant(
                        other,
                        &["exact", "prefix", "contains"],
                    )),
                }
            }
        }

        // Figment's enum deserializer reads the first table entry without checking for more.
        deserializer.deserialize_map(PatternVisitor)
    }
}

pub(crate) const fn exact(text: &'static str) -> Pattern {
    Pattern::Exact(Borrowed(text))
}

pub(crate) const fn prefix(text: &'static str) -> Pattern {
    Pattern::Prefix(Borrowed(text))
}

pub(crate) const fn contains(text: &'static str) -> Pattern {
    Pattern::Contains(Borrowed(text))
}

impl Pattern {
    pub(crate) fn text(&self) -> &str {
        match self {
            Exact(text) | Prefix(text) | Contains(text) => text,
        }
    }

    pub(crate) fn matches(&self, value: &str) -> bool {
        let text = self.text().trim().as_bytes();
        let value = value.trim().as_bytes();
        match self {
            Exact(_) => value.eq_ignore_ascii_case(text),
            Prefix(_) => value
                .get(..text.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(text)),
            Contains(_) => value
                .windows(text.len())
                .any(|part| part.eq_ignore_ascii_case(text)),
        }
    }
}
