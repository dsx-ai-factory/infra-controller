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

// tests/errors.rs
// Unit tests for error handling throughout the MQTT client library including
// error creation, categorization, and proper error propagation.

use mqttea::errors::{MqtteaClientError, unregistered_type_error};
use prost::DecodeError;
use rumqttc::{ClientError, Disconnect, Request};

// Helper functions to create test errors
fn create_test_connection_error() -> ClientError {
    // Create a mock connection error using Request variant (only 2 variants exist in rumqttc 0.24)
    ClientError::Request(Request::Disconnect(Disconnect))
}

fn create_test_decode_error() -> DecodeError {
    // Deprecation: if they remove DecodeError::new, they hopefully will provide some other way
    // to impl prost::Message.
    #[allow(deprecated)]
    DecodeError::new("Test protobuf decode error")
}

fn create_test_json_error() -> serde_json::Error {
    serde_json::from_str::<i32>("not a number").unwrap_err()
}

fn create_test_yaml_error() -> serde_yaml::Error {
    serde_yaml::from_str::<i32>("{ invalid: yaml: }}}").unwrap_err()
}

// The five category predicates an `MqtteaClientError` answers, captured as one
// value so a row can assert an error's entire categorization at once.
#[derive(Debug, PartialEq, Eq)]
struct Predicates {
    connection: bool,
    deserialization: bool,
    serialization: bool,
    topic: bool,
    registry: bool,
}

impl Predicates {
    fn of(error: &MqtteaClientError) -> Self {
        Self {
            connection: error.is_connection_error(),
            deserialization: error.is_deserialization_error(),
            serialization: error.is_serialization_error(),
            topic: error.is_topic_error(),
            registry: error.is_registry_error(),
        }
    }
}

// `MqtteaClientError` must stay `Send + Sync` so async callers can hold it across
// `.await` points and move it between tasks; a `!Send` / `!Sync` field would
// silently break them, so guard the bound at compile time.
#[test]
fn error_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MqtteaClientError>();
}

/// Concern (a): `from`-conversion and categorization. Each row builds an error —
/// via the `From` impl where one exists, otherwise via a constructor or variant
/// literal — and asserts the full set of `is_*` category predicates it answers.
/// Because each predicate is `matches!` over a fixed variant set, asserting the
/// whole `Predicates` vector pins down both the conversion target variant and
/// its category. Folds the old `from`-conversion and `categorization` tests.
#[test]
fn error_categorization_predicates() {
    use carbide_test_support::{Check, check_values};

    // Shorthand: only the named categories are true.
    fn only(connection: bool, deserialization: bool, serialization: bool) -> Predicates {
        Predicates {
            connection,
            deserialization,
            serialization,
            topic: false,
            registry: false,
        }
    }

    check_values(
        [
            // From-conversions land on the expected variant/category.
            Check {
                scenario: "from ClientError -> connection",
                input: MqtteaClientError::from(create_test_connection_error()),
                expect: only(true, false, false),
            },
            Check {
                scenario: "from DecodeError -> protobuf deserialization",
                input: MqtteaClientError::from(create_test_decode_error()),
                expect: only(false, true, false),
            },
            Check {
                scenario: "from serde_json::Error -> json serialization",
                input: MqtteaClientError::from(create_test_json_error()),
                expect: only(false, false, true),
            },
            Check {
                scenario: "from serde_yaml::Error -> yaml serialization",
                input: MqtteaClientError::from(create_test_yaml_error()),
                expect: only(false, false, true),
            },
            // Deserialization variants without a From impl.
            Check {
                scenario: "json deserialization",
                input: MqtteaClientError::JsonDeserializationError(create_test_json_error()),
                expect: only(false, true, false),
            },
            Check {
                scenario: "yaml deserialization",
                input: MqtteaClientError::YamlDeserializationError(create_test_yaml_error()),
                expect: only(false, true, false),
            },
            // Topic category.
            Check {
                scenario: "unknown message type is a topic error",
                input: MqtteaClientError::unknown_message_type("/pets/lizard/unknown"),
                expect: Predicates {
                    topic: true,
                    ..only(false, false, false)
                },
            },
            Check {
                scenario: "topic parsing is a topic error",
                input: MqtteaClientError::topic_parsing_error("Bad topic format"),
                expect: Predicates {
                    topic: true,
                    ..only(false, false, false)
                },
            },
            // Registry category.
            Check {
                scenario: "unregistered type is a registry error",
                input: MqtteaClientError::unregistered_type("UnknownType"),
                expect: Predicates {
                    registry: true,
                    ..only(false, false, false)
                },
            },
            Check {
                scenario: "pattern compilation is a registry error",
                input: MqtteaClientError::pattern_compilation_error("Bad regex"),
                expect: Predicates {
                    registry: true,
                    ..only(false, false, false)
                },
            },
            // Raw message belongs to no category.
            Check {
                scenario: "raw message belongs to no category",
                input: MqtteaClientError::raw_message_error("Failed to process bird song data"),
                expect: only(false, false, false),
            },
        ],
        |error| Predicates::of(&error),
    );
}

// One row of the Display/Debug/source table (concern b). Rows carry several
// expected values (substrings, a source presence flag), so this follows the
// local-named-struct convention rather than an equality table.
struct RenderCase {
    scenario: &'static str,
    error: MqtteaClientError,
    /// Substrings the `Display` rendering must contain.
    display_contains: &'static [&'static str],
    /// Whether `Error::source` should yield an underlying cause.
    has_source: bool,
}

