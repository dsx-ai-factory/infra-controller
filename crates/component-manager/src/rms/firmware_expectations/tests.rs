// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::capture_logs_async;
use carbide_test_support::{Check, check_values};
use carbide_uuid::rack::RackId;
use librms::RmsApi;
use model::rack::FirmwareUpgradeDeviceInfo;
use model::rack_type::{RackHardwareClass, RackProductFamily, RackProfile};

use super::*;
use crate::RackFirmwareUpdateManager;

/// Counts describe unique devices per role; repeated request slots add no inventory.
fn profile(compute: u32, switches: u32, shelves: u32) -> RackProfile {
    let mut profile = RackProfile {
        product_family: Some(RackProductFamily::Gb200),
        ..Default::default()
    };
    profile.rack_capabilities.compute.count = compute;
    profile.rack_capabilities.switch.count = switches;
    profile.rack_capabilities.power_shelf.count = shelves;
    profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_owned());
    profile.rack_capabilities.switch.vendor = Some("NVIDIA".to_owned());
    profile.rack_capabilities.power_shelf.vendor = Some("LiteOn".to_owned());
    profile
        .attributes
        .insert("site".to_owned(), "test-site".to_owned());
    profile
}

/// Stable IDs expose sorting while credentials stay independent of the ID.
fn devices(prefix: &str, count: usize) -> Vec<FirmwareUpgradeDeviceInfo> {
    (0..count)
        .map(|slot| FirmwareUpgradeDeviceInfo {
            node_id: format!("{prefix}-{slot:03}"),
            mac: "02:00:00:00:00:01".to_owned(),
            bmc_ip: "192.0.2.1".to_owned(),
            bmc_username: "bmc-user".to_owned(),
            bmc_password: "private-device-password".to_owned(),
            os_mac: Some("02:00:00:00:00:02".to_owned()),
            os_ip: Some("192.0.2.2".to_owned()),
            os_username: Some("os-user".to_owned()),
            os_password: Some("private-os-password".to_owned()),
            os_hostname: Some("switch.example".to_owned()),
        })
        .collect()
}

