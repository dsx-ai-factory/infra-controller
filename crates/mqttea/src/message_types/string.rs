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
// src/message_types/string.rs
// StringMessage provides a simple wrapper around String that
// implements RawMessageType, enabling direct sending of string
// messages without complex serialization.

use crate::traits::RawMessageType;

// StringMessage stores a simple text string for MQTT transmission.
// This allows for easy sending of plain text messages using the client's
// send_message method without needing protobuf or JSON serialization.
#[derive(Clone, Debug, PartialEq)]
pub struct StringMessage {
    // content is used for storing the actual text content
    // of the message.
    pub content: String,
}

impl StringMessage {
    // new creates a new StringMessage with the given content
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    // as_str returns the content as a string slice
    pub fn as_str(&self) -> &str {
        &self.content
    }

    // into_string consumes the StringMessage and
    // returns the inner String.
    pub fn into_string(self) -> String {
        self.content
    }
}

impl RawMessageType for StringMessage {
    // to_bytes converts the string content to UTF-8 bytes
    // for transmission
    fn to_bytes(&self) -> Vec<u8> {
        self.content.as_bytes().to_vec()
    }

    // from_bytes recreates a StringMessage from received UTF-8 bytes
    fn from_bytes(bytes: Vec<u8>) -> Self {
        let content = String::from_utf8(bytes).unwrap_or_else(|e| {
            // If invalid UTF-8, include the error info in the content
            format!("Invalid UTF-8 data: {e}")
        });
        Self { content }
    }
}

// Implement From traits.
impl From<String> for StringMessage {
    fn from(content: String) -> Self {
        Self { content }
    }
}

impl From<&str> for StringMessage {
    fn from(content: &str) -> Self {
        Self {
            content: content.to_string(),
        }
    }
}

impl From<StringMessage> for String {
    fn from(msg: StringMessage) -> String {
        msg.content
    }
}

// Implement FromStr trait for parsing from strings.
impl std::str::FromStr for StringMessage {
    type Err = std::convert::Infallible;

    fn from_str(content: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            content: content.to_string(),
        })
    }
}

impl std::fmt::Display for StringMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.content)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy)]
    enum StringSource {
        New,
        FromBorrowed,
        FromOwned,
        FromBytes,
        FromStr,
    }

    #[derive(Clone, Debug, PartialEq)]
    struct StringSummary {
        content: String,
        as_str: String,
        bytes: Vec<u8>,
        display: String,
        into_string: String,
    }

    fn message_from(source: StringSource) -> StringMessage {
        match source {
            StringSource::New => StringMessage::new("hello"),
            StringSource::FromBorrowed => StringMessage::from("hello"),
            StringSource::FromOwned => StringMessage::from(String::from("hello")),
            StringSource::FromBytes => StringMessage::from_bytes(b"hello".to_vec()),
            StringSource::FromStr => StringMessage::from_str("hello").expect("infallible parse"),
        }
    }

    fn summarize(source: StringSource) -> StringSummary {
        let message = message_from(source);
        let content = message.content.clone();
        let as_str = message.as_str().to_string();
        let bytes = message.to_bytes();
        let display = message.to_string();
        let into_string = String::from(message);

        StringSummary {
            content,
            as_str,
            bytes,
            display,
            into_string,
        }
    }

    #[test]
    fn test_string_message_sources() {
        let expected = StringSummary {
            content: "hello".to_string(),
            as_str: "hello".to_string(),
            bytes: b"hello".to_vec(),
            display: "hello".to_string(),
            into_string: "hello".to_string(),
        };

        value_scenarios!(
            run = summarize;
            "new" {
                StringSource::New => expected.clone(),
            }

            "from borrowed" {
                StringSource::FromBorrowed => expected.clone(),
            }

            "from owned" {
                StringSource::FromOwned => expected.clone(),
            }

            "from bytes" {
                StringSource::FromBytes => expected.clone(),
            }

            "from str" {
                StringSource::FromStr => expected,
            }
        );
    }

    #[test]
    fn test_string_message_into_string() {
        assert_eq!(StringMessage::new("hello").into_string(), "hello");
    }

    #[test]
    fn test_string_message_invalid_utf8() {
        let message = StringMessage::from_bytes(vec![0xff]);

        assert!(message.content.starts_with("Invalid UTF-8 data:"));
        assert!(message.to_bytes().starts_with(b"Invalid UTF-8 data:"));
    }
}
