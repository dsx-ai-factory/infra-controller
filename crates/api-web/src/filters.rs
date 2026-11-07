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

/*
 * This module has to be called 'filters'.
 * Askama makes all these functions accessible as template filters.
 */

#![allow(
    unreachable_pub,
    reason = "askama::filter_fn emits public helper items inside this template-filter module"
)]

use std::collections::BTreeSet;
use std::fmt::{Display, Write};
use std::str::FromStr;

use askama_escape::Escaper;
use carbide_uuid::machine::MachineId;

/// Generates HTML links for Machine IDs
#[askama::filter_fn]
pub(super) fn machine_id_link(
    id: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    machine_link(id, "machine")
}

/// Generates a formatted link for Machine IDs to a predefined path
pub(super) fn machine_link(id: impl Display, path: impl Display) -> ::askama::Result<String> {
    let id = id.to_string();
    let link_path: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();

    let short_id = if MachineId::from_str(&id).is_err() {
        // Not a Machine ID. Escape HTML to make it safe for post processing with safe filter
        let mut output = String::new();
        askama_escape::Html.write_escaped(&mut output, &id)?;
        return Ok(output);
    } else {
        // "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0" -> "acguc0"
        &id[id.len() - 6..]
    };

    let formatted = format!(
        r#"
    <a href="/admin/{path}/{link_path}">
        <div class="machine_id">
            <div>{id}</div><div>{short_id}</div>
        </div>
    </a>"#
    );

    Ok(formatted)
}

fn escaped_shortened_id_link(id: impl Display, path: impl Display) -> ::askama::Result<String> {
    // Sanitize ID for HTML content and links (it can contain arbitrary content)
    let id = id.to_string();
    if id == "Unlinked" || id.is_empty() {
        return Ok("Unlinked".to_string());
    }
    let link_path: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();

    let mut escaped_id = String::new();
    askama_escape::Html.write_escaped(&mut escaped_id, &id)?;

    if id.chars().count() <= 6 {
        return Ok(format!(
            r#"<a href="/admin/{path}/{link_path}">{escaped_id}</a>"#
        ));
    }

    let short_id = id
        .char_indices()
        .rev()
        .nth(5)
        .map(|(index, _)| &id[index..])
        .unwrap_or(&id);
    let mut escaped_short_id = String::new();
    askama_escape::Html.write_escaped(&mut escaped_short_id, short_id)?;
    let formatted = format!(
        r#"
    <a href="/admin/{path}/{link_path}">
        <div class="machine_id">
            <div>{escaped_id}</div><div>{escaped_short_id}</div>
        </div>
    </a>"#
    );

    Ok(formatted)
}

#[askama::filter_fn]
pub(super) fn rack_id_link(
    id: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    escaped_shortened_id_link(id, "rack")
}

#[askama::filter_fn]
pub(super) fn power_shelf_id_link(
    id: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    escaped_shortened_id_link(id, "power-shelf")
}

#[askama::filter_fn]
pub(super) fn switch_id_link(
    id: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    escaped_shortened_id_link(id, "switch")
}

/// Formats labels into HTML
#[askama::filter_fn]
pub(super) fn label_list_fmt(
    labels: &[rpc::forge::Label],
    _env: &dyn askama::Values,
    truncate: bool,
) -> ::askama::Result<String> {
    const MAX_LABEL_LENGTH: usize = 32;

    // Format labels by key to get a consistent order
    let mut labels = labels.to_vec();
    labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));

    let mut result = String::new();
    for label in labels.iter() {
        if !result.is_empty() {
            result += "<br>";
        }
        result += "<b>";
        let truncated_key = if truncate && label.key.len() > MAX_LABEL_LENGTH {
            &format!(
                "{}...",
                label.key.chars().take(MAX_LABEL_LENGTH).collect::<String>()
            )
        } else {
            &label.key
        };
        askama_escape::Html.write_escaped(&mut result, truncated_key)?;
        result += "</b>";

        if let Some(value) = label.value.as_ref() {
            result += ": ";
            let truncated_value = if truncate && value.len() > MAX_LABEL_LENGTH {
                &format!(
                    "{}...",
                    value.chars().take(MAX_LABEL_LENGTH).collect::<String>()
                )
            } else {
                value
            };
            askama_escape::Html.write_escaped(&mut result, truncated_value)?;
        }
    }
    Ok(result)
}