/// Every requested node has independent success evidence, including zero rows.
fn reply(ids: &[&str]) -> rms::CompareFirmwareObjectResponse {
    rms::CompareFirmwareObjectResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            node_results: ids
                .iter()
                .map(|id| rms::NodeOperationResult {
                    node_id: (*id).to_owned(),
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }),
        nodes: ids
            .iter()
            .map(|id| rms::NodeFirmwareManifestComparison {
                node_id: (*id).to_owned(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// Supplies responses in dispatch order without coupling them to caller slots.
async fn enqueue(mock: &MockRmsApi, devices: &[FirmwareUpgradeDeviceInfo]) {
    let mut ids: Vec<_> = devices
        .iter()
        .map(|device| device.node_id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    for batch in ids.chunks(64) {
        mock.enqueue_compare_firmware_object(Ok(reply(batch))).await;
    }
}

/// Supplies selected JSON and empty filters while borrowing the caller's targets.
fn request<'a>(
    rack: &'a RackId,
    profile: &'a RackProfile,
    machines: &'a [FirmwareUpgradeDeviceInfo],
    switches: &'a [FirmwareUpgradeDeviceInfo],
    power_shelves: &'a [FirmwareUpgradeDeviceInfo],
) -> RackFirmwareExpectationsRequest<'a> {
    RackFirmwareExpectationsRequest {
        rack_id: rack,
        profile,
        config_json: "{\"selected\":\"manifest\"}",
        components: &[],
        machines,
        switches,
        power_shelves,
    }
}

/// Empty selections have no observations to collect and must not spend the RPC budget.
#[tokio::test]
async fn empty_selection_returns_without_contacting_rms() {
    let rack = RackId::new("rack-a");
    let profile = profile(0, 0, 0);
    let mock = Arc::new(MockRmsApi::new());
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());

    let result = manager
        .get_firmware_expectations(request(&rack, &profile, &[], &[], &[]))
        .await
        .unwrap();

    assert!(result.is_empty());
    assert!(mock.compare_firmware_object_calls().await.is_empty());
}

/// Distinct populated replies and independently reordered `nodes` and
/// `node_results` expose evidence attached to the wrong node or repeated slot.
#[tokio::test]
async fn public_manager_reuses_apply_policy_without_writes() {
    let rack = RackId::new("rack-a");
    let mut profile = profile(3, 1, 1);
    profile.rack_hardware_class = Some(RackHardwareClass::Dev);
    let mut machines = devices("compute", 3);
    machines.reverse();
    machines.push(machines[0].clone());
    let switches = devices("switch", 1);
    let shelves = devices("shelf", 1);
    let filters = ["BMC", "CPLD", "GPU", "NVOS", "PMC"]
        .map(str::to_owned)
        .to_vec();
    let mock = Arc::new(MockRmsApi::new());
    let mut compute = reply(&["compute-001", "compute-002", "compute-000"]);
    compute.response.as_mut().unwrap().node_results[0].status = rms::ReturnCode::Failure as i32;
    compute.response.as_mut().unwrap().status = rms::ReturnCode::Failure as i32;
    compute.nodes = vec![
        rms::NodeFirmwareManifestComparison {
            node_id: "compute-000".to_owned(),
            components: vec![
                rms::FirmwareManifestComponentComparison {
                    component: "BMC".to_owned(),
                    proposed_version: "bmc-a-v2".to_owned(),
                    sku_id: "bmc-a".to_owned(),
                    target: "/FirmwareInventory/BMC_A".to_owned(),
                    status: rms::FirmwareComparisonStatus::Matched as i32,
                    ..Default::default()
                },
                rms::FirmwareManifestComponentComparison {
                    component: "BMC".to_owned(),
                    proposed_version: "bmc-b-v5".to_owned(),
                    sku_id: "bmc-b".to_owned(),
                    target: "/FirmwareInventory/BMC_B".to_owned(),
                    status: rms::FirmwareComparisonStatus::Missing as i32,
                    ..Default::default()
                },
                rms::FirmwareManifestComponentComparison {
                    component: "CPLD".to_owned(),
                    proposed_version: "cpld-v8".to_owned(),
                    sku_id: "cpld-a".to_owned(),
                    target: "/FirmwareInventory/CPLD_0".to_owned(),
                    status: rms::FirmwareComparisonStatus::Unverified as i32,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        rms::NodeFirmwareManifestComparison {
            node_id: "compute-002".to_owned(),
            components: vec![rms::FirmwareManifestComponentComparison {
                component: "GPU".to_owned(),
                proposed_version: "gpu-v9".to_owned(),
                sku_id: "gpu-a".to_owned(),
                target: "/FirmwareInventory/GPU_0".to_owned(),
                status: rms::FirmwareComparisonStatus::Mismatched as i32,
                ..Default::default()
            }],
            ..Default::default()
        },
    ];
    let mut switch = reply(&["switch-000"]);
    switch.nodes[0].components = vec![rms::FirmwareManifestComponentComparison {
        component: "NVOS".to_owned(),
        proposed_version: "nvos-v1".to_owned(),
        sku_id: "switch-a".to_owned(),
        target: "system_image".to_owned(),
        status: rms::FirmwareComparisonStatus::Matched as i32,
        ..Default::default()
    }];
    let mut shelf = reply(&["shelf-000"]);
    shelf.nodes[0].components = vec![rms::FirmwareManifestComponentComparison {
        component: "PMC".to_owned(),
        proposed_version: "pmc-v6".to_owned(),
        sku_id: "shelf-a".to_owned(),
        target: "/FirmwareInventory/PMC".to_owned(),
        status: rms::FirmwareComparisonStatus::Unverified as i32,
        ..Default::default()
    }];
    for response in [compute, switch, shelf] {
        mock.enqueue_compare_firmware_object(Ok(response)).await;
    }
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());
    let mut input = request(&rack, &profile, &machines, &switches, &shelves);
    input.components = &filters;

    let result = manager.get_firmware_expectations(input).await.unwrap();

    let gpu = FirmwareExpectation {
        component: Some("GPU".to_owned()),
        proposed_version: Some("gpu-v9".to_owned()),
        sku_id: Some("gpu-a".to_owned()),
        reported_target: Some("/FirmwareInventory/GPU_0".to_owned()),
        status: FirmwareExpectationStatus::Mismatched,
    };
    assert_eq!(
        result,
        vec![
            DeviceFirmwareExpectations {
                node_id: "compute-002".to_owned(),
                expectations: Ok(vec![gpu.clone()])
            },
            DeviceFirmwareExpectations {
                node_id: "compute-001".to_owned(),
                expectations: Err(FirmwareExpectationError::ReadFailed)
            },
            DeviceFirmwareExpectations {
                node_id: "compute-000".to_owned(),
                expectations: Ok(vec![
                    FirmwareExpectation {
                        component: Some("BMC".to_owned()),
                        proposed_version: Some("bmc-a-v2".to_owned()),
                        sku_id: Some("bmc-a".to_owned()),
                        reported_target: Some("/FirmwareInventory/BMC_A".to_owned()),
                        status: FirmwareExpectationStatus::Matched,
                    },
                    FirmwareExpectation {
                        component: Some("BMC".to_owned()),
                        proposed_version: Some("bmc-b-v5".to_owned()),
                        sku_id: Some("bmc-b".to_owned()),
                        reported_target: Some("/FirmwareInventory/BMC_B".to_owned()),
                        status: FirmwareExpectationStatus::Missing,
                    },
                    FirmwareExpectation {
                        component: Some("CPLD".to_owned()),
                        proposed_version: Some("cpld-v8".to_owned()),
                        sku_id: Some("cpld-a".to_owned()),
                        reported_target: Some("/FirmwareInventory/CPLD_0".to_owned()),
                        status: FirmwareExpectationStatus::Unverified,
                    },
                ])
            },
            DeviceFirmwareExpectations {
                node_id: "compute-002".to_owned(),
                expectations: Ok(vec![gpu])
            },
            DeviceFirmwareExpectations {
                node_id: "switch-000".to_owned(),
                expectations: Ok(vec![FirmwareExpectation {
                    component: Some("NVOS".to_owned()),
                    proposed_version: Some("nvos-v1".to_owned()),
                    sku_id: Some("switch-a".to_owned()),
                    reported_target: Some("system_image".to_owned()),
                    status: FirmwareExpectationStatus::Matched,
                }])
            },
            DeviceFirmwareExpectations {
                node_id: "shelf-000".to_owned(),
                expectations: Ok(vec![FirmwareExpectation {
                    component: Some("PMC".to_owned()),
                    proposed_version: Some("pmc-v6".to_owned()),
                    sku_id: Some("shelf-a".to_owned()),
                    reported_target: Some("/FirmwareInventory/PMC".to_owned()),
                    status: FirmwareExpectationStatus::Unverified,
                }])
            },
        ]
    );
    let calls = mock.compare_firmware_object_calls().await;
    assert_eq!(calls.len(), 3);
    for (call, role, node_type, count) in [
        (&calls[0], "compute", rms::NodeType::ComputeGb200Nvidia, 3),
        (&calls[1], "switch", rms::NodeType::SwitchGb200Nvidia, 1),
        (
            &calls[2],
            "power_shelf",
            rms::NodeType::PowershelfGb200Liteon,
            1,
        ),
    ] {
        assert_eq!(call.config_json, "{\"selected\":\"manifest\"}");
        assert_eq!(call.firmware_type, "dev");
        let nodes = &call.nodes.as_ref().unwrap().nodes;
        assert_eq!(nodes.len(), count);
        for node in nodes {
            assert_eq!(node.rack_id, rack.to_string());
            assert_eq!(node.r#type, Some(node_type as i32));
            let descriptor = node.node_descriptor.as_ref().unwrap();
            assert_eq!(descriptor.attributes["role"], role);
            assert_eq!(descriptor.attributes["site"], "test-site");
            let device = machines
                .iter()
                .chain(&switches)
                .chain(&shelves)
                .find(|device| device.node_id == node.node_id)
                .unwrap();
            assert_eq!(
                node.bmc_endpoint,
                Some(rms::Endpoint {
                    interface: Some(rms::NetworkInterface {
                        ip_address: device.bmc_ip.clone(),
                        mac_address: device.mac.clone(),
                        host_name: None,
                    }),
                    port: 443,
                    credentials: Some(rms::Credentials {
                        auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                            username: device.bmc_username.clone(),
                            password: device.bmc_password.clone(),
                        }))
                    }),
                })
            );
            let host = (role == "switch").then(|| rms::Endpoint {
                interface: Some(rms::NetworkInterface {
                    ip_address: device.os_ip.clone().unwrap(),
                    mac_address: device.os_mac.clone().unwrap(),
                    host_name: device.os_hostname.clone(),
                }),
                port: 0,
                credentials: Some(rms::Credentials {
                    auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                        username: device.os_username.clone().unwrap(),
                        password: device.os_password.clone().unwrap(),
                    })),
                }),
            });
            assert_eq!(node.host_endpoint, host);
        }
        assert_eq!(call.component_filters.len(), 1);
        assert_eq!(
            call.component_filters[&(node_type as i32)].components,
            filters
        );
        let [selector] = call.node_descriptor_component_filters.as_slice() else {
            panic!("one descriptor filter expected for the role");
        };
        assert_eq!(selector.node_descriptor, nodes[0].node_descriptor);
        assert_eq!(
            selector.component_filter.as_ref().unwrap().components,
            filters
        );
    }
    assert!(mock.add_firmware_object_calls().await.is_empty());
    assert!(mock.apply_firmware_object_calls().await.is_empty());
    assert!(mock.apply_stored_firmware_object_calls().await.is_empty());
    assert!(mock.set_default_firmware_object_calls().await.is_empty());
    assert!(mock.delete_firmware_object_calls().await.is_empty());
    assert!(mock.create_nodes_calls().await.is_empty());
    assert!(mock.update_node_calls().await.is_empty());
    assert!(mock.delete_node_calls().await.is_empty());
    assert!(mock.update_firmware_calls().await.is_empty());
    assert!(mock.batch_update_firmware_calls().await.is_empty());
    assert!(mock.apply_switch_system_image_calls().await.is_empty());
    assert!(mock.push_switch_firmware_calls().await.is_empty());
    assert!(mock.set_power_state_calls().await.is_empty());
    assert!(mock.batch_set_power_state_calls().await.is_empty());
}

