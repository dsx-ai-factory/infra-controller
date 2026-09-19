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

use std::net::IpAddr;

use mac_address::MacAddress;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

const REDFISH_ERROR_MESSAGE_LIMIT: usize = 1024;
const REDACTED: &str = "REDACTED";
const UNRECOGNIZED_REDFISH_ERROR_RESPONSE: &str = "<unrecognized Redfish error response>";

/// Logs the diagnostic fields shared by HTTP failures from both Redfish clients.
///
/// The complete response body and DMTF `MessageArgs` are deliberately excluded.
/// Only a bounded message extracted from the DMTF error envelope is recorded,
/// after masking sensitive values supplied by the caller.
pub fn log_redfish_http_error<'a>(
    backend: &str,
    operation: &str,
    url: &str,
    http_status: u16,
    response_body: &str,
    sensitive_values: impl IntoIterator<Item = &'a str>,
) {
    let error = redfish_error_message_for_log(response_body, sensitive_values);
    tracing::warn!(
        backend,
        operation,
        url = %url,
        http_status,
        error = %error,
        "external call failed"
    );
}

/// Redacts sensitive values from a Redfish response body before it is returned
/// to code that may format or log the complete HTTP error.
///
/// Valid JSON is decoded before masking its string values, object keys, and
/// matching non-string scalar values, so escaped or unquoted forms of a
/// credential cannot bypass redaction. JSON containers that cannot be parsed
/// safely are replaced rather than forwarded, while ordinary non-JSON responses
/// retain their original formatting and receive best-effort literal masking.
pub fn redact_redfish_response_body<'a>(
    response_body: &str,
    sensitive_values: impl IntoIterator<Item = &'a str>,
) -> String {
    let sensitive_values = sensitive_values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if sensitive_values.is_empty() {
        return response_body.to_string();
    }

    let mut response = match serde_json::from_str::<serde_json::Value>(response_body) {
        Ok(response) => response,
        Err(_) if starts_with_json_container(response_body) => {
            return UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string();
        }
        Err(_) => return mask_all(response_body, sensitive_values),
    };
    let sensitive_scalars = sensitive_values
        .iter()
        .filter_map(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .filter(|value| {
            matches!(
                value,
                serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_)
            )
        })
        .collect::<Vec<_>>();
    if !redact_json_values(&mut response, &sensitive_values, &sensitive_scalars) {
        match json_contains_sensitive_value(response_body, &sensitive_values, &sensitive_scalars) {
            Ok(false) => return response_body.to_string(),
            Ok(true) => {}
            Err(_) => return UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string(),
        }
    }
    serde_json::to_string(&response)
        .unwrap_or_else(|_| UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string())
}

/// Detects response bodies that should have been JSON objects or arrays.
///
/// `serde_json` rejects leading UTF-8 BOMs, so remove any leading BOMs and
/// whitespace before deciding whether a parse failure must fail closed.
fn starts_with_json_container(response_body: &str) -> bool {
    let response_body = response_body
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}');
    response_body
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'{' | b'['))
}

/// Matches sensitive JSON scalars, including equivalent numeric spellings.
///
/// Floating-point comparison intentionally biases toward redaction. Extremely
/// large adjacent integers can map to the same `f64`, but masking an additional
/// diagnostic value is safer than returning a credential-bearing response.
fn json_scalar_is_sensitive(
    value: &serde_json::Value,
    sensitive_scalars: &[serde_json::Value],
) -> bool {
    sensitive_scalars
        .iter()
        .any(|sensitive| match (sensitive, value) {
            (serde_json::Value::Number(sensitive), serde_json::Value::Number(value)) => {
                sensitive == value
                    || sensitive
                        .as_f64()
                        .zip(value.as_f64())
                        .is_some_and(|(sensitive, value)| sensitive == value)
            }
            _ => sensitive == value,
        })
}

/// Checks every JSON token before the original representation is returned.
/// Unlike `serde_json::Value`, this visitor observes object entries that are
/// later overwritten by duplicate keys.
fn json_contains_sensitive_value(
    response_body: &str,
    sensitive_values: &[&str],
    sensitive_scalars: &[serde_json::Value],
) -> serde_json::Result<bool> {
    let detector = SensitiveJsonDetector {
        sensitive_values,
        sensitive_scalars,
    };
    let mut deserializer = serde_json::Deserializer::from_str(response_body);
    let contains_sensitive_value = detector.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(contains_sensitive_value)
}

