/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use serde_json::json;

use super::*;

#[test]
fn desired_settings_cover_reported_and_required_attributes() {
    let current = serde_json::from_value(json!({"attributes": {
        "TpmSecurity": "Off",
        "LEM0003": "50",
        "Unrelated": "x",
    }}))
    .expect("settings");
    let expected = [
        BiosAttribute::string("TpmSecurity", "On"),
        BiosAttribute::integer("LEM0003", 50),
        // Alternative name this BIOS does not expose.
        BiosAttribute::string("IntelProcVtd", "Enabled"),
        // A setting this BIOS should expose but does not.
        BiosAttribute::string("EndlessBoot", "Enabled").required(),
    ];

    assert_eq!(
        desired_settings(&expected, &current),
        serde_json::from_value(json!({"attributes": {
            "TpmSecurity": "On",
            "LEM0003": "50",
            "EndlessBoot": "Enabled",
        }}))
        .expect("settings")
    );
}
