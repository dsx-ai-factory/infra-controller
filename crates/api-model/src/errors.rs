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
use std::fmt::{self, Display};
use std::str::FromStr;

use carbide_uuid::machine::{InvalidMachineType, MachineId};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hardware_info::HardwareInfoError;

/// The system that produced an [`ErrorCode`]. NICo is the only system today,
/// but keeping the prefix typed (rather than baking `"NICO"` into every string)
/// means the rendered code can never silently drift or be misspelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorSystem {
    /// NCX Infra Controller.
    Nico,
}

impl ErrorSystem {
    const fn as_str(self) -> &'static str {
        match self {
            ErrorSystem::Nico => "NICO",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "NICO" => Some(ErrorSystem::Nico),
            _ => None,
        }
    }
}

/// The subsystem that produced an [`ErrorCode`]. Spelled out in full so the
/// rendered code is unambiguous in logs and alerts -- e.g. `SITEEXPLORER`
/// rather than the ambiguous `SITE`. Add a variant here when a new subsystem
/// starts emitting operator codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorSubsystem {
    /// Public API / gRPC request handling.
    Api,
    /// Site explorer endpoint exploration.
    SiteExplorer,
    /// Redfish client / BMC interaction.
    Redfish,
    /// DPU-specific handling.
    Dpu,
}

impl ErrorSubsystem {
    const fn as_str(self) -> &'static str {
        match self {
            ErrorSubsystem::Api => "API",
            ErrorSubsystem::SiteExplorer => "SITEEXPLORER",
            ErrorSubsystem::Redfish => "REDFISH",
            ErrorSubsystem::Dpu => "DPU",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "API" => Some(ErrorSubsystem::Api),
            "SITEEXPLORER" => Some(ErrorSubsystem::SiteExplorer),
            "REDFISH" => Some(ErrorSubsystem::Redfish),
            "DPU" => Some(ErrorSubsystem::Dpu),
            _ => None,
        }
    }
}

/// A stable operator error code rendered as `SYSTEM-SUBSYSTEM-CODE`
/// (for example `NICO-SITEEXPLORER-100`).
///
/// Built from typed parts rather than a `&str` literal so codes stay greppable
/// end-to-end: a mistyped system or subsystem is a compile error, not a log
/// line that no alert will ever match. Construct with [`ErrorCode::nico`].
///
/// On the wire (JSON for UI/tooling, gRPC metadata) it is the rendered string;
/// it round-trips via [`Display`]/[`FromStr`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrorCode {
    pub system: ErrorSystem,
    pub subsystem: ErrorSubsystem,
    pub code: u32,
}

impl ErrorCode {
    /// A NICo error code for the given subsystem and numeric code.
    pub const fn nico(subsystem: ErrorSubsystem, code: u32) -> Self {
        Self {
            system: ErrorSystem::Nico,
            subsystem,
            code,
        }
    }
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}-{}",
            self.system.as_str(),
            self.subsystem.as_str(),
            self.code
        )
    }
}

/// Returned when an [`ErrorCode`] string cannot be parsed back into typed parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorCodeParseError(String);

impl Display for ErrorCodeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid operator error code: {}", self.0)
    }
}

impl std::error::Error for ErrorCodeParseError {}

impl FromStr for ErrorCode {
    type Err = ErrorCodeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Subsystem tokens never contain `-`, so split the system off the front
        // and the numeric code off the back.
        let parsed = s.split_once('-').and_then(|(system, rest)| {
            let (subsystem, code) = rest.rsplit_once('-')?;
            Some(ErrorCode {
                system: ErrorSystem::from_token(system)?,
                subsystem: ErrorSubsystem::from_token(subsystem)?,
                code: code.parse().ok()?,
            })
        });
        parsed.ok_or_else(|| ErrorCodeParseError(s.to_string()))
    }
}

// Serialized as the rendered `SYSTEM-SUBSYSTEM-CODE` string so the JSON/proto
// contract operator tooling reads stays a plain string.
impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(D::Error::custom)
    }
}

