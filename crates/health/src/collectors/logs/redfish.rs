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

//! Shared mapping helpers for Redfish event and log-entry records.

use std::borrow::Cow;

use nv_redfish::resource::Health;
use nv_redfish::schema::event::{DiagnosticDataTypes as EventDiagnosticDataTypes, EventType};
use nv_redfish::schema::log_entry::{EventSeverity, LogDiagnosticDataTypes};
use nv_redfish::schema::resource::Oem;

use super::diagnostic::redfish_enum_string;
use crate::metrics::MetricLabel;
use crate::sink::LogSeverity;

const NVIDIA_ERROR_ID_ATTR: &str = "oem.nvidia.error_id";
const REDFISH_EVENT_TYPE_ATTR: &str = "redfish.event.type";
const REDFISH_EVENT_SEVERITY_ATTR: &str = "redfish.event.severity";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RedfishLogType {
    Cper,
    Xid,
    RedfishEvent,
}

impl RedfishLogType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cper => "cper",
            Self::Xid => "xid",
            Self::RedfishEvent => "redfish_event",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct RedfishLogFields<'a> {
    pub message: Option<&'a str>,
    pub message_args: Option<&'a [String]>,
    /// Set by [`event_diagnostic_is_cper`] or [`log_entry_diagnostic_is_cper`],
    /// or by the presence of the `CPER` object.
    pub has_cper: bool,
}

pub(super) fn redfish_log_type(fields: RedfishLogFields<'_>) -> RedfishLogType {
    if fields.has_cper {
        return RedfishLogType::Cper;
    }

    if fields.message.is_some_and(contains_xid_token)
        || fields
            .message_args
            .unwrap_or_default()
            .iter()
            .any(|arg| contains_xid_token(arg))
    {
        return RedfishLogType::Xid;
    }

    RedfishLogType::RedfishEvent
}

/// Identity recovered from a log entry `Message` when `MessageId` is null.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MessageIdentity<'a> {
    /// Leading token of the message, e.g. `PowerDevicePresence`.
    pub family: Option<&'a str>,
    /// First token inside the parenthesised detail, e.g. `powerdevice1`.
    pub component: Option<&'a str>,
}

/// Parses the OpenBMC `Family` or `Family ( component detail... )` message shape.
///
/// Vendors that populate `MessageId` do not need this; the LiteOn power-shelf
/// PMC leaves `MessageId` null on every entry and puts identity in the text.
/// Prose messages (a second token that is not `(`) yield no identity, so a
/// free-text message is never mistaken for an event family.
pub(super) fn message_identity(message: &str) -> MessageIdentity<'_> {
    let mut tokens = message.split_whitespace();
    let family = tokens.next();
    match tokens.next() {
        None => MessageIdentity {
            family,
            component: None,
        },
        Some("(") => MessageIdentity {
            family,
            component: tokens.next(),
        },
        Some(_) => MessageIdentity::default(),
    }
}

pub(super) fn nvidia_error_id(oem: Option<&Oem>) -> Option<&str> {
    oem.and_then(|oem| {
        oem.additional_properties
            .pointer("/Nvidia/ErrorId")
            .and_then(serde_json::Value::as_str)
    })
}

