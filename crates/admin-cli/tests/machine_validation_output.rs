/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{
    MachineValidationAttempt, MachineValidationAttemptGetRequest, MachineValidationAttemptList,
    MachineValidationAttemptLogChunk, MachineValidationAttemptLogGetRequest,
    MachineValidationAttemptLogList, MachineValidationAttemptLogStream,
    MachineValidationAttemptSearchFilter, MachineValidationGetRequest, MachineValidationPlugin,
    MachineValidationResult, MachineValidationResultList, MachineValidationRun,
    MachineValidationRunItem, MachineValidationRunItemIdList, MachineValidationRunItemList,
    MachineValidationRunItemSearchFilter, MachineValidationRunItemsByIdsRequest,
    MachineValidationRunList, MachineValidationRunListGetRequest, MachineValidationTestAddRequest,
    MachineValidationTestAddUpdateResponse, MachineValidationTestEnableDisableTestRequest,
    MachineValidationTestEnableDisableTestResponse, MachineValidationTestFullHostApprovalRequest,
    MachineValidationTestFullHostApprovalResponse, MachineValidationTestVerfiedRequest,
    MachineValidationTestVerfiedResponse,
};
use support::{Exchange, assert_success, run_command};

const VALIDATION_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const HOST_ID: &str = "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530";
const ATTEMPT_ID: &str = "abcdef01-2345-6789-abcd-ef0123456789";
const TEST_ID: &str = "gpu-health";
const VERSION: &str = "V1-T1720000000000000";

async fn command_output(
    scenario: &str,
    args: &[&str],
    exchanges: Vec<Exchange>,
    to_file: bool,
) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("validation-output-{}", uuid::Uuid::new_v4()));
    let mut invocation = Vec::new();
    if to_file {
        invocation.extend(["--output", path.to_str().expect("UTF-8 output path")]);
    }
    invocation.extend_from_slice(args);
    let output = run_command(&invocation, None, exchanges).await;
    // Remove output before asserting, but report a CLI failure before a
    // missing-file error.
    let file_output = to_file.then(|| {
        let bytes = std::fs::read(&path);
        let removed = std::fs::remove_file(&path);
        (bytes, removed)
    });
    assert_success(&output);
    if let Some((bytes, removed)) = file_output {
        let bytes =
            bytes.unwrap_or_else(|error| panic!("{scenario}: read selected output file: {error}"));
        removed.unwrap_or_else(|error| panic!("{scenario}: remove selected output file: {error}"));
        assert!(
            output.stdout.is_empty(),
            "{scenario}: --output leaked to stdout: {}",
            String::from_utf8_lossy(&output.stdout),
        );
        bytes
    } else {
        output.stdout
    }
}

fn table_rows(output: &str) -> Vec<Vec<&str>> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect()
}

#[tokio::test]
async fn runs_show_routes_table_and_json_output() {
    let response = MachineValidationRunList {
        runs: vec![MachineValidationRun {
            validation_id: Some(VALIDATION_ID.parse().expect("validation ID")),
            machine_id: Some(HOST_ID.parse().expect("host ID")),
            start_time: Some(Default::default()),
            context: Some("Discovery".to_string()),
            ..Default::default()
        }],
    };
    struct Case {
        scenario: &'static str,
        format: &'static str,
        to_file: bool,
        check: fn(&str, &str, &MachineValidationRunList),
    }
    for case in [
        Case {
            scenario: "runs table file",
            format: "ascii-table",
            to_file: true,
            check: |scenario, output, _| {
                assert_eq!(
                    table_rows(output),
                    [
                        vec![
                            "Id",
                            "MachineId",
                            "StartTime",
                            "EndTime",
                            "Context",
                            "State"
                        ],
                        vec![
                            VALIDATION_ID,
                            HOST_ID,
                            "1970-01-01T00:00:00Z",
                            "",
                            "Discovery",
                            "Completed(Success)"
                        ],
                    ],
                    "{scenario}: {output}"
                );
                assert_eq!(output.lines().count(), 5, "{scenario}: {output}");
                assert!(output.ends_with("+\n"), "{scenario}: {output}");
            },
        },
        Case {
            scenario: "runs JSON file",
            format: "json",
            to_file: true,
            check: |scenario, output, response| {
                assert_eq!(
                    output,
                    format!("{}\n", serde_json::to_string_pretty(response).unwrap()),
                    "{scenario}"
                );
            },
        },
        Case {
            scenario: "runs default stdout",
            format: "ascii-table",
            to_file: false,
            check: |scenario, output, _| {
                assert!(output.contains(VALIDATION_ID), "{scenario}: {output}");
            },
        },
    ] {
        let output = command_output(
            case.scenario,
            &[
                "--format",
                case.format,
                "machine-validation",
                "runs",
                "show",
            ],
            vec![Exchange {
                method: "/forge.Forge/GetMachineValidationRuns",
                request: MachineValidationRunListGetRequest::default().encode_to_vec(),
                response: Ok(response.encode_to_vec()),
            }],
            case.to_file,
        )
        .await;
        let output = String::from_utf8(output)
            .unwrap_or_else(|error| panic!("{}: UTF-8 runs output: {error}", case.scenario));
        (case.check)(case.scenario, &output, &response);
    }
}

