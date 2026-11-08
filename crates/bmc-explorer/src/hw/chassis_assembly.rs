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

//! Chassis_0 assembly serial selection shared by both Redfish backends.

use std::fmt;

use super::vera_rubin::chassis_assembly_serial_model;

const GB300_TRAY_MODEL: &str = "GB300 Titania-Bianca Compute Tray";

/// Multiple GB300 tray assemblies report distinct usable serial numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmbiguousGb300TraySerial;

impl fmt::Display for AmbiguousGb300TraySerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("multiple distinct GB300 tray assembly serial numbers")
    }
}

impl std::error::Error for AmbiguousGb300TraySerial {}

/// Selects a whole-tray serial from Chassis_0 assembly (model, serial) pairs.
///
/// GB300 tray serials are trimmed, ignoring empty values and numeric-prefix
/// X-masked placeholders. Repeated copies of one serial are accepted; distinct
/// usable GB300 serials return an error, which callers must not turn into a
/// board-serial fallback. Without a usable GB300 serial, preserves the first
/// GB200/Vera Rubin model match, including a missing serial. `Ok(None)` allows
/// the caller's existing chassis-serial fallback.
pub fn select_serial_number<'a>(
    assemblies: impl IntoIterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> Result<Option<&'a str>, AmbiguousGb300TraySerial> {
    let mut gb300_serial = None;
    let mut legacy_serial = None;
    for (model, serial) in assemblies {
        if model == Some(GB300_TRAY_MODEL) {
            let Some(serial) = serial.map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            // Firmware can repeat the tray model for an unprogrammed FRU.
            // Do not impose a numeric-only format on real or legacy serials.
            let prefix = serial.trim_end_matches(['X', 'x']);
            if serial.len() - prefix.len() >= 3 && prefix.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            if gb300_serial.is_some_and(|previous| previous != serial) {
                return Err(AmbiguousGb300TraySerial);
            }
            gb300_serial = Some(serial);
        } else if legacy_serial.is_none()
            && model.is_some_and(|m| m == "GB200 NVL" || chassis_assembly_serial_model(m))
        {
            legacy_serial = Some(serial);
        }
    }
    Ok(gb300_serial.or(legacy_serial.flatten()))
}

#[cfg(test)]
mod tests {
    use super::{AmbiguousGb300TraySerial, GB300_TRAY_MODEL, select_serial_number};

    #[test]
    fn selects_whole_tray_serial_without_changing_legacy_selection() {
        let tray = Some(GB300_TRAY_MODEL);
        let real = (tray, Some("9990000000001"));
        let placeholder = (tray, Some("999XXXXXXXXXX"));
        let board = (Some("P5859 B03"), Some("9990000000002"));
        let cases: &[(_, &[_], _)] = &[
            (
                "real before placeholder",
                &[real, board, placeholder],
                Ok(Some("9990000000001")),
            ),
            (
                "placeholder before real",
                &[placeholder, board, real],
                Ok(Some("9990000000001")),
            ),
            (
                "duplicate serial",
                &[real, (tray, Some(" 9990000000001 "))],
                Ok(Some("9990000000001")),
            ),
            (
                "distinct tray serials",
                &[real, (tray, Some("9990000000003"))],
                Err(AmbiguousGb300TraySerial),
            ),
            ("missing assemblies", &[], Ok(None)),
            (
                "board is not a tray",
                &[board, (None, Some("9990000000004"))],
                Ok(None),
            ),
            ("missing tray serial", &[(tray, None)], Ok(None)),
            ("blank tray serial", &[(tray, Some(" \t "))], Ok(None)),
            (
                "placeholder only",
                &[placeholder, (tray, Some(" 999xxxxxxxxxx "))],
                Ok(None),
            ),
            (
                "missing serial before real",
                &[(tray, None), real],
                Ok(Some("9990000000001")),
            ),
            (
                "alphanumeric GB300 serial",
                &[(tray, Some(" TRAY-X123 "))],
                Ok(Some("TRAY-X123")),
            ),
            (
                "GB200 first match",
                &[
                    (Some("GB200 NVL"), Some("GB200-X123")),
                    (Some("GB200 NVL"), Some("other")),
                ],
                Ok(Some("GB200-X123")),
            ),
            (
                "Vera Rubin first match",
                &[
                    (Some("P4107"), Some("VR-XXX-123")),
                    (Some("P4107"), Some("other")),
                ],
                Ok(Some("VR-XXX-123")),
            ),
            (
                "legacy serial remains unfiltered",
                &[(Some("GB200 NVL"), Some("999XXXXXXXXXX"))],
                Ok(Some("999XXXXXXXXXX")),
            ),
            (
                "legacy missing first serial",
                &[(Some("P4107"), None), (Some("P4107"), Some("later"))],
                Ok(None),
            ),
        ];
        for (scenario, assemblies, expected) in cases {
            assert_eq!(
                select_serial_number(assemblies.iter().copied()),
                *expected,
                "{scenario}"
            );
        }
    }
}
