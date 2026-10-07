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

// src/error.rs
// Error types for mlxconfig-profile. Includes various
// implementations for working with error types across
// the other mlxconfig-* crates.

use thiserror::Error;

use crate::runner::error::MlxRunnerError;
use crate::variables::value::MlxValueError;

#[derive(Debug, Error)]
pub enum MlxProfileError {
    // RegistryNotFound is returned when a registry configured
    // to be used with the profile is not found.
    #[error("registry '{registry_name}' not found in available registries")]
    RegistryNotFound { registry_name: String },

    // VariableNotFound is returned when a mapped
    // variable for the profile is not found in
    // the configured registry.
    #[error("variable '{variable_name}' not found in registry '{registry_name}'")]
    VariableNotFound {
        variable_name: String,
        registry_name: String,
    },

    // ValueValidation is returned when a given MlxConfigValue
    // fails validation. Generally speaking this shouldn't really
    // happen, unless someone hand-creates a value outside of
    // the constructor.
    #[error("value validation failed for variable '{variable_name}': {error}")]
    ValueValidation {
        variable_name: String,
        error: MlxValueError,
    },

    // ProfileValidation is returned when validation of the
    // profile fails, which is likely when validation of
    // a value within the profile fails. Again, it shouldn't
    // really happen, but it's good to check just incase!
    #[error("profile validation failed: {message}")]
    ProfileValidation { message: String },

    // Serialization is returned when there is a serialization
    // error while attempting to serialize the profile out to
    // JSON or YAML.
    #[error("serialization error: {error}")]
    Serialization { error: String },

    // YamlParsing is returned when there is an error parsing
    // a profile (as YAML) to deserialize back into a profile.
    #[error("YAML parsing error: {error}")]
    YamlParsing { error: serde_yaml::Error },

    // JsonParsing is returned when there is an error parsing
    // a profile (as JSON) to deserialize back into a profile.
    #[error("JSON parsing error: {error}")]
    JsonParsing { error: serde_json::Error },

    #[error("TOML parsing error: {error}")]
    TomlParsing { error: toml::de::Error },

    // Runner is returned when the underlying mlxconfig-runner
    // returns an error while trying to sync or compare.
    #[error("MLX runner error: {error}")]
    Runner { error: MlxRunnerError },

    // Io is returned for a general I/O error.
    #[error("I/O error: {error}")]
    Io { error: std::io::Error },
}

impl From<toml::de::Error> for MlxProfileError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlParsing { error }
    }
}

impl MlxProfileError {
    // registry_not_found creates a registry not found error.
    pub fn registry_not_found<T: Into<String>>(registry_name: T) -> Self {
        Self::RegistryNotFound {
            registry_name: registry_name.into(),
        }
    }

    // variable_not_found creates a variable not found error.
    pub fn variable_not_found<T: Into<String>, R: Into<String>>(
        variable_name: T,
        registry_name: R,
    ) -> Self {
        Self::VariableNotFound {
            variable_name: variable_name.into(),
            registry_name: registry_name.into(),
        }
    }

    // value_validation creates a value validation error.
    pub fn value_validation<T: Into<String>>(variable_name: T, error: MlxValueError) -> Self {
        Self::ValueValidation {
            variable_name: variable_name.into(),
            error,
        }
    }

    // profile_validation creates a profile validation error.
    pub fn profile_validation<T: Into<String>>(message: T) -> Self {
        Self::ProfileValidation {
            message: message.into(),
        }
    }

    // serialization creates a serialization error.
    pub fn serialization<T: Into<String>>(error: T) -> Self {
        Self::Serialization {
            error: error.into(),
        }
    }
}

impl From<MlxRunnerError> for MlxProfileError {
    fn from(error: MlxRunnerError) -> Self {
        Self::Runner { error }
    }
}

impl From<MlxValueError> for MlxProfileError {
    fn from(error: MlxValueError) -> Self {
        Self::ValueValidation {
            variable_name: "unknown".to_string(),
            error,
        }
    }
}

