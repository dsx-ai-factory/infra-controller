// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge as pb;
use support::{Exchange, assert_success, run_command};

const SWITCH_ID: &str = "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60";
const MAC: &str = "00:11:22:33:44:55";
const SECOND_SWITCH_ID: &str = "sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

#[tokio::test]
async fn reset_commands_require_an_action_and_one_target_selector() {
    let cases = [
        (
            vec!["component-reset", "switch", "--switch-id", SWITCH_ID],
            "required arguments",
        ),
        (
            vec!["get-reset-status", "switch", "--mac-address", MAC],
            "required arguments",
        ),
        (
            vec!["component-reset", "--action", "sdn-factory-reset", "switch"],
            "required arguments",
        ),
        (
            vec![
                "get-reset-status",
                "--action",
                "sdn-factory-reset",
                "switch",
                "--switch-id",
                SWITCH_ID,
                "--mac-address",
                MAC,
            ],
            "cannot be used with",
        ),
        (
            vec![
                "get-reset-status",
                "--action",
                "sdn-factory-reset",
                "--domain-name",
                "nvl-domain",
                "switch",
                "--mac-address",
                MAC,
            ],
            "unexpected argument",
        ),
    ];
    for (args, expected) in cases {
        let args: Vec<_> = ["component-manager"].into_iter().chain(args).collect();
        let output = run_command(&args, None, vec![]).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        for field in ["error:", "Usage:", "--help", expected] {
            assert!(
                error.contains(field),
                "{args:?}: missing {field:?} in {error}"
            );
        }
    }
}

#[tokio::test]
async fn public_component_reset_returns_per_device_results_and_forwards_targets() {
    for (label, args, target, action, domain_name, results, format, success) in [
        (
            "switch ID and certificate domain",
            vec![
                "component-reset",
                "--action",
                "sdn-factory-reset",
                "--domain-name",
                "nvl-domain",
                "switch",
                "--switch-id",
                SWITCH_ID,
            ],
            pb::component_reset_request::Target::SwitchIds(pb::SwitchIdList {
                ids: vec![SWITCH_ID.parse().unwrap()],
            }),
            pb::ComponentResetAction::SdnFactoryReset,
            Some("nvl-domain".to_string()),
            vec![pb::ComponentResult {
                component_id: Some(SWITCH_ID.into()),
                ..Default::default()
            }],
            "ascii-table",
            true,
        ),
        (
            "NVOS MAC partial submission and backend error",
            vec![
                "component-reset",
                "--action",
                "nvos-factory-reset",
                "switch",
                "--mac-address",
                "00:11:22:33:44:55,00:11:22:33:44:66",
            ],
            pb::component_reset_request::Target::SwitchBmcMacs(pb::MacAddressList {
                mac_addresses: vec![MAC.into(), "00:11:22:33:44:66".into()],
            }),
            pb::ComponentResetAction::NvosFactoryReset,
            None,
            vec![
                pb::ComponentResult {
                    mac_address: Some(MAC.into()),
                    ..Default::default()
                },
                pb::ComponentResult {
                    mac_address: Some("00:11:22:33:44:66".into()),
                    status: pb::ComponentManagerStatusCode::InternalError as i32,
                    error: "submission outcome is unknown; do not automatically retry".into(),
                    ..Default::default()
                },
            ],
            "json",
            false,
        ),
    ] {
        let args: Vec<_> = ["--format", format, "component-manager"]
            .into_iter()
            .chain(args)
            .collect();
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/ComponentReset",
                request: pb::ComponentResetRequest {
                    target: Some(target),
                    action: action as i32,
                    domain_name,
                }
                .encode_to_vec(),
                response: Ok(pb::ComponentResetResponse { results }.encode_to_vec()),
            }],
        )
        .await;
        assert_eq!(output.status.success(), success, "{label}: {:?}", output);
        let output = String::from_utf8(output.stdout).unwrap();
        if format == "json" {
            let json: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(json[0]["component_id"], serde_json::Value::Null);
            assert_eq!(json[0]["mac_address"], MAC);
            assert_eq!(json[0]["status"], "success");
            assert_eq!(json[1]["status"], "internal-error");
            assert!(
                json[1]["error"]
                    .as_str()
                    .unwrap()
                    .contains("do not automatically retry")
            );
            assert_eq!(json.as_array().unwrap().len(), 2);
        } else {
            for field in [
                "Component ID / MAC",
                "Status",
                "Error",
                SWITCH_ID,
                "success",
                " - ",
            ] {
                assert!(
                    output.contains(field),
                    "{label}: missing {field:?} in {output}"
                );
            }
        }
        assert!(!output.contains("job_id"));
    }
}

