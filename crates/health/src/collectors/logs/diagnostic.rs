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

//! Helpers for carrying Redfish diagnostic payload fields into log sinks.
//!
//! Redfish exposes these fields as part of the event or log-entry schema.
//! The health crate intentionally does not parse CPER or other diagnostic
//! formats. It preserves the payload and related Redfish metadata so each sink,
//! or its downstream consumer, can decide how to handle the opaque body.

use std::borrow::Cow;

use serde::Serialize;

use crate::metrics::MetricLabel;
use crate::sink::DiagnosticLogRecord;

const DIAGNOSTIC_DATA_TYPE_ATTR: &str = "redfish.diagnostic_data.type";
const DIAGNOSTIC_DATA_OEM_TYPE_ATTR: &str = "redfish.diagnostic_data.oem_type";
const DIAGNOSTIC_DATA_ADDITIONAL_URI_ATTR: &str = "redfish.diagnostic_data.additional_uri";
const DIAGNOSTIC_DATA_SIZE_BYTES_ATTR: &str = "redfish.diagnostic_data.size_bytes";
const PARENT_MESSAGE_ID_ATTR: &str = "redfish.parent.message_id";
const PARENT_EVENT_ID_ATTR: &str = "redfish.parent.event_id";
const PARENT_LOG_ENTRY_ID_ATTR: &str = "redfish.parent.log_entry_id";

/// Borrowed diagnostic fields extracted from one Redfish event or log entry.
pub(crate) struct DiagnosticPayload<'a> {
    /// Opaque Redfish `DiagnosticData` payload, commonly base64 text for CPER.
    pub diagnostic_data: Option<&'a str>,

    /// Redfish `DiagnosticDataType`, serialized with the schema wire spelling.
    pub diagnostic_data_type: Option<String>,

    /// Vendor-specific diagnostic data type when Redfish provides one.
    pub oem_diagnostic_data_type: Option<&'a str>,

    /// Redfish `AdditionalDataURI`; this module forwards the URI but does not
    /// fetch it.
    pub additional_data_uri: Option<&'a str>,

    /// Optional Redfish size metadata for `AdditionalDataURI`.
    pub additional_data_size_bytes: Option<i64>,

    /// Parent Redfish message id for correlating the diagnostic fields.
    pub message_id: Option<&'a str>,

    /// Parent Redfish event id for correlating the diagnostic fields.
    pub event_id: Option<&'a str>,

    /// Parent Redfish log entry id for correlating the diagnostic fields.
    pub log_entry_id: Option<&'a str>,
}

/// Flattens generated Redfish nullable option fields.
///
/// Redfish generated models use `Option<Option<T>>` to distinguish an absent
/// property from an explicit JSON null. Diagnostic export treats both as absent.
pub(crate) fn nullable_ref<T>(value: &Option<Option<T>>) -> Option<&T> {
    value.as_ref().and_then(Option::as_ref)
}

/// Flattens generated Redfish nullable string fields.
///
/// See [`nullable_ref`] for why explicit null and missing values are handled the
/// same way in diagnostic fields.
pub(crate) fn nullable_str(value: &Option<Option<String>>) -> Option<&str> {
    nullable_ref(value).map(String::as_str)
}

/// Serializes generated Redfish enums through serde to preserve wire spelling.
pub(crate) fn redfish_enum_string<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .and_then(|value| serde_json::from_str::<String>(&value).ok())
}