pub(super) fn redfish_event_type_string(event_type: Option<&EventType>) -> Option<String> {
    event_type
        .filter(|event_type| !matches!(event_type, EventType::UnsupportedValue))
        .and_then(redfish_enum_string)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RedfishSeverity {
    Ok,
    Warning,
    Critical,
    Unknown,
}

impl RedfishSeverity {
    pub(super) fn from_health(health: &Health) -> Option<Self> {
        match health {
            Health::Ok => Some(Self::Ok),
            Health::Warning => Some(Self::Warning),
            Health::Critical => Some(Self::Critical),
            _ => None,
        }
    }

    pub(super) fn from_event_severity(severity: &EventSeverity) -> Self {
        match severity {
            EventSeverity::Ok => Self::Ok,
            EventSeverity::Warning => Self::Warning,
            EventSeverity::Critical => Self::Critical,
            _ => Self::Unknown,
        }
    }

    pub(super) fn from_raw(severity: &str) -> Self {
        if severity.eq_ignore_ascii_case("Critical") {
            Self::Critical
        } else if severity.eq_ignore_ascii_case("Warning") {
            Self::Warning
        } else if severity.eq_ignore_ascii_case("OK") {
            Self::Ok
        } else {
            Self::Unknown
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "Warning",
            Self::Critical => "Critical",
            Self::Unknown => "Unknown",
        }
    }
}

impl From<RedfishSeverity> for LogSeverity {
    fn from(severity: RedfishSeverity) -> Self {
        match severity {
            RedfishSeverity::Critical => Self::Fatal,
            RedfishSeverity::Warning => Self::Warn,
            RedfishSeverity::Ok => Self::Info,
            RedfishSeverity::Unknown => Self::Unspecified,
        }
    }
}

pub(super) fn add_redfish_analyzer_attributes(
    attributes: &mut Vec<MetricLabel>,
    log_type: RedfishLogType,
    severity: RedfishSeverity,
    error_id: Option<&str>,
) {
    attributes.push((
        Cow::Borrowed(REDFISH_EVENT_TYPE_ATTR),
        log_type.as_str().to_string(),
    ));
    attributes.push((
        Cow::Borrowed(REDFISH_EVENT_SEVERITY_ATTR),
        severity.as_str().to_string(),
    ));
    if let Some(error_id) = error_id {
        attributes.push((Cow::Borrowed(NVIDIA_ERROR_ID_ATTR), error_id.to_string()));
    }
}

/// True when `DiagnosticDataType` says the payload is a UEFI CPER record.
///
/// Event records and log entries carry the same two values in two generated
/// enums, so each schema gets its own predicate.
pub(super) fn event_diagnostic_is_cper(diagnostic_data_type: &EventDiagnosticDataTypes) -> bool {
    matches!(
        diagnostic_data_type,
        EventDiagnosticDataTypes::Cper | EventDiagnosticDataTypes::CperSection
    )
}

pub(super) fn log_entry_diagnostic_is_cper(diagnostic_data_type: &LogDiagnosticDataTypes) -> bool {
    matches!(
        diagnostic_data_type,
        LogDiagnosticDataTypes::Cper | LogDiagnosticDataTypes::CperSection
    )
}

fn contains_xid_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 3 {
        return false;
    }

    (0..=bytes.len() - 3).any(|index| {
        let before = index
            .checked_sub(1)
            .and_then(|before| bytes.get(before))
            .copied();
        bytes[index..index + 3].eq_ignore_ascii_case(b"xid")
            && is_xid_boundary(before)
            && is_xid_boundary(bytes.get(index + 3).copied())
    })
}