/// Errors that carry an operator-facing [`OperatorErrorSchema`]: a stable
/// [`ErrorCode`], the human-readable text, and an optional mitigation.
///
/// Implement it once per error type and callers can render the schema
/// generically (logs, gRPC metadata, UI) without each call site re-deriving the
/// code or the mitigation.
pub trait OperatorError: Display {
    /// The stable code for this error.
    fn operator_error_code(&self) -> ErrorCode;

    /// A concrete operator action, when NICo knows one. Defaults to none.
    fn operator_mitigation(&self) -> Option<&'static str> {
        None
    }

    /// The full operator-facing schema. The default renders the human-readable
    /// text via [`Display`] and rarely needs overriding.
    fn operator_error_schema(&self) -> OperatorErrorSchema {
        OperatorErrorSchema::new(
            self.operator_error_code(),
            self.to_string(),
            self.operator_mitigation().map(str::to_string),
        )
    }
}

/// Operator-facing error schema suitable for logs, API metadata, and UI display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperatorErrorSchema {
    /// Stable identifier for alerting and centralized log filtering.
    pub error_code: ErrorCode,
    /// Human-readable problem description.
    pub text: String,
    /// Suggested operator action when NICo knows a specific mitigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mitigation: Option<String>,
}

impl OperatorErrorSchema {
    pub fn new(error_code: ErrorCode, text: impl Into<String>, mitigation: Option<String>) -> Self {
        Self {
            error_code,
            text: text.into(),
            mitigation,
        }
    }

    pub fn mitigation_for_log(&self) -> &str {
        self.mitigation.as_deref().unwrap_or("")
    }
}

#[cfg(test)]
mod error_code_tests {
    use super::*;

    #[test]
    fn renders_system_subsystem_code() {
        assert_eq!(
            ErrorCode::nico(ErrorSubsystem::SiteExplorer, 100).to_string(),
            "NICO-SITEEXPLORER-100"
        );
        assert_eq!(
            ErrorCode::nico(ErrorSubsystem::Api, 400).to_string(),
            "NICO-API-400"
        );
    }

    #[test]
    fn round_trips_through_string() {
        for code in [
            ErrorCode::nico(ErrorSubsystem::Api, 503),
            ErrorCode::nico(ErrorSubsystem::SiteExplorer, 145),
            ErrorCode::nico(ErrorSubsystem::Redfish, 500),
            ErrorCode::nico(ErrorSubsystem::Dpu, 134),
        ] {
            assert_eq!(code.to_string().parse(), Ok(code));
        }
    }

    #[test]
    fn rejects_unknown_or_malformed_codes() {
        for bad in ["NICO-BOGUS-100", "NICO-API", "API-400", "NICO-API-xx", ""] {
            assert!(
                bad.parse::<ErrorCode>().is_err(),
                "expected {bad:?} to fail"
            );
        }
    }

    #[test]
    fn serializes_as_rendered_string() {
        let schema = OperatorErrorSchema::new(
            ErrorCode::nico(ErrorSubsystem::SiteExplorer, 100),
            "boom",
            None,
        );
        let json = serde_json::to_value(&schema).expect("serializes");
        assert_eq!(json["error_code"], "NICO-SITEEXPLORER-100");
        let back: OperatorErrorSchema = serde_json::from_value(json).expect("round-trips");
        assert_eq!(back, schema);
    }
}

/// Errors specifically for the (eventual) models crate
#[derive(thiserror::Error, Debug)]
pub enum ModelError {
    #[error("failed to map device to dpu: {0}")]
    DpuMappingError(String),
    #[error("DPU {0} is missing from host snapshot")]
    MissingDpu(MachineId),
    #[error("database type conversion error: {0}")]
    DatabaseTypeConversionError(String),
    #[error("argument is missing in input: {0}")]
    MissingArgument(&'static str),
    #[error("hardware info error: {0}")]
    HardwareInfo(#[from] HardwareInfoError),
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    #[error("{0}")]
    InvalidMachindId(String),
}

impl From<InvalidMachineType> for ModelError {
    #[track_caller]
    fn from(err: InvalidMachineType) -> Self {
        // Having a wrong machine type represents a bug in the code, so track the caller
        let caller = std::panic::Location::caller();
        ModelError::InvalidMachindId(format!("bug: wrong machine type at {caller}: {err}"))
    }
}

pub type ModelResult<T> = Result<T, ModelError>;