/// Formats a list of Health Probe Alerts
/// If there is no alert, generates a green "None" bubble
/// Generates HTML using the unified bubble system
#[askama::filter_fn]
pub(super) fn health_alerts_fmt(
    alerts: &[health_report::HealthProbeAlert],
    _env: &dyn askama::Values,
    include_message: bool,
    include_target: bool,
) -> ::askama::Result<String> {
    if alerts.is_empty() {
        return Ok(r#"<span class="bubble success">None</span>"#.to_string());
    }

    let mut result = String::new();
    for alert in alerts.iter() {
        if !result.is_empty() {
            result += " ";
        }

        result += r#"<span class="bubble error">"#;
        askama_escape::Html.write_escaped(&mut result, &alert.id.to_string())?;
        if include_target && let Some(target) = alert.target.as_ref() {
            result += " [Target: ";
            askama_escape::Html.write_escaped(&mut result, target)?;
            result.push(']');
        }

        if include_message {
            result += ": ";
            askama_escape::Html.write_escaped(&mut result, &alert.message)?;
        }
        result += r#"</span>"#;
    }
    Ok(result)
}

/// Formats a list of Health Alert Classifications
/// If there is no alert, the generated String will be empty
#[askama::filter_fn]
pub(super) fn health_alert_classifications_fmt(
    alerts: &Vec<health_report::HealthProbeAlert>,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    health_alert_classifications_generic(alerts)
}

/// Formats a single Health Alert Classification
#[askama::filter_fn]
pub(super) fn health_alert_classification_fmt(
    alert: &health_report::HealthProbeAlert,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    health_alert_classifications_generic([alert])
}

// Note, we can't call generic functions like these directly from templates as of askama 0.15, so
// this is the "inner" function called by both health_alert_classifications_fmt and
// health_alert_classification_fmt
fn health_alert_classifications_generic<'a, T, AlertRef>(alerts: T) -> ::askama::Result<String>
where
    T: IntoIterator<Item = AlertRef>,
    AlertRef: std::borrow::Borrow<&'a health_report::HealthProbeAlert> + 'a,
{
    let mut result = String::new();
    let mut classifications = BTreeSet::<health_report::HealthAlertClassification>::new();

    for alert in alerts.into_iter() {
        classifications.extend(alert.borrow().classifications.iter().cloned());
    }

    for classification in classifications.iter() {
        if !result.is_empty() {
            result += "<br>";
        }
        result += r#"<div class="health_alert_classification">"#;
        askama_escape::Html.write_escaped(&mut result, &classification.to_string())?;
        result += r#"</div>"#;
    }

    Ok(result)
}

/// Renders version strings including timestamps
/// Also shows the localized timestamp on Mouseover
#[askama::filter_fn]
pub(super) fn config_version(
    version: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    let string_version = version.to_string();
    let version = match string_version.parse::<::config_version::ConfigVersion>() {
        Ok(version) => version,
        Err(_) => return Ok(string_version),
    };

    let utc_time = version.timestamp();
    let formatted_utc_time = utc_time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    Ok(format!(
        "{string_version}<br><small>[<span title=\"{formatted_utc_time}\" onmouseover=\"setTitleToLocalizedTime(this)\">{formatted_utc_time}</span>]</small>"
    ))
}

/// Prints the value of the `Option` in case it's `Some(x)`, and otherwise an empty string
#[askama::filter_fn]
pub(super) fn option_fmt(
    value: &Option<impl Display>,
    _env: &dyn askama::Values,
) -> askama::Result<String> {
    Ok(match value {
        Some(value) => value.to_string(),
        None => String::new(),
    })
}

#[askama::filter_fn]
pub(super) fn option_fmt_or(
    value: &Option<impl Display>,
    _env: &dyn askama::Values,
    default: &str,
) -> askama::Result<String> {
    Ok(match value {
        Some(value) => value.to_string(),
        None => default.to_string(),
    })
}

/// Formats JSON for display without routing integer values through JavaScript numbers.
///
/// Invalid JSON is returned unchanged so this can also be used for endpoints whose
/// response bodies are not guaranteed to be JSON.
#[askama::filter_fn]
pub(super) fn pretty_json(
    value: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    Ok(format_json(value))
}

// separate function because askama filter functions can't be tested directly
fn format_json(value: impl Display) -> String {
    let input = value.to_string();
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&input) else {
        return input;
    };

    // Some Redfish responses contain JSON encoded inside this top-level field.
    if let Some(response_body) = json
        .get("ResponseBody")
        .and_then(serde_json::Value::as_str)
        .and_then(|body| serde_json::from_str(body).ok())
    {
        json["ResponseBody"] = response_body;
    }

    serde_json::to_string_pretty(&json).unwrap_or(input)
}