/// `installed_component` and `installed_version` can group physical instances.
/// Keep duplicate logical rows, empty proposals and unknown status values intact.
#[test]
fn logical_rows_preserve_order_duplicates_absence_and_unknown_status() {
    let mut response = reply(&["node"]);
    response.object_id = "private-object-id".to_owned();
    response.matches_manifest = true;
    let grouped = rms::FirmwareManifestComponentComparison {
        component: "CPLD".to_owned(),
        installed_component: "CPLD_0,CPLD_1,CPLD_2".to_owned(),
        installed_version: "v1,v2".to_owned(),
        proposed_version: "v3".to_owned(),
        status: rms::FirmwareComparisonStatus::Unverified as i32,
        target: "/UpdateService/FirmwareInventory/CPLD_0".to_owned(),
        reason: "private comparison diagnostics".to_owned(),
        sku_id: "sku-1".to_owned(),
    };
    response.nodes[0].components = vec![grouped.clone(), grouped];
    response.nodes[0]
        .components
        .push(rms::FirmwareManifestComponentComparison {
            status: 987,
            ..Default::default()
        });

    let rows = expectations_for_node(&response, "node").unwrap();

    let expected = FirmwareExpectation {
        component: Some("CPLD".to_owned()),
        proposed_version: Some("v3".to_owned()),
        sku_id: Some("sku-1".to_owned()),
        reported_target: Some("/UpdateService/FirmwareInventory/CPLD_0".to_owned()),
        status: FirmwareExpectationStatus::Unverified,
    };
    assert_eq!(
        rows,
        vec![
            expected.clone(),
            expected,
            FirmwareExpectation {
                component: None,
                proposed_version: None,
                sku_id: None,
                reported_target: None,
                status: FirmwareExpectationStatus::Unknown(987),
            }
        ]
    );
    assert!(!format!("{rows:?}").contains("private"));
    check_values(
        [
            Check {
                scenario: "unspecified",
                input: 0,
                expect: FirmwareExpectationStatus::Unspecified,
            },
            Check {
                scenario: "matched",
                input: 1,
                expect: FirmwareExpectationStatus::Matched,
            },
            Check {
                scenario: "mismatched",
                input: 2,
                expect: FirmwareExpectationStatus::Mismatched,
            },
            Check {
                scenario: "missing",
                input: 3,
                expect: FirmwareExpectationStatus::Missing,
            },
        ],
        |status| {
            let mut response = response.clone();
            response.nodes[0].components[0].status = status;
            expectations_for_node(&response, "node").unwrap()[0].status
        },
    );
}