/// Builds diagnostic fields from Redfish payload fields.
///
/// URI-only diagnostics are still forwarded because the URI and size metadata
/// may be enough for a downstream collector to fetch or correlate the payload.
pub(crate) fn make_diagnostic_record(
    payload: DiagnosticPayload<'_>,
) -> Option<DiagnosticLogRecord> {
    if payload.diagnostic_data.is_none() && payload.additional_data_uri.is_none() {
        return None;
    }

    // These attributes leave health through generic sinks, so keep Redfish
    // schema fields namespaced instead of competing with sink-level keys.
    let mut attributes: Vec<MetricLabel> = [
        (
            DIAGNOSTIC_DATA_TYPE_ATTR,
            payload.diagnostic_data_type.as_deref(),
        ),
        (
            DIAGNOSTIC_DATA_OEM_TYPE_ATTR,
            payload.oem_diagnostic_data_type,
        ),
        (
            DIAGNOSTIC_DATA_ADDITIONAL_URI_ATTR,
            payload.additional_data_uri,
        ),
        (PARENT_MESSAGE_ID_ATTR, payload.message_id),
        (PARENT_EVENT_ID_ATTR, payload.event_id),
        (PARENT_LOG_ENTRY_ID_ATTR, payload.log_entry_id),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (Cow::Borrowed(key), value.to_string())))
    .collect();

    if let Some(size_bytes) = payload.additional_data_size_bytes {
        attributes.push((
            Cow::Borrowed(DIAGNOSTIC_DATA_SIZE_BYTES_ATTR),
            size_bytes.to_string(),
        ));
    }

    // Keep the diagnostic body opaque. CPER and vendor formats are parsed by
    // downstream consumers that understand their schema.
    Some(DiagnosticLogRecord {
        body: payload.diagnostic_data.unwrap_or_default().to_string(),
        attributes,
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::scenarios;

    use super::*;

    /// Minimal generated-enum stand-in used to verify serde wire spelling.
    #[derive(Serialize)]
    enum TestDiagnosticDataType {
        #[serde(rename = "CPERSection")]
        CperSection,
    }

    /// Input variants exercised by the table-driven diagnostic tests.
    enum DiagnosticInput {
        EnumWireSpelling,
        Record(DiagnosticRecordInput),
    }

    /// Owned test input mirroring the borrowed production payload fields.
    struct DiagnosticRecordInput {
        diagnostic_data: Option<&'static str>,
        diagnostic_data_type: Option<String>,
        oem_diagnostic_data_type: Option<&'static str>,
        additional_data_uri: Option<&'static str>,
        additional_data_size_bytes: Option<i64>,
        message_id: Option<&'static str>,
        event_id: Option<&'static str>,
        log_entry_id: Option<&'static str>,
    }

    /// Expected output variants from diagnostic helper scenarios.
    #[derive(Debug, PartialEq)]
    enum DiagnosticOutput {
        EnumWireSpelling(Option<String>),
        Record(Option<DiagnosticRecordOutput>),
    }

    /// Owned diagnostic record output used for scenario comparisons.
    #[derive(Debug, PartialEq)]
    struct DiagnosticRecordOutput {
        body: String,
        attributes: Vec<(String, String)>,
    }

    /// Runs one diagnostic helper scenario and normalizes owned output.
    fn run_diagnostic_case(input: DiagnosticInput) -> Result<DiagnosticOutput, ()> {
        let output = match input {
            DiagnosticInput::EnumWireSpelling => DiagnosticOutput::EnumWireSpelling(
                redfish_enum_string(&TestDiagnosticDataType::CperSection),
            ),
            DiagnosticInput::Record(input) => {
                let record = make_diagnostic_record(DiagnosticPayload {
                    diagnostic_data: input.diagnostic_data,
                    diagnostic_data_type: input.diagnostic_data_type,
                    oem_diagnostic_data_type: input.oem_diagnostic_data_type,
                    additional_data_uri: input.additional_data_uri,
                    additional_data_size_bytes: input.additional_data_size_bytes,
                    message_id: input.message_id,
                    event_id: input.event_id,
                    log_entry_id: input.log_entry_id,
                })
                .map(|record| DiagnosticRecordOutput {
                    body: record.body,
                    attributes: record
                        .attributes
                        .into_iter()
                        .map(|(key, value)| (key.into_owned(), value))
                        .collect(),
                });

                DiagnosticOutput::Record(record)
            }
        };

        Ok(output)
    }

    /// Builds a complete diagnostic payload input with common parent metadata.
    fn record_input(
        diagnostic_data: Option<&'static str>,
        diagnostic_data_type: Option<&'static str>,
        additional_data_uri: Option<&'static str>,
    ) -> DiagnosticInput {
        DiagnosticInput::Record(DiagnosticRecordInput {
            diagnostic_data,
            diagnostic_data_type: diagnostic_data_type.map(str::to_string),
            oem_diagnostic_data_type: None,
            additional_data_uri,
            additional_data_size_bytes: Some(2048),
            message_id: Some("ResourceEvent.1.0.ResourceErrorsDetected"),
            event_id: Some("ev-1"),
            log_entry_id: Some("42"),
        })
    }

    /// Builds the expected record output with owned attribute strings.
    fn expected_record(body: &str, attributes: &[(&str, &str)]) -> DiagnosticOutput {
        DiagnosticOutput::Record(Some(DiagnosticRecordOutput {
            body: body.to_string(),
            attributes: attributes
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        }))
    }

    /// Verifies diagnostic payload conversion cases.
    #[test]
    fn diagnostic_payload_fields() {
        scenarios!(run_diagnostic_case:
            "enum wire spelling" {
                DiagnosticInput::EnumWireSpelling => Yields(
                    DiagnosticOutput::EnumWireSpelling(Some("CPERSection".to_string()))
                ),
            }

            "opaque diagnostic payload" {
                record_input(
                    Some("base64-payload"),
                    Some("CPER"),
                    Some("/redfish/v1/Log/1/data"),
                ) => Yields(expected_record(
                    "base64-payload",
                    &[
                        ("redfish.diagnostic_data.type", "CPER"),
                        (
                            "redfish.diagnostic_data.additional_uri",
                            "/redfish/v1/Log/1/data",
                        ),
                        (
                            "redfish.parent.message_id",
                            "ResourceEvent.1.0.ResourceErrorsDetected",
                        ),
                        ("redfish.parent.event_id", "ev-1"),
                        ("redfish.parent.log_entry_id", "42"),
                        ("redfish.diagnostic_data.size_bytes", "2048"),
                    ],
                )),
            }

            "absent payload and uri" {
                DiagnosticInput::Record(DiagnosticRecordInput {
                    diagnostic_data: None,
                    diagnostic_data_type: Some("CPER".to_string()),
                    oem_diagnostic_data_type: None,
                    additional_data_uri: None,
                    additional_data_size_bytes: None,
                    message_id: None,
                    event_id: None,
                    log_entry_id: None,
                }) => Yields(DiagnosticOutput::Record(None)),
            }
        );
    }
}
