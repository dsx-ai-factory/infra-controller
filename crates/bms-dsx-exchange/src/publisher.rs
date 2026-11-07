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
use std::collections::hash_map::{Entry, OccupiedEntry, VacantEntry};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::source_mapping::{SourceId, SourceUpdate, SourceValue};
use crate::{SupportedMetadata, ValueMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    pub topic: String,
    pub message: ValueMessage,
}

impl Publication {
    pub fn payload_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.message)
    }
}

#[derive(Debug, Clone)]
pub struct PublisherConfig {
    pub republish_interval: Duration,
    pub heartbeat_interval: Duration,
}

impl Default for PublisherConfig {
    fn default() -> Self {
        Self {
            republish_interval: Duration::from_secs(100),
            heartbeat_interval: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
pub struct BmsDsxExchangePublisher {
    config: PublisherConfig,
    entries: HashMap<SourceId, EntryState>,
}

#[derive(Debug)]
enum EntryState {
    Unrouted { current_value: SourceValue },
    Routed(RoutedEntryState),
}

#[derive(Debug)]
struct RoutedEntryState {
    topic: String,
    current_value: Option<SourceValue>,
    last_published_at: Option<DateTime<Utc>>,
    heartbeat: bool,
}

impl RoutedEntryState {
    fn publish(&mut self, value: SourceValue, now: DateTime<Utc>) -> Publication {
        self.last_published_at = Some(now);

        Publication {
            topic: self.topic.clone(),
            message: ValueMessage::new(value, now.timestamp_millis()),
        }
    }
}

#[derive(Debug)]
enum PublishDecision {
    None,
    Value(SourceValue),
    Heartbeat,
}

impl BmsDsxExchangePublisher {
    pub fn new(config: PublisherConfig) -> Self {
        Self {
            config,
            entries: HashMap::new(),
        }
    }

    pub fn upsert_metadata(
        &mut self,
        metadata: SupportedMetadata,
        now: DateTime<Utc>,
    ) -> Vec<Publication> {
        let source_id = metadata.source_id();
        let topic = metadata.value_topic().to_string();
        let heartbeat = matches!(metadata, SupportedMetadata::Heartbeat(_));

        match self.entries.entry(source_id) {
            Entry::Vacant(entry) => upsert_vacant_metadata(entry, topic, heartbeat, now),
            Entry::Occupied(entry) => upsert_existing_metadata(entry, topic, heartbeat, now),
        }
    }

    pub fn update_source(&mut self, update: SourceUpdate, now: DateTime<Utc>) -> Vec<Publication> {
        let source_id = update.source_id();
        let next_value = update.value();

        match self.entries.entry(source_id) {
            Entry::Vacant(entry) => {
                entry.insert(EntryState::Unrouted {
                    current_value: next_value,
                });
                Vec::new()
            }
            Entry::Occupied(mut entry) => match entry.get_mut() {
                EntryState::Unrouted { current_value } => {
                    *current_value = next_value;
                    Vec::new()
                }
                EntryState::Routed(routed) => {
                    if routed.current_value == Some(next_value) {
                        Vec::new()
                    } else {
                        routed.current_value = Some(next_value);
                        publish_decision(routed, PublishDecision::Value(next_value), now)
                    }
                }
            },
        }
    }

    pub fn tick(&mut self, now: DateTime<Utc>) -> Vec<Publication> {
        let mut publications = Vec::new();

        for entry in self.entries.values_mut() {
            let EntryState::Routed(routed) = entry else {
                continue;
            };

            let interval = if routed.heartbeat {
                self.config.heartbeat_interval
            } else {
                self.config.republish_interval
            };

            if !is_due(routed.last_published_at, now, interval) {
                continue;
            }

            let decision = if routed.heartbeat {
                PublishDecision::Heartbeat
            } else if let Some(value) = routed.current_value {
                PublishDecision::Value(value)
            } else {
                PublishDecision::None
            };

            publications.extend(publish_decision(routed, decision, now));
        }

        publications
    }
}

fn upsert_vacant_metadata(
    entry: VacantEntry<'_, SourceId, EntryState>,
    topic: String,
    heartbeat: bool,
    now: DateTime<Utc>,
) -> Vec<Publication> {
    let mut routed = RoutedEntryState {
        topic,
        current_value: None,
        last_published_at: None,
        heartbeat,
    };
    let decision = if heartbeat {
        PublishDecision::Heartbeat
    } else {
        PublishDecision::None
    };
    let publications = publish_decision(&mut routed, decision, now);
    entry.insert(EntryState::Routed(routed));
    publications
}

fn upsert_existing_metadata(
    mut entry: OccupiedEntry<'_, SourceId, EntryState>,
    topic: String,
    heartbeat: bool,
    now: DateTime<Utc>,
) -> Vec<Publication> {
    match entry.get_mut() {
        EntryState::Unrouted { current_value } => {
            let current_value = *current_value;
            let mut routed = RoutedEntryState {
                topic,
                current_value: Some(current_value),
                last_published_at: None,
                heartbeat,
            };
            let decision = if heartbeat {
                PublishDecision::Heartbeat
            } else {
                PublishDecision::Value(current_value)
            };
            let publications = publish_decision(&mut routed, decision, now);
            entry.insert(EntryState::Routed(routed));
            publications
        }
        EntryState::Routed(routed) => {
            let topic_changed = routed.topic != topic;
            routed.topic = topic;
            routed.heartbeat = heartbeat;

            if heartbeat {
                publish_decision(routed, PublishDecision::Heartbeat, now)
            } else {
                match routed.current_value {
                    Some(value) if topic_changed || routed.last_published_at.is_none() => {
                        publish_decision(routed, PublishDecision::Value(value), now)
                    }
                    _ => Vec::new(),
                }
            }
        }
    }
}

fn publish_decision(
    routed: &mut RoutedEntryState,
    decision: PublishDecision,
    now: DateTime<Utc>,
) -> Vec<Publication> {
    match decision {
        PublishDecision::None => Vec::new(),
        PublishDecision::Value(value) => vec![routed.publish(value, now)],
        PublishDecision::Heartbeat => {
            vec![routed.publish(SourceValue::HeartbeatTimestamp(now.timestamp_millis()), now)]
        }
    }
}

fn is_due(
    last_published_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    interval: Duration,
) -> bool {
    match last_published_at {
        None => true,
        Some(previous) => now
            .signed_duration_since(previous)
            .to_std()
            .map(|elapsed| elapsed >= interval)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::{BinaryState, parse_supported_metadata};

    fn now(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).unwrap()
    }

    fn publisher() -> BmsDsxExchangePublisher {
        BmsDsxExchangePublisher::new(PublisherConfig::default())
    }

    fn liquid_isolation_metadata() -> SupportedMetadata {
        parse_supported_metadata(
            "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01",
            r#"{
                "pointType": "RackLiquidIsolationRequest",
                "objectType": "Rack",
                "rackName": "Rack-01",
                "rackId": "rack-01",
                "integration": "CM"
            }"#
            .as_bytes(),
        )
        .unwrap()
        .unwrap()
    }

    fn heartbeat_metadata() -> SupportedMetadata {
        parse_supported_metadata(
            "BMS/v1/PUB/Metadata/System/HeartbeatTimestampIntegration/site",
            r#"{
                "pointType": "HeartbeatTimestampIntegration",
                "objectType": "System",
                "integration": "CM"
            }"#
            .as_bytes(),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn update_before_metadata_publishes_when_metadata_arrives() {
        let mut publisher = publisher();
        assert!(
            publisher
                .update_source(
                    SourceUpdate::liquid_isolation_request("rack-01", true),
                    now(10)
                )
                .is_empty()
        );

        let publications = publisher.upsert_metadata(liquid_isolation_metadata(), now(11));

        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].message.value,
            SourceValue::Binary(BinaryState::Active)
        );
    }

    #[test]
    fn value_change_publishes_immediately() {
        let mut publisher = publisher();
        publisher.upsert_metadata(liquid_isolation_metadata(), now(10));

        let publications = publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", true),
            now(11),
        );

        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].message.value,
            SourceValue::Binary(BinaryState::Active)
        );
        assert_eq!(
            publications[0].message.timestamp,
            now(11).timestamp_millis()
        );
    }

    #[test]
    fn unchanged_value_waits_for_republish_interval() {
        let mut publisher = publisher();
        publisher.upsert_metadata(liquid_isolation_metadata(), now(10));
        publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", true),
            now(11),
        );

        assert!(
            publisher
                .update_source(
                    SourceUpdate::liquid_isolation_request("rack-01", true),
                    now(50)
                )
                .is_empty()
        );
        assert!(publisher.tick(now(100)).is_empty());

        let publications = publisher.tick(now(111));
        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].message.value,
            SourceValue::Binary(BinaryState::Active)
        );
    }

    #[test]
    fn changed_value_publishes_before_republish_interval() {
        let mut publisher = publisher();
        publisher.upsert_metadata(liquid_isolation_metadata(), now(10));
        publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", false),
            now(11),
        );

        let publications = publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", true),
            now(12),
        );

        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].message.value,
            SourceValue::Binary(BinaryState::Active)
        );
    }

    #[test]
    fn liquid_isolation_uses_binary_values() {
        let mut publisher = publisher();
        publisher.upsert_metadata(liquid_isolation_metadata(), now(10));

        let publications = publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", true),
            now(11),
        );

        assert_eq!(publications.len(), 1);
        let json = serde_json::to_value(&publications[0].message).unwrap();
        assert_eq!(json["value"], 1);
    }

    #[test]
    fn heartbeat_publishes_immediately_and_periodically() {
        let mut publisher = publisher();

        let first = publisher.upsert_metadata(heartbeat_metadata(), now(10));
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].message.value,
            SourceValue::HeartbeatTimestamp(now(10).timestamp_millis())
        );

        assert!(publisher.tick(now(14)).is_empty());

        let second = publisher.tick(now(15));
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].message.value,
            SourceValue::HeartbeatTimestamp(now(15).timestamp_millis())
        );
    }

    #[test]
    fn metadata_from_another_integration_is_used_directly() {
        let mut publisher = publisher();
        let metadata = parse_supported_metadata(
            "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01",
            r#"{
                "pointType": "RackLiquidIsolationRequest",
                "objectType": "Rack",
                "rackName": "Rack-01",
                "rackId": "rack-01",
                "integration": "OTHER"
            }"#
            .as_bytes(),
        )
        .unwrap()
        .unwrap();

        let publications = publisher.upsert_metadata(metadata, now(11));

        assert!(publications.is_empty());

        let publications = publisher.update_source(
            SourceUpdate::liquid_isolation_request("rack-01", true),
            now(12),
        );

        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].topic,
            "BMS/v1/OTHER/Value/Rack/RackLiquidIsolationRequest/site/rack-01"
        );
    }
}