/// Each mutation targets a distinct attribution boundary. A full batch plus
/// one duplicate must still leave the healthy peer's evidence available.
#[test]
fn malformed_node_evidence_preserves_healthy_peers() {
    let others: Vec<_> = (0..62).map(|slot| format!("other-{slot}")).collect();
    let ids: Vec<_> = ["bad", "good"]
        .into_iter()
        .chain(others.iter().map(String::as_str))
        .collect();
    let good = reply(&ids);
    let mut missing = good.clone();
    missing.nodes.remove(0);
    let mut duplicate = good.clone();
    duplicate.nodes.push(duplicate.nodes[0].clone());
    let mut missing_operation = good.clone();
    missing_operation
        .response
        .as_mut()
        .unwrap()
        .node_results
        .remove(0);
    let mut duplicate_operation = good.clone();
    let operation = duplicate_operation.response.as_ref().unwrap().node_results[0].clone();
    duplicate_operation
        .response
        .as_mut()
        .unwrap()
        .node_results
        .push(operation);
    let mut failed = good.clone();
    failed.response.as_mut().unwrap().node_results[0].status = rms::ReturnCode::Failure as i32;
    let mut unspecified = good.clone();
    unspecified.response.as_mut().unwrap().node_results[0].status =
        rms::ReturnCode::Unspecified as i32;
    let mut contradictory = good.clone();
    contradictory.response.as_mut().unwrap().node_results[0].error_message =
        "private body".to_owned();
    let mut aggregate_failure = good.clone();
    aggregate_failure.response.as_mut().unwrap().status = rms::ReturnCode::Failure as i32;
    let mut excessive_rows = good;
    excessive_rows.nodes[0].components = vec![Default::default(); 257];

    check_values(
        [
            Check {
                scenario: "missing comparison",
                input: missing,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "duplicate comparison",
                input: duplicate,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "missing operation",
                input: missing_operation,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "duplicate operation",
                input: duplicate_operation,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "failed operation",
                input: failed,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "unspecified operation",
                input: unspecified,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "contradictory success",
                input: contradictory,
                expect: Err(FirmwareExpectationError::ReadFailed),
            },
            Check {
                scenario: "aggregate belongs to another node",
                input: aggregate_failure,
                expect: Ok(Vec::new()),
            },
            Check {
                scenario: "excessive rows",
                input: excessive_rows,
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
        ],
        |response| {
            assert_eq!(expectations_for_node(&response, "good"), Ok(Vec::new()));
            expectations_for_node(&response, "bad")
        },
    );
    assert_eq!(
        expectations_for_node(&rms::CompareFirmwareObjectResponse::default(), "bad"),
        Err(FirmwareExpectationError::ReadFailed)
    );
}

/// Each decoded response array has its own admission bound. The other array
/// stays small so it cannot hide an omitted guard on `nodes` or `node_results`.
#[test]
fn decoded_response_array_limits_are_independent() {
    let mut nodes_at_limit = reply(&["bad", "good"]);
    nodes_at_limit
        .nodes
        .extend((0..254).map(|slot| rms::NodeFirmwareManifestComparison {
            node_id: format!("extra-{slot}"),
            ..Default::default()
        }));
    let mut excess_nodes = nodes_at_limit.clone();
    excess_nodes
        .nodes
        .push(rms::NodeFirmwareManifestComparison {
            node_id: "extra-254".to_owned(),
            ..Default::default()
        });
    let mut operations_at_limit = reply(&["bad", "good"]);
    operations_at_limit
        .response
        .as_mut()
        .unwrap()
        .node_results
        .extend((0..254).map(|slot| rms::NodeOperationResult {
            node_id: format!("extra-{slot}"),
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        }));
    let mut excess_operations = operations_at_limit.clone();
    excess_operations
        .response
        .as_mut()
        .unwrap()
        .node_results
        .push(rms::NodeOperationResult {
            node_id: "extra-254".to_owned(),
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        });

    check_values(
        [
            Check {
                scenario: "256 node entries",
                input: nodes_at_limit,
                expect: Ok(0),
            },
            Check {
                scenario: "257 node entries",
                input: excess_nodes,
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "256 operation entries",
                input: operations_at_limit,
                expect: Ok(0),
            },
            Check {
                scenario: "257 operation entries",
                input: excess_operations,
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
        ],
        |response| {
            let result = expectations_for_node(&response, "bad");
            assert_eq!(expectations_for_node(&response, "good"), result);
            result.map(|rows| rows.len())
        },
    );
}

/// Retained fields share one byte budget. `reason`, `installed_component` and
/// `installed_version` are discarded and cannot consume that retention budget.
#[test]
fn retained_rows_and_metadata_have_independent_bounds() {
    let component = rms::FirmwareManifestComponentComparison {
        component: "x".repeat(1024),
        ..Default::default()
    };
    let mixed = rms::FirmwareManifestComponentComparison {
        component: "c".repeat(1024),
        proposed_version: "v".repeat(1024),
        sku_id: "s".repeat(1024),
        target: "t".repeat(1024),
        ..Default::default()
    };
    let mut mixed_over_budget = vec![mixed.clone(); 16];
    mixed_over_budget.push(rms::FirmwareManifestComponentComparison {
        component: "x".to_owned(),
        ..Default::default()
    });

    check_values(
        [
            Check {
                scenario: "256 empty rows",
                input: vec![Default::default(); 256],
                expect: Ok(256),
            },
            Check {
                scenario: "1024-byte field",
                input: vec![component],
                expect: Ok(1),
            },
            Check {
                scenario: "component bytes exceed characters",
                input: vec![rms::FirmwareManifestComponentComparison {
                    component: "é".repeat(512) + "x",
                    ..Default::default()
                }],
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "overlong proposal",
                input: vec![rms::FirmwareManifestComponentComparison {
                    proposed_version: "v".repeat(1025),
                    ..Default::default()
                }],
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "overlong SKU",
                input: vec![rms::FirmwareManifestComponentComparison {
                    sku_id: "s".repeat(1025),
                    ..Default::default()
                }],
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "overlong target",
                input: vec![rms::FirmwareManifestComponentComparison {
                    target: "t".repeat(1025),
                    ..Default::default()
                }],
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "64KiB mixed metadata",
                input: vec![mixed; 16],
                expect: Ok(16),
            },
            Check {
                scenario: "mixed metadata one byte over budget",
                input: mixed_over_budget,
                expect: Err(FirmwareExpectationError::LimitExceeded),
            },
            Check {
                scenario: "discarded text is outside retention budget",
                input: vec![rms::FirmwareManifestComponentComparison {
                    component: "BMC".to_owned(),
                    proposed_version: "v1".to_owned(),
                    reason: "r".repeat(65537),
                    installed_component: "c".repeat(65537),
                    installed_version: "v".repeat(65537),
                    ..Default::default()
                }],
                expect: Ok(1),
            },
        ],
        |rows| {
            let mut response = reply(&["node"]);
            response.nodes[0].components = rows;

            expectations_for_node(&response, "node").map(|rows| rows.len())
        },
    );
}

/// 256 slots can require six calls across three roles. Reversed caller order
/// distinguishes stable output slots from sorted, bounded transport batches.
#[tokio::test]
async fn role_batches_preserve_all_slots_at_the_admission_boundary() {
    let rack = RackId::new("rack-a");
    let profile = profile(65, 65, 126);
    let mut machines = devices("compute", 65);
    let mut switches = devices("switch", 65);
    let mut shelves = devices("shelf", 126);
    for devices in [&mut machines, &mut switches, &mut shelves] {
        devices.reverse();
    }
    let mock = Arc::new(MockRmsApi::new());
    for devices in [&machines, &switches, &shelves] {
        enqueue(&mock, devices).await;
    }
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());

    let result = manager
        .get_firmware_expectations(request(&rack, &profile, &machines, &switches, &shelves))
        .await
        .unwrap();

    assert_eq!(result.len(), 256);
    for (slot, device) in result
        .iter()
        .zip(machines.iter().chain(&switches).chain(&shelves))
    {
        assert_eq!(slot.node_id, device.node_id);
        assert_eq!(slot.expectations, Ok(Vec::new()));
    }
    let calls = mock.compare_firmware_object_calls().await;
    assert_eq!(
        calls
            .iter()
            .map(|call| call.nodes.as_ref().unwrap().nodes.len())
            .collect::<Vec<_>>(),
        vec![64, 1, 64, 1, 64, 62]
    );
    assert!(calls.iter().all(|call| call.firmware_type == "prod"
        && call.component_filters.is_empty()
        && call.node_descriptor_component_filters.is_empty()));
    machines.push(machines[0].clone());

    let excessive = manager
        .get_firmware_expectations(request(&rack, &profile, &machines, &switches, &shelves))
        .await;

    assert!(matches!(
        excessive,
        Err(ComponentManagerError::InvalidArgument(_))
    ));
    assert_eq!(mock.compare_firmware_object_calls().await.len(), 6);
}

/// A conflict after the first 64 slots must be absorbed before any dispatch.
/// A later identical repeat and another role cannot restore ambiguous identity.
#[tokio::test]
async fn late_and_cross_role_conflicts_never_send_credentials() {
    let rack = RackId::new("rack-a");
    let profile = profile(65, 1, 0);
    let mut machines = devices("compute", 65);
    let mut conflict = machines[0].clone();
    conflict.bmc_password = "different-secret".to_owned();
    machines.push(conflict);
    machines.push(machines[0].clone());
    let switches = vec![machines[1].clone()];
    let mock = Arc::new(MockRmsApi::new());
    enqueue(&mock, &machines[2..65]).await;
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());

    let result = manager
        .get_firmware_expectations(request(&rack, &profile, &machines, &switches, &[]))
        .await
        .unwrap();

    for slot in [0, 1, 65, 66, 67] {
        assert_eq!(
            result[slot].expectations,
            Err(FirmwareExpectationError::ReadFailed)
        );
    }
    assert!(
        result[2..65]
            .iter()
            .all(|slot| slot.expectations == Ok(Vec::new()))
    );
    let calls = mock.compare_firmware_object_calls().await;
    let nodes = &calls[0].nodes.as_ref().unwrap().nodes;
    assert_eq!(calls.len(), 1);
    assert_eq!(nodes.len(), 63);
    assert!(
        nodes
            .iter()
            .all(|node| node.node_id != "compute-000" && node.node_id != "compute-001")
    );
}