fn state_and_substate_labels(state_json: impl Display) -> (String, String) {
    let state_json = state_json.to_string();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&state_json) else {
        return (state_json, String::new());
    };
    let Some(object) = value.as_object() else {
        return (state_json, String::new());
    };
    let Some(state) = object.get("state").and_then(|value| value.as_str()) else {
        return (state_json, String::new());
    };

    let state_specific_key = format!("{state}_state");
    let substate = object
        .get(&state_specific_key)
        .or_else(|| {
            object
                .iter()
                .find(|(key, _)| key.as_str() != "state" && key.ends_with("_state"))
                .map(|(_, value)| value)
        })
        .map(format_substate_label)
        .unwrap_or_default();

    (capitalize_state(state), substate)
}

#[askama::filter_fn]
pub(super) fn state_with_substate_label(
    state: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    let (state, substate) = state_and_substate_labels(state);
    Ok(if substate.is_empty() {
        state
    } else {
        format!("{state}/{substate}")
    })
}

fn format_substate_label(value: &serde_json::Value) -> String {
    value
        .as_str()
        .or_else(|| value.get("state").and_then(|value| value.as_str()))
        .map(capitalize_state)
        .unwrap_or_else(|| value.to_string())
}

fn capitalize_state(state: &str) -> String {
    let mut chars = state.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Formats the boot order list
#[askama::filter_fn]
pub(super) fn boot_order_fmt(
    boot_order: &Option<rpc::site_explorer::BootOrder>,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    let json_result = boot_order
        .as_ref()
        .and_then(|order| serde_json::to_string_pretty(order).ok())
        .unwrap_or_default();

    Ok(json_result
        .trim_matches(|c| c == '{' || c == '}')
        .trim()
        .to_string())
}

#[askama::filter_fn]
pub(super) fn colorize_output(
    ansi_text: &str,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    let html = ansi_to_html::Converter::new()
        .convert(ansi_text)
        .unwrap_or_default();
    Ok(html)
}

/// Formats a state handler outcome
#[askama::filter_fn]
pub(super) fn controller_state_reason_fmt(
    reason: &Option<::rpc::forge::ControllerStateReason>,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    let Some(reason) = reason else {
        return Ok(String::new());
    };

    let mut result = String::new();
    let classes = match reason.outcome() {
        rpc::forge::ControllerStateOutcome::Wait => "bubble warning".to_string(),
        rpc::forge::ControllerStateOutcome::Error => "bubble error".to_string(),
        _ => "bubble".to_string(),
    };

    write!(
        &mut result,
        "<b>Outcome:</b> <span class=\"{classes}\">{:?}</span>",
        reason.outcome()
    )
    .unwrap();
    if let Some(message) = &reason.outcome_msg {
        write!(&mut result, "<br><b>Message:</b> ").unwrap();
        askama_escape::Html.write_escaped(&mut result, message)?;
    }

    if let Some(source_ref) = reason.source_ref.as_ref() {
        const GITHUB_REPO: &str = "https://github.com/NVIDIA/infra-controller";

        // TODO: carbide_version::v!(git_sha) should work here - however it returns an
        // outdated commit ID.
        let build_version = carbide_version::v!(build_version);
        let commit_hash = match build_version.rfind('g') {
            Some(idx) if idx != build_version.len() - 1 => &build_version[idx + 1..],
            _ => "trunk",
        };

        write!(
            &mut result,
            "<br><b>Source:</b> <a href=\"{}/blob/{}/{}#L{}\">{}:{}</a>",
            GITHUB_REPO,
            commit_hash,
            source_ref.file,
            source_ref.line,
            source_ref.file,
            source_ref.line
        )
        .unwrap();
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::{escaped_shortened_id_link, format_json};

    #[test]
    fn shortened_id_links_render_exact_output() {
        check_values(
            [
                Check {
                    scenario: "short ID",
                    input: "a12",
                    expect: r#"<a href="/admin/rack/a12">a12</a>"#.to_string(),
                },
                Check {
                    scenario: "long ASCII ID",
                    input: "rack-a12",
                    expect: concat!(
                        "\n    <a href=\"/admin/rack/rack-a12\">\n",
                        "        <div class=\"machine_id\">\n",
                        "            <div>rack-a12</div><div>ck-a12</div>\n",
                        "        </div>\n",
                        "    </a>"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "six accented characters",
                    input: "áéíóúñ",
                    expect: concat!(
                        "<a href=\"/admin/rack/",
                        "%C3%A1%C3%A9%C3%AD%C3%B3%C3%BA%C3%B1",
                        "\">áéíóúñ</a>"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "seven accented characters",
                    input: "áéíóúñü",
                    expect: concat!(
                        "\n    <a href=\"/admin/rack/",
                        "%C3%A1%C3%A9%C3%AD%C3%B3%C3%BA%C3%B1%C3%BC",
                        "\">\n",
                        "        <div class=\"machine_id\">\n",
                        "            <div>áéíóúñü</div><div>éíóúñü</div>\n",
                        "        </div>\n",
                        "    </a>"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "Unicode suffix",
                    input: "rack-abcdeå",
                    expect: concat!(
                        "\n    <a href=\"/admin/rack/rack-abcde%C3%A5\">\n",
                        "        <div class=\"machine_id\">\n",
                        "            <div>rack-abcdeå</div><div>abcdeå</div>\n",
                        "        </div>\n",
                        "    </a>"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "HTML metacharacters",
                    input: "rack-<&abcd",
                    expect: concat!(
                        "\n    <a href=\"/admin/rack/rack-%3C%26abcd\">\n",
                        "        <div class=\"machine_id\">\n",
                        "            <div>rack-&#60;&#38;abcd</div>",
                        "<div>&#60;&#38;abcd</div>\n",
                        "        </div>\n",
                        "    </a>"
                    )
                    .to_string(),
                },
            ],
            |id| escaped_shortened_id_link(id, "rack").unwrap(),
        );
    }

    #[test]
    fn pretty_json_preserves_values_and_fallbacks() {
        check_values(
            [
                Check {
                    scenario: "smallest unsafe JavaScript integer",
                    input: r#"{"id":9007199254740993}"#,
                    expect: "{\n  \"id\": 9007199254740993\n}".to_string(),
                },
                Check {
                    scenario: "maximum 64-bit integer",
                    input: r#"{"gpu_uid":18446744073709551615}"#,
                    expect: "{\n  \"gpu_uid\": 18446744073709551615\n}".to_string(),
                },
                Check {
                    scenario: "nested ResponseBody JSON",
                    input: r#"{"ResponseBody":"{\"id\":18446744073709551615}"}"#,
                    expect: concat!(
                        "{\n",
                        "  \"ResponseBody\": {\n",
                        "    \"id\": 18446744073709551615\n",
                        "  }\n",
                        "}"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "non-JSON response",
                    input: "GPU not found: 18446744073709551615",
                    expect: "GPU not found: 18446744073709551615".to_string(),
                },
            ],
            format_json,
        );
    }
}