impl From<serde_yaml::Error> for MlxProfileError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::YamlParsing { error }
    }
}

impl From<serde_json::Error> for MlxProfileError {
    fn from(error: serde_json::Error) -> Self {
        Self::JsonParsing { error }
    }
}

impl From<std::io::Error> for MlxProfileError {
    fn from(error: std::io::Error) -> Self {
        Self::Io { error }
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, scenarios, value_scenarios};

    use super::*;

    // discriminant maps an MlxProfileError to a stable &'static str tag so
    // tests can assert which variant was produced without needing PartialEq
    // (several variants wrap non-PartialEq errors like serde_json::Error).
    fn discriminant(error: &MlxProfileError) -> &'static str {
        match error {
            MlxProfileError::RegistryNotFound { .. } => "RegistryNotFound",
            MlxProfileError::VariableNotFound { .. } => "VariableNotFound",
            MlxProfileError::ValueValidation { .. } => "ValueValidation",
            MlxProfileError::ProfileValidation { .. } => "ProfileValidation",
            MlxProfileError::Serialization { .. } => "Serialization",
            MlxProfileError::YamlParsing { .. } => "YamlParsing",
            MlxProfileError::JsonParsing { .. } => "JsonParsing",
            MlxProfileError::TomlParsing { .. } => "TomlParsing",
            MlxProfileError::Runner { .. } => "Runner",
            MlxProfileError::Io { .. } => "Io",
        }
    }

    // The constructors each build one specific variant. Project to the
    // discriminant so we verify the constructor selects the right arm,
    // independent of the (non-PartialEq) payloads.
    #[test]
    fn constructors_select_the_right_variant() {
        value_scenarios!(
            run = |error| discriminant(&error);
            "registry_not_found -> RegistryNotFound" {
                MlxProfileError::registry_not_found("reg") => "RegistryNotFound",
            }

            "variable_not_found -> VariableNotFound" {
                MlxProfileError::variable_not_found("var", "reg") => "VariableNotFound",
            }

            "value_validation -> ValueValidation" {
                MlxProfileError::value_validation(
                    "var",
                    MlxValueError::TypeMismatch {
                        expected: "Integer".to_string(),
                        got: "bool".to_string(),
                    },
                ) => "ValueValidation",
            }

            "profile_validation -> ProfileValidation" {
                MlxProfileError::profile_validation("bad") => "ProfileValidation",
            }

            "serialization -> Serialization" {
                MlxProfileError::serialization("boom") => "Serialization",
            }
        );
    }

    // The Display strings for the hand-built variants are fixed format
    // strings, so we can pin them exactly. The ValueValidation row also
    // exercises the nested MlxValueError Display ("Type mismatch: ...").
    #[test]
    fn constructors_render_their_display_strings() {
        value_scenarios!(
            run = |error| error.to_string();
            "RegistryNotFound Display" {
                MlxProfileError::registry_not_found("my_registry") => "registry 'my_registry' not found in available registries".to_string(),
            }

            "VariableNotFound Display" {
                MlxProfileError::variable_not_found("my_var", "my_registry") => "variable 'my_var' not found in registry 'my_registry'".to_string(),
            }

            "ValueValidation Display nests the MlxValueError" {
                MlxProfileError::value_validation(
                    "my_var",
                    MlxValueError::TypeMismatch {
                        expected: "Integer".to_string(),
                        got: "bool".to_string(),
                    },
                ) => "value validation failed for variable 'my_var': \
                     Type mismatch: expected Integer, got bool"
                .to_string(),
            }

            "ProfileValidation Display" {
                MlxProfileError::profile_validation("something went wrong") => "profile validation failed: something went wrong".to_string(),
            }

            "Serialization Display" {
                MlxProfileError::serialization("could not serialize") => "serialization error: could not serialize".to_string(),
            }
        );
    }

    // The generic Into<String> bounds accept both &str and String; confirm
    // both flavors flow through the constructors and land in Display.
    #[test]
    fn constructors_accept_str_and_string() {
        value_scenarios!(
            run = |error| error.to_string();
            "registry_not_found from &str" {
                MlxProfileError::registry_not_found("a") => "registry 'a' not found in available registries".to_string(),
            }

            "registry_not_found from String" {
                MlxProfileError::registry_not_found("b".to_string()) => "registry 'b' not found in available registries".to_string(),
            }

            "profile_validation from String" {
                MlxProfileError::profile_validation("msg".to_string()) => "profile validation failed: msg".to_string(),
            }
        );
    }

    // Each From impl must select its matching variant. We build a real
    // wrapped error per source type, convert via Into, and assert the
    // resulting discriminant (the wrapped payloads are not PartialEq).
    #[test]
    fn from_impls_select_the_right_variant() {
        value_scenarios!(
            run = |error| discriminant(&error);
            "MlxValueError -> ValueValidation" {
                MlxProfileError::from(MlxValueError::ReadOnlyVariable {
                    variable_name: "ro".to_string(),
                }) => "ValueValidation",
            }

            "MlxRunnerError -> Runner" {
                MlxProfileError::from(MlxRunnerError::NoDeviceFound) => "Runner",
            }

            "std::io::Error -> Io" {
                MlxProfileError::from(std::io::Error::other("disk gone")) => "Io",
            }

            "serde_json::Error -> JsonParsing" {
                MlxProfileError::from(
                    serde_json::from_str::<i32>("not json").unwrap_err(),
                ) => "JsonParsing",
            }

            "serde_yaml::Error -> YamlParsing" {
                MlxProfileError::from(
                    serde_yaml::from_str::<i32>("{ : : invalid").unwrap_err(),
                ) => "YamlParsing",
            }

            "toml::de::Error -> TomlParsing" {
                MlxProfileError::from(
                    toml::from_str::<toml::Value>("= broken").unwrap_err(),
                ) => "TomlParsing",
            }
        );
    }

    // The From<MlxValueError> impl tags the variable name as "unknown" and
    // nests the inner Display; pin the full rendered string to lock that
    // contract in place.
    #[test]
    fn from_value_error_uses_unknown_variable_name() {
        Check {
            scenario: "From<MlxValueError> Display uses 'unknown'",
            input: MlxProfileError::from(MlxValueError::PresetOutOfRange {
                value: 9,
                max_allowed: 5,
            }),
            expect: "value validation failed for variable 'unknown': \
                     Preset value 9 exceeds maximum 5"
                .to_string(),
        }
        .check(|error| error.to_string());
    }

    // The `?`-style conversion path: a function returning MlxProfileError
    // that uses `?` on a fallible toml parse must surface TomlParsing. This
    // exercises the From<toml::de::Error> impl through real `?` desugaring.
    #[test]
    fn question_mark_conversions_propagate_the_right_variant() {
        fn parse_toml(raw: &str) -> Result<toml::Value, MlxProfileError> {
            let value: toml::Value = toml::from_str(raw)?;
            Ok(value)
        }

        fn parse_json(raw: &str) -> Result<serde_json::Value, MlxProfileError> {
            let value: serde_json::Value = serde_json::from_str(raw)?;
            Ok(value)
        }

        scenarios!(
            run = |raw| {
                parse_toml(raw)
                    .map(|_| "TomlParsing-or-ok")
                    .map_err(|error| discriminant(&error))
            };
            "valid toml yields" {
                "key = 1" => Yields("TomlParsing-or-ok"),
            }

            "invalid toml fails as TomlParsing" {
                "= nope" => FailsWith("TomlParsing"),
            }
        );

        scenarios!(
            run = |raw| {
                parse_json(raw)
                    .map(|_| "JsonParsing-or-ok")
                    .map_err(|error| discriminant(&error))
            };
            "valid json yields" {
                "{}" => Yields("JsonParsing-or-ok"),
            }

            "invalid json fails as JsonParsing" {
                "not json" => FailsWith("JsonParsing"),
            }
        );
    }
}
