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

//! BMS BMS message types defined from the AsyncAPI spec in BMS.yaml.
//!
//! This module contains the message types for leak detection events published
//! by BMS on the DSX Exchange Event Bus.

use chrono::{DateTime, Utc};
use health_report::HealthProbeId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Point type identifier for leak detection events.
///
/// Note: Variant names match the BMS AsyncAPI spec exactly.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum LeakPointType {
    /// Rack-level leak detection. Binary value: 0 = No Leak, 1 = Leak Detected.
    LeakDetectRack,
    /// Rack-level leak sensor fault. Binary value: 0 = OK, 1 = Fault.
    LeakSensorFaultRack,
    /// Rack tray leak detection. Binary value: 0 = No Leak, 1 = Leak Detected.
    LeakDetectRackTray,
}

impl LeakPointType {
    /// Returns the health probe ID for this leak type.
    pub fn probe_id(&self) -> HealthProbeId {
        match self {
            LeakPointType::LeakDetectRack => "BmsLeakDetectRack",
            LeakPointType::LeakSensorFaultRack => "BmsLeakSensorFaultRack",
            LeakPointType::LeakDetectRackTray => "BmsLeakDetectRackTray",
        }
        .parse()
        .expect("non-empty strings are always valid probe ids")
    }

    /// Returns a human-readable description for alert messages.
    pub fn description(&self) -> &'static str {
        match self {
            LeakPointType::LeakDetectRack => "Leak detected",
            LeakPointType::LeakSensorFaultRack => "Leak sensor fault",
            LeakPointType::LeakDetectRackTray => "Rack tray leak detected",
        }
    }
}

/// Fault value from BMS points.
///
/// Deserialized from f64 where 0.0 = Clear and 1.0 = Active.
/// - For leak detection: Clear = No Leak, Active = Leak Detected
/// - For sensor fault: Clear = OK, Active = Fault
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultValue {
    /// Value is 0 (no leak / OK).
    Clear,
    /// Value is 1 (leak detected / fault).
    Faulting,
}

impl Serialize for FaultValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            FaultValue::Clear => serializer.serialize_u8(0),
            FaultValue::Faulting => serializer.serialize_u8(1),
        }
    }
}

impl<'de> Deserialize<'de> for FaultValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BinaryValueVisitor;

        impl<'de> de::Visitor<'de> for BinaryValueVisitor {
            type Value = FaultValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("0 or 1")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                match v {
                    0 => Ok(FaultValue::Clear),
                    1 => Ok(FaultValue::Faulting),
                    other => Err(E::custom(format!(
                        "invalid binary value: expected 0 or 1, got {other}"
                    ))),
                }
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                // JSON parsers may represent 0.0/1.0 as floats
                if v == 0.0 {
                    Ok(FaultValue::Clear)
                } else if v == 1.0 {
                    Ok(FaultValue::Faulting)
                } else {
                    Err(E::custom(format!(
                        "invalid binary value: expected 0 or 1, got {v}"
                    )))
                }
            }
        }

        deserializer.deserialize_f64(BinaryValueVisitor)
    }
}

/// Value message for all BMS points.
///
/// Published on `BMS/v1/{pointPath}/Value` topics.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ValueMessage {
    /// Binary value for the point.
    /// - Leak detection: Clear = No Leak, Active = Leak Detected
    /// - Sensor fault: Clear = OK, Active = Fault
    pub value: FaultValue,
    /// Timestamp corresponding to the event (deserialized from unix timestamp seconds).
    #[serde(with = "chrono::serde::ts_seconds")]
    pub timestamp: DateTime<Utc>,
}

/// Unified metadata type that can represent any of the leak detection metadata types.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LeakMetadata {
    /// Canonical point type identifier.
    pub point_type: String,
    /// Canonical object type.
    pub object_type: String,
    /// Human-readable rack name as defined by the BMS.
    pub rack_name: String,
    /// Stable unique identifier for the rack. Maps to racks.id in the database.
    #[serde(rename = "rackID")]
    pub rack_id: String,
}

impl LeakMetadata {
    /// Check if this is a leak detection point type we care about.
    pub fn is_supported_leak_type(&self) -> bool {
        matches!(
            self.point_type.as_str(),
            "LeakDetectRack" | "LeakSensorFaultRack" | "LeakDetectRackTray"
        )
    }