#[tokio::test]
async fn public_reset_status_renders_device_states_and_errors() {
    for (label, args, target, action, statuses, format, success) in [
        (
            "text completion and backend failure",
            vec![
                "get-reset-status",
                "--action",
                "sdn-factory-reset",
                "switch",
                "--switch-id",
                SWITCH_ID,
                SECOND_SWITCH_ID,
            ],
            pb::get_component_reset_status_request::Target::SwitchIds(pb::SwitchIdList {
                ids: vec![
                    SWITCH_ID.parse().unwrap(),
                    SECOND_SWITCH_ID.parse().unwrap(),
                ],
            }),
            pb::ComponentResetAction::SdnFactoryReset,
            vec![
                pb::ComponentResetStatus {
                    result: Some(pb::ComponentResult {
                        component_id: Some(SWITCH_ID.into()),
                        ..Default::default()
                    }),
                    action: pb::ComponentResetAction::SdnFactoryReset as i32,
                    state: pb::ComponentResetState::Completed as i32,
                },
                pb::ComponentResetStatus {
                    result: Some(pb::ComponentResult {
                        component_id: Some(SECOND_SWITCH_ID.into()),
                        error: "reset failed".into(),
                        ..Default::default()
                    }),
                    action: pb::ComponentResetAction::SdnFactoryReset as i32,
                    state: pb::ComponentResetState::Failed as i32,
                },
            ],
            "ascii-table",
            false,
        ),
        (
            "MAC status with queued NVOS reset",
            vec![
                "get-reset-status",
                "--action",
                "nvos-factory-reset",
                "switch",
                "--mac-address",
                MAC,
            ],
            pb::get_component_reset_status_request::Target::SwitchBmcMacs(pb::MacAddressList {
                mac_addresses: vec![MAC.into()],
            }),
            pb::ComponentResetAction::NvosFactoryReset,
            vec![pb::ComponentResetStatus {
                result: Some(pb::ComponentResult {
                    mac_address: Some(MAC.into()),
                    ..Default::default()
                }),
                action: pb::ComponentResetAction::NvosFactoryReset as i32,
                state: pb::ComponentResetState::Queued as i32,
            }],
            "json",
            true,
        ),
        (
            "MAC status without a reference",
            vec![
                "get-reset-status",
                "--action",
                "sdn-factory-reset",
                "switch",
                "--mac-address",
                MAC,
            ],
            pb::get_component_reset_status_request::Target::SwitchBmcMacs(pb::MacAddressList {
                mac_addresses: vec![MAC.into()],
            }),
            pb::ComponentResetAction::SdnFactoryReset,
            vec![pb::ComponentResetStatus {
                result: Some(pb::ComponentResult {
                    mac_address: Some(MAC.into()),
                    status: pb::ComponentManagerStatusCode::NotFound as i32,
                    error: "no reset tracked".into(),
                    ..Default::default()
                }),
                action: pb::ComponentResetAction::SdnFactoryReset as i32,
                state: pb::ComponentResetState::Unknown as i32,
            }],
            "json",
            false,
        ),
    ] {
        let response = pb::GetComponentResetStatusResponse { statuses };
        let args: Vec<_> = ["--format", format, "component-manager"]
            .into_iter()
            .chain(args)
            .collect();
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/GetComponentResetStatus",
                request: pb::GetComponentResetStatusRequest {
                    target: Some(target),
                    action: action as i32,
                }
                .encode_to_vec(),
                response: Ok(response.encode_to_vec()),
            }],
        )
        .await;
        assert_eq!(output.status.success(), success, "{label}: {:?}", output);
        let output = String::from_utf8(output.stdout).unwrap();
        if format == "json" {
            let json: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(json[0]["result"]["component_id"], serde_json::Value::Null);
            assert_eq!(json[0]["result"]["mac_address"], MAC);
            assert_eq!(
                json[0]["action"],
                match action {
                    pb::ComponentResetAction::NvosFactoryReset => "nvos-factory-reset",
                    pb::ComponentResetAction::SdnFactoryReset => "sdn-factory-reset",
                    _ => unreachable!("test only queries SDN and NVOS resets"),
                },
            );
            assert_eq!(json[0]["state_code"], response.statuses[0].state);
            assert_eq!(
                json[0]["result"]["error"],
                response.statuses[0].result.as_ref().unwrap().error
            );
        } else {
            for field in [
                "Component ID / MAC",
                "Action",
                "Result",
                "State",
                "Error",
                SWITCH_ID,
                SECOND_SWITCH_ID,
                "sdn-factory-reset",
                "completed",
                "failed",
                "reset failed",
                " - ",
            ] {
                assert!(
                    output.contains(field),
                    "{label}: missing {field:?} in {output}"
                );
            }
        }
    }
}

