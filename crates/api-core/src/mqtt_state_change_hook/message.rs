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

//! Message types shared by the MQTT state change hook and periodic republisher.

use carbide_uuid::machine::HostMachineId;
use chrono::{DateTime, Utc};
use model::machine::ManagedHostState;
use serde::Serialize;

/// MQTT message carrying managed host state.
///
/// Serializes to JSON with the state flattened directly into the message,
/// using `ManagedHostState`'s native serde serialization (lowercase state names).
#[derive(Debug, Clone, Serialize)]
pub(super) struct ManagedHostStateMessage<'a> {
    /// Unique identifier for the managed host machine.
    pub(super) machine_id: &'a HostMachineId,
    /// ISO 8601 timestamp when the state was observed for publishing.
    pub(super) timestamp: DateTime<Utc>,
    /// The managed host state.
    pub(super) managed_host_state: &'a ManagedHostState,
}

impl ManagedHostStateMessage<'_> {
    /// Serialize the message to JSON bytes for MQTT publishing.
    pub(super) fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// MQTT topic this message publishes to: `{topic_prefix}/{machineId}/state`.
    ///
    /// Shared by the change-driven hook and the periodic republisher so the
    /// topic layout is defined in exactly one place.
    pub(super) fn topic(&self, topic_prefix: &str) -> String {
        format!("{topic_prefix}/{}/state", self.machine_id)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::machine::InstanceState;

    use super::*;

    #[derive(Debug, PartialEq)]
    struct ObservedMessage {
        state: Option<String>,
        has_instance_state: bool,
        machine_id: Option<String>,
        timestamp_is_rfc3339: bool,
    }

    #[allow(deprecated)]
    fn test_machine_id() -> HostMachineId {
        carbide_uuid::machine::MachineId::new(
            carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
            [0; 32],
            carbide_uuid::machine::MachineType::Host,
        )
        .try_into()
        .unwrap()
    }

    #[test]
    fn serialization_preserves_message_fields() {
        let machine_id = test_machine_id();
        check_values(
            [
                Check {
                    scenario: "ready message includes the common fields",
                    input: ManagedHostState::Ready,
                    expect: ObservedMessage {
                        state: Some("ready".to_string()),
                        has_instance_state: false,
                        machine_id: Some(machine_id.to_string()),
                        timestamp_is_rfc3339: true,
                    },
                },
                Check {
                    scenario: "assigned message includes its nested instance state",
                    input: ManagedHostState::Assigned {
                        instance_state: InstanceState::Ready,
                    },
                    expect: ObservedMessage {
                        state: Some("assigned".to_string()),
                        has_instance_state: true,
                        machine_id: Some(machine_id.to_string()),
                        timestamp_is_rfc3339: true,
                    },
                },
            ],
            |state| {
                let message = ManagedHostStateMessage {
                    machine_id: &machine_id,
                    managed_host_state: &state,
                    timestamp: Utc::now(),
                };
                let json = message.to_json_bytes().unwrap();
                let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
                let managed_host_state = parsed.get("managed_host_state");

                ObservedMessage {
                    state: managed_host_state
                        .and_then(|state| state.get("state"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    has_instance_state: managed_host_state
                        .is_some_and(|state| state.get("instance_state").is_some()),
                    machine_id: parsed
                        .get("machine_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    timestamp_is_rfc3339: parsed
                        .get("timestamp")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|timestamp| {
                            chrono::DateTime::parse_from_rfc3339(timestamp).is_ok()
                        }),
                }
            },
        );
    }
}
