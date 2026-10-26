/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    GetMachineValidationExternalConfigsRequest, GetMachineValidationExternalConfigsResponse,
    MachineValidationExternalConfig,
};
use support::{Exchange, assert_success, run_command};

#[tokio::test]
async fn external_config_show_displays_present_timestamps_and_leaves_absent_timestamps_blank() {
    let response = GetMachineValidationExternalConfigsResponse {
        configs: vec![
            MachineValidationExternalConfig {
                name: "with-timestamp".to_string(),
                description: Some("recorded config".to_string()),
                config: b"first = true".to_vec(),
                version: "v1".to_string(),
                timestamp: Some(
                    "2026-10-23T12:34:56.789012Z"
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .expect("fixture timestamp")
                        .into(),
                ),
            },
            MachineValidationExternalConfig {
                name: "without-timestamp".to_string(),
                description: Some("undated config".to_string()),
                config: b"second = true".to_vec(),
                version: "v2".to_string(),
                timestamp: None,
            },
        ],
    };
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        check: fn(&str, &str),
    }
    for case in [
        Case {
            scenario: "extended output",
            args: &[
                "machine-validation",
                "external-config",
                "show",
                "--extended",
            ],
            check: |scenario, stdout| {
                let details: Vec<_> = stdout
                    .lines()
                    .filter_map(|line| line.split_once(": "))
                    .map(|(label, value)| (label.trim(), value))
                    .collect();
                assert_eq!(
                    details,
                    [
                        ("Name", "with-timestamp"),
                        ("Description", "recorded config"),
                        ("Version", "v1"),
                        ("TimeStamp", "2026-10-23T12:34:56.789012Z"),
                        ("Config", "first = true"),
                        ("Name", "without-timestamp"),
                        ("Description", "undated config"),
                        ("Version", "v2"),
                        ("TimeStamp", ""),
                        ("Config", "second = true"),
                    ],
                    "{scenario}: {stdout}",
                );
            },
        },
        Case {
            scenario: "ordinary table",
            args: &["machine-validation", "external-config", "show"],
            check: |scenario, stdout| {
                let rows: Vec<Vec<_>> = stdout
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
                    .map(|line| line.split('|').map(str::trim).collect())
                    .collect();
                assert_eq!(
                    rows,
                    [
                        vec!["Name", "Description", "Version", "Timestamp"],
                        vec![
                            "with-timestamp",
                            "recorded config",
                            "v1",
                            "2026-10-23T12:34:56.789012Z",
                        ],
                        vec!["without-timestamp", "undated config", "v2", ""],
                    ],
                    "{scenario}: {stdout}",
                );
            },
        },
    ] {
        let output = run_command(
            case.args,
            None,
            vec![Exchange {
                method: "/forge.Forge/GetMachineValidationExternalConfigs",
                request: GetMachineValidationExternalConfigsRequest::default().encode_to_vec(),
                response: Ok(response.encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 external config output");
        (case.check)(case.scenario, &stdout);
    }
}