fn is_xid_boundary(byte: Option<u8>) -> bool {
    byte.map(|byte| !byte.is_ascii_alphanumeric())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    struct TestFields {
        message: Option<&'static str>,
        message_args: &'static [&'static str],
        has_cper: bool,
    }

    impl TestFields {
        fn basic(message: &'static str) -> Self {
            Self {
                message: Some(message),
                message_args: &[],
                has_cper: false,
            }
        }
    }

    fn with_fields<T>(input: TestFields, run: impl FnOnce(RedfishLogFields<'_>) -> T) -> T {
        let message_args: Vec<String> = input
            .message_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        run(RedfishLogFields {
            message: input.message,
            message_args: Some(&message_args),
            has_cper: input.has_cper,
        })
    }

    #[test]
    fn message_identity_cases() {
        check_values(
            [
                Check {
                    scenario: "openbmc family and component",
                    input: "PowerDevicePresence ( powerdevice1 chassis_SN: 613337RXX01X75101UG Assert )",
                    expect: MessageIdentity {
                        family: Some("PowerDevicePresence"),
                        component: Some("powerdevice1"),
                    },
                },
                Check {
                    scenario: "family without detail",
                    input: "BmcSystemBootComplete",
                    expect: MessageIdentity {
                        family: Some("BmcSystemBootComplete"),
                        component: None,
                    },
                },
                Check {
                    scenario: "prose message yields no identity",
                    input: "Platform event occurred",
                    expect: MessageIdentity::default(),
                },
                Check {
                    scenario: "empty message",
                    input: "",
                    expect: MessageIdentity::default(),
                },
            ],
            message_identity,
        );
    }

    fn classify(input: TestFields) -> RedfishLogType {
        with_fields(input, redfish_log_type)
    }

    #[test]
    fn classifies_redfish_log_records() {
        check_values(
            [
                Check {
                    scenario: "bare platform event",
                    input: TestFields::basic("Fan 1 returned to OK"),
                    expect: RedfishLogType::RedfishEvent,
                },
                Check {
                    scenario: "xid in message",
                    input: TestFields::basic("GPU reported XID 94"),
                    expect: RedfishLogType::Xid,
                },
                Check {
                    scenario: "xid in message args",
                    input: TestFields {
                        message: Some("GPU fault"),
                        message_args: &["GPU0", "Xid 79"],
                        has_cper: false,
                    },
                    expect: RedfishLogType::Xid,
                },
                Check {
                    scenario: "non-token xid substring",
                    input: TestFields::basic("oxidized connector warning"),
                    expect: RedfishLogType::RedfishEvent,
                },
                Check {
                    scenario: "cper takes precedence over xid",
                    input: TestFields {
                        message: Some("GPU XID 94"),
                        message_args: &[],
                        has_cper: true,
                    },
                    expect: RedfishLogType::Cper,
                },
            ],
            classify,
        );
    }

    #[test]
    fn cper_diagnostic_data_types_are_recognised() {
        check_values(
            [
                Check {
                    scenario: "event record cper",
                    input: EventDiagnosticDataTypes::Cper,
                    expect: true,
                },
                Check {
                    scenario: "event record cper section",
                    input: EventDiagnosticDataTypes::CperSection,
                    expect: true,
                },
                Check {
                    scenario: "event record oem",
                    input: EventDiagnosticDataTypes::Oem,
                    expect: false,
                },
                Check {
                    scenario: "event record unsupported",
                    input: EventDiagnosticDataTypes::UnsupportedValue,
                    expect: false,
                },
            ],
            |diagnostic_data_type| event_diagnostic_is_cper(&diagnostic_data_type),
        );

        check_values(
            [
                Check {
                    scenario: "log entry cper",
                    input: LogDiagnosticDataTypes::Cper,
                    expect: true,
                },
                Check {
                    scenario: "log entry cper section",
                    input: LogDiagnosticDataTypes::CperSection,
                    expect: true,
                },
                Check {
                    scenario: "log entry oem",
                    input: LogDiagnosticDataTypes::Oem,
                    expect: false,
                },
                Check {
                    scenario: "log entry unsupported",
                    input: LogDiagnosticDataTypes::UnsupportedValue,
                    expect: false,
                },
            ],
            |diagnostic_data_type| log_entry_diagnostic_is_cper(&diagnostic_data_type),
        );
    }

    #[test]
    fn analyzer_attributes_include_type_severity_and_error_id() {
        let mut attributes = Vec::new();

        add_redfish_analyzer_attributes(
            &mut attributes,
            RedfishLogType::RedfishEvent,
            RedfishSeverity::Unknown,
            Some("CPLD-PSEQ-FAULT"),
        );
        add_redfish_analyzer_attributes(
            &mut attributes,
            RedfishLogType::Xid,
            RedfishSeverity::Warning,
            None,
        );
        add_redfish_analyzer_attributes(
            &mut attributes,
            RedfishLogType::Cper,
            RedfishSeverity::Critical,
            None,
        );

        let attributes: Vec<(String, String)> = attributes
            .into_iter()
            .map(|(key, value)| (key.into_owned(), value))
            .collect();

        assert_eq!(
            attributes,
            [
                (
                    "redfish.event.type".to_string(),
                    "redfish_event".to_string()
                ),
                ("redfish.event.severity".to_string(), "Unknown".to_string()),
                (
                    "oem.nvidia.error_id".to_string(),
                    "CPLD-PSEQ-FAULT".to_string()
                ),
                ("redfish.event.type".to_string(), "xid".to_string()),
                ("redfish.event.severity".to_string(), "Warning".to_string()),
                ("redfish.event.type".to_string(), "cper".to_string()),
                ("redfish.event.severity".to_string(), "Critical".to_string()),
            ]
        );
    }

    #[test]
    fn extracts_nvidia_error_id_from_oem_data() {
        let oem: Oem = serde_json::from_value(serde_json::json!({
            "Nvidia": {"ErrorId": "CPLD-PSEQ-FAULT"}
        }))
        .expect("valid Redfish OEM object");

        assert_eq!(nvidia_error_id(Some(&oem)), Some("CPLD-PSEQ-FAULT"));
        assert_eq!(nvidia_error_id(None), None);
    }

    #[test]
    fn event_type_uses_redfish_wire_spelling() {
        check_values(
            [
                Check {
                    scenario: "known event type",
                    input: Some(EventType::Alert),
                    expect: Some("Alert".to_string()),
                },
                Check {
                    scenario: "unsupported event type",
                    input: Some(EventType::UnsupportedValue),
                    expect: None,
                },
                Check {
                    scenario: "missing event type",
                    input: None,
                    expect: None,
                },
            ],
            |event_type| redfish_event_type_string(event_type.as_ref()),
        );
    }

    #[test]
    fn severity_decodes_from_both_schema_enums() {
        check_values(
            [
                Check {
                    scenario: "event record ok",
                    input: Health::Ok,
                    expect: Some(RedfishSeverity::Ok),
                },
                Check {
                    scenario: "event record warning",
                    input: Health::Warning,
                    expect: Some(RedfishSeverity::Warning),
                },
                Check {
                    scenario: "event record critical",
                    input: Health::Critical,
                    expect: Some(RedfishSeverity::Critical),
                },
                // None so the caller falls back to the raw Severity string.
                Check {
                    scenario: "event record unsupported",
                    input: Health::UnsupportedValue,
                    expect: None,
                },
            ],
            |health| RedfishSeverity::from_health(&health),
        );

        check_values(
            [
                Check {
                    scenario: "log entry ok",
                    input: EventSeverity::Ok,
                    expect: RedfishSeverity::Ok,
                },
                Check {
                    scenario: "log entry warning",
                    input: EventSeverity::Warning,
                    expect: RedfishSeverity::Warning,
                },
                Check {
                    scenario: "log entry critical",
                    input: EventSeverity::Critical,
                    expect: RedfishSeverity::Critical,
                },
                // Log entries carry no raw severity string to fall back to.
                Check {
                    scenario: "log entry unsupported",
                    input: EventSeverity::UnsupportedValue,
                    expect: RedfishSeverity::Unknown,
                },
            ],
            |severity| RedfishSeverity::from_event_severity(&severity),
        );
    }

    #[test]
    fn raw_severity_tolerates_bmc_capitalisation() {
        check_values(
            [
                Check {
                    scenario: "canonical",
                    input: "Critical",
                    expect: RedfishSeverity::Critical,
                },
                Check {
                    scenario: "shouted",
                    input: "CRITICAL",
                    expect: RedfishSeverity::Critical,
                },
                Check {
                    scenario: "lowercase",
                    input: "warning",
                    expect: RedfishSeverity::Warning,
                },
                Check {
                    scenario: "mixed case",
                    input: "Ok",
                    expect: RedfishSeverity::Ok,
                },
                Check {
                    scenario: "unrecognised",
                    input: "meltdown",
                    expect: RedfishSeverity::Unknown,
                },
                Check {
                    scenario: "empty",
                    input: "",
                    expect: RedfishSeverity::Unknown,
                },
            ],
            RedfishSeverity::from_raw,
        );
    }

    #[test]
    fn severity_renders_redfish_and_otel_spellings() {
        check_values(
            [
                Check {
                    scenario: "ok",
                    input: RedfishSeverity::Ok,
                    expect: ("OK", "INFO"),
                },
                Check {
                    scenario: "warning",
                    input: RedfishSeverity::Warning,
                    expect: ("Warning", "WARN"),
                },
                Check {
                    scenario: "critical",
                    input: RedfishSeverity::Critical,
                    expect: ("Critical", "FATAL"),
                },
                Check {
                    scenario: "unknown",
                    input: RedfishSeverity::Unknown,
                    expect: ("Unknown", "UNSPECIFIED"),
                },
            ],
            |severity| (severity.as_str(), LogSeverity::from(severity).as_str()),
        );
    }
}
