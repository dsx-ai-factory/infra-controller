// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::task::Poll;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::{CapturedLog, capture_logs_async};
use carbide_test_support::{Check, check_values};
use tokio::sync::oneshot;

use super::*;

/// Supplies explicit successful inventories, including empty collections.
fn reply(ids: &[&str]) -> rms::BatchGetFirmwareInventoryResponse {
    rms::BatchGetFirmwareInventoryResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        }),
        nodes: ids
            .iter()
            .map(|id| rms::NodeFirmwareInventory {
                node_id: (*id).to_owned(),
                firmware_list: Vec::new(),
            })
            .collect(),
    }
}

/// Expected metadata keeps every absent backend field explicit.
fn entry(name: &str, version: &str, target: &str) -> FirmwareInventoryEntry {
    FirmwareInventoryEntry {
        inventory_id: None,
        firmware: Ok(FirmwareComponent {
            id: None,
            resource_uri: None,
            name: Some(name.to_owned()),
            version: Some(version.to_owned()),
            update_target: Some(target.to_owned()),
            description: None,
            release_date: None,
        }),
    }
}

/// Distinct payloads expose response-order and node-association mistakes.
fn populated_reply(ids: &[&str]) -> rms::BatchGetFirmwareInventoryResponse {
    let mut response = reply(ids);
    for node in &mut response.nodes {
        node.firmware_list = vec![rms::FirmwareInventoryInfo {
            name: node.node_id.clone(),
            version: format!("v-{}", node.node_id),
            target: format!("/{}", node.node_id),
            ..Default::default()
        }];
    }
    response
}

/// Names each slot independently from the RMS node ID to expose reordering.
fn mac(slot: usize) -> MacAddress {
    MacAddress::new([2, 0, 0, 0, (slot >> 8) as u8, slot as u8])
}

/// Gives every node a stable rack-local ID for exact batch assertions.
fn nodes(count: usize) -> Vec<rms::NodeInfo> {
    (0..count)
        .map(|slot| rms::NodeInfo {
            rack_id: "rack-a".to_owned(),
            node_id: format!("node-{slot:03}"),
            ..Default::default()
        })
        .collect()
}

/// Runs the query seam without repeating database preparation policy tests.
async fn read(
    mock: &MockRmsApi,
    nodes: Vec<rms::NodeInfo>,
) -> Vec<ComponentFirmwareInventoryResult> {
    let macs: Vec<_> = (0..nodes.len()).map(mac).collect();
    query(
        mock,
        &macs,
        async { Ok(nodes.into_iter().map(Ok).collect()) },
        Instant::now() + READ_TIMEOUT,
    )
    .await
    .expect("inventory query")
}