#[tokio::test]
async fn public_rms_job_status_preserves_structured_records() {
    let parent = pb::RmsJobStatus {
        job_id: "parent-job".into(),
        execution_state: pb::RmsJobExecutionState::Completed as i32,
        state_description: "reset completed".into(),
        rack_id: Some("rack-1".into()),
        node_id: Some("switch-1".into()),
        result_json: "{\"result\":\"ok\"}".into(),
        created_at: Some(chrono::DateTime::from_timestamp(1, 0).unwrap().into()),
        updated_at: Some(chrono::DateTime::from_timestamp(2, 0).unwrap().into()),
        ..Default::default()
    };
    let child = pb::RmsJobStatus {
        job_id: "child-job".into(),
        parent_job_id: Some("parent-job".into()),
        execution_state: 87,
        error_code: 92,
        error_message: "backend diagnostic".into(),
        ..Default::default()
    };
    for (include_children, format) in [(false, "ascii-table"), (true, "json")] {
        let mut args = vec![
            "--format",
            format,
            "backend",
            "rms",
            "get-job-status",
            "--job-id",
            "parent-job",
        ];
        if include_children {
            args.push("--include-child-job-states");
        }
        let mut queried = parent.clone();
        let jobs = if include_children {
            queried.child_job_ids.push("child-job".into());
            vec![queried, child.clone()]
        } else {
            vec![queried]
        };
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/GetRmsJobStatus",
                request: pb::GetRmsJobStatusRequest {
                    job_id: "parent-job".into(),
                    include_child_job_states: include_children,
                }
                .encode_to_vec(),
                response: Ok(pb::GetRmsJobStatusResponse { job_states: jobs }.encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        let output = String::from_utf8(output.stdout).unwrap();
        if format == "json" {
            let json: serde_json::Value = serde_json::from_str(&output).unwrap();
            let jobs = json["job_states"].as_array().unwrap();
            assert_eq!(jobs.len(), 2);
            assert_eq!(jobs[0]["job_id"], "parent-job");
            assert_eq!(jobs[0]["parent_job_id"], serde_json::Value::Null);
            assert_eq!(jobs[0]["child_job_ids"][0], "child-job");
            assert_eq!(jobs[0]["rack_id"], "rack-1");
            assert_eq!(jobs[0]["node_id"], "switch-1");
            assert_eq!(jobs[0]["result_json"], parent.result_json);
            assert_eq!(jobs[0]["created_at"]["seconds"], 1);
            assert_eq!(jobs[0]["updated_at"]["seconds"], 2);
            assert_eq!(jobs[1]["parent_job_id"], "parent-job");
            assert_eq!(jobs[1]["execution_state"], "UNKNOWN");
            assert_eq!(jobs[1]["execution_state_code"], 87);
            assert_eq!(jobs[1]["error_code"], 92);
            assert_eq!(jobs[1]["error_message"], "backend diagnostic");
        } else {
            for field in [
                "Field",
                "Value",
                "Job ID",
                "Parent Job ID",
                "Child Job IDs",
                "State",
                "Description",
                "Error Code",
                "Error",
                "Result JSON",
                "Rack ID",
                "Node ID",
                "Created At",
                "Updated At",
                "parent-job",
                "RMS_JOB_EXECUTION_STATE_COMPLETED",
                "reset completed",
                "rack-1",
                "switch-1",
                &parent.result_json,
                "1970-01-01T00:00:01+00:00",
                "1970-01-01T00:00:02+00:00",
                " - ",
            ] {
                assert!(output.contains(field), "missing {field:?} in {output}");
            }
        }
    }
}
