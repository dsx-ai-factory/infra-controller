/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use prost::Message;
use rpc::common::MachineIdList;
use rpc::forge::{
    Machine, MachineList, MachineSearchConfig, MachineStateList, MachineStateSummary,
    MachineStatesByIdsRequest, MachinesByIdsRequest,
};
use support::{
    CommandOptions, ConcurrentStatePages, Exchange, assert_success, run_command,
    run_command_with_options,
};
use tonic::Status;

fn ids() -> Vec<MachineId> {
    let mut ids = vec![
        MachineId::new(MachineIdSource::Tpm, [1; 32], MachineType::Host),
        MachineId::new(MachineIdSource::Tpm, [2; 32], MachineType::Dpu),
    ];
    ids.sort_unstable();
    ids
}

fn compact(id: MachineId, state: &str) -> Exchange {
    Exchange {
        method: "/forge.Forge/FindMachineStatesByIds",
        request: MachineStatesByIdsRequest {
            machine_ids: vec![id],
        }
        .encode_to_vec(),
        response: Ok(MachineStateList {
            machines: vec![MachineStateSummary {
                id: Some(id),
                state: state.into(),
            }],
        }
        .encode_to_vec()),
    }
}

/// The server advertises a one-ID limit; a larger CLI page size must still
/// produce one request per machine. Sorting and width handling run in the binary.
#[tokio::test]
async fn machine_states_renders_bounded_sorted_pages() {
    let mut ids = ids();
    ids.push(MachineId::new(
        MachineIdSource::Tpm,
        [3; 32],
        MachineType::Host,
    ));
    ids.sort_unstable();
    let id_strings: Vec<_> = ids.iter().map(ToString::to_string).collect();
    for format in ["ascii-table", "csv", "json", "yaml"] {
        let output = run_command_with_options(
            &[
                "--format",
                format,
                "--sort-by",
                "state",
                "--internal-page-size",
                "25",
                "machine",
                "states",
                &id_strings[1],
                &id_strings[0],
                &id_strings[1],
                &id_strings[2],
                "--max-width",
                "State=8",
            ],
            None,
            vec![
                compact(ids[0], "Ready"),
                compact(ids[1], "DPUInitializing/WaitingForNetworkConfig"),
                compact(ids[2], ""),
            ],
            CommandOptions {
                unordered_state_pages: true,
                ..Default::default()
            },
        )
        .await;
        assert_success(&output);
        match format {
            "json" | "yaml" => {
                let actual: serde_json::Value = if format == "json" {
                    serde_json::from_slice(&output.stdout).unwrap()
                } else {
                    serde_yaml::from_slice(&output.stdout).unwrap()
                };
                assert_eq!(
                    actual,
                    serde_json::json!({"machines": [
                        {"id": id_strings[2], "state": ""},
                        {"id": id_strings[1], "state": "DPUInitializing/WaitingForNetworkConfig"},
                        {"id": id_strings[0], "state": "Ready"},
                    ]})
                );
            }
            _ => {
                let rows: Vec<Vec<String>> = if format == "csv" {
                    csv::ReaderBuilder::new()
                        .has_headers(false)
                        .from_reader(output.stdout.as_slice())
                        .records()
                        .map(|r| r.unwrap().iter().map(str::to_owned).collect())
                        .collect()
                } else {
                    std::str::from_utf8(&output.stdout)
                        .unwrap()
                        .lines()
                        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
                        .map(|line| line.split('|').map(|cell| cell.trim().to_owned()).collect())
                        .collect()
                };
                assert_eq!(
                    rows,
                    vec![
                        vec!["Id".into(), "State".into()],
                        vec![id_strings[2].clone(), String::new()],
                        vec![
                            id_strings[1].clone(),
                            if format == "csv" {
                                "DPUInitializing/WaitingForNetworkConfig".into()
                            } else {
                                "DPUIn...".into()
                            }
                        ],
                        vec![id_strings[0].clone(), "Ready".into()],
                    ]
                );
            }
        }
    }
}

/// Empty filtered fleets render a header without sending an invalid empty batch.
#[tokio::test]
async fn machine_states_selects_types_and_renders_empty_fleets() {
    for (flags, include_dpus, exclude_hosts, include_predicted_host) in [
        (vec![], true, false, true),
        (vec!["--hosts"], false, false, true),
        (vec!["--dpus"], true, true, false),
        (vec!["--hosts", "--dpus"], true, false, true),
    ] {
        let mut args = vec!["--format", "csv", "machine", "states"];
        args.extend(flags);
        let output = run_command(
            &args,
            None,
            vec![Exchange {
                method: "/forge.Forge/FindMachineIds",
                request: MachineSearchConfig {
                    include_dpus,
                    exclude_hosts,
                    include_predicted_host,
                    ..Default::default()
                }
                .encode_to_vec(),
                response: Ok(MachineIdList {
                    machine_ids: vec![],
                }
                .encode_to_vec()),
            }],
        )
        .await;
        assert_success(&output);
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "Id,State\n");
    }
}