#[tokio::test]
async fn results_show_routes_table_details_and_json_output() {
    let response = MachineValidationResultList {
        results: vec![MachineValidationResult {
            validation_id: Some(VALIDATION_ID.parse().expect("validation ID")),
            name: TEST_ID.to_string(),
            command: "/plugin/entrypoint".to_string(),
            std_out: "all GPUs healthy".to_string(),
            start_time: Some(Default::default()),
            end_time: Some(Default::default()),
            ..Default::default()
        }],
    };
    struct Case {
        scenario: &'static str,
        format: &'static str,
        extended: bool,
        check: fn(&str, &str, &MachineValidationResultList),
    }
    for case in [
        Case {
            scenario: "results table file",
            format: "ascii-table",
            extended: false,
            check: |scenario, output, _| {
                assert_eq!(
                    table_rows(output),
                    [
                        vec![
                            "RunID",
                            "Name",
                            "Context",
                            "ExitCode",
                            "StartTime",
                            "EndTime"
                        ],
                        vec![
                            VALIDATION_ID,
                            TEST_ID,
                            "",
                            "0",
                            "1970-01-01T00:00:00Z",
                            "1970-01-01T00:00:00Z"
                        ],
                    ],
                    "{scenario}: {output}"
                );
            },
        },
        Case {
            scenario: "results details file",
            format: "ascii-table",
            extended: true,
            check: |scenario, output, _| {
                let details: Vec<_> = output
                    .lines()
                    .filter_map(|line| line.split_once(": "))
                    .map(|(label, value)| (label.trim(), value))
                    .collect();
                assert_eq!(
                    details,
                    [
                        ("ID", VALIDATION_ID),
                        ("CONTEXT", ""),
                        ("Name", TEST_ID),
                        ("Description", ""),
                        ("Command", "/plugin/entrypoint"),
                        ("Args", ""),
                        ("StdOut", "all GPUs healthy"),
                        ("StdErr", ""),
                        ("ExitCode", "0"),
                        ("StartTime", "1970-01-01T00:00:00Z"),
                        ("EndTime", "1970-01-01T00:00:00Z"),
                    ],
                    "{scenario}: {output}"
                );
                assert_eq!(output.lines().count(), 14, "{scenario}: {output}");
                assert!(
                    output.ends_with(
                        "\t------------------------------------------------------------------------\n\n"
                    ),
                    "{scenario}: {output}"
                );
            },
        },
        Case {
            scenario: "results JSON file",
            format: "json",
            extended: false,
            check: |scenario, output, response| {
                assert_eq!(
                    output,
                    format!("{}\n", serde_json::to_string_pretty(response).unwrap()),
                    "{scenario}"
                );
            },
        },
        Case {
            scenario: "results extended JSON file",
            format: "json",
            extended: true,
            check: |scenario, output, response| {
                assert_eq!(
                    output,
                    format!("{}\n", serde_json::to_string_pretty(response).unwrap()),
                    "{scenario}"
                );
            },
        },
    ] {
        let mut args = vec![
            "--format",
            case.format,
            "machine-validation",
            "results",
            "show",
            "--validation-id",
            VALIDATION_ID,
        ];
        if case.extended {
            args.push("--extended");
        }
        let output = command_output(
            case.scenario,
            &args,
            vec![Exchange {
                method: "/forge.Forge/GetMachineValidationResults",
                request: MachineValidationGetRequest {
                    validation_id: Some(VALIDATION_ID.parse().expect("validation ID")),
                    ..Default::default()
                }
                .encode_to_vec(),
                response: Ok(response.encode_to_vec()),
            }],
            true,
        )
        .await;
        let output = String::from_utf8(output)
            .unwrap_or_else(|error| panic!("{}: UTF-8 results output: {error}", case.scenario));
        (case.check)(case.scenario, &output, &response);
    }
}

