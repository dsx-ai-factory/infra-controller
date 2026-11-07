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

use std::fmt::{Debug, Display};
use std::panic::Location;

use serde::{Deserialize, Serialize};

/// DB storage of the result of a state handler iteration
/// It is different from a StateHandlerOutcome in that it also stores the error message,
/// and does not store the state, which is already stored elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum PersistentStateHandlerOutcome {
    Wait {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_ref: Option<PersistentSourceReference>,
    },
    Error {
        err: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_ref: Option<PersistentSourceReference>,
    },
    Transition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_ref: Option<PersistentSourceReference>,
    },
    DoNothing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_ref: Option<PersistentSourceReference>,
    },
    /// Exists for backward compatibility with DB in case of a race condition with migration.
    /// Remove in future
    DoNothingWithDetails,
}

impl Display for PersistentStateHandlerOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PersistentSourceReference {
    pub file: String,
    pub line: u32,
}

impl Display for PersistentSourceReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl From<&&'static Location<'static>> for PersistentSourceReference {
    fn from(value: &&'static Location) -> Self {
        Self {
            file: value.file().to_string(),
            line: value.line(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    fn source_ref() -> PersistentSourceReference {
        PersistentSourceReference {
            file: "a.rs".to_string(),
            line: 100,
        }
    }

    // Serialize each outcome variant to JSON. The serialized String is the
    // contract, so we yield it directly. serde_json::Error is not PartialEq, so
    // the (unreachable here) failing path would use Fails; every row succeeds.
    // The tag is "outcome" and variants are lowercased, with source_ref omitted
    // when None.
    #[test]
    fn test_state_outcome_serialize() {
        scenarios!(
            run = |outcome| serde_json::to_string(&outcome).map_err(drop);
            "wait with reason, no source ref" {
                PersistentStateHandlerOutcome::Wait {
                    reason: "Reason goes here".to_string(),
                    source_ref: None,
                } => Yields(r#"{"outcome":"wait","reason":"Reason goes here"}"#.to_string()),
            }

            "wait with empty reason" {
                PersistentStateHandlerOutcome::Wait {
                    reason: String::new(),
                    source_ref: None,
                } => Yields(r#"{"outcome":"wait","reason":""}"#.to_string()),
            }

            "wait with reason and source ref" {
                PersistentStateHandlerOutcome::Wait {
                    reason: "waiting".to_string(),
                    source_ref: Some(source_ref()),
                } => Yields(
                    r#"{"outcome":"wait","reason":"waiting","source_ref":{"file":"a.rs","line":100}}"#
                        .to_string(),
                ),
            }

            "error variant, no source ref" {
                PersistentStateHandlerOutcome::Error {
                    err: "boom".to_string(),
                    source_ref: None,
                } => Yields(r#"{"outcome":"error","err":"boom"}"#.to_string()),
            }

            "error variant with source ref" {
                PersistentStateHandlerOutcome::Error {
                    err: "boom".to_string(),
                    source_ref: Some(source_ref()),
                } => Yields(
                    r#"{"outcome":"error","err":"boom","source_ref":{"file":"a.rs","line":100}}"#
                        .to_string(),
                ),
            }

            "transition, no source ref" {
                PersistentStateHandlerOutcome::Transition { source_ref: None } => Yields(r#"{"outcome":"transition"}"#.to_string()),
            }

            "transition with source ref" {
                PersistentStateHandlerOutcome::Transition {
                    source_ref: Some(source_ref()),
                } => Yields(
                    r#"{"outcome":"transition","source_ref":{"file":"a.rs","line":100}}"#
                        .to_string(),
                ),
            }

            "donothing, no source ref" {
                PersistentStateHandlerOutcome::DoNothing { source_ref: None } => Yields(r#"{"outcome":"donothing"}"#.to_string()),
            }

            "donothing with source ref details" {
                PersistentStateHandlerOutcome::DoNothing {
                    source_ref: Some(source_ref()),
                } => Yields(
                    r#"{"outcome":"donothing","source_ref":{"file":"a.rs","line":100}}"#
                        .to_string(),
                ),
            }

            "donothingwithdetails legacy variant" {
                PersistentStateHandlerOutcome::DoNothingWithDetails => Yields(r#"{"outcome":"donothingwithdetails"}"#.to_string()),
            }
        );
    }

    // Deserialize JSON back into the outcome variant (the round-trip targets and
    // the standalone deserialize case). The deserialized type is PartialEq+Debug,
    // so we yield it directly. serde_json::Error is not PartialEq, hence map_err.
    #[test]
    fn test_state_outcome_deserialize() {
        check_cases(
            [
                Case {
                    scenario: "wait round-trip, no source ref",
                    input: r#"{"outcome":"wait","reason":"r"}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Wait {
                        reason: "r".to_string(),
                        source_ref: None,
                    }),
                },
                Case {
                    scenario: "wait with source ref round-trip",
                    input: r#"{"outcome":"wait","reason":"r","source_ref":{"file":"a.rs","line":100}}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Wait {
                        reason: "r".to_string(),
                        source_ref: Some(source_ref()),
                    }),
                },
                Case {
                    scenario: "error variant",
                    input: r#"{"outcome":"error","err":"Error message here"}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Error {
                        err: "Error message here".to_string(),
                        source_ref: None,
                    }),
                },
                Case {
                    scenario: "error with source ref round-trip",
                    input: r#"{"outcome":"error","err":"e","source_ref":{"file":"a.rs","line":100}}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Error {
                        err: "e".to_string(),
                        source_ref: Some(source_ref()),
                    }),
                },
                Case {
                    scenario: "transition round-trip",
                    input: r#"{"outcome":"transition"}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Transition { source_ref: None }),
                },
                Case {
                    scenario: "transition with explicit null source ref (serde default)",
                    input: r#"{"outcome":"transition","source_ref":null}"#,
                    expect: Yields(PersistentStateHandlerOutcome::Transition { source_ref: None }),
                },
                Case {
                    scenario: "donothing, no source ref",
                    input: r#"{"outcome":"donothing"}"#,
                    expect: Yields(PersistentStateHandlerOutcome::DoNothing { source_ref: None }),
                },
                Case {
                    scenario: "donothing with source ref round-trip",
                    input: r#"{"outcome":"donothing","source_ref":{"file":"a.rs","line":100}}"#,
                    expect: Yields(PersistentStateHandlerOutcome::DoNothing {
                        source_ref: Some(source_ref()),
                    }),
                },
                Case {
                    scenario: "donothingwithdetails legacy variant",
                    input: r#"{"outcome":"donothingwithdetails"}"#,
                    expect: Yields(PersistentStateHandlerOutcome::DoNothingWithDetails),
                },
                // Rejected inputs: the error type is not PartialEq, so use Fails.
                Case {
                    scenario: "unknown outcome tag is rejected",
                    input: r#"{"outcome":"bogus"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "missing required reason on wait is rejected",
                    input: r#"{"outcome":"wait"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "missing required err on error is rejected",
                    input: r#"{"outcome":"error"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "missing outcome tag is rejected",
                    input: r#"{"reason":"r"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "uppercase tag is rejected (variants are lowercased)",
                    input: r#"{"outcome":"Transition"}"#,
                    expect: Fails,
                },
                Case {
                    scenario: "malformed json is rejected",
                    input: r#"{"outcome":"transition""#,
                    expect: Fails,
                },
                Case {
                    scenario: "empty string is rejected",
                    input: "",
                    expect: Fails,
                },
            ],
            |json| serde_json::from_str::<PersistentStateHandlerOutcome>(json).map_err(drop),
        );
    }

    // Display for the source reference contains both the file and the line.
    #[test]
    fn test_source_reference_display_tokens() {
        scenarios!(
            run = |(reference, tokens): (PersistentSourceReference, &[&str])| {
                let produced = format!("{reference}");
                Ok::<_, ()>(tokens.iter().all(|t| produced.contains(t)))
            };
            "display carries file and line" {
                (source_ref(), &["a.rs", "100"][..]) => Yields(true),
            }
        );
    }

    // From<&&'static Location> copies the panic location's file and line into a
    // PersistentSourceReference. Location::caller() gives a real location to
    // convert; we assert the line is carried through and the file is non-empty.
    #[test]
    fn test_source_reference_from_location() {
        let location = Location::caller();
        let reference = PersistentSourceReference::from(&location);
        value_scenarios!(
            run = |value| value;
            "line carried from location" {
                reference.line == location.line() => true,
            }

            "file carried from location" {
                reference.file.as_str() == location.file() => true,
            }
        );
    }
}