/// Discovered IDs feed capped pages; out-of-order rows and a later switch to
/// full reads must preserve every result and both documented sort orders.
#[tokio::test]
async fn machine_states_pages_discovered_ids_and_preserves_late_fallback() {
    let mut ids = ids();
    ids.push(MachineId::new(
        MachineIdSource::Tpm,
        [3; 32],
        MachineType::Host,
    ));
    ids.sort_unstable();
    for (sort, expected_order) in [
        ("primary-id", vec![ids[0], ids[1], ids[2]]),
        ("state", vec![ids[2], ids[0], ids[1]]),
    ] {
        let mut rejected = compact(ids[2], "unused");
        rejected.response = Err(Status::unimplemented("unknown method"));
        let output = run_command_with_options(
            &[
                "--format",
                "json",
                "--sort-by",
                sort,
                "--internal-page-size",
                "2",
                "machine",
                "states",
            ],
            None,
            vec![
                Exchange {
                    method: "/forge.Forge/FindMachineIds",
                    request: MachineSearchConfig {
                        include_dpus: true,
                        include_predicted_host: true,
                        ..Default::default()
                    }
                    .encode_to_vec(),
                    response: Ok(MachineIdList {
                        machine_ids: vec![ids[2], ids[1], ids[0], ids[1]],
                    }
                    .encode_to_vec()),
                },
                Exchange {
                    method: "/forge.Forge/FindMachineStatesByIds",
                    request: MachineStatesByIdsRequest {
                        machine_ids: ids[..2].to_vec(),
                    }
                    .encode_to_vec(),
                    response: Ok(MachineStateList {
                        machines: vec![
                            MachineStateSummary {
                                id: Some(ids[1]),
                                state: "Ready".into(),
                            },
                            MachineStateSummary {
                                id: Some(ids[0]),
                                state: "Ready".into(),
                            },
                        ],
                    }
                    .encode_to_vec()),
                },
                rejected,
                Exchange {
                    method: "/forge.Forge/FindMachinesByIds",
                    request: MachinesByIdsRequest {
                        machine_ids: vec![ids[2]],
                        include_history: false,
                    }
                    .encode_to_vec(),
                    response: Ok(MachineList {
                        machines: vec![Machine {
                            id: Some(ids[2]),
                            state: "Created".into(),
                            ..Default::default()
                        }],
                    }
                    .encode_to_vec()),
                },
            ],
            CommandOptions {
                max_find_by_ids: 3,
                ..Default::default()
            },
        )
        .await;
        assert_success(&output);
        let expected: Vec<_> = expected_order
            .into_iter()
            .map(|id| {
                serde_json::json!({
                    "id": id.to_string(), "state": if id == ids[2] { "Created" } else { "Ready" }
                })
            })
            .collect();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::json!({"machines": expected}),
            "sort: {sort}",
        );
    }
}

