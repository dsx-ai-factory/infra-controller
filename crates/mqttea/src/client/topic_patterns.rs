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

// src/client/topic_patterns.rs
// Flexible topic pattern input handling for registration methods.
//
// Provides the TopicPatterns enum and From implementations to allow users
// to pass topics in many convenient formats without manual conversions.

// TopicPatterns provides flexible input handling for topic registration methods.
// Accepts single topics, multiple topics, string literals, owned strings, etc.
#[derive(Debug, Clone, PartialEq)]
pub enum TopicPatterns {
    // Single pattern.
    Single(String),
    // Multiple patterns for one message type.
    Multiple(Vec<String>),
}

impl TopicPatterns {
    // into_vec converts any TopicPatterns variant to Vec<String>.
    // Used internally by registration methods to normalize input.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(pattern) => vec![pattern],
            Self::Multiple(patterns) => patterns,
        }
    }

    // len returns the number of patterns contained.
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Multiple(patterns) => patterns.len(),
        }
    }

    // is_empty checks if there are any patterns.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Single(pattern) => pattern.is_empty(),
            Self::Multiple(patterns) => {
                patterns.is_empty() || patterns.iter().all(|p| p.is_empty())
            }
        }
    }

    // contains checks if a specific pattern is included.
    pub fn contains(&self, pattern: &str) -> bool {
        match self {
            Self::Single(p) => p == pattern,
            Self::Multiple(patterns) => patterns.iter().any(|p| p == pattern),
        }
    }

    // as_slice returns a slice view of all patterns.
    pub fn as_slice(&self) -> Vec<&str> {
        match self {
            Self::Single(pattern) => vec![pattern.as_str()],
            Self::Multiple(patterns) => patterns.iter().map(|s| s.as_str()).collect(),
        }
    }

    // from_single creates TopicPatterns from a single pattern.
    pub fn from_single(pattern: impl Into<String>) -> Self {
        Self::Single(pattern.into())
    }

    // from_multiple creates TopicPatterns from multiple patterns.
    pub fn from_multiple(patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Multiple(patterns.into_iter().map(|p| p.into()).collect())
    }
}

// Convert string literal to TopicPatterns.
// register_message("hello-world")
impl From<&str> for TopicPatterns {
    fn from(pattern: &str) -> Self {
        Self::Single(pattern.to_string())
    }
}

// Convert owned String to TopicPatterns.
// register_message(some_string_var)
impl From<String> for TopicPatterns {
    fn from(pattern: String) -> Self {
        Self::Single(pattern)
    }
}

// Convert Vec<&str> to TopicPatterns.
// register_message(vec!["hello", "hi", "greeting"])
impl From<Vec<&str>> for TopicPatterns {
    fn from(patterns: Vec<&str>) -> Self {
        Self::Multiple(patterns.into_iter().map(String::from).collect())
    }
}

// Convert Vec<String> to TopicPatterns.
// register_message(my_string_vec)
impl From<Vec<String>> for TopicPatterns {
    fn from(patterns: Vec<String>) -> Self {
        Self::Multiple(patterns)
    }
}

// Convert array of string literals to TopicPatterns.
// register_message(["hello", "hi", "greeting"])
impl<const N: usize> From<[&str; N]> for TopicPatterns {
    fn from(patterns: [&str; N]) -> Self {
        Self::Multiple(patterns.into_iter().map(String::from).collect())
    }
}

// Convert array of Strings to TopicPatterns.
// register_message([string1, string2, string3])
impl<const N: usize> From<[String; N]> for TopicPatterns {
    fn from(patterns: [String; N]) -> Self {
        Self::Multiple(patterns.into_iter().collect())
    }
}

// Convert slice of string literals to TopicPatterns.
// register_message(&["hello", "hi"])
impl From<&[&str]> for TopicPatterns {
    fn from(patterns: &[&str]) -> Self {
        Self::Multiple(patterns.iter().map(|s| s.to_string()).collect())
    }
}

// Convert slice of Strings to TopicPatterns.
// register_message(&[string1, string2])
impl From<&[String]> for TopicPatterns {
    fn from(patterns: &[String]) -> Self {
        Self::Multiple(patterns.to_vec())
    }
}

