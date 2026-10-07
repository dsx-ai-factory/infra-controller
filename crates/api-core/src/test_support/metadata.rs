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

/// Returns a set of testcases in order to check whether validating Metadata
/// on various Forge objects works as intended.
///
/// The function returns a tuple of
/// 1. Metadata to check
/// 2. Expected part of error message
pub fn invalid_metadata_testcases(
    include_min_length_test_cases: bool,
) -> Vec<(rpc::forge::Metadata, String)> {
    let mut results = [
        (
            // Missing Label Key
            rpc::forge::Metadata {
                name: "abc".to_string(),
                description: "def".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "".to_string(),
                    value: None,
                }],
            },
            "invalid value: Label key cannot be empty.".to_string(),
        ),
        (
            // Duplicated Key
            rpc::forge::Metadata {
                name: "abc".to_string(),
                description: "def".to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: "a".to_string(),
                        value: None,
                    },
                    rpc::forge::Label {
                        key: "a".to_string(),
                        value: Some("other".to_string()),
                    },
                ],
            },
            "label is not valid: Duplicate key found: a".to_string(),
        ),
        (
            // Name is longer than limit
            rpc::forge::Metadata {
                name: String::from_utf8(vec![b'A'; 257]).unwrap(),
                description: "def".to_string(),
                labels: vec![],
            },
            format!(
                "invalid value: Name must be between {} and 256 characters long, got 257 characters",
                if include_min_length_test_cases { 2 } else { 0 }),
        ),
        (
            // Description is longer than limit
            rpc::forge::Metadata {
                name: "name".to_string(),
                description: String::from_utf8(vec![b'A'; 1025]).unwrap(),
                labels: vec![],
            },
            "invalid value: Description must be between 0 and 1024 characters long, got 1025 characters".to_string(),
        ),
        (
            // Invalid Chars in Name
            rpc::forge::Metadata {
                name: "asdf😊".to_string(),
                description: "a\u{211D}".to_string(),
                labels: vec![],
            },
            "invalid value: Name 'asdf😊' must contain ASCII characters only".to_string(),
        ),
        (
            // Overlong Key
            rpc::forge::Metadata {
                name: "aa".to_string(),
                description: "".to_string(),
                labels: vec![rpc::forge::Label {
                    key: concat!(
                        "0123456789012345678901234567890123456789012345678901234567890123456789",
                        "0123456789012345678901234567890123456789012345678901234567890123456789",
                        "0123456789012345678901234567890123456789012345678901234567890123456789",
                        "0123456789012345678901234567890123456789012345678901234567890123456789")
                        .to_string(),
                    value: Some("".to_string()),
                }],
            },
            concat!(
                "invalid value: Label key '",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "' is too long (max 255 characters)",
            ).to_string()
        ),
        (
            // Overlong Value
            rpc::forge::Metadata {
                name: "aa".to_string(),
                description: "".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "abc".to_string(),
                    value: Some(
                        concat!(
                            "0123456789012345678901234567890123456789012345678901234567890123456789",
                            "0123456789012345678901234567890123456789012345678901234567890123456789",
                            "0123456789012345678901234567890123456789012345678901234567890123456789",
                            "0123456789012345678901234567890123456789012345678901234567890123456789")
                            .to_string(),
                    ),
                }],
            },
            concat!(
                "invalid value: Label value '",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "0123456789012345678901234567890123456789012345678901234567890123456789",
                "' for key 'abc' is too long (max 255 characters)"
            ).to_string(),
        ),
        (
            // Maximum of 16 labels
            rpc::forge::Metadata {
                name: "aa".to_string(),
                description: "".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "key1".to_string(),
                    value: Some("value1".to_string()),
                },
                rpc::forge::Label {
                    key: "key2".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key3".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key4".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key5".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key6".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key7".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key8".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key9".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key10".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key11".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key12".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key13".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key14".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key15".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key16".to_string(),
                    value: None,
                },
                rpc::forge::Label {
                    key: "key17".to_string(),
                    value: None,
                },],
            },
            "invalid value: Cannot have more than 16 labels".to_string()
        ),
    ].to_vec();

    if include_min_length_test_cases {
        results.extend([
            (
                // Name is empty
                rpc::forge::Metadata {
                    name: String::new(),
                    description: "def".to_string(),
                    labels: vec![],
                },
                "invalid value: Name must be between 2 and 256 characters long, got 0 characters"
                    .to_string(),
            ),
            (
                // Name is shorter than limit
                rpc::forge::Metadata {
                    name: "A".to_string(),
                    description: "def".to_string(),
                    labels: vec![],
                },
                "invalid value: Name must be between 2 and 256 characters long, got 1 characters"
                    .to_string(),
            ),
        ]);
    }

    results
}