/// Each row isolates evidence that must override a successful aggregate.
#[test]
fn response_integrity_preserves_independent_healthy_nodes() {
    let good = reply(&["bad", "good"]);
    let mut missing = good.clone();
    missing.nodes.remove(0);
    let mut duplicate = good.clone();
    duplicate.nodes.push(duplicate.nodes[0].clone());
    let operation = rms::NodeOperationResult {
        node_id: "bad".to_owned(),
        status: rms::ReturnCode::Success as i32,
        ..Default::default()
    };
    let mut duplicate_operation = good.clone();
    duplicate_operation.response.as_mut().unwrap().node_results =
        vec![operation.clone(), operation.clone()];
    let mut failed = good.clone();
    failed.response.as_mut().unwrap().node_results = vec![rms::NodeOperationResult {
        status: rms::ReturnCode::Failure as i32,
        ..operation.clone()
    }];
    let mut unknown = good.clone();
    unknown.response.as_mut().unwrap().node_results = vec![rms::NodeOperationResult {
        status: 999,
        ..operation.clone()
    }];
    let mut error_text = good.clone();
    error_text.response.as_mut().unwrap().node_results = vec![rms::NodeOperationResult {
        error_message: "private backend body".to_owned(),
        ..operation
    }];
    let mut single_success = good.clone();
    single_success.response.as_mut().unwrap().node_results = vec![rms::NodeOperationResult {
        node_id: "bad".to_owned(),
        status: rms::ReturnCode::Success as i32,
        ..Default::default()
    }];
    let mut unspecified = good.clone();
    unspecified.response.as_mut().unwrap().node_results = vec![rms::NodeOperationResult {
        node_id: "bad".to_owned(),
        status: rms::ReturnCode::Unspecified as i32,
        ..Default::default()
    }];
    let mut aggregate_failure = good;
    aggregate_failure.response.as_mut().unwrap().status = rms::ReturnCode::Failure as i32;

    check_values(
        [
            Check {
                scenario: "missing inventory",
                input: missing,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "duplicate inventory",
                input: duplicate,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "duplicate operation",
                input: duplicate_operation,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "failed operation",
                input: failed,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "unknown operation status",
                input: unknown,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "success with error text",
                input: error_text,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "single successful operation",
                input: single_success,
                expect: Ok(Vec::new()),
            },
            Check {
                scenario: "unspecified operation",
                input: unspecified,
                expect: Err(FirmwareReadError::ReadFailed),
            },
            Check {
                scenario: "aggregate failure retains inventory",
                input: aggregate_failure,
                expect: Ok(Vec::new()),
            },
        ],
        |response| {
            assert_eq!(inventory_for_node(&response, "good"), Ok(Vec::new()));
            inventory_for_node(&response, "bad")
        },
    );
    // Remove only the envelope: the populated inventory is not sufficient.
    let mut missing_envelope = populated_reply(&["bad"]);
    missing_envelope.response = None;
    assert_eq!(
        inventory_for_node(&missing_envelope, "bad"),
        Err(FirmwareReadError::ReadFailed)
    );
}

/// RMS scalar strings cannot distinguish omitted fields from empty ones.
#[test]
fn projection_preserves_members_and_enforces_retained_metadata_limits() {
    let mut response = reply(&["node"]);
    let empty = rms::FirmwareInventoryInfo::default();
    let full = rms::FirmwareInventoryInfo {
        name: "same".to_owned(),
        version: "v1".to_owned(),
        target: "/target".to_owned(),
        ..Default::default()
    };
    response.nodes[0].firmware_list = vec![
        full.clone(),
        empty.clone(),
        full,
        rms::FirmwareInventoryInfo {
            name: "other".to_owned(),
            version: "v2".to_owned(),
            target: "/other".to_owned(),
            ..Default::default()
        },
    ];

    let projected = inventory_for_node(&response, "node").expect("project members");

    assert_eq!(
        projected,
        [
            entry("same", "v1", "/target"),
            entry("", "", ""),
            entry("same", "v1", "/target"),
            entry("other", "v2", "/other")
        ]
    );

    // Exact limits use literals so changing a production constant cannot
    // silently change the promised retention budget.
    for (label, members, expected) in [
        ("member count boundary", vec![empty.clone(); 256], Ok(256)),
        (
            "member count exceeded",
            vec![empty.clone(); 257],
            Err(FirmwareReadError::LimitExceeded),
        ),
    ] {
        response.nodes[0].firmware_list = members;
        assert_eq!(
            inventory_for_node(&response, "node").map(|items| items.len()),
            expected,
            "{label}"
        );
    }
    for field in ["name", "version", "target"] {
        let mut member = empty.clone();
        match field {
            "name" => member.name = "x".repeat(1025),
            "version" => member.version = "x".repeat(1025),
            _ => member.target = "x".repeat(1025),
        }
        response.nodes[0].firmware_list = vec![member, empty.clone()];
        let entries = inventory_for_node(&response, "node").unwrap();
        assert_eq!(
            entries[0].firmware,
            Err(FirmwareReadError::LimitExceeded),
            "{field}"
        );
        assert!(
            entries[1].firmware.is_ok(),
            "healthy peer after oversized {field}"
        );
    }
    // 21 full members plus one 1 KiB name fill 64 KiB exactly; the next
    // nonempty member must fail without consuming the final empty member.
    response.nodes[0].firmware_list = vec![
        rms::FirmwareInventoryInfo {
            name: "x".repeat(1024),
            version: "v".repeat(1024),
            target: "t".repeat(1024),
            ..empty.clone()
        };
        21
    ];
    response.nodes[0].firmware_list.extend([
        rms::FirmwareInventoryInfo {
            name: "x".repeat(1024),
            ..empty.clone()
        },
        rms::FirmwareInventoryInfo {
            name: "x".to_owned(),
            ..empty.clone()
        },
        empty,
    ]);
    let entries = inventory_for_node(&response, "node").unwrap();
    assert!(entries[..22].iter().all(|entry| entry.firmware.is_ok()));
    assert_eq!(entries[22].firmware, Err(FirmwareReadError::LimitExceeded));
    assert!(entries[23].firmware.is_ok());
}