#[derive(Clone, Copy)]
struct SensitiveJsonDetector<'a, 's> {
    sensitive_values: &'s [&'a str],
    sensitive_scalars: &'s [serde_json::Value],
}

impl SensitiveJsonDetector<'_, '_> {
    fn string_is_sensitive(self, value: &str) -> bool {
        mask_all(value, self.sensitive_values.iter().copied()) != value
    }

    fn scalar_is_sensitive(self, value: serde_json::Value) -> bool {
        json_scalar_is_sensitive(&value, self.sensitive_scalars)
    }
}

impl<'de> DeserializeSeed<'de> for SensitiveJsonDetector<'_, '_> {
    type Value = bool;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for SensitiveJsonDetector<'_, '_> {
    type Value = bool;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(self.scalar_is_sensitive(serde_json::Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(self.scalar_is_sensitive(serde_json::Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(self.scalar_is_sensitive(serde_json::Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .is_some_and(|value| self.scalar_is_sensitive(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(self.string_is_sensitive(value))
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(self.string_is_sensitive(value))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(self.string_is_sensitive(&value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(self.scalar_is_sensitive(serde_json::Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(self.scalar_is_sensitive(serde_json::Value::Null))
    }

    fn visit_seq<A>(self, mut values: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut contains_sensitive_value = false;
        while let Some(value_is_sensitive) = values.next_element_seed(self)? {
            contains_sensitive_value |= value_is_sensitive;
        }
        Ok(contains_sensitive_value)
    }

    fn visit_map<A>(self, mut values: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut contains_sensitive_value = false;
        while let Some(key_is_sensitive) = values.next_key_seed(self)? {
            contains_sensitive_value |= key_is_sensitive;
            contains_sensitive_value |= values.next_value_seed(self)?;
        }
        Ok(contains_sensitive_value)
    }
}

/// Redacts JSON values and object keys, returning whether anything changed.
fn redact_json_values(
    value: &mut serde_json::Value,
    sensitive_values: &[&str],
    sensitive_scalars: &[serde_json::Value],
) -> bool {
    match value {
        serde_json::Value::String(value) => {
            let redacted = mask_all(value, sensitive_values.iter().copied());
            if redacted == *value {
                false
            } else {
                *value = redacted;
                true
            }
        }
        serde_json::Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |= redact_json_values(value, sensitive_values, sensitive_scalars);
            }
            changed
        }
        serde_json::Value::Object(values) => {
            let mut changed = false;
            let original = std::mem::take(values);
            for (key, mut value) in original {
                let redacted_key = mask_all(&key, sensitive_values.iter().copied());
                changed |= redacted_key != key;
                changed |= redact_json_values(&mut value, sensitive_values, sensitive_scalars);
                insert_json_object_entry(values, redacted_key, value);
            }
            changed
        }
        scalar @ (serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)) => {
            if json_scalar_is_sensitive(scalar, sensitive_scalars) {
                *scalar = serde_json::Value::String(REDACTED.to_string());
                true
            } else {
                false
            }
        }
    }
}

/// Keeps every diagnostic value if distinct sensitive keys collapse to the
/// same redacted spelling.
fn insert_json_object_entry(
    values: &mut serde_json::Map<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    if !values.contains_key(&key) {
        values.insert(key, value);
        return;
    }

    let mut suffix = 2;
    loop {
        let candidate = format!("{key}_{suffix}");
        if !values.contains_key(&candidate) {
            values.insert(candidate, value);
            return;
        }
        suffix += 1;
    }
}

/// Extracts a bounded, log-safe message from a DMTF Redfish error response.
fn redfish_error_message_for_log<'a>(
    response_body: &str,
    sensitive_values: impl IntoIterator<Item = &'a str>,
) -> String {
    let message = extract_redfish_error_message(response_body)
        .unwrap_or_else(|| UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string());
    truncate_redfish_error_message(mask_all(&message, sensitive_values))
}

fn extract_redfish_error_message(response_body: &str) -> Option<String> {
    let response: serde_json::Value = serde_json::from_str(response_body).ok()?;
    let error = response.get("error")?.as_object()?;

    if let Some(extended_info) = error
        .get("@Message.ExtendedInfo")
        .and_then(serde_json::Value::as_array)
    {
        let messages = extended_info
            .iter()
            .filter_map(|entry| {
                let entry = entry.as_object()?;
                usable_redfish_message(entry.get("Message"))
                    .or_else(|| usable_redfish_message(entry.get("MessageId")))
            })
            .collect::<Vec<_>>();
        if !messages.is_empty() {
            return Some(messages.join("; "));
        }
    }

    usable_redfish_message(error.get("message"))
        .or_else(|| usable_redfish_message(error.get("code")))
        .map(str::to_string)
}

fn usable_redfish_message(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
}

fn truncate_redfish_error_message(mut message: String) -> String {
    const ELLIPSIS: &str = "…";

    if message.len() <= REDFISH_ERROR_MESSAGE_LIMIT {
        return message;
    }

    let mut end = REDFISH_ERROR_MESSAGE_LIMIT - ELLIPSIS.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str(ELLIPSIS);
    message
}

/// Masks the union of all sensitive-value matches, including overlapping ones,
/// without retaining a range for every occurrence.
fn mask_all<'a>(text: &str, sensitive_values: impl IntoIterator<Item = &'a str>) -> String {
    let sensitive_values = sensitive_values
        .into_iter()
        .filter(|value| !value.is_empty())
        .map(str::as_bytes)
        .collect::<Vec<_>>();
    let text_bytes = text.as_bytes();

    let mut redacted = String::with_capacity(text.len());
    let mut copied_until = 0;
    let mut active_range: Option<(usize, usize)> = None;
    for start in 0..text_bytes.len() {
        let Some(end) = sensitive_values
            .iter()
            .filter(|value| text_bytes[start..].starts_with(value))
            .map(|value| start + value.len())
            .max()
        else {
            continue;
        };

        match active_range {
            Some((range_start, range_end)) if start <= range_end => {
                active_range = Some((range_start, range_end.max(end)));
            }
            Some((range_start, range_end)) => {
                redacted.push_str(&text[copied_until..range_start]);
                redacted.push_str(REDACTED);
                copied_until = range_end;
                active_range = Some((start, end));
            }
            None => active_range = Some((start, end)),
        }
    }

    if let Some((start, end)) = active_range {
        redacted.push_str(&text[copied_until..start]);
        redacted.push_str(REDACTED);
        copied_until = end;
    }
    redacted.push_str(&text[copied_until..]);
    redacted
}

/// `parse_uri_host_ip` parses an IPv4 literal or a bare/bracketed IPv6 literal.
///
/// `http::Uri::host` retains IPv6 brackets, while other callers can provide
/// bare addresses. Accept both forms here; hostnames return `None`.
pub fn parse_uri_host_ip(host: &str) -> Option<IpAddr> {
    host.parse().ok().or_else(|| {
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .and_then(|host| host.parse().ok())
    })
}

/// Formats the `host` parameter for an HTTP `Forwarded` header.
///
/// RFC 7239 requires an IPv6 host to use brackets inside a quoted string.
/// Hostnames and IPv4 literals retain the existing token form.
pub fn format_forwarded_host_parameter(host: &str) -> String {
    match parse_uri_host_ip(host) {
        Some(IpAddr::V6(host)) => format!("host=\"[{host}]\""),
        _ => format!("host={host}"),
    }
}

/// Data needed to access BMC via Redfish.
///
/// It is regular host, port pair + MAC address that identifies auth
/// key identifier for the access.
pub struct BmcAccessInfo {
    pub host: String,
    pub port: Option<u16>,
    pub mac_address: MacAddress,
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedFieldKind, capture_logs};
    use carbide_test_support::{Check, check_values, value_scenarios};

    use super::*;

    #[test]
    fn redfish_error_message_uses_dmtf_fallbacks_without_message_arguments() {
        check_values(
            [
                Check {
                    scenario: "extended information takes precedence",
                    input: r#"{
                        "error": {
                            "code": "Base.1.0.FallbackCode",
                            "message": "fallback message",
                            "@Message.ExtendedInfo": [
                                {
                                    "Message": "first detail",
                                    "MessageId": "Base.1.0.Ignored",
                                    "MessageArgs": ["must-not-be-logged"]
                                },
                                {
                                    "Message": "  ",
                                    "MessageId": "Base.1.0.SecondDetail"
                                }
                            ]
                        }
                    }"#,
                    expect: "first detail; Base.1.0.SecondDetail".to_string(),
                },
                Check {
                    scenario: "top-level message fallback",
                    input: r#"{"error":{"message":"top-level message","code":"Code"}}"#,
                    expect: "top-level message".to_string(),
                },
                Check {
                    scenario: "top-level code fallback",
                    input: r#"{"error":{"code":"Base.1.0.CodeOnly"}}"#,
                    expect: "Base.1.0.CodeOnly".to_string(),
                },
                Check {
                    scenario: "malformed response",
                    input: "not JSON",
                    expect: UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string(),
                },
            ],
            |response| redfish_error_message_for_log(response, std::iter::empty()),
        );
    }

    #[test]
    fn redfish_error_message_redacts_decoded_and_overlapping_sensitive_values() {
        let response = r#"{
            "error": {
                "@Message.ExtendedInfo": [{
                    "Message": "password p\u00e4ss\/\uD83D\uDD11 and abcdefghi rejected"
                }]
            }
        }"#;

        assert_eq!(
            redfish_error_message_for_log(response, ["päss/🔑", "abcdef", "defghi"]),
            "password REDACTED and REDACTED rejected"
        );
    }

    #[test]
    fn redfish_response_body_redaction_uses_decoded_json_strings() {
        let response = r#"{
            "error": {
                "@Message.ExtendedInfo": [{
                    "Message": "credential s\u0065cret rejected",
                    "MessageArgs": ["s\u0065cret"],
                    "credential-s\u0065cret": "rejected",
                    "s\u0065cret": "first key",
                    "s\u0065crets\u0065cret": "second key"
                }]
            }
        }"#;

        let redacted = redact_redfish_response_body(response, ["secret"]);
        let redacted: serde_json::Value =
            serde_json::from_str(&redacted).expect("redacted response remains valid JSON");

        assert_eq!(
            redacted["error"]["@Message.ExtendedInfo"][0]["Message"],
            "credential REDACTED rejected"
        );
        assert_eq!(
            redacted["error"]["@Message.ExtendedInfo"][0]["MessageArgs"][0],
            "REDACTED"
        );
        assert_eq!(
            redacted["error"]["@Message.ExtendedInfo"][0]["credential-REDACTED"],
            "rejected"
        );
        assert_eq!(
            redacted["error"]["@Message.ExtendedInfo"][0]["REDACTED"],
            "first key"
        );
        assert_eq!(
            redacted["error"]["@Message.ExtendedInfo"][0]["REDACTED_2"],
            "second key"
        );
    }

    #[test]
    fn redfish_response_body_redaction_masks_matching_non_string_json_scalars() {
        check_values(
            [
                Check {
                    scenario: "numeric secret",
                    input: (r#"{"credential":1234}"#, "1234"),
                    expect: serde_json::json!({"credential": REDACTED}),
                },
                Check {
                    scenario: "equivalent integer and decimal spellings",
                    input: (r#"{"credential":1.0}"#, "1"),
                    expect: serde_json::json!({"credential": REDACTED}),
                },
                Check {
                    scenario: "boolean-like secret",
                    input: (r#"{"credential":true}"#, "true"),
                    expect: serde_json::json!({"credential": REDACTED}),
                },
                Check {
                    scenario: "null-like secret",
                    input: (r#"{"credential":null}"#, "null"),
                    expect: serde_json::json!({"credential": REDACTED}),
                },
            ],
            |(response, sensitive_value)| {
                let redacted = redact_redfish_response_body(response, [sensitive_value]);
                serde_json::from_str(&redacted).expect("redacted response remains valid JSON")
            },
        );
    }

    #[test]
    fn redfish_response_body_redaction_sanitizes_overwritten_duplicate_values() {
        check_values(
            [
                Check {
                    scenario: "escaped string",
                    input: (r#"{"detail":"s\u0065cret","detail":"safe"}"#, "secret"),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "escaped string in a nested array",
                    input: (
                        r#"{"detail":{"messages":["s\u0065cret"]},"detail":"safe"}"#,
                        "secret",
                    ),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "escaped object key",
                    input: (
                        r#"{"detail":{"credential-s\u0065cret":"rejected"},"detail":"safe"}"#,
                        "secret",
                    ),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "number",
                    input: (r#"{"detail":1234,"detail":"safe"}"#, "1234"),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "equivalent integer and decimal spellings",
                    input: (r#"{"detail":1.0,"detail":"safe"}"#, "1"),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "boolean",
                    input: (r#"{"detail":true,"detail":"safe"}"#, "true"),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
                Check {
                    scenario: "null",
                    input: (r#"{"detail":null,"detail":"safe"}"#, "null"),
                    expect: r#"{"detail":"safe"}"#.to_string(),
                },
            ],
            |(response, sensitive_value)| redact_redfish_response_body(response, [sensitive_value]),
        );
    }

    #[test]
    fn redfish_response_body_redaction_preserves_an_unmatched_body() {
        let response = "{\n  \"error\": {\"message\": \"ordinary failure\"}\n}";

        assert_eq!(redact_redfish_response_body(response, ["secret"]), response);
    }

    #[test]
    fn redfish_response_body_redaction_masks_plain_text_fallback() {
        assert_eq!(
            redact_redfish_response_body("credential secret rejected", ["secret"]),
            "credential REDACTED rejected"
        );
    }

    #[test]
    fn redfish_response_body_redaction_fails_closed_for_unparseable_json_containers() {
        check_values(
            [
                Check {
                    scenario: "above the JSON depth limit",
                    input: format!("{}\"s\\u0065cret\"{}", "[".repeat(128), "]".repeat(128)),
                    expect: UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string(),
                },
                Check {
                    scenario: "UTF-8 BOM before escaped credential",
                    input: "\u{feff}{\"credential\":\"s\\u0065cret\"}".to_string(),
                    expect: UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string(),
                },
                Check {
                    scenario: "repeated UTF-8 BOMs before escaped credential",
                    input: "\u{feff} \u{feff}\n{\"credential\":\"s\\u0065cret\"}".to_string(),
                    expect: UNRECOGNIZED_REDFISH_ERROR_RESPONSE.to_string(),
                },
            ],
            |response| redact_redfish_response_body(&response, ["secret"]),
        );
    }

    #[test]
    fn redfish_error_message_has_a_utf8_safe_length_limit() {
        let response = serde_json::json!({
            "error": {"message": "é".repeat(REDFISH_ERROR_MESSAGE_LIMIT)}
        })
        .to_string();

        let message = redfish_error_message_for_log(&response, std::iter::empty());

        assert_eq!(message.len(), REDFISH_ERROR_MESSAGE_LIMIT - 1);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn redfish_http_error_log_has_the_canonical_structured_fields() {
        let logs = capture_logs(|| {
            log_redfish_http_error(
                "redfish",
                "set_boot_order_dpu_first",
                "https://bmc.example/redfish/v1/Systems/1",
                500,
                r#"{
                    "error": {
                        "message": "fallback",
                        "@Message.ExtendedInfo": [{"Message": "internal service error"}]
                    }
                }"#,
                std::iter::empty(),
            );
        });

        let log = logs.first().expect("one Redfish failure log");
        assert_eq!(logs.len(), 1);
        assert_eq!(log.level, tracing::Level::WARN);
        assert_eq!(log.message, "external call failed");
        assert_eq!(log.field("backend"), Some("redfish"));
        assert_eq!(log.field("operation"), Some("set_boot_order_dpu_first"));
        assert_eq!(
            log.field("url"),
            Some("https://bmc.example/redfish/v1/Systems/1")
        );
        assert_eq!(log.field("http_status"), Some("500"));
        assert_eq!(log.field_kind("http_status"), Some(CapturedFieldKind::U64));
        assert_eq!(log.field("error"), Some("internal service error"));
        assert_eq!(log.field("error_message"), None);
    }

    #[test]
    fn uri_host_parser_accepts_bare_and_bracketed_ip_literals() {
        value_scenarios!(run = |host| parse_uri_host_ip(host).map(|ip| ip.to_string());
            "IP literals" {
                "192.0.2.10" => Some("192.0.2.10".to_string()),
                "2001:db8::10" => Some("2001:db8::10".to_string()),
                "[2001:db8::10]" => Some("2001:db8::10".to_string()),
            }

            "hostname" {
                "bmc.example.com" => None,
            }
        );
    }

    #[test]
    fn forwarded_host_parameter_quotes_ipv6_literals() {
        value_scenarios!(run = format_forwarded_host_parameter;
            "token form" {
                "bmc.example.com" => "host=bmc.example.com".to_string(),
                "192.0.2.10" => "host=192.0.2.10".to_string(),
            }

            "quoted IPv6 form" {
                "2001:db8::10" => "host=\"[2001:db8::10]\"".to_string(),
                "[2001:db8::10]" => "host=\"[2001:db8::10]\"".to_string(),
            }
        );
    }
}