/// Admission must inspect the last slot before sending any earlier batch.
/// An empty node ID is rejected just as early as an oversized credential.
#[tokio::test]
async fn invalid_last_slot_prevents_dispatch() {
    let rack = RackId::new("rack-a");
    let profile = profile(65, 0, 0);
    let machines = devices("compute", 65);
    let mut oversized = machines[64].clone();
    oversized.bmc_password = "x".repeat(1025);
    let mut missing_id = machines[64].clone();
    missing_id.node_id.clear();
    let mock = Arc::new(MockRmsApi::new());
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());
    for (scenario, last) in [
        ("last credential is oversized", oversized),
        ("last node ID is empty", missing_id),
    ] {
        let mut machines = machines.clone();
        machines[64] = last;

        let result = manager
            .get_firmware_expectations(request(&rack, &profile, &machines, &[], &[]))
            .await;

        assert!(
            matches!(result, Err(ComponentManagerError::InvalidArgument(_))),
            "{scenario}"
        );
        assert!(
            mock.compare_firmware_object_calls().await.is_empty(),
            "{scenario}"
        );
    }
}

/// A missing compute vendor prevents that role's dispatch while a valid switch
/// still returns evidence. This is distinct from rejecting the whole input.
#[tokio::test]
async fn invalid_role_preserves_healthy_peer_roles() {
    let rack = RackId::new("rack-a");
    let mut profile = profile(1, 1, 0);
    profile.rack_capabilities.compute.vendor = None;
    let machines = devices("compute", 1);
    let switches = devices("switch", 1);
    let mock = Arc::new(MockRmsApi::new());
    enqueue(&mock, &switches).await;
    let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());

    let result = manager
        .get_firmware_expectations(request(&rack, &profile, &machines, &switches, &[]))
        .await
        .unwrap();

    let expected: Vec<_> = machines
        .iter()
        .map(|device| DeviceFirmwareExpectations {
            node_id: device.node_id.clone(),
            expectations: Err(FirmwareExpectationError::ReadFailed),
        })
        .chain([DeviceFirmwareExpectations {
            node_id: "switch-000".to_owned(),
            expectations: Ok(Vec::new()),
        }])
        .collect();
    assert_eq!(result, expected);
    let calls = mock.compare_firmware_object_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].nodes.as_ref().unwrap().nodes[0].node_id,
        "switch-000"
    );
}