/// Chunking and transport errors must preserve every original result slot.
#[tokio::test]
async fn batches_are_sorted_bounded_and_retain_completed_peers() {
    let mock = MockRmsApi::new();
    let sorted = nodes(130);
    let ids: Vec<_> = sorted[..64]
        .iter()
        .map(|node| node.node_id.as_str())
        .collect();
    let mut response = populated_reply(&ids);
    response.nodes.reverse();
    mock.enqueue_batch_get_firmware_inventory(Ok(response))
        .await;
    mock.enqueue_batch_get_firmware_inventory(Err(
        tonic::Status::unavailable("private body").into()
    ))
    .await;
    mock.enqueue_batch_get_firmware_inventory(Err(
        tonic::Status::unimplemented("private body").into()
    ))
    .await;
    let mut input = sorted.clone();
    input.reverse();

    let results = read(&mock, input).await;

    assert_eq!(results.len(), 130);
    for (slot, result) in results.iter().enumerate() {
        assert_eq!(result.mac_address, mac(slot));
        let expected = match slot {
            0..2 => Err(FirmwareReadError::Unsupported),
            2..66 => Err(FirmwareReadError::ReadFailed),
            _ => {
                let id = &sorted[129 - slot].node_id;
                Ok(vec![entry(id, &format!("v-{id}"), &format!("/{id}"))])
            }
        };
        assert_eq!(result.inventory, expected, "slot {slot}");
    }
    let calls = mock.batch_get_firmware_inventory_calls().await;
    assert_eq!(calls.len(), 3);
    for (call, expected) in calls.iter().zip(sorted.chunks(64)) {
        assert_eq!(call.nodes.as_ref().unwrap().nodes, expected);
    }
}

