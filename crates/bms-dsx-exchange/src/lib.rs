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

pub mod publisher;
pub mod source_mapping;

pub use publisher::{BmsDsxExchangePublisher, Publication, PublisherConfig};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
pub use source_mapping::{SourceId, SourceUpdate, SourceValue};

pub const GOOD_QUALITY: &str = "1";

const OBJECT_TYPE_RACK: &str = "Rack";
const OBJECT_TYPE_SYSTEM: &str = "System";

const POINT_TYPE_RACK_TRAY_LEAK: &str = "RackLeakDetectTray";
const POINT_TYPE_RACK_LIQUID_ISOLATION_REQUEST: &str = "RackLiquidIsolationRequest";
const POINT_TYPE_RACK_ELECTRICAL_ISOLATION_REQUEST: &str = "RackElectricalIsolationRequest";
const POINT_TYPE_HEARTBEAT_TIMESTAMP_INTEGRATION: &str = "HeartbeatTimestampIntegration";

#[derive(Debug, thiserror::Error)]
pub enum BmsDsxExchangeError {
    #[error("failed to parse metadata: {0}")]
    InvalidMetadata(#[from] serde_json::Error),

    #[error("metadata for point type `{point_type}` is missing `{field}` or empty")]
    MissingMetadataField {
        point_type: String,
        field: &'static str,
    },

    #[error("metadata topic `{topic}` is not a valid BMS metadata topic")]
    InvalidMetadataTopic { topic: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryState {
    Inactive,
    Active,
}

impl From<bool> for BinaryState {
    fn from(value: bool) -> Self {
        if value { Self::Active } else { Self::Inactive }
    }
}

impl Serialize for BinaryState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Inactive => serializer.serialize_i64(0),
            Self::Active => serializer.serialize_i64(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RackPointMetadata {
    RackTrayLeak {
        rack_name: String,
        rack_id: String,
        integration: String,
        value_topic: String,
    },
    LiquidIsolationRequest {
        rack_name: String,
        rack_id: String,
        integration: String,
        value_topic: String,
    },
    ElectricalIsolationRequest {
        rack_name: String,
        rack_id: String,
        integration: String,
        value_topic: String,
    },
}

impl RackPointMetadata {
    pub fn point_type(&self) -> &'static str {
        match self {
            Self::RackTrayLeak { .. } => POINT_TYPE_RACK_TRAY_LEAK,
            Self::LiquidIsolationRequest { .. } => POINT_TYPE_RACK_LIQUID_ISOLATION_REQUEST,
            Self::ElectricalIsolationRequest { .. } => POINT_TYPE_RACK_ELECTRICAL_ISOLATION_REQUEST,
        }
    }

    pub fn rack_id(&self) -> &str {
        match self {
            Self::RackTrayLeak { rack_id, .. }
            | Self::LiquidIsolationRequest { rack_id, .. }
            | Self::ElectricalIsolationRequest { rack_id, .. } => rack_id,
        }
    }

    pub fn integration(&self) -> &str {
        match self {
            Self::RackTrayLeak { integration, .. }
            | Self::LiquidIsolationRequest { integration, .. }
            | Self::ElectricalIsolationRequest { integration, .. } => integration,
        }
    }

    pub fn value_topic(&self) -> &str {
        match self {
            Self::RackTrayLeak { value_topic, .. }
            | Self::LiquidIsolationRequest { value_topic, .. }
            | Self::ElectricalIsolationRequest { value_topic, .. } => value_topic,
        }
    }

    pub fn source_id(&self) -> SourceId {
        SourceId::from_rack_metadata(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatMetadata {
    pub integration: String,
    pub value_topic: String,
}

impl HeartbeatMetadata {
    pub fn point_type(&self) -> &'static str {
        POINT_TYPE_HEARTBEAT_TIMESTAMP_INTEGRATION
    }

    pub fn source_id(&self) -> SourceId {
        SourceId::from_heartbeat_metadata(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupportedMetadata {
    Rack(RackPointMetadata),
    Heartbeat(HeartbeatMetadata),
}

impl SupportedMetadata {
    pub fn integration(&self) -> &str {
        match self {
            Self::Rack(metadata) => metadata.integration(),
            Self::Heartbeat(metadata) => &metadata.integration,
        }
    }

    pub fn value_topic(&self) -> &str {
        match self {
            Self::Rack(metadata) => metadata.value_topic(),
            Self::Heartbeat(metadata) => &metadata.value_topic,
        }
    }

    pub fn point_type(&self) -> &str {
        match self {
            Self::Rack(metadata) => metadata.point_type(),
            Self::Heartbeat(metadata) => metadata.point_type(),
        }
    }

    pub fn source_id(&self) -> SourceId {
        match self {
            Self::Rack(metadata) => metadata.source_id(),
            Self::Heartbeat(metadata) => metadata.source_id(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueMessage {
    pub value: SourceValue,
    pub timestamp: i64,
    pub quality: String,
}

impl ValueMessage {
    pub fn new(value: SourceValue, timestamp: i64) -> Self {
        Self {
            value,
            timestamp,
            quality: GOOD_QUALITY.to_string(),
        }
    }
}

impl Serialize for ValueMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("value", &self.value)?;
        map.serialize_entry("timestamp", &self.timestamp)?;
        map.serialize_entry("quality", &self.quality)?;
        map.end()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMetadata {
    point_type: String,
    object_type: String,
    #[serde(default)]
    rack_name: Option<String>,
    #[serde(default)]
    rack_id: Option<String>,
    #[serde(default)]
    integration: Option<String>,
}

pub fn parse_supported_metadata(
    metadata_topic: impl AsRef<str>,
    input: &[u8],
) -> Result<Option<SupportedMetadata>, BmsDsxExchangeError> {
    let raw: RawMetadata = serde_json::from_slice(input)?;
    let metadata_topic = metadata_topic.as_ref();
    SupportedMetadata::from_raw(raw, metadata_topic)
}

impl SupportedMetadata {
    fn from_raw(
        raw: RawMetadata,
        metadata_topic: &str,
    ) -> Result<Option<Self>, BmsDsxExchangeError> {
        let integration = required_field(&raw.point_type, "integration", raw.integration.clone())?;
        let rack_name = || required_field(&raw.point_type, "rackName", raw.rack_name.clone());
        let rack_id = || required_field(&raw.point_type, "rackId", raw.rack_id.clone());

        let value_topic = value_topic(metadata_topic, &integration)?;

        match (raw.object_type.as_str(), raw.point_type.as_str()) {
            (OBJECT_TYPE_RACK, POINT_TYPE_RACK_LIQUID_ISOLATION_REQUEST) => Ok(Some(Self::Rack(
                RackPointMetadata::LiquidIsolationRequest {
                    rack_name: rack_name()?,
                    rack_id: rack_id()?,
                    value_topic,
                    integration,
                },
            ))),
            (OBJECT_TYPE_RACK, POINT_TYPE_RACK_ELECTRICAL_ISOLATION_REQUEST) => Ok(Some(
                Self::Rack(RackPointMetadata::ElectricalIsolationRequest {
                    rack_name: rack_name()?,
                    rack_id: rack_id()?,
                    value_topic,
                    integration,
                }),
            )),
            (OBJECT_TYPE_SYSTEM, POINT_TYPE_HEARTBEAT_TIMESTAMP_INTEGRATION) => {
                Ok(Some(Self::Heartbeat(HeartbeatMetadata {
                    value_topic,
                    integration,
                })))
            }
            _ => Ok(None),
        }
    }
}

fn value_topic(metadata_topic: &str, integration: &str) -> Result<String, BmsDsxExchangeError> {
    let Some(path) = metadata_topic.strip_prefix("BMS/v1/PUB/Metadata/") else {
        return Err(BmsDsxExchangeError::InvalidMetadataTopic {
            topic: metadata_topic.to_string(),
        });
    };

    let mut segments = path.split('/');
    let Some(object_type) = segments.next() else {
        return Err(BmsDsxExchangeError::InvalidMetadataTopic {
            topic: metadata_topic.to_string(),
        });
    };
    let Some(point_type) = segments.next() else {
        return Err(BmsDsxExchangeError::InvalidMetadataTopic {
            topic: metadata_topic.to_string(),
        });
    };
    let tag_path = segments.collect::<Vec<_>>().join("/");

    if object_type.is_empty() || point_type.is_empty() || tag_path.is_empty() {
        return Err(BmsDsxExchangeError::InvalidMetadataTopic {
            topic: metadata_topic.to_string(),
        });
    }

    Ok(format!(
        "BMS/v1/{integration}/Value/{object_type}/{point_type}/{tag_path}"
    ))
}

fn required_field(
    point_type: &str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, BmsDsxExchangeError> {
    let value = value.ok_or_else(|| BmsDsxExchangeError::MissingMetadataField {
        point_type: point_type.to_string(),
        field,
    })?;

    if value.trim().is_empty() {
        return Err(BmsDsxExchangeError::MissingMetadataField {
            point_type: point_type.to_string(),
            field,
        });
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_metadata() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        /// A metadata topic paired with its raw JSON payload.
        struct Message {
            topic: &'static str,
            payload: &'static str,
        }

        // Each success row pins the three observable fields the parse derives:
        // the canonical point type, the integration, and the value topic.
        scenarios!(
            run = |Message { topic, payload }: Message| {
                parse_supported_metadata(topic, payload.as_bytes())
                    .map(|maybe| {
                        maybe.map(|m| {
                            (
                                m.point_type().to_string(),
                                m.integration().to_string(),
                                m.value_topic().to_string(),
                            )
                        })
                    })
                    // The error type isn't PartialEq; the failing rows assert only
                    // that the input is rejected, so carry the message as a String.
                    .map_err(|e| e.to_string())
            };
            "a supported point type parses, deriving the value topic from the metadata topic" {
                Message {
                    topic: "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01",
                    payload: r#"{
                        "pointType": "RackLiquidIsolationRequest",
                        "objectType": "Rack",
                        "rackName": "Rack-01",
                        "rackId": "rack-01",
                        "integration": "CM"
                    }"#,
                } => Yields(Some((
                    POINT_TYPE_RACK_LIQUID_ISOLATION_REQUEST.to_string(),
                    "CM".to_string(),
                    "BMS/v1/CM/Value/Rack/RackLiquidIsolationRequest/site/rack-01".to_string(),
                ))),
                Message {
                    topic: "BMS/v1/PUB/Metadata/System/HeartbeatTimestampIntegration/site",
                    payload: r#"{
                        "pointType": "HeartbeatTimestampIntegration",
                        "objectType": "System",
                        "integration": "CM"
                    }"#,
                } => Yields(Some((
                    POINT_TYPE_HEARTBEAT_TIMESTAMP_INTEGRATION.to_string(),
                    "CM".to_string(),
                    "BMS/v1/CM/Value/System/HeartbeatTimestampIntegration/site".to_string(),
                ))),
                // The value topic is derived from the metadata topic, not the payload's
                // point type — here the topic names a different request than the body.
                Message {
                    topic: "BMS/v1/PUB/Metadata/Rack/RackElectricalIsolationRequest/site/rack-01",
                    payload: r#"{
                        "pointType": "RackLiquidIsolationRequest",
                        "objectType": "Rack",
                        "rackName": "Rack-01",
                        "rackId": "rack-01",
                        "integration": "CM"
                    }"#,
                } => Yields(Some((
                    POINT_TYPE_RACK_LIQUID_ISOLATION_REQUEST.to_string(),
                    "CM".to_string(),
                    "BMS/v1/CM/Value/Rack/RackElectricalIsolationRequest/site/rack-01".to_string(),
                ))),
            }
            "an empty required field is rejected (MissingMetadataField)" {
                Message {
                    topic: "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01",
                    payload: r#"{
                        "pointType": "RackLiquidIsolationRequest",
                        "objectType": "Rack",
                        "rackName": "Rack-01",
                        "rackId": "rack-01",
                        "integration": ""
                    }"#,
                } => Fails,
            }
            "a topic that is not a metadata topic is rejected (InvalidMetadataTopic)" {
                Message {
                    topic: "BMS/v1/CM/Value/Rack/RackLiquidIsolationRequest/site/rack-01",
                    payload: r#"{
                        "pointType": "RackLiquidIsolationRequest",
                        "objectType": "Rack",
                        "rackName": "Rack-01",
                        "rackId": "rack-01",
                        "integration": "CM"
                    }"#,
                } => Fails,
            }
        );
    }

    #[test]
    fn serializes_value_message() {
        let message =
            ValueMessage::new(SourceValue::Binary(BinaryState::Active), 1_712_345_678_901);

        let json = serde_json::to_value(message).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "value": 1,
                "timestamp": 1_712_345_678_901_i64,
                "quality": "1"
            })
        );
    }
}