/// Count and text limits protect different request fields. Keep these at the
/// validator so the public admission proof need not repeat an RPC case matrix.
#[test]
fn request_collection_and_text_limits_are_independent() {
    /// Owned fields let each case change one admission boundary independently.
    #[derive(Clone)]
    struct Input {
        rack: RackId,
        profile: RackProfile,
        manifest: String,
        filters: Vec<String>,
        switches: Vec<FirmwareUpgradeDeviceInfo>,
    }
    let base = Input {
        rack: RackId::new("rack-a"),
        profile: profile(0, 1, 0),
        manifest: "{}".to_owned(),
        filters: Vec::new(),
        switches: devices("switch", 1),
    };
    let changed = |change: fn(&mut Input)| {
        let mut input = base.clone();
        change(&mut input);
        input
    };
    let max_text = changed(|input| {
        // Each three-byte key and 1021-byte value uses exactly 1024 bytes.
        input.profile.attributes = (0..64)
            .map(|index| (format!("{index:03}"), "v".repeat(1021)))
            .collect();
    });
    let mut excess_text = max_text.clone();
    excess_text
        .profile
        .rack_capabilities
        .switch
        .attributes
        .insert("k".to_owned(), "v".to_owned());

    check_values(
        [
            Check {
                scenario: "16MiB manifest",
                input: changed(|input| input.manifest = " ".repeat(16 * 1024 * 1024)),
                expect: true,
            },
            Check {
                scenario: "manifest one byte over limit",
                input: changed(|input| input.manifest = " ".repeat(16 * 1024 * 1024 + 1)),
                expect: false,
            },
            Check {
                scenario: "rack ID size",
                input: changed(|input| input.rack = RackId::new("r".repeat(1025))),
                expect: false,
            },
            Check {
                scenario: "one long filter",
                input: changed(|input| input.filters = vec!["x".repeat(1025)]),
                expect: false,
            },
            Check {
                scenario: "64KiB filter text",
                input: changed(|input| input.filters = vec!["x".repeat(1024); 64]),
                expect: true,
            },
            Check {
                scenario: "filter text over budget",
                input: changed(|input| input.filters = vec!["x".repeat(1024); 65]),
                expect: false,
            },
            Check {
                scenario: "256 short filters",
                input: changed(|input| input.filters = vec!["BMC".to_owned(); 256]),
                expect: true,
            },
            Check {
                scenario: "257 short filters",
                input: changed(|input| input.filters = vec!["BMC".to_owned(); 257]),
                expect: false,
            },
            Check {
                scenario: "attribute key size",
                input: changed(|input| {
                    input
                        .profile
                        .attributes
                        .insert("k".repeat(1025), "v".to_owned());
                }),
                expect: false,
            },
            Check {
                scenario: "attribute value size",
                input: changed(|input| {
                    input
                        .profile
                        .attributes
                        .insert("k".to_owned(), "v".repeat(1025));
                }),
                expect: false,
            },
            Check {
                scenario: "256 short rack attributes",
                input: changed(|input| {
                    input.profile.attributes = (0..256)
                        .map(|index| (format!("key-{index}"), "v".to_owned()))
                        .collect();
                }),
                expect: true,
            },
            Check {
                scenario: "257 short rack attributes",
                input: changed(|input| {
                    input.profile.attributes = (0..257)
                        .map(|index| (format!("key-{index}"), "v".to_owned()))
                        .collect();
                }),
                expect: false,
            },
            Check {
                scenario: "role attribute count",
                input: changed(|input| {
                    input.profile.rack_capabilities.switch.attributes = (0..257)
                        .map(|index| (format!("key-{index}"), "v".to_owned()))
                        .collect();
                }),
                expect: false,
            },
            Check {
                scenario: "64KiB inherited attributes",
                input: max_text,
                expect: true,
            },
            Check {
                scenario: "role text exceeds combined budget",
                input: excess_text,
                expect: false,
            },
            Check {
                scenario: "vendor size",
                input: changed(|input| {
                    input.profile.rack_capabilities.switch.vendor = Some("v".repeat(1025));
                }),
                expect: false,
            },
            Check {
                scenario: "custom product family size",
                input: changed(|input| {
                    input.profile.product_family = Some(RackProductFamily::Other("f".repeat(1025)));
                }),
                expect: false,
            },
            Check {
                scenario: "1024-byte credential",
                input: changed(|input| {
                    input.switches[0].bmc_password = "p".repeat(1024);
                }),
                expect: true,
            },
            Check {
                scenario: "OS endpoint hostname size",
                input: changed(|input| {
                    input.switches[0].os_hostname = Some("h".repeat(1025));
                }),
                expect: false,
            },
        ],
        |input| {
            let mut request = request(&input.rack, &input.profile, &[], &input.switches, &[]);
            request.config_json = &input.manifest;
            request.components = &input.filters;

            validate_request(&request).is_ok()
        },
    );
}

