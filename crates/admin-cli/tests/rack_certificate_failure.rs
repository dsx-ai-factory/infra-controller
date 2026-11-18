// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge;
use serde_json::{Value, json};
use support::{Exchange, assert_success, run_command};

const RACK: &str = "rack-certificates";

fn failed_state() -> Value {
    json!({
        "state": "error", "cause": "switch certificate configuration failed",
        "recovery_policy": "maintenance_request_required",
        "certificate_failure": {
            "job_id": "parent-job",
            "node_failures": [
                {"node_id": "switch-a", "job_id": "child-a", "error_code": 13, "message": "operation timed out"},
                {"node_id": "switch-b", "job_id": null, "error_code": null, "message": "authentication failed"}
            ]
        }
    })
}

fn show_exchanges(state: &Value) -> Vec<Exchange> {
    let rack_id: carbide_uuid::rack::RackId = RACK.parse().unwrap();
    vec![
        Exchange {
            method: "/forge.Forge/FindRacksByIds",
            request: forge::RacksByIdsRequest {
                rack_ids: vec![rack_id.clone()],
            }
            .encode_to_vec(),
            response: Ok(forge::RackList {
                racks: vec![forge::Rack {
                    id: Some(rack_id.clone()),
                    rack_state: "Error(switch certificate configuration failed)".into(),
                    status: Some(forge::RackStatus {
                        lifecycle: Some(forge::LifecycleStatus {
                            state: state.to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }
            .encode_to_vec()),
        },
        Exchange {
            method: "/forge.Forge/FindMachineIds",
            request: forge::MachineSearchConfig {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(rpc::common::MachineIdList::default().encode_to_vec()),
        },
        Exchange {
            method: "/forge.Forge/FindPowerShelfIds",
            request: forge::PowerShelfSearchFilter {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(forge::PowerShelfIdList::default().encode_to_vec()),
        },
        Exchange {
            method: "/forge.Forge/FindSwitchIds",
            request: forge::SwitchSearchFilter {
                rack_id: Some(rack_id),
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(forge::SwitchIdList::default().encode_to_vec()),
        },
    ]
}

fn history_exchanges(state: &Value) -> Vec<Exchange> {
    vec![Exchange {
        method: "/forge.Forge/FindRackStateHistories",
        request: forge::RackStateHistoriesRequest {
            rack_ids: vec![RACK.parse().unwrap()],
        }
        .encode_to_vec(),
        response: Ok(forge::StateHistories {
            histories: [(
                RACK.into(),
                forge::StateHistoryRecords {
                    records: vec![
                        forge::StateHistoryRecord {
                            state: state.to_string(),
                            version: "V1".into(),
                            time: None,
                        },
                        forge::StateHistoryRecord {
                            state: "{\"state\":\"error\",\"cause\":\"legacy failure\"}".into(),
                            version: "V0".into(),
                            time: None,
                        },
                    ],
                },
            )]
            .into_iter()
            .collect(),
        }
        .encode_to_vec()),
    }]
}

#[tokio::test]
async fn rack_certificate_failure_public_commands_preserve_structured_output() {
    let state = failed_state();
    // Each row covers a public serialization boundary, with no helper-only proof.
    for (command, format, exchanges) in [
        ("show", "json", show_exchanges(&state)),
        ("state-history", "yaml", history_exchanges(&state)),
    ] {
        let output = run_command(
            &["--format", format, "rack", command, RACK],
            None,
            exchanges,
        )
        .await;
        assert_success(&output);
        let value: Value = if format == "json" {
            serde_json::from_slice(&output.stdout).unwrap()
        } else {
            serde_yaml::from_slice(&output.stdout).unwrap()
        };
        let result = if command == "show" { &value } else { &value[0] };
        assert_eq!(result["certificate_failure"], state["certificate_failure"]);
        if command == "state-history" {
            assert!(value[1]["certificate_failure"].is_null());
            assert_eq!(value[1]["state"], "Error(legacy failure)");
        }
    }
}

#[tokio::test]
async fn rack_certificate_failure_public_tables_include_details_and_empty_cells() {
    let failure = failed_state();
    let legacy = json!({"state": "error", "cause": "legacy failure"});
    for (command, state, exchanges) in [
        ("show", &failure, show_exchanges(&failure)),
        ("show", &legacy, show_exchanges(&legacy)),
        ("state-history", &failure, history_exchanges(&failure)),
    ] {
        let output = run_command(&["rack", command, RACK], None, exchanges).await;
        assert_success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Certificate failure"), "{text}");
        if state.get("certificate_failure").is_some() {
            for expected in [
                "parent-job",
                "switch-a",
                "child-a",
                "operation timed out",
                "switch-b",
                "authentication failed",
            ] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
        } else {
            let row = text
                .lines()
                .find(|line| line.contains("Certificate failure"))
                .unwrap();
            let cells: Vec<_> = row.split('|').map(str::trim).collect();
            assert_eq!(cells[2], "");
        }
    }
}