/// Concurrent failures cancel pending pages. Ordered consumption prevents a later
/// success or compatibility rejection from skipping an earlier blocked page.
#[tokio::test]
async fn machine_states_orders_concurrent_pages_and_cancels_on_failure() {
    let mut ids: Vec<_> = (1..=6)
        .map(|byte| MachineId::new(MachineIdSource::Tpm, [byte; 32], MachineType::Host))
        .collect();
    ids.sort_unstable();
    let id_strings: Vec<_> = ids.iter().map(ToString::to_string).collect();
    let mut args = vec!["--format", "json", "machine", "states"];
    args.extend(id_strings.iter().rev().map(String::as_str));

    struct Case {
        status: Status,
        rejected_index: usize,
        pending_bodies: [bool; 4],
        falls_back: bool,
        diagnostic: &'static str,
        timeout_secs: Option<u64>,
    }
    for case in [
        Case {
            status: Status::unimplemented("compact method unavailable"),
            rejected_index: 0,
            pending_bodies: [false, true, true, true],
            falls_back: true,
            diagnostic: "",
            timeout_secs: None,
        },
        Case {
            status: Status::internal("invalid concurrent state"),
            rejected_index: 0,
            pending_bodies: [false, true, true, true],
            falls_back: false,
            diagnostic: "invalid concurrent state",
            timeout_secs: None,
        },
        Case {
            status: Status::unimplemented("later compact page rejected"),
            rejected_index: 2,
            pending_bodies: [true, false, false, true],
            falls_back: false,
            diagnostic: "read machine states before the deadline",
            timeout_secs: Some(1),
        },
    ] {
        let mut exchanges = vec![compact(ids[0], "Ready")];
        for (index, id) in ids[1..5].iter().enumerate() {
            let mut page = compact(*id, "Created");
            if index == case.rejected_index {
                page.response = Err(case.status.clone());
            }
            exchanges.push(page);
        }
        if case.falls_back {
            for id in &ids[1..] {
                exchanges.push(Exchange {
                    method: "/forge.Forge/FindMachinesByIds",
                    request: MachinesByIdsRequest {
                        machine_ids: vec![*id],
                        include_history: false,
                    }
                    .encode_to_vec(),
                    response: Ok(MachineList {
                        machines: vec![Machine {
                            id: Some(*id),
                            state: "Created".into(),
                            ..Default::default()
                        }],
                    }
                    .encode_to_vec()),
                });
            }
        }

        let output = run_command_with_options(
            &args,
            None,
            exchanges,
            CommandOptions {
                concurrent_state_pages: Some(ConcurrentStatePages {
                    ids: [ids[1], ids[2], ids[3], ids[4]],
                    pending_bodies: case.pending_bodies,
                }),
                request_timeout_secs: case.timeout_secs,
                ..Default::default()
            },
        )
        .await;
        if case.falls_back {
            assert_success(&output);
            let expected: Vec<_> = ids
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "state": if *id == ids[0] { "Ready" } else { "Created" },
                    })
                })
                .collect();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
                serde_json::json!({"machines": expected}),
            );
        } else {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(diagnostic.contains("read machine states"), "{diagnostic}");
            assert!(diagnostic.contains(case.diagnostic), "{diagnostic}");
        }
    }
}

/// Discovery and batch-limit failures must stop before state requests or output.
#[tokio::test]
async fn machine_states_reports_prerequisite_failures() {
    let id = ids()[0].to_string();
    struct Case<'a> {
        args: Vec<&'a str>,
        exchanges: Vec<Exchange>,
        options: CommandOptions,
        context: &'static str,
        cause: &'static str,
    }
    for case in [
        Case {
            args: vec!["machine", "states"],
            exchanges: vec![Exchange {
                method: "/forge.Forge/FindMachineIds",
                request: MachineSearchConfig {
                    include_dpus: true,
                    include_predicted_host: true,
                    ..Default::default()
                }
                .encode_to_vec(),
                response: Err(Status::unavailable("inventory unavailable")),
            }],
            options: CommandOptions::default(),
            context: "list machine IDs",
            cause: "inventory unavailable",
        },
        Case {
            args: vec!["machine", "states", &id],
            exchanges: vec![],
            options: CommandOptions {
                runtime_config_error: Some(tonic::Code::Internal),
                ..Default::default()
            },
            context: "read the machine batch limit",
            cause: "batch limit unavailable",
        },
    ] {
        let output = run_command_with_options(&case.args, None, case.exchanges, case.options).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains(case.context), "{diagnostic}");
        assert!(diagnostic.contains(case.cause), "{diagnostic}");
    }
}

