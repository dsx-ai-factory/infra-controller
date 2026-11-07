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

use std::collections::HashMap;

use serde::Deserialize;

use crate::ConfigValidationError;

/// Maximum number of labels allowed on a resource's metadata.
const MAX_LABELS: usize = 16;

/// Metadata that can get associated with Forge managed resources
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct Metadata {
    /// user-defined resource name
    pub name: String,
    /// optional user-defined resource description
    pub description: String,
    /// optional user-defined key/ value pairs
    pub labels: HashMap<String, String>,
}

impl Metadata {
    pub fn new_with_default_name() -> Self {
        Metadata {
            name: "default_name".to_string(),
            ..Metadata::default()
        }
    }
}

/// default_metadata_for_deserializer returns empty Metadata for serde deserialization of expected device models.
pub fn default_metadata_for_deserializer() -> Metadata {
    Metadata::default()
}

impl Metadata {
    pub fn validate(&self, require_min_length: bool) -> Result<(), ConfigValidationError> {
        let min_len = if require_min_length { 2 } else { 0 };

        if self.name.len() < min_len || self.name.len() > 256 {
            return Err(ConfigValidationError::InvalidValue(format!(
                "Name must be between {} and 256 characters long, got {} characters",
                min_len,
                self.name.len()
            )));
        }

        if !self.name.is_ascii() {
            return Err(ConfigValidationError::InvalidValue(format!(
                "Name '{}' must contain ASCII characters only",
                self.name
            )));
        }

        if self.description.len() > 1024 {
            return Err(ConfigValidationError::InvalidValue(format!(
                "Description must be between 0 and 1024 characters long, got {} characters",
                self.description.len()
            )));
        }

        for (key, value) in &self.labels {
            if !key.is_ascii() {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "Label key '{key}' must contain ASCII characters only"
                )));
            }

            if key.len() > 255 {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "Label key '{key}' is too long (max 255 characters)"
                )));
            }
            if key.is_empty() {
                return Err(ConfigValidationError::InvalidValue(
                    "Label key cannot be empty.".to_string(),
                ));
            }
            if value.len() > 255 {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "Label value '{value}' for key '{key}' is too long (max 255 characters)"
                )));
            }
        }

        if self.labels.len() > MAX_LABELS {
            return Err(ConfigValidationError::InvalidValue(format!(
                "Cannot have more than {} labels, got {}",
                MAX_LABELS,
                self.labels.len()
            )));
        }

        Ok(())
    }
}