/// A/B/A descriptor conflicts stay rejected; rack-local IDs remain separate.
#[tokio::test]
async fn duplicates_share_only_identical_queries() {
    for conflict in ["host", "credentials"] {
        let mock = MockRmsApi::new();
        let mut input = nodes(2);
        let mut original = input[0].clone();
        original.bmc_endpoint = Some(rms::Endpoint {
            credentials: Some(rms::Credentials {
                auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                    username: "admin".to_owned(),
                    password: "first".to_owned(),
                })),
            }),
            ..Default::default()
        });
        input[0] = original.clone();
        let mut conflicting = original.clone();
        if conflict == "host" {
            conflicting.host_endpoint = Some(rms::Endpoint {
                port: 443,
                ..Default::default()
            });
        } else {
            // Equal connection fields with a different password still conflict.
            conflicting.bmc_endpoint.as_mut().unwrap().credentials = Some(rms::Credentials {
                auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                    username: "admin".to_owned(),
                    password: "second".to_owned(),
                })),
            });
        }
        let mut other_rack = original.clone();
        other_rack.rack_id = "rack-b".to_owned();
        input.extend([conflicting, original, input[1].clone(), other_rack]);
        mock.enqueue_batch_get_firmware_inventory(Ok(reply(&["node-001"])))
            .await;
        mock.enqueue_batch_get_firmware_inventory(Ok(reply(&["node-000"])))
            .await;

        let results = read(&mock, input.clone()).await;

        assert_eq!(
            results
                .iter()
                .map(|result| result.mac_address)
                .collect::<Vec<_>>(),
            (0..6).map(mac).collect::<Vec<_>>()
        );
        for (slot, result) in results.iter().enumerate() {
            assert_eq!(
                result.inventory,
                if [0, 2, 3].contains(&slot) {
                    Err(FirmwareReadError::ReadFailed)
                } else {
                    Ok(Vec::new())
                }
            );
        }
        let calls = mock.batch_get_firmware_inventory_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].nodes.as_ref().unwrap().nodes, [input[1].clone()]);
        assert_eq!(calls[1].nodes.as_ref().unwrap().nodes, [input[5].clone()]);
    }
}

/// Admission rejects excessive work before polling any preparation future.
#[tokio::test]
async fn endpoint_admission_checks_the_exact_boundary() {
    let mock = MockRmsApi::new();
    for count in [0, 256, 257] {
        let macs: Vec<_> = (0..count).map(mac).collect();
        let result = query(
            &mock,
            &macs,
            async {
                assert_eq!(count, 256, "empty and oversized inputs must not prepare");
                Ok(vec![Err("private preparation detail".to_owned()); count])
            },
            Instant::now() + READ_TIMEOUT,
        )
        .await;
        if count == 0 {
            assert_eq!(result.unwrap(), []);
        } else if count == 256 {
            let results = result.unwrap();
            assert_eq!(results.len(), 256);
            assert!(
                results
                    .iter()
                    .all(|result| result.inventory == Err(FirmwareReadError::ReadFailed))
            );
        } else {
            assert!(matches!(
                result,
                Err(ComponentManagerError::InvalidArgument(_))
            ));
        }
    }
    assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
}