    /// Get the leak point type enum variant.
    pub fn leak_point_type(&self) -> Option<LeakPointType> {
        match self.point_type.as_str() {
            "LeakDetectRack" => Some(LeakPointType::LeakDetectRack),
            "LeakSensorFaultRack" => Some(LeakPointType::LeakSensorFaultRack),
            "LeakDetectRackTray" => Some(LeakPointType::LeakDetectRackTray),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_leak_detect_rack_metadata() {
        let json = r#"{
            "pointType": "LeakDetectRack",
            "objectType": "Rack",
            "rackName": "Rack-01",
            "rackID": "rack-001"
        }"#;

        let metadata: LeakMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.point_type, "LeakDetectRack");
        assert_eq!(metadata.object_type, "Rack");
        assert_eq!(metadata.rack_name, "Rack-01");
        assert_eq!(metadata.rack_id, "rack-001");
        assert!(metadata.is_supported_leak_type());
        assert_eq!(
            metadata.leak_point_type(),
            Some(LeakPointType::LeakDetectRack)
        );
    }

    #[test]
    fn parses_value_message_binary_value() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        // Every fixture carries the same timestamp; 1706284800 = 2024-01-26T12:00:00Z.
        // Each success row asserts both the decoded fault value and that the
        // timestamp round-trips through `ts_seconds`.
        scenarios!(
            run = |json: &str| {
                serde_json::from_str::<ValueMessage>(json)
                    .map(|m| (m.value, m.timestamp))
                    // The error type isn't PartialEq; these rows assert only that
                    // an out-of-range value fails, so carry the message as a String.
                    .map_err(|e| e.to_string())
            };
            "0 or 1 decode to Clear / Faulting, as int or float" {
                r#"{"value": 1, "timestamp": 1706284800}"#
                    => Yields((FaultValue::Faulting, DateTime::from_timestamp(1706284800, 0).unwrap())),
                r#"{"value": 1.0, "timestamp": 1706284800}"#
                    => Yields((FaultValue::Faulting, DateTime::from_timestamp(1706284800, 0).unwrap())),
                r#"{"value": 0, "timestamp": 1706284800}"#
                    => Yields((FaultValue::Clear, DateTime::from_timestamp(1706284800, 0).unwrap())),
                r#"{"value": 0.0, "timestamp": 1706284800}"#
                    => Yields((FaultValue::Clear, DateTime::from_timestamp(1706284800, 0).unwrap())),
            }
            "anything other than 0 or 1 is rejected, as int or float" {
                r#"{"value": 2, "timestamp": 1706284800}"# => Fails,
                r#"{"value": 0.5, "timestamp": 1706284800}"# => Fails,
            }
        );
    }

    #[test]
    fn leak_point_type_probe_id() {
        use carbide_test_support::value_scenarios;

        // The probe id is part of the contract with the health pipeline; pin each
        // variant's exact id.
        value_scenarios!(
            run = |leak_type: LeakPointType| leak_type.probe_id();
            "each leak type maps to its health probe id" {
                LeakPointType::LeakDetectRack => "BmsLeakDetectRack".parse().unwrap(),
                LeakPointType::LeakSensorFaultRack => "BmsLeakSensorFaultRack".parse().unwrap(),
                LeakPointType::LeakDetectRackTray => "BmsLeakDetectRackTray".parse().unwrap(),
            }
        );
    }

    #[test]
    fn leak_point_type_description() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(
            run = |leak_type: LeakPointType| leak_type.description();
            "each leak type carries a human-readable alert description" {
                LeakPointType::LeakDetectRack => "Leak detected",
                LeakPointType::LeakSensorFaultRack => "Leak sensor fault",
                LeakPointType::LeakDetectRackTray => "Rack tray leak detected",
            }
        );
    }

    #[test]
    fn test_unsupported_point_type() {
        let metadata = LeakMetadata {
            point_type: "LeakResponseRackLiquidIsolationStatus".to_string(),
            object_type: "Rack".to_string(),
            rack_name: "Rack-01".to_string(),
            rack_id: "rack-001".to_string(),
        };
        assert!(!metadata.is_supported_leak_type());
        assert_eq!(metadata.leak_point_type(), None);
    }
}