/// A single label filter used for searching resources by label key and/or value
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelFilter {
    pub key: String,
    pub value: Option<String>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    /// Build a `Metadata` from parts, with sensible defaults so each row only
    /// names the field it is exercising.
    fn meta(name: &str, description: &str, labels: &[(&str, &str)]) -> Metadata {
        Metadata {
            name: name.to_string(),
            description: description.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// A string of `n` repeated `'a'` characters.
    fn long(n: usize) -> String {
        "a".repeat(n)
    }

    #[test]
    fn validate_with_min_length_required() {
        scenarios!(
            run = |m| m.validate(true).map_err(drop);
            "valid name, description, and label" {
                meta("nice_name", "anything is fine", &[("key1", "val1")]) => Yields(()),
            }

            "no labels is fine" {
                meta("nice_name", "", &[]) => Yields(()),
            }

            "name at min length (2)" {
                meta("ab", "", &[]) => Yields(()),
            }

            "name one below min length (1)" {
                meta("x", "", &[]) => Fails,
            }

            "empty name rejected when min length required" {
                meta("", "", &[]) => Fails,
            }

            "name at max length (256)" {
                Metadata {
                    name: long(256),
                    ..Metadata::default()
                } => Yields(()),
            }

            "name one over max length (257)" {
                Metadata {
                    name: long(257),
                    ..Metadata::default()
                } => Fails,
            }

            "non-ascii name rejected" {
                meta("것봐", "", &[]) => Fails,
            }

            "description at max length (1024)" {
                Metadata {
                    name: "nice name".to_string(),
                    description: long(1024),
                    ..Metadata::default()
                } => Yields(()),
            }

            "description one over max length (1025)" {
                Metadata {
                    name: "nice name".to_string(),
                    description: long(1025),
                    ..Metadata::default()
                } => Fails,
            }

            "empty label key rejected" {
                meta("nice name", "", &[("", "val1")]) => Fails,
            }

            "non-ascii label key rejected" {
                meta("nice name", "", &[("것봐", "val1")]) => Fails,
            }

            "label key at max length (255)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: HashMap::from([(long(255), "val1".to_string())]),
                    ..Metadata::default()
                } => Yields(()),
            }

            "label key one over max length (256)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: HashMap::from([(long(256), "val1".to_string())]),
                    ..Metadata::default()
                } => Fails,
            }

            "label value at max length (255)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: HashMap::from([("key1".to_string(), long(255))]),
                    ..Metadata::default()
                } => Yields(()),
            }

            "label value one over max length (256)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: HashMap::from([("key1".to_string(), long(256))]),
                    ..Metadata::default()
                } => Fails,
            }

            "empty label value is fine" {
                meta("nice name", "", &[("key1", "")]) => Yields(()),
            }

            "labels at max count (16)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: "abcdefghijklmnop"
                        .chars()
                        .map(|c| (c.to_string(), "x".to_string()))
                        .collect(),
                    ..Metadata::default()
                } => Yields(()),
            }

            "labels one over max count (17)" {
                Metadata {
                    name: "nice name".to_string(),
                    labels: "abcdefghijklmnopq"
                        .chars()
                        .map(|c| (c.to_string(), "x".to_string()))
                        .collect(),
                    ..Metadata::default()
                } => Fails,
            }
        );
    }

    #[test]
    fn validate_without_min_length_required() {
        scenarios!(
            run = |m| m.validate(false).map_err(drop);
            "empty name allowed when min length not required" {
                meta("", "anything is fine", &[("key1", "val1")]) => Yields(()),
            }

            "single-char name allowed when min length not required" {
                meta("x", "", &[]) => Yields(()),
            }

            "name still capped at max length (257 rejected)" {
                Metadata {
                    name: long(257),
                    ..Metadata::default()
                } => Fails,
            }

            "non-ascii name still rejected" {
                meta("것봐", "", &[]) => Fails,
            }

            "label checks still apply (empty key rejected)" {
                meta("", "", &[("", "val1")]) => Fails,
            }
        );
    }

    #[test]
    fn validate_error_message_names_the_offending_field() {
        scenarios!(
            run = |(m, tokens)| {
                let message = m.validate(true).unwrap_err().to_string();
                Ok::<_, ()>(tokens.iter().all(|t| message.contains(t)))
            };
            "short-name error mentions length bounds" {
                (meta("x", "", &[]), &["between", "256"][..]) => Yields(true),
            }

            "non-ascii name error mentions ASCII" {
                (meta("것봐", "", &[]), &["ASCII"][..]) => Yields(true),
            }

            "long-description error mentions Description" {
                (
                    Metadata {
                        name: "nice name".to_string(),
                        description: long(1025),
                        ..Metadata::default()
                    },
                    &["Description", "1024"][..],
                ) => Yields(true),
            }

            "empty-key error mentions empty" {
                (meta("nice name", "", &[("", "v")]), &["empty"][..]) => Yields(true),
            }

            "too-many-labels error mentions the count" {
                (
                    Metadata {
                        name: "nice name".to_string(),
                        labels: "abcdefghijklmnopq"
                            .chars()
                            .map(|c| (c.to_string(), "x".to_string()))
                            .collect(),
                        ..Metadata::default()
                    },
                    &["more than 16", "17"][..],
                ) => Yields(true),
            }
        );
    }

    #[test]
    fn constructors_produce_expected_metadata() {
        value_scenarios!(
            run = |m| m;
            "new_with_default_name sets the default name" {
                Metadata::new_with_default_name() => Metadata {
                    name: "default_name".to_string(),
                    description: String::new(),
                    labels: HashMap::new(),
                },
            }

            "default is fully empty" {
                Metadata::default() => Metadata {
                    name: String::new(),
                    description: String::new(),
                    labels: HashMap::new(),
                },
            }

            "deserializer default matches Metadata::default" {
                default_metadata_for_deserializer() => Metadata::default(),
            }
        );
    }
}
