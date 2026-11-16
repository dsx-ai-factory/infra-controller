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

//! What checks read, in words.

use std::fmt::Debug;

use bmc_platform::BootOrderStatus;
use serde_json::Value;

use super::model::Detail;

/// One expected-versus-actual setting difference.
type Difference<'a> = (&'a str, &'a Value, Option<&'a Value>);

/// "in effect", or the first few expected-versus-actual differences.
pub(super) fn differences<'a>(diffs: impl ExactSizeIterator<Item = Difference<'a>>) -> String {
    const SHOWN: usize = 5;
    let count = diffs.len();
    if count == 0 {
        return "in effect".to_string();
    }
    let shown: Vec<String> = diffs
        .take(SHOWN)
        .map(|(key, expected, actual)| format!("{key} ({})", expected_actual(expected, actual)))
        .collect();
    let mut text = format!(
        "{}: {}",
        counted(count, "difference", "differences"),
        shown.join("; ")
    );
    if count > SHOWN {
        text.push_str(&format!("; {} more", count - SHOWN));
    }
    text
}

/// "in effect", or a count with one field per differing setting.
pub(super) fn difference_detail<'a>(
    diffs: impl ExactSizeIterator<Item = Difference<'a>>,
) -> Detail {
    let count = diffs.len();
    if count == 0 {
        return "in effect".to_string().into();
    }
    Detail {
        summary: counted(count, "difference", "differences"),
        fields: diffs
            .map(|(key, expected, actual)| (key.to_string(), expected_actual(expected, actual)))
            .collect(),
    }
}

fn expected_actual(expected: &Value, actual: Option<&Value>) -> String {
    match actual {
        Some(actual) => format!("expected {}, actual {}", plain(expected), plain(actual)),
        None => format!("expected {}, not reported", plain(expected)),
    }
}

/// A JSON value as text, without the quotes around a string.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) if text.is_empty() => "(empty)".to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `count` with `one` or `many`, as `1 entry` or `2 entries`.
pub(super) fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// A value the BMC may not report, as reported or `unreported`.
pub(super) fn reported<T: Debug>(value: &Option<T>) -> String {
    match value {
        Some(value) => format!("{value:?}").trim_matches('"').to_string(),
        None => "unreported".to_string(),
    }
}

pub(super) fn yes_no(holds: bool, yes: &str, no: &str) -> String {
    if holds { yes } else { no }.to_string()
}

/// The boot-order conditions in words; a condition the driver does not
/// manage is reported as such rather than as holding.
pub(super) fn boot_order_summary(status: BootOrderStatus) -> String {
    let managed = |holds: Option<bool>, name: &str, yes: &str, no: &str| match holds {
        Some(true) => yes.to_string(),
        Some(false) => no.to_string(),
        None => format!("{name} not managed"),
    };
    [
        if status.boot_interface_first {
            "boot interface first".to_string()
        } else {
            "boot interface not first".to_string()
        },
        managed(
            status.disk_enabled,
            "disks",
            "disks enabled",
            "disks dropped",
        ),
        managed(
            status.other_network_options_disabled,
            "other network boot",
            "other network boot off",
            "other network boot on",
        ),
    ]
    .join(" · ")
}