#[tokio::test]
async fn plugins_route_creation_and_revision_messages() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        exchange: Exchange,
        expected: &'static str,
    }
    let image = "registry.example.com/plugins/gpu-health@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for case in [
        Case {
            scenario: "plugin creation message",
            args: vec![
                "create",
                "--name",
                TEST_ID,
                "--image",
                image,
                "--entrypoint",
                "/plugin/entrypoint",
            ],
            exchange: Exchange {
                method: "/forge.Forge/AddMachineValidationTest",
                request: MachineValidationTestAddRequest {
                    name: TEST_ID.to_string(),
                    contexts: vec!["OnDemand".to_string()],
                    timeout: Some(7200),
                    plugin: Some(MachineValidationPlugin {
                        r#type: "container".to_string(),
                        image: image.to_string(),
                        entrypoint: vec!["/plugin/entrypoint".to_string()],
                        parameters_json: "{}".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }
                .encode_to_vec(),
                response: Ok(MachineValidationTestAddUpdateResponse {
                    test_id: TEST_ID.to_string(),
                    version: VERSION.to_string(),
                }
                .encode_to_vec()),
            },
            expected: "Created plugin revision: gpu-health V1-T1720000000000000\n",
        },
        Case {
            scenario: "plugin verification message",
            args: vec!["verify", "--test-id", TEST_ID, "--version", VERSION],
            exchange: Exchange {
                method: "/forge.Forge/MachineValidationTestVerfied",
                request: MachineValidationTestVerfiedRequest {
                    test_id: TEST_ID.to_string(),
                    version: VERSION.to_string(),
                }
                .encode_to_vec(),
                response: Ok(MachineValidationTestVerfiedResponse {
                    message: "verified gpu-health".to_string(),
                }
                .encode_to_vec()),
            },
            expected: "verified gpu-health\n",
        },
        Case {
            scenario: "plugin full-host approval message",
            args: vec![
                "approve-full-host",
                "--test-id",
                TEST_ID,
                "--version",
                VERSION,
            ],
            exchange: Exchange {
                method: "/forge.Forge/MachineValidationTestApproveFullHost",
                request: MachineValidationTestFullHostApprovalRequest {
                    test_id: TEST_ID.to_string(),
                    version: VERSION.to_string(),
                }
                .encode_to_vec(),
                response: Ok(MachineValidationTestFullHostApprovalResponse {
                    message: "approved gpu-health".to_string(),
                }
                .encode_to_vec()),
            },
            expected: "approved gpu-health\n",
        },
        Case {
            scenario: "plugin enable message",
            args: vec!["enable", "--test-id", TEST_ID, "--version", VERSION],
            exchange: Exchange {
                method: "/forge.Forge/MachineValidationTestEnableDisableTest",
                request: MachineValidationTestEnableDisableTestRequest {
                    test_id: TEST_ID.to_string(),
                    version: VERSION.to_string(),
                    is_enabled: true,
                }
                .encode_to_vec(),
                response: Ok(MachineValidationTestEnableDisableTestResponse {
                    message: "enabled gpu-health".to_string(),
                }
                .encode_to_vec()),
            },
            expected: "enabled gpu-health\n",
        },
        Case {
            scenario: "plugin disable message",
            args: vec!["disable", "--test-id", TEST_ID, "--version", VERSION],
            exchange: Exchange {
                method: "/forge.Forge/MachineValidationTestEnableDisableTest",
                request: MachineValidationTestEnableDisableTestRequest {
                    test_id: TEST_ID.to_string(),
                    version: VERSION.to_string(),
                    is_enabled: false,
                }
                .encode_to_vec(),
                response: Ok(MachineValidationTestEnableDisableTestResponse {
                    message: "disabled gpu-health".to_string(),
                }
                .encode_to_vec()),
            },
            expected: "disabled gpu-health\n",
        },
    ] {
        let mut args = vec!["machine-validation", "plugins"];
        args.extend(case.args);
        let output = command_output(case.scenario, &args, vec![case.exchange], true).await;
        assert_eq!(output, case.expected.as_bytes(), "{}", case.scenario);
    }
}