/// Each independently awaited RPC bounds its response body. The fallback case
/// also verifies that an earlier successful page cannot produce partial output.
#[tokio::test]
async fn machine_states_bounds_response_bodies() {
    let ids = ids();
    let first_id = ids[0].to_string();
    let second_id = ids[1].to_string();
    let explicit = vec!["machine", "states", first_id.as_str()];
    let mut rejected = compact(ids[1], "unused");
    rejected.response = Err(Status::unimplemented("unknown method"));
    let cases = [
        (
            "/forge.Forge/FindMachineIds",
            vec!["machine", "states"],
            vec![Exchange {
                method: "/forge.Forge/FindMachineIds",
                request: MachineSearchConfig {
                    include_dpus: true,
                    include_predicted_host: true,
                    ..Default::default()
                }
                .encode_to_vec(),
                response: Ok(MachineIdList {
                    machine_ids: vec![ids[0]],
                }
                .encode_to_vec()),
            }],
            "list machine IDs before the deadline",
        ),
        (
            "/forge.Forge/Version",
            explicit.clone(),
            vec![],
            "read the machine batch limit before the deadline",
        ),
        (
            "/forge.Forge/FindMachineStatesByIds",
            explicit,
            vec![compact(ids[0], "Ready")],
            "read machine states before the deadline",
        ),
        (
            "/forge.Forge/FindMachinesByIds",
            vec!["machine", "states", first_id.as_str(), second_id.as_str()],
            vec![
                compact(ids[0], "Ready"),
                rejected,
                Exchange {
                    method: "/forge.Forge/FindMachinesByIds",
                    request: MachinesByIdsRequest {
                        machine_ids: vec![ids[1]],
                        include_history: false,
                    }
                    .encode_to_vec(),
                    response: Ok(MachineList::default().encode_to_vec()),
                },
            ],
            "read full machine records before the deadline",
        ),
    ];
    for (method, args, exchanges, context) in cases {
        let output = run_command_with_options(
            &args,
            None,
            exchanges,
            CommandOptions {
                request_timeout_secs: Some(1),
                pending_body_method: Some(method),
                ..Default::default()
            },
        )
        .await;
        assert!(!output.status.success(), "{method}");
        assert!(output.stdout.is_empty(), "{method}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains(context), "{method}: {diagnostic}");
    }
}

/// Unknown methods can be rejected at routing or authorization on older Core.
/// After the rejected page, use only full reads with their own authorization.
#[tokio::test]
async fn machine_states_falls_back_for_older_servers() {
    let ids = ids();
    let id_strings: Vec<_> = ids.iter().map(ToString::to_string).collect();
    for status in [
        Status::unimplemented("unknown method"),
        Status::permission_denied("unknown permission"),
    ] {
        let mut probe = compact(ids[0], "unused");
        probe.response = Err(status);
        let mut exchanges = vec![probe];
        for id in &ids {
            exchanges.push(Exchange {
                method: "/forge.Forge/FindMachinesByIds",
                request: MachinesByIdsRequest {
                    machine_ids: vec![*id],
                    include_history: false,
                }
                .encode_to_vec(),
                response: Ok(MachineList {
                    machines: vec![Machine {
                        id: Some(*id),
                        state: "Ready".into(),
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            });
        }
        let output = run_command_with_options(
            &[
                "--format",
                "csv",
                "machine",
                "states",
                &id_strings[1],
                &id_strings[0],
            ],
            None,
            exchanges,
            CommandOptions {
                unordered_state_pages: true,
                ..Default::default()
            },
        )
        .await;
        assert_success(&output);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "Id,State\n{},Ready\n{},Ready\n",
                id_strings[0], id_strings[1]
            )
        );
    }
}

/// Do not replace a failed compact read with a plausible full-read result or
/// print earlier pages after a later failure. Omit missing IDs from the output.
#[tokio::test]
async fn machine_states_preserves_failure_and_absence() {
    let ids = ids();
    let id_strings: Vec<_> = ids.iter().map(ToString::to_string).collect();
    for (reply, expected_error) in [
        (
            Err(Status::internal("invalid persisted state")),
            Some("invalid persisted state"),
        ),
        (
            Ok(MachineStateList {
                machines: vec![MachineStateSummary {
                    id: None,
                    state: "Ready".into(),
                }],
            }
            .encode_to_vec()),
            Some("missing a machine ID"),
        ),
        (
            Ok(MachineStateList { machines: vec![] }.encode_to_vec()),
            None,
        ),
    ] {
        let mut second = compact(ids[1], "unused");
        second.response = reply;
        let output = run_command(
            &[
                "--format",
                "json",
                "machine",
                "states",
                &id_strings[0],
                &id_strings[1],
            ],
            None,
            vec![compact(ids[0], "Ready"), second],
        )
        .await;
        if let Some(message) = expected_error {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
            if message == "invalid persisted state" {
                assert!(String::from_utf8_lossy(&output.stderr).contains("read machine states"));
            }
        } else {
            assert_success(&output);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
                serde_json::json!({"machines": [{"id": id_strings[0], "state": "Ready"}]})
            );
        }
    }

    let mut denied_probe = compact(ids[0], "unused");
    denied_probe.response = Err(Status::permission_denied("compact denied"));
    let output = run_command(
        &["machine", "states", &id_strings[0]],
        None,
        vec![
            denied_probe,
            Exchange {
                method: "/forge.Forge/FindMachinesByIds",
                request: MachinesByIdsRequest {
                    machine_ids: vec![ids[0]],
                    include_history: false,
                }
                .encode_to_vec(),
                response: Err(Status::permission_denied("full read denied")),
            },
        ],
    )
    .await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("full read denied"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("read full machine records for lifecycle states")
    );
}