/// Paused time applies only to the private seam, which performs no SQL.
#[tokio::test(start_paused = true)]
async fn preparation_and_rpc_share_one_owned_deadline() {
    let proof = async {
        let mock = MockRmsApi::new();
        let input = nodes(129);
        let macs: Vec<_> = (0..129).map(mac).collect();
        let ids: Vec<_> = input[..64]
            .iter()
            .map(|node| node.node_id.as_str())
            .collect();
        mock.enqueue_batch_get_firmware_inventory(Ok(populated_reply(&ids)))
            .await;
        let deadline = Instant::now() + READ_TIMEOUT;
        let (first_arrival, first_release) = mock.block_next_batch_get_firmware_inventory().await;
        let prepare = async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            Ok(input.into_iter().map(Ok).collect())
        };
        let (results, logs) = capture_logs_async(async {
            let read = query(&mock, &macs, prepare, deadline);
            tokio::pin!(read);
            tokio::select! { _ = first_arrival => {}, _ = &mut read => panic!("first RPC must wait") }
            let (arrival, release) = mock.block_next_batch_get_firmware_inventory().await;
            first_release.send(()).expect("release successful first batch");
            tokio::select! { _ = arrival => {}, _ = &mut read => panic!("second RPC must wait") }
            assert_eq!(deadline - Instant::now(), Duration::from_secs(10));

            let results = read.await.unwrap();

            assert!(release.is_closed(), "deadline drops the second RPC");
            results
        }).await;
        assert_eq!(Instant::now(), deadline);
        assert_eq!(results.len(), 129);
        for (slot, result) in results.iter().enumerate() {
            assert_eq!(result.mac_address, mac(slot));
            let id = format!("node-{slot:03}");
            assert_eq!(
                result.inventory,
                if slot < 64 {
                    Ok(vec![entry(&id, &format!("v-{id}"), &format!("/{id}"))])
                } else {
                    Err(FirmwareReadError::DeadlineExceeded)
                }
            );
        }
        assert_eq!(mock.batch_get_firmware_inventory_calls().await.len(), 2);
        assert_eq!(
            logs.iter()
                .filter(|log| log.message == "external call failed"
                    && log.field("operation") == Some("batch_get_firmware_inventory"))
                .count(),
            1
        );
        assert!(
            logs.iter()
                .any(|log| log.field("category") == Some("unattempted_deadline"))
        );
        assert_eq!(READ_TIMEOUT, Duration::from_secs(30));
        assert_eq!(STATEMENT_TIMEOUT, Duration::from_secs(5));

        let preparation_timeout = query(
            &mock,
            &macs,
            std::future::pending(),
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(
            preparation_timeout
                .iter()
                .all(|result| result.inventory == Err(FirmwareReadError::DeadlineExceeded))
        );
        assert_eq!(mock.batch_get_firmware_inventory_calls().await.len(), 2);

        // `advance` changes the clock on its first poll, then yields. Return a
        // ready result in that same poll so the post-deadline check is required.
        for ready_error in [false, true] {
            let deadline = Instant::now() + Duration::from_secs(30);
            let advance = tokio::time::advance(Duration::from_secs(30));
            tokio::pin!(advance);
            let result = before_deadline(
                deadline,
                std::future::poll_fn(|cx| {
                    assert!(advance.as_mut().poll(cx).is_pending());
                    Poll::Ready(if ready_error { Err(()) } else { Ok(()) })
                }),
            )
            .await;
            assert_eq!(result, Err(FirmwareReadError::DeadlineExceeded));
        }
    };
    tokio::time::timeout(Duration::from_secs(300), proof)
        .await
        .expect("deadline proof must finish within its independent watchdog");
}

/// Dropping the caller must drop both preparation and an in-flight RPC.
#[tokio::test]
async fn cancellation_drops_owned_work() {
    let proof = async {
        let mock = MockRmsApi::new();
        let macs = [mac(0)];
        let (entered, arrival) = oneshot::channel();
        let (release, held) = oneshot::channel::<()>();
        let prepare = async {
            entered.send(()).unwrap();
            held.await.unwrap();
            Ok(vec![Ok(nodes(1).remove(0))])
        };
        let mut read = Box::pin(query(&mock, &macs, prepare, Instant::now() + READ_TIMEOUT));
        tokio::select! { _ = arrival => {}, _ = &mut read => panic!("preparation must wait") }
        drop(read);
        assert!(release.is_closed());
        assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());

        let (arrival, release) = mock.block_next_batch_get_firmware_inventory().await;
        let mut read = Box::pin(self::read(&mock, nodes(65)));
        tokio::select! { _ = arrival => {}, _ = &mut read => panic!("RPC must wait") }
        drop(read);
        assert!(release.is_closed());
        assert_eq!(mock.batch_get_firmware_inventory_calls().await.len(), 1);
    };
    tokio::time::timeout(Duration::from_secs(60), proof)
        .await
        .expect("cancellation gates must finish within watchdog");
}