impl std::fmt::Display for TopicPatterns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(pattern) => write!(f, "'{pattern}'"),
            Self::Multiple(patterns) => {
                write!(
                    f,
                    "[{}]",
                    patterns
                        .iter()
                        .map(|p| format!("'{p}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[derive(Clone, Copy)]
    enum PatternSource {
        BorrowedStr,
        OwnedString,
        VecBorrowed,
        VecOwned,
        ArrayBorrowed,
        ArrayOwned,
        SliceBorrowed,
        SliceOwned,
        FromSingle,
        FromMultiple,
        EmptySingle,
        EmptyMultiple,
        AllEmptyMultiple,
        MixedEmptyMultiple,
        SingleLevelWildcard,
        MultiLevelWildcard,
    }

    #[derive(Debug, PartialEq)]
    struct PatternSummary {
        patterns: Vec<String>,
        len: usize,
        is_empty: bool,
        contains_alpha: bool,
        contains_beta: bool,
        contains_single_level_wildcard: bool,
        contains_multi_level_wildcard: bool,
        as_slice: Vec<String>,
        display: String,
    }

    fn patterns_from(source: PatternSource) -> TopicPatterns {
        match source {
            PatternSource::BorrowedStr => TopicPatterns::from("alpha"),
            PatternSource::OwnedString => TopicPatterns::from(String::from("alpha")),
            PatternSource::VecBorrowed => TopicPatterns::from(vec!["alpha", "beta"]),
            PatternSource::VecOwned => {
                TopicPatterns::from(vec![String::from("alpha"), String::from("beta")])
            }
            PatternSource::ArrayBorrowed => TopicPatterns::from(["alpha", "beta"]),
            PatternSource::ArrayOwned => {
                TopicPatterns::from([String::from("alpha"), String::from("beta")])
            }
            PatternSource::SliceBorrowed => TopicPatterns::from(["alpha", "beta"].as_slice()),
            PatternSource::SliceOwned => {
                let patterns = [String::from("alpha"), String::from("beta")];
                TopicPatterns::from(patterns.as_slice())
            }
            PatternSource::FromSingle => TopicPatterns::from_single("alpha"),
            PatternSource::FromMultiple => TopicPatterns::from_multiple(["alpha", "beta"]),
            PatternSource::EmptySingle => TopicPatterns::from(""),
            PatternSource::EmptyMultiple => TopicPatterns::from(Vec::<String>::new()),
            PatternSource::AllEmptyMultiple => TopicPatterns::from(["", ""]),
            PatternSource::MixedEmptyMultiple => TopicPatterns::from(["", "alpha"]),
            PatternSource::SingleLevelWildcard => TopicPatterns::from("sensors/+/temp"),
            PatternSource::MultiLevelWildcard => TopicPatterns::from("sensors/#"),
        }
    }

    fn summarize(patterns: TopicPatterns) -> PatternSummary {
        let len = patterns.len();
        let is_empty = patterns.is_empty();
        let contains_alpha = patterns.contains("alpha");
        let contains_beta = patterns.contains("beta");
        let contains_single_level_wildcard = patterns.contains("sensors/+/temp");
        let contains_multi_level_wildcard = patterns.contains("sensors/#");
        let as_slice = patterns
            .as_slice()
            .iter()
            .copied()
            .map(str::to_string)
            .collect();
        let display = patterns.to_string();
        let patterns = patterns.into_vec();

        PatternSummary {
            patterns,
            len,
            is_empty,
            contains_alpha,
            contains_beta,
            contains_single_level_wildcard,
            contains_multi_level_wildcard,
            as_slice,
            display,
        }
    }

    fn single_alpha() -> PatternSummary {
        PatternSummary {
            patterns: vec!["alpha".to_string()],
            len: 1,
            is_empty: false,
            contains_alpha: true,
            contains_beta: false,
            contains_single_level_wildcard: false,
            contains_multi_level_wildcard: false,
            as_slice: vec!["alpha".to_string()],
            display: "'alpha'".to_string(),
        }
    }

    fn alpha_beta() -> PatternSummary {
        PatternSummary {
            patterns: vec!["alpha".to_string(), "beta".to_string()],
            len: 2,
            is_empty: false,
            contains_alpha: true,
            contains_beta: true,
            contains_single_level_wildcard: false,
            contains_multi_level_wildcard: false,
            as_slice: vec!["alpha".to_string(), "beta".to_string()],
            display: "['alpha', 'beta']".to_string(),
        }
    }

    #[test]
    fn test_topic_patterns_sources() {
        check_values(
            [
                Check {
                    scenario: "borrowed str",
                    input: PatternSource::BorrowedStr,
                    expect: single_alpha(),
                },
                Check {
                    scenario: "owned string",
                    input: PatternSource::OwnedString,
                    expect: single_alpha(),
                },
                Check {
                    scenario: "vec borrowed",
                    input: PatternSource::VecBorrowed,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "vec owned",
                    input: PatternSource::VecOwned,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "array borrowed",
                    input: PatternSource::ArrayBorrowed,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "array owned",
                    input: PatternSource::ArrayOwned,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "slice borrowed",
                    input: PatternSource::SliceBorrowed,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "slice owned",
                    input: PatternSource::SliceOwned,
                    expect: alpha_beta(),
                },
                Check {
                    scenario: "from_single",
                    input: PatternSource::FromSingle,
                    expect: single_alpha(),
                },
                Check {
                    scenario: "from_multiple",
                    input: PatternSource::FromMultiple,
                    expect: alpha_beta(),
                },
                // TopicPatterns stores MQTT wildcards as literal patterns; it
                // does not evaluate topic-match semantics.
                Check {
                    scenario: "single-level wildcard",
                    input: PatternSource::SingleLevelWildcard,
                    expect: PatternSummary {
                        patterns: vec!["sensors/+/temp".to_string()],
                        len: 1,
                        is_empty: false,
                        contains_alpha: false,
                        contains_beta: false,
                        contains_single_level_wildcard: true,
                        contains_multi_level_wildcard: false,
                        as_slice: vec!["sensors/+/temp".to_string()],
                        display: "'sensors/+/temp'".to_string(),
                    },
                },
                Check {
                    scenario: "multi-level wildcard",
                    input: PatternSource::MultiLevelWildcard,
                    expect: PatternSummary {
                        patterns: vec!["sensors/#".to_string()],
                        len: 1,
                        is_empty: false,
                        contains_alpha: false,
                        contains_beta: false,
                        contains_single_level_wildcard: false,
                        contains_multi_level_wildcard: true,
                        as_slice: vec!["sensors/#".to_string()],
                        display: "'sensors/#'".to_string(),
                    },
                },
            ],
            |source| summarize(patterns_from(source)),
        );
    }

    #[test]
    fn test_topic_patterns_empty_cases() {
        check_values(
            [
                Check {
                    // `Single("")` still has one stored pattern, but is empty by content.
                    scenario: "empty single reports empty by content",
                    input: PatternSource::EmptySingle,
                    expect: PatternSummary {
                        patterns: vec![String::new()],
                        len: 1,
                        is_empty: true,
                        contains_alpha: false,
                        contains_beta: false,
                        contains_single_level_wildcard: false,
                        contains_multi_level_wildcard: false,
                        as_slice: vec![String::new()],
                        display: "''".to_string(),
                    },
                },
                Check {
                    scenario: "empty multiple",
                    input: PatternSource::EmptyMultiple,
                    expect: PatternSummary {
                        patterns: vec![],
                        len: 0,
                        is_empty: true,
                        contains_alpha: false,
                        contains_beta: false,
                        contains_single_level_wildcard: false,
                        contains_multi_level_wildcard: false,
                        as_slice: vec![],
                        display: "[]".to_string(),
                    },
                },
                Check {
                    scenario: "all empty multiple",
                    input: PatternSource::AllEmptyMultiple,
                    expect: PatternSummary {
                        patterns: vec![String::new(), String::new()],
                        len: 2,
                        is_empty: true,
                        contains_alpha: false,
                        contains_beta: false,
                        contains_single_level_wildcard: false,
                        contains_multi_level_wildcard: false,
                        as_slice: vec![String::new(), String::new()],
                        display: "['', '']".to_string(),
                    },
                },
                Check {
                    scenario: "mixed empty multiple",
                    input: PatternSource::MixedEmptyMultiple,
                    expect: PatternSummary {
                        patterns: vec![String::new(), "alpha".to_string()],
                        len: 2,
                        is_empty: false,
                        contains_alpha: true,
                        contains_beta: false,
                        contains_single_level_wildcard: false,
                        contains_multi_level_wildcard: false,
                        as_slice: vec![String::new(), "alpha".to_string()],
                        display: "['', 'alpha']".to_string(),
                    },
                },
            ],
            |source| summarize(patterns_from(source)),
        );
    }
}
