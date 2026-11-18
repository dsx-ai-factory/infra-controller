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
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::switch::SwitchId;

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

    if MachineId::from_str(&id).is_err() {
        // Not a Machine ID. Escape HTML to make it safe for post processing with safe filter
        let mut output = String::new();
        askama_escape::Html.write_escaped(&mut output, &id)?;
        return Ok(output);
    }
    // The halves join into the full ID; CSS cuts the first half's end when space is short,
    // so "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0" -> "fm100ds…acguc0".
    let (head, tail) = id.split_at(id.len() - 6);

    let formatted = format!(
        r#"
    <a href="/admin/{path}/{link_path}">
        <div class="machine_id">
            <div>{head}</div><div>{tail}</div>
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

    if id.chars().count() <= 6 {
        let mut escaped_id = String::new();
        askama_escape::Html.write_escaped(&mut escaped_id, &id)?;
        return Ok(format!(
            r#"<a href="/admin/{path}/{link_path}">{escaped_id}</a>"#
        ));
    }

    let (head, tail) = id.split_at(id.char_indices().rev().nth(5).map_or(0, |(index, _)| index));
    let mut escaped_head = String::new();
    askama_escape::Html.write_escaped(&mut escaped_head, head)?;
    let mut escaped_tail = String::new();
    askama_escape::Html.write_escaped(&mut escaped_tail, tail)?;
    let formatted = format!(
        r#"
    <a href="/admin/{path}/{link_path}">
        <div class="machine_id">
            <div>{escaped_head}</div><div>{escaped_tail}</div>
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
pub(super) fn interface_id_link(
    id: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    escaped_shortened_id_link(id, "interface")
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

/// Links a health alert target to its object page as a shortened ID when it
/// is a machine, switch, or power shelf ID; any other target is escaped text.
#[askama::filter_fn]
pub(super) fn health_target_link(
    target: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    health_target_html(&target.to_string())
}

/// Escapes `text` and lets it wrap after each `/`, so long state names such as
/// `Assigned/BootingWithDiscoveryImage` break between levels.
#[askama::filter_fn]
pub(super) fn slash_breaks(
    text: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    slash_breaks_html(&text.to_string())
}

fn slash_breaks_html(text: &str) -> ::askama::Result<String> {
    let mut html = String::new();
    for (index, part) in text.split('/').enumerate() {
        if index > 0 {
            html.push_str("/<wbr>");
        }
        askama_escape::Html.write_escaped(&mut html, part)?;
    }
    Ok(html)
}

fn health_target_html(target: &str) -> ::askama::Result<String> {
    if MachineId::from_str(target).is_ok() {
        machine_link(target, "machine")
    } else if SwitchId::from_str(target).is_ok() {
        escaped_shortened_id_link(target, "switch")
    } else if PowerShelfId::from_str(target).is_ok() {
        escaped_shortened_id_link(target, "power-shelf")
    } else {
        let mut output = String::new();
        askama_escape::Html.write_escaped(&mut output, target)?;
        Ok(output)
    }
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

/// One line per alert for list tables: the alert ID in a chip, then its target,
/// with the full "ID [Target: target]: message" as the line's tooltip.
#[askama::filter_fn]
pub(super) fn health_alerts_compact(
    alerts: &[health_report::HealthProbeAlert],
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    health_alerts_compact_html(alerts)
}

fn health_alerts_compact_html(
    alerts: &[health_report::HealthProbeAlert],
) -> ::askama::Result<String> {
    if alerts.is_empty() {
        return Ok(r#"<span class="bubble success">Healthy</span>"#.to_string());
    }

    let mut result = String::new();
    for alert in alerts {
        let id = alert.id.to_string();
        let mut full = id.clone();
        if let Some(target) = alert.target.as_ref() {
            write!(&mut full, " [Target: {target}]").unwrap();
        }
        write!(&mut full, ": {}", alert.message).unwrap();

        result += r#"<div class="health-alert-line" title=""#;
        askama_escape::Html.write_escaped(&mut result, &full)?;
        result += r#""><span class="health-alert-chip">"#;
        askama_escape::Html.write_escaped(&mut result, &id)?;
        result += "</span>";
        if let Some(target) = alert.target.as_ref() {
            result += r#"<span class="health-alert-target">"#;
            match middle_cut(target) {
                // Both forms are rendered; table_filter.js shows the full one
                // when the table has room for it.
                Some(short) => {
                    result += r#"<span class="fit-short">"#;
                    askama_escape::Html.write_escaped(&mut result, &short)?;
                    result += r#"</span><span class="fit-full-only">"#;
                    askama_escape::Html.write_escaped(&mut result, target)?;
                    result += "</span>";
                }
                None => askama_escape::Html.write_escaped(&mut result, target)?,
            }
            result += "</span>";
        }
        result += "</div>";
    }
    Ok(result)
}

/// Shortens long text, such as a machine ID used as an alert target, to its
/// first eight and last six characters around an ellipsis. `None` when the
/// text is already short.
fn middle_cut(text: &str) -> Option<String> {
    const HEAD: usize = 8;
    const TAIL: usize = 6;
    let count = text.chars().count();
    if count <= HEAD + TAIL + 4 {
        return None;
    }
    let head: String = text.chars().take(HEAD).collect();
    let tail: String = text.chars().skip(count - TAIL).collect();
    Some(format!("{head}…{tail}"))
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

/// Top-level state name only, for compact displays such as a detail page
/// header where nested per-DPU sub-states would not fit.
#[askama::filter_fn]
pub(super) fn state_prefix_label(
    state: impl Display,
    _env: &dyn askama::Values,
) -> ::askama::Result<String> {
    Ok(state_prefix(&state.to_string()))
}

fn state_prefix(state_json: &str) -> String {
    state_path(state_json).swap_remove(0)
}

/// Readable state hierarchy, outermost first, e.g. `["WaitingForCleanup", "HostCleanup"]`.
/// Never empty.
fn state_path(state_json: &str) -> Vec<String> {
    let label = match serde_json::from_str::<model::machine::ManagedHostState>(state_json) {
        Ok(state) => state.to_string(),
        Err(_) => {
            let (state, substate) = state_and_substate_labels(state_json);
            if substate.is_empty() {
                state
            } else {
                format!("{state}/{substate}")
            }
        }
    };
    // `ManagedHostState` renders as `Prefix/sub/...`, except `Maintenance(op)`
    // and sub-states rendered with `Debug`, such as `RebootHost { .. }`.
    let mut parts = label.split('/');
    let first = parts.next().unwrap_or_default().trim();
    let name = leading_name(first);
    let mut path = vec![if name.is_empty() {
        "Unknown".to_string()
    } else {
        name.to_string()
    }];
    if let Some(inner) = first[name.len()..].strip_prefix('(') {
        let inner = leading_name(inner);
        if !inner.is_empty() {
            path.push(inner.to_string());
        }
    }
    path.extend(
        parts
            .map(leading_name)
            .filter(|part| !part.is_empty())
            .map(str::to_string),
    );
    path
}

fn leading_name(label: &str) -> &str {
    label
        .trim()
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
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
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use health_report::{HealthProbeAlert, HealthProbeId};

    use super::{
        escaped_shortened_id_link, format_json, health_target_html, state_path, state_prefix,
    };

    #[test]
    fn compact_health_alerts_list_each_alert_with_full_text_on_hover() {
        let alert = |target: Option<&str>| HealthProbeAlert {
            id: HealthProbeId::from_str("HeartbeatTimeout").unwrap(),
            target: target.map(str::to_string),
            in_alert_since: None,
            message: "Last seen over <PT300S> ago".to_string(),
            tenant_message: None,
            classifications: vec![],
        };
        let compact =
            |alerts: &[HealthProbeAlert]| super::health_alerts_compact_html(alerts).unwrap();
        check_values(
            [
                Check {
                    scenario: "no alerts",
                    input: vec![],
                    expect: r#"<span class="bubble success">Healthy</span>"#.to_string(),
                },
                Check {
                    scenario: "alert with target",
                    input: vec![alert(Some("forge-dpu-agent"))],
                    expect: concat!(
                        r#"<div class="health-alert-line" title="HeartbeatTimeout [Target: forge-dpu-agent]: "#,
                        r#"Last seen over &#60;PT300S&#62; ago">"#,
                        r#"<span class="health-alert-chip">HeartbeatTimeout</span>"#,
                        r#"<span class="health-alert-target">forge-dpu-agent</span></div>"#
                    )
                    .to_string(),
                },
                Check {
                    scenario: "long target renders shortened and full forms",
                    input: vec![alert(Some(
                        "fm100ds33vkic2i020dsma4nv9elgfuoq26foi7daf44tulvkt7r2iemvtg",
                    ))],
                    expect: concat!(
                        r#"<div class="health-alert-line" title="HeartbeatTimeout [Target: "#,
                        "fm100ds33vkic2i020dsma4nv9elgfuoq26foi7daf44tulvkt7r2iemvtg]: ",
                        r#"Last seen over &#60;PT300S&#62; ago">"#,
                        r#"<span class="health-alert-chip">HeartbeatTimeout</span>"#,
                        r#"<span class="health-alert-target"><span class="fit-short">fm100ds3…iemvtg</span>"#,
                        r#"<span class="fit-full-only">"#,
                        "fm100ds33vkic2i020dsma4nv9elgfuoq26foi7daf44tulvkt7r2iemvtg",
                        "</span></span></div>"
                    )
                    .to_string(),
                },
                Check {
                    scenario: "alert without target",
                    input: vec![alert(None)],
                    expect: concat!(
                        r#"<div class="health-alert-line" title="HeartbeatTimeout: Last seen over &#60;PT300S&#62; ago">"#,
                        r#"<span class="health-alert-chip">HeartbeatTimeout</span></div>"#
                    )
                    .to_string(),
                },
            ],
            |alerts| compact(&alerts),
        );
    }

    #[test]
    fn state_path_lists_each_nesting_level() {
        check_values(
            [
                Check {
                    scenario: "unparsed state with a sub-state",
                    input: r#"{"state": "futurestate", "future_state": {"state": "step2"}}"#,
                    expect: vec!["Futurestate".to_string(), "Step2".to_string()],
                },
                Check {
                    scenario: "sub-state that is not a named state is omitted",
                    input: r#"{"state": "futurestate", "future_state": {"detail": [1, 2]}}"#,
                    expect: vec!["Futurestate".to_string()],
                },
                Check {
                    scenario: "unreadable state",
                    input: "{",
                    expect: vec!["Unknown".to_string()],
                },
            ],
            state_path,
        );
    }

    #[test]
    fn state_prefix_keeps_only_the_top_level_state() {
        check_values(
            [
                Check {
                    scenario: "state without sub-state",
                    input: r#"{"state": "ready"}"#,
                    expect: "Ready".to_string(),
                },
                Check {
                    scenario: "per-DPU sub-states",
                    input: concat!(
                        r#"{"state": "dpureprovision", "dpu_states": {"states": {"#,
                        r#""fm100ds60gqhm763jb1tdpv4orrig5b5eqfr8p8i3iv5i1dja5p8jc144m0": "#,
                        r#"{"dpfstates": {"substate": {"dpfstate": "waitingforready", "phase_detail": "DpuClusterConfig"}}}, "#,
                        r#""fm100dsvt9g1b3si4njfseurqcm5an22lvjjpoi4sb6qq76q1g7di7mt3s0": "#,
                        r#"{"dpfstates": {"substate": {"dpfstate": "waitingforready", "phase_detail": "DpuClusterConfig"}}}}}}"#,
                    ),
                    expect: "Reprovisioning".to_string(),
                },
                Check {
                    scenario: "validation sub-state rendered as a struct",
                    input: concat!(
                        r#"{"state": "validation", "validation_state": {"validation_type": "machinevalidation", "#,
                        r#""machine_validation": {"reboothost": {"validation_id": "554aa7f6-2f56-467f-ac7b-1e33a460d495"}}}}"#,
                    ),
                    expect: "MachineValidation".to_string(),
                },
                Check {
                    scenario: "unparsed state falls back to the state field",
                    input: r#"{"state": "futurestate", "future_state": {"detail": [1, 2, 3]}}"#,
                    expect: "Futurestate".to_string(),
                },
            ],
            state_prefix,
        );
    }

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
                        "            <div>ra</div><div>ck-a12</div>\n",
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
                        "            <div>á</div><div>éíóúñü</div>\n",
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
                        "            <div>rack-</div><div>abcdeå</div>\n",
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
                        "            <div>rack-</div>",
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
    fn slash_breaks_escape_and_break_after_each_slash() {
        check_values(
            [
                Check {
                    scenario: "single level",
                    input: "Ready",
                    expect: "Ready".to_string(),
                },
                Check {
                    scenario: "nested levels",
                    input: "Assigned/BootingWithDiscoveryImage",
                    expect: "Assigned/<wbr>BootingWithDiscoveryImage".to_string(),
                },
                Check {
                    scenario: "markup in a level is escaped",
                    input: "Failed/<b>",
                    expect: "Failed/<wbr>&#60;b&#62;".to_string(),
                },
            ],
            |text| super::slash_breaks_html(text).unwrap(),
        );
    }

    #[test]
    fn health_targets_link_to_their_object_page() {
        check_values(
            [
                Check {
                    scenario: "machine ID",
                    input: "fm100ht09g4atrqgjb0b83b2to1qa1hfugks9mhutb0umcng1rkr54vliqg",
                    expect: "/admin/machine/fm100ht09g4atrqgjb0b83b2to1qa1hfugks9mhutb0umcng1rkr54vliqg"
                        .to_string(),
                },
                Check {
                    scenario: "switch ID",
                    input: "sw100nsatkhogcptl4dgkaerckihe0ji51mj1scfh3745ffhlq5bpnr0chg",
                    expect: "/admin/switch/sw100nsatkhogcptl4dgkaerckihe0ji51mj1scfh3745ffhlq5bpnr0chg"
                        .to_string(),
                },
                Check {
                    scenario: "power shelf ID",
                    input: "ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
                    expect: "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .to_string(),
                },
                Check {
                    scenario: "other target stays escaped text",
                    input: "HostBMC <&>",
                    expect: "HostBMC &#60;&#38;&#62;".to_string(),
                },
            ],
            |target| {
                let html = health_target_html(target).unwrap();
                match html.split_once("href=\"") {
                    Some((_, rest)) => rest.split('"').next().unwrap_or_default().to_string(),
                    None => html,
                }
            },
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