/// Diagnostics identify the failed stage without retaining private backend text.
#[tokio::test]
async fn transport_and_tls_text_stays_out_of_results_and_logs() {
    const PRIVATE: &str = "firmware-private-credential-and-response-body";
    let safe = |logs: &[CapturedLog], category: &str| {
        assert!(
            logs.iter()
                .any(|log| log.field("category") == Some(category)),
            "{category}"
        );
        assert!(logs.iter().all(|log| {
            !log.message.contains(PRIVATE)
                && log
                    .fields
                    .iter()
                    .all(|(key, value)| !key.contains(PRIVATE) && !value.contains(PRIVATE))
        }));
    };
    for (category, code, error, expected) in [
        (
            "rpc",
            Some("Unavailable"),
            RackManagerError::ApiInvocationError(tonic::Status::unavailable(PRIVATE)),
            FirmwareReadError::ReadFailed,
        ),
        (
            "tls",
            None,
            RackManagerError::TlsError(librms::RmsTlsClientError::Connection(PRIVATE.to_owned())),
            FirmwareReadError::ReadFailed,
        ),
        (
            "rpc",
            Some("Unimplemented"),
            RackManagerError::ApiInvocationError(tonic::Status::unimplemented(PRIVATE)),
            FirmwareReadError::Unsupported,
        ),
    ] {
        let mock = MockRmsApi::new();
        mock.enqueue_batch_get_firmware_inventory(Err(error)).await;

        let (results, logs) = capture_logs_async(read(&mock, nodes(1))).await;

        assert_eq!(results[0].inventory, Err(expected));
        safe(&logs, category);
        let diagnostic = logs
            .iter()
            .find(|log| log.field("category") == Some(category))
            .unwrap();
        assert_eq!(diagnostic.field("rack_id"), Some("rack-a"));
        if let Some(code) = code {
            assert_eq!(diagnostic.field("grpc_status_code"), Some(code));
        }
        let warning = logs
            .iter()
            .find(|log| log.message == "external call failed")
            .unwrap();
        assert_eq!(warning.level, tracing::Level::WARN);
        assert_eq!(warning.field("backend"), Some("rms"));
        assert_eq!(warning.field("error"), Some(expected.to_string().as_str()));
    }
    for category in [
        "preparation_error",
        "preparation_length",
        "endpoint_preparation",
    ] {
        let mock = MockRmsApi::new();
        let macs = [mac(0)];
        let prepare = async {
            match category {
                "preparation_error" => Err(ComponentManagerError::Internal(PRIVATE.to_owned())),
                "preparation_length" => Ok(Vec::new()),
                _ => Ok(vec![Err(PRIVATE.to_owned())]),
            }
        };

        let (results, logs) =
            capture_logs_async(query(&mock, &macs, prepare, Instant::now() + READ_TIMEOUT)).await;

        assert_eq!(
            results.unwrap()[0].inventory,
            Err(FirmwareReadError::ReadFailed)
        );
        safe(&logs, category);
        let diagnostic = logs
            .iter()
            .find(|log| log.field("category") == Some(category))
            .unwrap();
        match category {
            "preparation_error" => assert_eq!(diagnostic.field("component_count"), Some("1")),
            "preparation_length" => {
                assert_eq!(diagnostic.field("expected_count"), Some("1"));
                assert_eq!(diagnostic.field("actual_count"), Some("0"));
            }
            _ => assert_eq!(
                diagnostic.field("management_mac_address"),
                Some(mac(0).to_string().as_str())
            ),
        }
        assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
    }
    let mock = MockRmsApi::new();
    let mut response = populated_reply(&["node-000"]);
    let batch = response.response.as_mut().unwrap();
    batch.message = PRIVATE.to_owned();
    batch.node_results = vec![rms::NodeOperationResult {
        node_id: "node-000".to_owned(),
        status: rms::ReturnCode::Failure as i32,
        error_message: PRIVATE.to_owned(),
    }];
    mock.enqueue_batch_get_firmware_inventory(Ok(response))
        .await;

    let (results, logs) = capture_logs_async(read(&mock, nodes(1))).await;

    assert_eq!(results[0].inventory, Err(FirmwareReadError::ReadFailed));
    safe(&logs, "response");
    let diagnostic = logs
        .iter()
        .find(|log| log.field("category") == Some("response"))
        .unwrap();
    assert_eq!(diagnostic.field("node_id"), Some("node-000"));
    assert_eq!(diagnostic.field("reason"), Some("failed_operation"));
}