/// Compute and switch batches spend the same budget. Completed compute
/// evidence survives while timeout drops the first switch RPC and skips its next batch.
#[tokio::test(start_paused = true)]
async fn sequential_batches_share_one_owned_thirty_second_deadline() {
    let proof = async {
        let rack = RackId::new("rack-a");
        let profile = profile(64, 65, 0);
        let machines = devices("compute", 64);
        let switches = devices("switch", 65);
        let mock = Arc::new(MockRmsApi::new());
        enqueue(&mock, &machines).await;
        enqueue(&mock, &switches).await;
        let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());
        let (arrival, release) = mock.block_next_compare_firmware_object().await;
        let started = Instant::now();
        let read =
            manager.get_firmware_expectations(request(&rack, &profile, &machines, &switches, &[]));
        tokio::pin!(read);
        tokio::select! {
            arrived = arrival => arrived.expect("first RPC reached its gate"),
            _ = &mut read => panic!("first RPC must wait"),
        }
        assert!(!release.is_closed(), "first RPC owns the waiting receiver");
        tokio::time::advance(Duration::from_secs(20)).await;
        let (second_arrival, second_release) = mock.block_next_compare_firmware_object().await;
        release.send(()).unwrap();
        tokio::select! {
            arrived = second_arrival => arrived.expect("switch RPC reached its gate"),
            _ = &mut read => panic!("second RPC must wait"),
        }
        assert!(
            !second_release.is_closed(),
            "switch RPC owns the waiting receiver"
        );

        let result = read.await.unwrap();

        assert_eq!(result.len(), 129);
        assert_eq!(
            result
                .iter()
                .map(|slot| slot.node_id.as_str())
                .collect::<Vec<_>>(),
            machines
                .iter()
                .chain(&switches)
                .map(|device| device.node_id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(Instant::now() - started, Duration::from_secs(30));
        assert!(second_release.is_closed());
        assert!(
            result[..64]
                .iter()
                .all(|slot| slot.expectations == Ok(Vec::new()))
        );
        assert!(
            result[64..]
                .iter()
                .all(|slot| slot.expectations == Err(FirmwareExpectationError::DeadlineExceeded))
        );
        assert_eq!(mock.compare_firmware_object_calls().await.len(), 2);
    };
    tokio::time::timeout(Duration::from_secs(60), proof)
        .await
        .expect("independent deadline watchdog");
}

/// Dropping a caller releases its in-flight mock gate; no detached task can
/// consume the response or continue into the next batch after cancellation.
#[tokio::test]
async fn caller_cancellation_drops_the_owned_rpc() {
    let proof = async {
        let rack = RackId::new("rack-a");
        let profile = profile(65, 0, 0);
        let machines = devices("compute", 65);
        let mock = Arc::new(MockRmsApi::new());
        let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());
        let (arrival, release) = mock.block_next_compare_firmware_object().await;
        let mut read = Box::pin(manager.get_firmware_expectations(request(
            &rack,
            &profile,
            &machines,
            &[],
            &[],
        )));
        tokio::select! {
            arrived = arrival => arrived.expect("RPC reached its gate"),
            _ = &mut read => panic!("RPC must wait"),
        }
        assert!(
            !release.is_closed(),
            "RPC owns the waiting receiver before cancellation"
        );

        drop(read);

        assert!(release.is_closed());
        assert_eq!(mock.compare_firmware_object_calls().await.len(), 1);
    };
    tokio::time::timeout(Duration::from_secs(60), proof)
        .await
        .expect("cancellation watchdog");
}

