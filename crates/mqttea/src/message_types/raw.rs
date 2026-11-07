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

// src/message_types/raw.rs
// Raw message types for binary data handling

use crate::traits::RawMessageType;

// RawMessage handles arbitrary binary data, including
// from unmapped MQTT topics.
#[derive(Clone, Debug, PartialEq)]
pub struct RawMessage {
    pub payload: Vec<u8>,
}

impl RawMessageType for RawMessage {
    fn to_bytes(&self) -> Vec<u8> {
        self.payload.clone()
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { payload: bytes }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct RawSummary {
        payload: Vec<u8>,
        bytes: Vec<u8>,
        round_trip: RawMessage,
    }

    fn summarize(payload: Vec<u8>) -> RawSummary {
        let message = RawMessage {
            payload: payload.clone(),
        };
        let bytes = message.to_bytes();
        let round_trip = RawMessage::from_bytes(bytes.clone());

        RawSummary {
            payload,
            bytes,
            round_trip,
        }
    }

    #[test]
    fn test_raw_message_bytes() {
        value_scenarios!(
            run = summarize;
            "empty payload" {
                vec![] => RawSummary {
                    payload: vec![],
                    bytes: vec![],
                    round_trip: RawMessage { payload: vec![] },
                },
            }

            "binary payload" {
                vec![0, 1, 2, 255] => RawSummary {
                    payload: vec![0, 1, 2, 255],
                    bytes: vec![0, 1, 2, 255],
                    round_trip: RawMessage {
                        payload: vec![0, 1, 2, 255],
                    },
                },
            }
        );
    }
}