/// Concern (b): `Display` rendering, `Debug` rendering, and error `source`.
/// Each row asserts that an error renders with the expected human-readable
/// fragments and reports a source iff it wraps an inner error. Folds the old
/// `display`, `debug_format`, `source`, owned-values, special-characters, and
/// the display halves of the deserialization-creation tests.
#[test]
fn error_display_debug_and_source() {
    let cases = [
        RenderCase {
            scenario: "connection",
            error: MqtteaClientError::ConnectionError(create_test_connection_error()),
            // Specific inner message depends on rumqttc internals.
            display_contains: &["MQTT connection error"],
            has_source: true,
        },
        RenderCase {
            scenario: "protobuf deserialization has a source",
            error: MqtteaClientError::ProtobufDeserializationError(create_test_decode_error()),
            display_contains: &["protobuf deserialization error"],
            has_source: true,
        },
        RenderCase {
            scenario: "json deserialization",
            error: MqtteaClientError::JsonDeserializationError(create_test_json_error()),
            display_contains: &["JSON deserialization error"],
            has_source: true,
        },
        RenderCase {
            scenario: "yaml deserialization",
            error: MqtteaClientError::YamlDeserializationError(create_test_yaml_error()),
            display_contains: &["YAML deserialization error"],
            has_source: true,
        },
        RenderCase {
            scenario: "json serialization has a source",
            error: MqtteaClientError::JsonSerializationError(create_test_json_error()),
            display_contains: &["JSON serialization error"],
            has_source: true,
        },
        RenderCase {
            scenario: "yaml serialization has a source",
            error: MqtteaClientError::YamlSerializationError(create_test_yaml_error()),
            display_contains: &["YAML serialization error"],
            has_source: true,
        },
        RenderCase {
            scenario: "unknown message type echoes the topic",
            error: MqtteaClientError::unknown_message_type("/pets/parrot/songs"),
            display_contains: &["unknown message type", "/pets/parrot/songs"],
            has_source: false,
        },
        RenderCase {
            scenario: "topic parsing echoes the message",
            error: MqtteaClientError::topic_parsing_error("topic must start with /"),
            display_contains: &["topic parsing error", "topic must start with /"],
            has_source: false,
        },
        RenderCase {
            scenario: "raw message echoes the message",
            error: MqtteaClientError::raw_message_error("Failed to decode turtle sensor data"),
            display_contains: &["raw message error", "turtle sensor data"],
            has_source: false,
        },
        RenderCase {
            scenario: "unregistered type echoes the type name",
            error: MqtteaClientError::unregistered_type("FishMessage"),
            display_contains: &["type not registered", "FishMessage"],
            has_source: false,
        },
        RenderCase {
            scenario: "invalid utf8 echoes the message",
            error: MqtteaClientError::invalid_utf8("Contains invalid UTF-8 bytes"),
            display_contains: &["invalid UTF-8", "invalid UTF-8 bytes"],
            has_source: false,
        },
        RenderCase {
            scenario: "pattern compilation echoes the message",
            error: MqtteaClientError::pattern_compilation_error("Missing closing bracket in regex"),
            display_contains: &["pattern compilation error", "closing bracket"],
            has_source: false,
        },
        RenderCase {
            scenario: "display preserves special characters",
            error: MqtteaClientError::unknown_message_type("/pets/🐱/data/emoji-test"),
            display_contains: &["🐱", "emoji-test"],
            has_source: false,
        },
    ];

    for case in cases {
        let RenderCase {
            scenario,
            error,
            display_contains,
            has_source,
        } = case;

        let display = format!("{error}");
        for fragment in display_contains {
            assert!(
                display.contains(fragment),
                "{scenario}: Display {display:?} should contain {fragment:?}"
            );
        }

        assert_eq!(
            std::error::Error::source(&error).is_some(),
            has_source,
            "{scenario}: source presence"
        );
    }

    // Debug rendering names the variant and echoes its payload.
    let debug_error = MqtteaClientError::unknown_message_type("/debug/test");
    let debug = format!("{debug_error:?}");
    assert!(
        debug.contains("UnknownMessageType"),
        "debug names the variant"
    );
    assert!(debug.contains("/debug/test"), "debug echoes the payload");
}

// Tests for unregistered_type_error helper function
#[test]
fn test_unregistered_type_error_function() {
    let error = unregistered_type_error::<String>();

    match error {
        MqtteaClientError::UnregisteredType(type_name) => {
            assert!(type_name.contains("String"));
        }
        _ => panic!("Should be UnregisteredType"),
    }
}

#[test]
fn test_unregistered_type_error_custom_type() {
    #[derive(Debug)]
    struct CustomAnimalMessage;

    let error = unregistered_type_error::<CustomAnimalMessage>();

    match error {
        MqtteaClientError::UnregisteredType(type_name) => {
            assert!(type_name.contains("CustomAnimalMessage"));
        }
        _ => panic!("Should be UnregisteredType"),
    }
}

// Tests for error default implementation
#[test]
fn test_error_default() {
    let default_error = MqtteaClientError::default();

    match default_error {
        MqtteaClientError::UnregisteredType(type_name) => {
            assert_eq!(type_name, "unknown");
        }
        _ => panic!("Should be default UnregisteredType"),
    }
}

// Test error with very long messages (edge case): the payload round-trips
// untruncated and Display renders the whole thing.
#[test]
fn test_error_with_long_message() {
    let long_topic = "/pets/".to_string() + &"a".repeat(10_000) + "/data";
    let error = MqtteaClientError::unknown_message_type(long_topic.clone());

    match error {
        MqtteaClientError::UnknownMessageType(ref topic) => {
            assert_eq!(topic.len(), long_topic.len());
            assert!(topic.starts_with("/pets/aa"));
            assert!(topic.ends_with("/data"));
        }
        _ => panic!("Should be UnknownMessageType"),
    }

    // Should be able to display even very long errors
    let display = format!("{error}");
    assert!(display.len() > 1000);
}