/// Installed evidence cannot supply a missing comparison. Leave it queued
/// during comparison, then read it explicitly as a positive control. A later
/// switch reply proves that one role's failure does not stop healthy peers.
#[tokio::test]
async fn comparison_failures_keep_installed_evidence_separate_and_diagnostics_private() {
    const PRIVATE: &str = "private-manifest-status-response-body";
    let rack = RackId::new("rack-a");
    let profile = profile(1, 1, 0);
    let machines = devices("compute", 1);
    let switches = devices("switch", 1);
    let mut malformed = reply(&["compute-000"]);
    malformed.response.as_mut().unwrap().node_results[0].status = rms::ReturnCode::Failure as i32;
    malformed.response.as_mut().unwrap().node_results[0].error_message = PRIVATE.to_owned();
    malformed.response.as_mut().unwrap().message = PRIVATE.to_owned();
    malformed.object_id = PRIVATE.to_owned();
    malformed.nodes[0].components = vec![rms::FirmwareManifestComponentComparison {
        reason: PRIVATE.to_owned(),
        ..Default::default()
    }];
    let installed = rms::BatchGetFirmwareInventoryResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        }),
        nodes: vec![rms::NodeFirmwareInventory {
            node_id: "compute-000".to_owned(),
            firmware_list: vec![rms::FirmwareInventoryInfo {
                name: "BMC".to_owned(),
                version: "installed-v1".to_owned(),
                target: "/FirmwareInventory/BMC".to_owned(),
                ..Default::default()
            }],
        }],
    };
    let mut healthy_switch = reply(&["switch-000"]);
    healthy_switch.nodes[0].components = vec![rms::FirmwareManifestComponentComparison {
        component: "NVOS".to_owned(),
        proposed_version: "expected-v2".to_owned(),
        sku_id: "switch-a".to_owned(),
        target: "system_image".to_owned(),
        status: rms::FirmwareComparisonStatus::Unverified as i32,
        ..Default::default()
    }];
    for (category, code, comparison, expected, warnings) in [
        (
            "rpc",
            Some("Unavailable"),
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable(PRIVATE),
            )),
            FirmwareExpectationError::ReadFailed,
            1,
        ),
        (
            "rpc",
            Some("Unimplemented"),
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::unimplemented(PRIVATE),
            )),
            FirmwareExpectationError::Unsupported,
            1,
        ),
        (
            "tls",
            None,
            Err(RackManagerError::TlsError(
                librms::RmsTlsClientError::Connection(PRIVATE.to_owned()),
            )),
            FirmwareExpectationError::ReadFailed,
            1,
        ),
        (
            "response",
            None,
            Ok(malformed),
            FirmwareExpectationError::ReadFailed,
            0,
        ),
    ] {
        let mock = Arc::new(MockRmsApi::new());
        mock.enqueue_batch_get_firmware_inventory(Ok(installed.clone()))
            .await;
        mock.enqueue_compare_firmware_object(comparison).await;
        mock.enqueue_compare_firmware_object(Ok(healthy_switch.clone()))
            .await;
        let manager = crate::rms::rms_rack_firmware_update_manager(mock.clone());
        let mut input = request(&rack, &profile, &machines, &switches, &[]);
        input.config_json = PRIVATE;

        let (result, logs) = capture_logs_async(manager.get_firmware_expectations(input)).await;

        let result = result.unwrap();
        assert_eq!(
            result,
            vec![
                DeviceFirmwareExpectations {
                    node_id: "compute-000".to_owned(),
                    expectations: Err(expected),
                },
                DeviceFirmwareExpectations {
                    node_id: "switch-000".to_owned(),
                    expectations: Ok(vec![FirmwareExpectation {
                        component: Some("NVOS".to_owned()),
                        proposed_version: Some("expected-v2".to_owned()),
                        sku_id: Some("switch-a".to_owned()),
                        reported_target: Some("system_image".to_owned()),
                        status: FirmwareExpectationStatus::Unverified,
                    }]),
                },
            ]
        );
        assert!(mock.batch_get_firmware_inventory_calls().await.is_empty());
        assert!(mock.get_node_firmware_inventory_calls().await.is_empty());
        assert!(mock.get_rack_firmware_inventory_calls().await.is_empty());
        let calls = mock.compare_firmware_object_calls().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].nodes.as_ref().unwrap().nodes[0].node_id,
            "compute-000"
        );
        assert_eq!(
            calls[1].nodes.as_ref().unwrap().nodes[0].node_id,
            "switch-000"
        );
        let diagnostic = logs
            .iter()
            .find(|log| log.field("category") == Some(category))
            .expect("safe diagnostic is a positive log-capture control");
        assert_eq!(diagnostic.field("grpc_status_code"), code);
        if category == "response" {
            assert_eq!(diagnostic.field("reason"), Some("failed_operation"));
        }
        for private in [PRIVATE, "private-device-password", "private-os-password"] {
            assert!(!format!("{result:?}").contains(private));
            assert!(logs.iter().all(|log| {
                !log.message.contains(private)
                    && log
                        .fields
                        .iter()
                        .all(|(key, value)| !key.contains(private) && !value.contains(private))
            }));
        }
        assert_eq!(
            logs.iter()
                .filter(|log| log.message == "external call failed"
                    && log.field("operation") == Some("compare_firmware_object"))
                .count(),
            warnings
        );
        assert!(mock.add_firmware_object_calls().await.is_empty());
        assert!(mock.apply_firmware_object_calls().await.is_empty());
        assert!(mock.apply_stored_firmware_object_calls().await.is_empty());
        assert!(mock.update_firmware_calls().await.is_empty());
        assert!(mock.batch_update_firmware_calls().await.is_empty());
        assert!(mock.set_power_state_calls().await.is_empty());
        assert!(mock.batch_set_power_state_calls().await.is_empty());

        let control = rms::BatchGetFirmwareInventoryRequest {
            nodes: Some(rms::NodeSet {
                nodes: vec![rms::NodeInfo {
                    node_id: "compute-000".to_owned(),
                    ..Default::default()
                }],
            }),
        };
        let observed = mock
            .batch_get_firmware_inventory(control.clone())
            .await
            .unwrap();

        assert_eq!(observed, installed);
        assert_eq!(
            mock.batch_get_firmware_inventory_calls().await,
            vec![control]
        );
    }
}