fn logs_exchange(after_sequence: u32, content: &str) -> Exchange {
    Exchange {
        method: "/forge.Forge/GetMachineValidationAttemptLogs",
        request: MachineValidationAttemptLogGetRequest {
            attempt_id: Some(rpc::common::Uuid {
                value: ATTEMPT_ID.to_string(),
            }),
            after_sequence,
            limit: 100,
        }
        .encode_to_vec(),
        response: Ok(MachineValidationAttemptLogList {
            chunks: vec![MachineValidationAttemptLogChunk {
                sequence: after_sequence + 1,
                stream: MachineValidationAttemptLogStream::Stdout.into(),
                content: content.to_string(),
                ..Default::default()
            }],
            has_more: false,
        }
        .encode_to_vec()),
    }
}

#[tokio::test]
async fn logs_show_and_follow_route_content() {
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        exchanges: Vec<Exchange>,
        expected: &'static str,
    }
    for case in [
        Case {
            scenario: "raw logs show preserves bytes without a final newline",
            args: &["show", "--raw"],
            exchanges: vec![logs_exchange(0, "GPU ✓\npartial")],
            expected: "GPU ✓\npartial",
        },
        Case {
            scenario: "logs follow joins the final fetch to the pending line",
            args: &["follow"],
            exchanges: vec![
                logs_exchange(0, "GPU ✓\npartial"),
                Exchange {
                    method: "/forge.Forge/GetMachineValidationAttempt",
                    request: MachineValidationAttemptGetRequest {
                        attempt_id: Some(rpc::common::Uuid {
                            value: ATTEMPT_ID.to_string(),
                        }),
                    }
                    .encode_to_vec(),
                    response: Ok(MachineValidationAttempt {
                        state: "Success".to_string(),
                        ..Default::default()
                    }
                    .encode_to_vec()),
                },
                logs_exchange(1, " tail"),
            ],
            expected: "[ stdout #1] GPU ✓\n[ stdout #1] partial tail\n",
        },
    ] {
        let mut args = vec!["machine-validation", "logs"];
        args.extend_from_slice(case.args);
        args.extend(["--attempt-id", ATTEMPT_ID]);
        let output = command_output(case.scenario, &args, case.exchanges, true).await;
        assert_eq!(output, case.expected.as_bytes(), "{}", case.scenario);
    }
}

#[tokio::test]
async fn logs_attempts_routes_attempt_summary() {
    let run_item_id = rpc::common::Uuid {
        value: "abcdef01-2345-6789-abcd-ef0123456780".to_string(),
    };
    let output = command_output(
        "logs attempts summary file",
        &[
            "machine-validation",
            "logs",
            "attempts",
            "--validation-id",
            VALIDATION_ID,
            "--test-id",
            TEST_ID,
        ],
        vec![
            Exchange {
                method: "/forge.Forge/FindMachineValidationRunItemIds",
                request: MachineValidationRunItemSearchFilter {
                    validation_id: Some(VALIDATION_ID.parse().expect("validation ID")),
                }
                .encode_to_vec(),
                response: Ok(MachineValidationRunItemIdList {
                    run_item_ids: vec![run_item_id.clone()],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindMachineValidationRunItemsByIds",
                request: MachineValidationRunItemsByIdsRequest {
                    run_item_ids: vec![run_item_id.clone()],
                }
                .encode_to_vec(),
                response: Ok(MachineValidationRunItemList {
                    run_items: vec![MachineValidationRunItem {
                        run_item_id: Some(run_item_id.clone()),
                        test_id: TEST_ID.to_string(),
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            },
            Exchange {
                method: "/forge.Forge/FindMachineValidationAttempts",
                request: MachineValidationAttemptSearchFilter {
                    run_item_id: Some(run_item_id),
                }
                .encode_to_vec(),
                response: Ok(MachineValidationAttemptList {
                    attempts: vec![MachineValidationAttempt {
                        attempt_id: Some(rpc::common::Uuid {
                            value: ATTEMPT_ID.to_string(),
                        }),
                        attempt_number: 2,
                        state: "Success".to_string(),
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            },
        ],
        true,
    )
    .await;
    assert_eq!(
        output,
        format!("attempt=2 id={ATTEMPT_ID} state=Success\n").as_bytes()
    );
}
