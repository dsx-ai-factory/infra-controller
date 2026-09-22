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

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::LogRecord;

#[cfg(not(feature = "bench-hooks"))]
pub(crate) trait RedfishEventMapper: Send + Sync {
    /// Returns the latest-wins queue key for one log record.
    ///
    /// Decoded protobuf notifications use their payload hash. Redfish state
    /// notifications share a resource key. Periodic entries use their service
    /// and entry IDs. SSE records prefer the event ID, then the linked log
    /// entry URI, then the event record URI. Other records use message or body
    /// content as a fallback.
    fn queue_key(&self, bmc_id: &str, attributes: &[(Cow<'static, str>, String)]) -> String;
}

#[cfg(feature = "bench-hooks")]
pub trait RedfishEventMapper: Send + Sync {
    /// Returns the latest-wins queue key for one log record.
    ///
    /// Decoded protobuf notifications use their payload hash. Redfish state
    /// notifications share a resource key. Periodic entries use their service
    /// and entry IDs. SSE records prefer the event ID, then the linked log
    /// entry URI, then the event record URI. Other records use message or body
    /// content as a fallback.
    fn queue_key(&self, bmc_id: &str, attributes: &[(Cow<'static, str>, String)]) -> String;
}

#[cfg(not(feature = "bench-hooks"))]
pub(crate) struct OpenBmcEventMapper;

#[cfg(feature = "bench-hooks")]
pub struct OpenBmcEventMapper;

impl OpenBmcEventMapper {
    fn find_attr<'a>(attributes: &'a [(Cow<'static, str>, String)], key: &str) -> Option<&'a str> {
        attributes
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| v.as_str())
    }

    fn first_message_arg(attributes: &[(Cow<'static, str>, String)]) -> String {
        Self::find_attr(attributes, "message_args")
            .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
            .and_then(|args| args.into_iter().next())
            .unwrap_or_default()
    }

    fn hash_string(s: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }
}

impl RedfishEventMapper for OpenBmcEventMapper {
    fn queue_key(&self, bmc_id: &str, attributes: &[(Cow<'static, str>, String)]) -> String {
        // The decoded protobuf represents the complete notification. Its hash
        // keeps distinct notifications separate while retaining latest-wins
        // replacement for exact duplicates.
        if let Some(payload) =
            Self::find_attr(attributes, LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE)
        {
            return format!("{bmc_id}|protobuf|{}", Self::hash_string(payload));
        }

        let message_id = Self::find_attr(attributes, "message_id").unwrap_or("");

        // State notifications share a resource key so a later state replaces
        // the queued state for that resource.
        if message_id.contains("SensorThreshold") {
            let resource = Self::first_message_arg(attributes);
            return format!("{bmc_id}|SensorThreshold|{resource}");
        }

        if message_id.starts_with("ResourceEvent.") && message_id.contains("ResourceStatusChanged")
        {
            let resource = Self::first_message_arg(attributes);
            return format!("{bmc_id}|ResourceStatusChanged|{resource}");
        }

        // A periodic LogEntry ID is unique within its LogService, so both
        // fields identify one occurrence.
        if let (Some(service_id), Some(entry_id)) = (
            Self::find_attr(attributes, "service_id").filter(|value| !value.is_empty()),
            Self::find_attr(attributes, "entry_id").filter(|value| !value.is_empty()),
        ) {
            return format!("{bmc_id}|redfish-entry|{service_id}|{entry_id}");
        }

        // An SSE EventId identifies the delivered event and takes precedence
        // over an optional link to its LogEntry.
        if let Some(event_id) =
            Self::find_attr(attributes, "event_id").filter(|value| !value.is_empty())
        {
            return format!("{bmc_id}|redfish-event|{event_id}");
        }

        // SSE events without an EventId use the linked LogEntry URI.
        if let Some(log_entry_id) =
            Self::find_attr(attributes, "log_entry_id").filter(|value| !value.is_empty())
        {
            return format!("{bmc_id}|redfish-entry|{log_entry_id}");
        }

        // SSE events without either optional identifier use the EventRecord URI.
        if let Some(event_record_id) =
            Self::find_attr(attributes, "event_record_id").filter(|value| !value.is_empty())
        {
            return format!("{bmc_id}|redfish-event-record|{event_record_id}");
        }

        if message_id.is_empty() {
            let body = Self::find_attr(attributes, "body").unwrap_or("");
            return format!("{bmc_id}|raw|{}", Self::hash_string(body));
        }

        let resource = Self::first_message_arg(attributes);
        format!("{bmc_id}|{message_id}|{resource}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Attrs = &'static [(&'static str, &'static str)];

    use carbide_test_support::value_scenarios;

    fn attrs(pairs: &[(&str, &str)]) -> Vec<(Cow<'static, str>, String)> {
        pairs
            .iter()
            .map(|(k, v)| (Cow::Owned(k.to_string()), v.to_string()))
            .collect()
    }

    #[test]
    fn sensor_threshold_normalizes_direction() {
        let mapper = OpenBmcEventMapper;
        let key_high = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                (
                    "message_id",
                    "OpenBMC.0.1.SensorThresholdWarningLowGoingHigh",
                ),
                ("message_args", r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#),
                ("service_id", "/redfish/v1/Systems/1/LogServices/EventLog"),
                ("entry_id", "41"),
            ]),
        );
        let key_low = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                (
                    "message_id",
                    "OpenBMC.0.1.SensorThresholdWarningHighGoingLow",
                ),
                ("message_args", r#"["HGX_GPU_0_Temp_1","3.96","-0.05"]"#),
                ("service_id", "/redfish/v1/Systems/1/LogServices/EventLog"),
                ("entry_id", "42"),
            ]),
        );

        assert_eq!(key_high, key_low);
        assert!(key_high.contains("SensorThreshold"));
        assert!(key_high.contains("HGX_GPU_0_Temp_1"));
    }

    #[test]
    fn health_status_normalizes_severity() {
        let mapper = OpenBmcEventMapper;
        let key_critical = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                (
                    "message_id",
                    "ResourceEvent.1.0.ResourceStatusChangedCritical",
                ),
                ("message_args", r#"["leakage1"]"#),
                ("event_id", "event-41"),
            ]),
        );
        let key_ok = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                ("message_id", "ResourceEvent.1.0.ResourceStatusChangedOK"),
                ("message_args", r#"["leakage1"]"#),
                ("event_id", "event-42"),
            ]),
        );

        assert_eq!(key_critical, key_ok);
        assert!(key_critical.contains("ResourceStatusChanged"));
        assert!(key_critical.contains("leakage1"));
    }

    #[test]
    fn different_devices_are_different_keys() {
        let mapper = OpenBmcEventMapper;
        let key_gpu0 = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                ("message_id", "ResourceEvent.1.0.ResourceErrorsDetected"),
                (
                    "message_args",
                    r#"["GPU_0 NVLink_9","NVLink Training Error"]"#,
                ),
            ]),
        );
        let key_gpu1 = mapper.queue_key(
            "10.85.14.144",
            &attrs(&[
                ("message_id", "ResourceEvent.1.0.ResourceErrorsDetected"),
                (
                    "message_args",
                    r#"["GPU_1 NVLink_9","NVLink Training Error"]"#,
                ),
            ]),
        );
        assert_ne!(key_gpu0, key_gpu1);
    }

    #[test]
    fn ordinary_occurrences_use_stable_redfish_identifiers() {
        value_scenarios!(run = |(left, right): (Attrs, Attrs)| {
            let mapper = OpenBmcEventMapper;
            let left = mapper.queue_key("10.85.14.144", &attrs(left));
            let right = mapper.queue_key("10.85.14.144", &attrs(right));

            left == right
        };
            "periodic entry IDs are scoped by log service" {
                (&[("service_id", "EventLog"), ("entry_id", "41")][..],
                 &[("service_id", "Journal"), ("entry_id", "41")][..]) => false,
            }

            "periodic entry identity takes precedence over its event ID" {
                (&[("service_id", "EventLog"), ("entry_id", "41"), ("event_id", "event-1")][..],
                 &[("service_id", "EventLog"), ("entry_id", "41"), ("event_id", "event-2")][..]) => true,
            }

            "SSE events use their event ID" {
                (&[("event_id", "event-41")][..], &[("event_id", "event-42")][..]) => false,
            }

            "SSE event ID takes precedence over its log entry link" {
                (&[("event_id", "event-41"), ("log_entry_id", "entry-1")][..],
                 &[("event_id", "event-41"), ("log_entry_id", "entry-2")][..]) => true,
            }

            "SSE events fall back to their log entry link" {
                (&[("log_entry_id", "entry-1")][..],
                 &[("log_entry_id", "entry-2")][..]) => false,
            }

            "empty SSE event IDs use the log entry link" {
                (&[("event_id", ""), ("log_entry_id", "entry-1")][..],
                 &[("event_id", ""), ("log_entry_id", "entry-2")][..]) => false,
            }

            "SSE events fall back to their event record URI" {
                (&[("event_record_id", "record-1")][..],
                 &[("event_record_id", "record-2")][..]) => false,
            }

            "SSE log entry links take precedence over event record URIs" {
                (&[("log_entry_id", "entry-1"), ("event_record_id", "record-1")][..],
                 &[("log_entry_id", "entry-1"), ("event_record_id", "record-2")][..]) => true,
            }
        );
    }

    #[test]
    fn legacy_entry_without_message_id_uses_message_hash() {
        let mapper = OpenBmcEventMapper;
        let key = mapper.queue_key("10.85.14.144", &attrs(&[("message_id", "")]));
        assert!(key.contains("raw|"));
    }
}
