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
use carbide_uuid::machine::MachineInterfaceId;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedInterfaceIpAllocation,
};
use prost_types::FieldMask;
use rpc::forge::forge_server::Forge;
use rpc::{common, forge};
use serde_json::{Value, json};
use sqlx::PgPool;
use tonic::Request;
use uuid::Uuid;

use crate::tests::common::api_fixtures::{TestEnv, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;

fn mask(paths: &[&str]) -> Option<FieldMask> {
    Some(FieldMask {
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
    })
}

fn rpc_id(id: Uuid) -> Option<common::Uuid> {
    Some(common::Uuid {
        value: id.to_string(),
    })
}

fn machine(id: Uuid, suffix: u8) -> forge::ExpectedMachine {
    let bmc_mac_address = format!("02:00:00:00:59:{suffix:02x}");
    forge::ExpectedMachine {
        id: rpc_id(id),
        bmc_mac_address: bmc_mac_address.clone(),
        bmc_username: format!("bmc-user-{suffix}"),
        bmc_password: format!("bmc-password-{suffix}"),
        chassis_serial_number: format!("PATCH-{suffix}"),
        fallback_dpu_serial_numbers: vec![format!("DPU-{suffix}")],
        metadata: Some(forge::Metadata {
            name: "before".to_string(),
            description: "keep description".to_string(),
            labels: vec![forge::Label {
                key: "keep".to_string(),
                value: Some("label".to_string()),
            }],
        }),
        is_dpf_enabled: Some(true),
        default_pause_ingestion_and_poweron: Some(true),
        bmc_retain_credentials: Some(true),
        dpu_mode: Some(forge::DpuMode::NicMode as i32),
        host_lifecycle_profile: Some(forge::HostLifecycleProfile {
            disable_lockdown: Some(true),
        }),
        host_nics: vec![
            forge::ExpectedInterface {
                mac_address: bmc_mac_address,
                role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            },
            forge::ExpectedInterface {
                mac_address: format!("02:00:00:01:59:{suffix:02x}"),
                role: Some(forge::ExpectedInterfaceRole::DpuOs as i32),
                ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

async fn machine_row(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(expected_machines) FROM expected_machines WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn switch_row(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT to_jsonb(expected_switches) FROM expected_switches WHERE expected_switch_id=$1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn power_shelf_row(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT to_jsonb(expected_power_shelves) FROM expected_power_shelves WHERE expected_power_shelf_id=$1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn static_interface(pool: &PgPool, mac: &str, address: &str) -> MachineInterfaceId {
    let mut txn = pool.begin().await.unwrap();
    let address = address.parse().unwrap();
    let segment = db::network_segment::for_static_address(&mut txn, address)
        .await
        .unwrap();
    let interface = db::machine_interface::create_with_type(
        &mut txn,
        &[segment],
        &mac.parse().unwrap(),
        false,
        AddressSelectionStrategy::StaticAddress(address),
        model::machine_interface::InterfaceType::Bmc,
        None,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    interface.id
}

async fn addressless_interface(env: &TestEnv, mac: &str, address: &str) -> MachineInterfaceId {
    let id = static_interface(&env.pool, mac, address).await;
    let response = env
        .api
        .remove_static_address(Request::new(forge::RemoveStaticAddressRequest {
            interface_id: Some(id),
            ip_address: address.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.status(), forge::RemoveStaticAddressStatus::Removed);
    id
}

#[crate::sqlx_test]
async fn patch_expected_machine_preserves_unselected_fields(pool: PgPool) {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    env.api
        .add_expected_machine(Request::new(machine(id, 1)))
        .await
        .unwrap();
    let mut expected = machine_row(&env.pool, id).await;

    env.api
        .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                metadata: Some(forge::Metadata {
                    name: "after".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["metadata.name"]),
        }))
        .await
        .unwrap();
    expected["metadata_name"] = json!("after");
    assert_eq!(machine_row(&env.pool, id).await, expected);

    env.api
        .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                is_dpf_enabled: Some(false),
                default_pause_ingestion_and_poweron: Some(false),
                bmc_retain_credentials: Some(false),
                dpu_mode: Some(forge::DpuMode::NoDpu as i32),
                host_lifecycle_profile: Some(forge::HostLifecycleProfile {
                    disable_lockdown: Some(false),
                }),
                ..Default::default()
            }),
            update_mask: mask(&[
                "fallback_dpu_serial_numbers",
                "metadata.labels",
                "is_dpf_enabled",
                "default_pause_ingestion_and_poweron",
                "bmc_retain_credentials",
                "dpu_mode",
                "host_lifecycle_profile.disable_lockdown",
            ]),
        }))
        .await
        .unwrap();
    expected["fallback_dpu_serial_numbers"] = json!([]);
    expected["metadata_labels"] = json!({});
    expected["dpf_enabled"] = json!(false);
    expected["default_pause_ingestion_and_poweron"] = json!(false);
    expected["bmc_retain_credentials"] = json!(false);
    expected["dpu_mode"] = json!("no_dpu");
    expected["host_lifecycle_profile"] = json!({"disable_lockdown": false});
    assert_eq!(machine_row(&env.pool, id).await, expected);

    env.api
        .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                dpu_mode: Some(forge::DpuMode::Unspecified as i32),
                ..Default::default()
            }),
            update_mask: mask(&["dpu_mode"]),
        }))
        .await
        .unwrap();
    expected["dpu_mode"] = json!("dpu_mode");
    assert_eq!(machine_row(&env.pool, id).await, expected);

    env.api
        .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                bmc_password: "unselected".to_string(),
                ..Default::default()
            }),
            update_mask: mask(&[]),
        }))
        .await
        .unwrap();
    assert_eq!(machine_row(&env.pool, id).await, expected);
}

#[crate::sqlx_test]
async fn patch_expected_machine_sets_and_clears_nested_host_bmc_address(pool: PgPool) {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let mut fixture = machine(id, 7);
    fixture.interfaces_mut()[0].ip_allocation =
        Some(forge::ExpectedInterfaceIpAllocation::Dynamic as i32);
    env.api
        .add_expected_machine(Request::new(fixture))
        .await
        .unwrap();
    let original = machine_row(&env.pool, id).await;
    assert_eq!(
        original["bmc_ip_allocation"],
        json!(BmcIpAllocationType::Dynamic)
    );

    let error = env
        .api
        .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                bmc_ip_address: Some("192.0.2.240".to_string()),
                metadata: Some(forge::Metadata {
                    name: "must not change".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["bmc_ip_address", "metadata.name"]),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(machine_row(&env.pool, id).await, original);

    struct Case {
        name: &'static str,
        address: &'static str,
        allocation: Option<i32>,
        expected_address: Option<&'static str>,
        expected_allocation: BmcIpAllocationType,
        expected_nested_allocation: Option<ExpectedInterfaceIpAllocation>,
    }
    for case in [
        Case {
            name: "clearing an address preserves unselected Dynamic allocation",
            address: "",
            allocation: None,
            expected_address: None,
            expected_allocation: BmcIpAllocationType::Dynamic,
            expected_nested_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
        },
        Case {
            name: "setting an address succeeds with selected compatible allocation",
            address: "192.0.2.240",
            allocation: Some(forge::BmcIpAllocationType::Auto as i32),
            expected_address: Some("192.0.2.240"),
            expected_allocation: BmcIpAllocationType::Auto,
            expected_nested_allocation: None,
        },
        Case {
            name: "clearing an address preserves unselected Auto allocation",
            address: "",
            allocation: None,
            expected_address: None,
            expected_allocation: BmcIpAllocationType::Auto,
            expected_nested_allocation: None,
        },
    ] {
        let mut paths = vec!["bmc_ip_address"];
        if case.allocation.is_some() {
            paths.push("bmc_ip_allocation");
        }
        env.api
            .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
                expected_machine: Some(forge::ExpectedMachine {
                    id: rpc_id(id),
                    bmc_ip_address: Some(case.address.to_string()),
                    bmc_ip_allocation: case.allocation,
                    ..Default::default()
                }),
                update_mask: mask(&paths),
            }))
            .await
            .unwrap_or_else(|error| panic!("case {}: {error}", case.name));

        let mut expected = original.clone();
        let mut interfaces: Vec<ExpectedInterface> =
            serde_json::from_value(original["host_nics"].clone()).unwrap();
        interfaces[0].fixed_ip = case
            .expected_address
            .map(|address| address.parse().unwrap());
        interfaces[0].ip_allocation = case.expected_nested_allocation;
        expected["bmc_ip_address"] = json!(case.expected_address);
        expected["bmc_ip_allocation"] = json!(case.expected_allocation);
        expected["host_nics"] = json!(interfaces);
        assert_eq!(
            machine_row(&env.pool, id).await,
            expected,
            "case: {}",
            case.name
        );
    }
}

#[crate::sqlx_test]
async fn patch_expected_machine_replaces_interfaces_and_applies_selected_bmc_fields(pool: PgPool) {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let fixture = machine(id, 9);
    let bmc_mac = fixture.bmc_mac_address.clone();
    let dpu_mac = fixture.interfaces()[1].mac_address.clone();
    env.api
        .add_expected_machine(Request::new(fixture))
        .await
        .unwrap();
    let original = machine_row(&env.pool, id).await;
    let interfaces: Vec<ExpectedInterface> =
        serde_json::from_value(original["host_nics"].clone()).unwrap();
    let mut reordered = vec![interfaces[1].clone(), interfaces[0].clone()];
    reordered[0].nic_type = Some("updated".to_string());
    let mut dynamic_bmc = interfaces[0].clone();
    dynamic_bmc.ip_allocation = Some(ExpectedInterfaceIpAllocation::Dynamic);
    let mut inferred_bmc = interfaces[0].clone();
    inferred_bmc.ip_allocation = None;
    let mut fixed_bmc = inferred_bmc.clone();
    fixed_bmc.fixed_ip = Some("192.0.2.245".parse().unwrap());

    struct Case {
        name: &'static str,
        patch: forge::ExpectedMachine,
        paths: &'static [&'static str],
        interfaces: Vec<ExpectedInterface>,
        allocation: BmcIpAllocationType,
    }

    for case in [
        Case {
            name: "reordered interfaces retain omitted role and allocation by MAC",
            patch: forge::ExpectedMachine {
                host_nics: vec![
                    forge::ExpectedInterface {
                        mac_address: dpu_mac,
                        nic_type: Some("updated".to_string()),
                        ..Default::default()
                    },
                    forge::ExpectedInterface {
                        mac_address: bmc_mac.clone(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            paths: &["host_nics"],
            interfaces: reordered,
            allocation: BmcIpAllocationType::Retained,
        },
        Case {
            name: "replacement changes nested allocation and ignores unselected top-level value",
            patch: forge::ExpectedMachine {
                host_nics: vec![forge::ExpectedInterface {
                    mac_address: bmc_mac.clone(),
                    role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    ..Default::default()
                }],
                bmc_ip_allocation: Some(forge::BmcIpAllocationType::Retained as i32),
                ..Default::default()
            },
            paths: &["host_nics"],
            interfaces: vec![dynamic_bmc.clone()],
            allocation: BmcIpAllocationType::Dynamic,
        },
        Case {
            name: "selected top-level allocation overrides the list even when unchanged",
            patch: forge::ExpectedMachine {
                host_nics: vec![forge::ExpectedInterface {
                    mac_address: bmc_mac.clone(),
                    role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                }],
                bmc_ip_allocation: Some(forge::BmcIpAllocationType::Dynamic as i32),
                ..Default::default()
            },
            paths: &["host_nics", "bmc_ip_allocation"],
            interfaces: vec![dynamic_bmc.clone()],
            allocation: BmcIpAllocationType::Dynamic,
        },
        Case {
            name: "explicit allocation reset ignores an unselected interface list",
            patch: forge::ExpectedMachine {
                host_nics: vec![forge::ExpectedInterface {
                    mac_address: "invalid unselected MAC".to_string(),
                    ..Default::default()
                }],
                bmc_ip_allocation: Some(forge::BmcIpAllocationType::Unspecified as i32),
                ..Default::default()
            },
            paths: &["bmc_ip_allocation"],
            interfaces: vec![inferred_bmc.clone()],
            allocation: BmcIpAllocationType::Auto,
        },
        Case {
            name: "nested address ignores an unselected top-level address",
            patch: forge::ExpectedMachine {
                host_nics: vec![forge::ExpectedInterface {
                    mac_address: bmc_mac.clone(),
                    role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                    fixed_ip: Some("192.0.2.245".to_string()),
                    ..Default::default()
                }],
                bmc_ip_address: Some("invalid unselected address".to_string()),
                ..Default::default()
            },
            paths: &["host_nics"],
            interfaces: vec![fixed_bmc],
            allocation: BmcIpAllocationType::Auto,
        },
        Case {
            name: "selected dynamic allocation clears the persisted fixed address",
            patch: forge::ExpectedMachine {
                bmc_ip_allocation: Some(forge::BmcIpAllocationType::Dynamic as i32),
                ..Default::default()
            },
            paths: &["bmc_ip_allocation"],
            interfaces: vec![dynamic_bmc],
            allocation: BmcIpAllocationType::Dynamic,
        },
        Case {
            name: "selected empty top-level address clears a new nested address",
            patch: forge::ExpectedMachine {
                host_nics: vec![forge::ExpectedInterface {
                    mac_address: bmc_mac,
                    role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                    fixed_ip: Some("192.0.2.245".to_string()),
                    ..Default::default()
                }],
                bmc_ip_address: Some(String::new()),
                ..Default::default()
            },
            paths: &["host_nics", "bmc_ip_address"],
            interfaces: vec![inferred_bmc],
            allocation: BmcIpAllocationType::Auto,
        },
        Case {
            name: "selected empty interface list removes every declaration",
            patch: forge::ExpectedMachine::default(),
            paths: &["host_nics"],
            interfaces: vec![],
            allocation: BmcIpAllocationType::Auto,
        },
    ] {
        env.api
            .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
                expected_machine: Some(forge::ExpectedMachine {
                    id: rpc_id(id),
                    ..case.patch
                }),
                update_mask: mask(case.paths),
            }))
            .await
            .unwrap_or_else(|error| panic!("case {}: {error}", case.name));

        let mut expected = original.clone();
        expected["bmc_ip_address"] = json!(
            case.interfaces
                .iter()
                .find(|interface| interface.role.is_host_bmc())
                .and_then(|interface| interface.fixed_ip)
        );
        expected["host_nics"] = json!(case.interfaces);
        expected["bmc_ip_allocation"] = json!(case.allocation);
        assert_eq!(
            machine_row(&env.pool, id).await,
            expected,
            "case: {}",
            case.name
        );
    }
}

#[crate::sqlx_test]
async fn patch_expected_power_shelf_preserves_unselected_fields(pool: PgPool) {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    env.api
        .add_expected_power_shelf(Request::new(forge::ExpectedPowerShelf {
            expected_power_shelf_id: rpc_id(id),
            bmc_mac_address: "02:00:00:00:59:02".to_string(),
            bmc_username: "shelf-user".to_string(),
            bmc_password: "shelf-password".to_string(),
            shelf_serial_number: "SHELF-002".to_string(),
            bmc_retain_credentials: Some(true),
            metadata: Some(forge::Metadata {
                description: "keep description".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await
        .unwrap();
    let mut expected = power_shelf_row(&env.pool, id).await;
    env.api
        .patch_expected_power_shelf(Request::new(forge::PatchExpectedPowerShelfRequest {
            expected_power_shelf: Some(forge::ExpectedPowerShelf {
                expected_power_shelf_id: rpc_id(id),
                metadata: Some(forge::Metadata {
                    name: "after".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["metadata.name"]),
        }))
        .await
        .unwrap();
    expected["metadata_name"] = json!("after");
    assert_eq!(power_shelf_row(&env.pool, id).await, expected);

    env.api
        .patch_expected_power_shelf(Request::new(forge::PatchExpectedPowerShelfRequest {
            expected_power_shelf: Some(forge::ExpectedPowerShelf {
                expected_power_shelf_id: rpc_id(id),
                bmc_retain_credentials: Some(false),
                ..Default::default()
            }),
            update_mask: mask(&["bmc_retain_credentials"]),
        }))
        .await
        .unwrap();
    expected["bmc_retain_credentials"] = json!(false);
    assert_eq!(power_shelf_row(&env.pool, id).await, expected);
}

#[crate::sqlx_test]
async fn patch_expected_switch_preserves_unselected_pairs(pool: PgPool) {
    struct Case {
        paths: Vec<&'static str>,
        patch: forge::ExpectedSwitch,
        changed_columns: Vec<(&'static str, Value)>,
    }

    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    env.api
        .add_expected_switch(Request::new(forge::ExpectedSwitch {
            expected_switch_id: rpc_id(id),
            bmc_mac_address: "02:00:00:00:59:03".to_string(),
            bmc_username: "bmc-user".to_string(),
            bmc_password: "bmc-password".to_string(),
            nvos_username: Some("nvos-user".to_string()),
            nvos_password: Some("nvos-password".to_string()),
            nvos_mac_addresses: vec!["02:00:00:01:59:03".to_string()],
            switch_serial_number: "SWITCH-003".to_string(),
            bmc_retain_credentials: Some(true),
            ..Default::default()
        }))
        .await
        .unwrap();
    let mut expected = switch_row(&env.pool, id).await;
    for Case {
        paths,
        patch,
        changed_columns,
    } in [
        Case {
            paths: vec!["metadata.name"],
            patch: forge::ExpectedSwitch {
                metadata: Some(forge::Metadata {
                    name: "after".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            changed_columns: vec![("metadata_name", json!("after"))],
        },
        Case {
            paths: vec!["bmc_username", "bmc_password"],
            patch: forge::ExpectedSwitch {
                bmc_username: "new-bmc-user".to_string(),
                bmc_password: "new-bmc-password".to_string(),
                ..Default::default()
            },
            changed_columns: vec![
                ("bmc_username", json!("new-bmc-user")),
                ("bmc_password", json!("new-bmc-password")),
            ],
        },
        Case {
            paths: vec!["nvos_username", "nvos_password"],
            patch: forge::ExpectedSwitch {
                nvos_username: Some("new-nvos-user".to_string()),
                nvos_password: Some("new-nvos-password".to_string()),
                ..Default::default()
            },
            changed_columns: vec![
                ("nvos_username", json!("new-nvos-user")),
                ("nvos_password", json!("new-nvos-password")),
            ],
        },
        Case {
            paths: vec!["bmc_retain_credentials"],
            patch: forge::ExpectedSwitch {
                bmc_retain_credentials: Some(false),
                ..Default::default()
            },
            changed_columns: vec![("bmc_retain_credentials", json!(false))],
        },
    ] {
        env.api
            .patch_expected_switch(Request::new(forge::PatchExpectedSwitchRequest {
                expected_switch: Some(forge::ExpectedSwitch {
                    expected_switch_id: rpc_id(id),
                    ..patch
                }),
                update_mask: mask(&paths),
            }))
            .await
            .unwrap();
        for (column, value) in changed_columns {
            expected[column] = value;
        }
        assert_eq!(
            switch_row(&env.pool, id).await,
            expected,
            "selected fields: {paths:?}"
        );
    }
}

#[crate::sqlx_test]
async fn patch_expected_switch_rejects_clearing_macs_for_stored_nvos_ip(pool: PgPool) {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    env.api
        .add_expected_switch(Request::new(forge::ExpectedSwitch {
            expected_switch_id: rpc_id(id),
            bmc_mac_address: "02:00:00:00:59:08".to_string(),
            bmc_username: "bmc-user".to_string(),
            bmc_password: "bmc-password".to_string(),
            nvos_mac_addresses: vec!["02:00:00:01:59:08".to_string()],
            nvos_ip_address: Some("192.0.2.241".to_string()),
            switch_serial_number: "SWITCH-008".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap();
    let original = switch_row(&env.pool, id).await;

    let error = env
        .api
        .patch_expected_switch(Request::new(forge::PatchExpectedSwitchRequest {
            expected_switch: Some(forge::ExpectedSwitch {
                expected_switch_id: rpc_id(id),
                ..Default::default()
            }),
            update_mask: mask(&["nvos_mac_addresses"]),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error}");
    assert!(
        error
            .message()
            .contains("nvos_ip_address requires exactly one"),
        "{error}"
    );
    assert_eq!(switch_row(&env.pool, id).await, original);

    env.api
        .patch_expected_switch(Request::new(forge::PatchExpectedSwitchRequest {
            expected_switch: Some(forge::ExpectedSwitch {
                expected_switch_id: rpc_id(id),
                ..Default::default()
            }),
            update_mask: mask(&["nvos_ip_address", "nvos_mac_addresses"]),
        }))
        .await
        .unwrap();
    let mut expected = original;
    expected["nvos_ip_address"] = Value::Null;
    expected["nvos_mac_addresses"] = json!([]);
    assert_eq!(switch_row(&env.pool, id).await, expected);
}

#[crate::sqlx_test]
async fn patch_expected_switch_validates_nvos_ip_against_retained_macs(pool: PgPool) {
    struct Case {
        name: &'static str,
        suffix: u8,
        nvos_macs: Vec<&'static str>,
        expected_code: tonic::Code,
    }

    let env = create_test_env(pool).await;
    for Case {
        name,
        suffix,
        nvos_macs,
        expected_code,
    } in [
        Case {
            name: "one retained MAC accepts the new address",
            suffix: 10,
            nvos_macs: vec!["02:00:00:01:59:10"],
            expected_code: tonic::Code::Ok,
        },
        Case {
            name: "multiple retained MACs reject the address and every other field",
            suffix: 11,
            nvos_macs: vec!["02:00:00:01:59:11", "02:00:00:01:59:12"],
            expected_code: tonic::Code::InvalidArgument,
        },
    ] {
        let id = Uuid::new_v4();
        env.api
            .add_expected_switch(Request::new(forge::ExpectedSwitch {
                expected_switch_id: rpc_id(id),
                bmc_mac_address: format!("02:00:00:00:59:{suffix:02x}"),
                bmc_username: "bmc-user".to_string(),
                bmc_password: "bmc-password".to_string(),
                nvos_mac_addresses: nvos_macs.into_iter().map(str::to_string).collect(),
                switch_serial_number: format!("SWITCH-{suffix:03}"),
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut expected = switch_row(&env.pool, id).await;
        let result = env
            .api
            .patch_expected_switch(Request::new(forge::PatchExpectedSwitchRequest {
                expected_switch: Some(forge::ExpectedSwitch {
                    expected_switch_id: rpc_id(id),
                    nvos_ip_address: Some("192.0.2.246".to_string()),
                    metadata: Some(forge::Metadata {
                        name: "updated with address".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                update_mask: mask(&["nvos_ip_address", "metadata.name"]),
            }))
            .await;
        match result {
            Ok(_) => {
                assert_eq!(expected_code, tonic::Code::Ok, "case: {name}");
                expected["nvos_ip_address"] = json!("192.0.2.246");
                expected["metadata_name"] = json!("updated with address");
            }
            Err(error) => assert_eq!(error.code(), expected_code, "case {name}: {error}"),
        }
        assert_eq!(switch_row(&env.pool, id).await, expected, "case: {name}");
    }
}

#[crate::sqlx_test]
async fn patch_expected_machines_preserves_distinct_credentials_and_rolls_back(pool: PgPool) {
    let env = create_test_env(pool).await;
    let ids = [Uuid::from_u128(1), Uuid::from_u128(2)];
    for (index, id) in ids.into_iter().enumerate() {
        env.api
            .add_expected_machine(Request::new(machine(id, index as u8 + 4)))
            .await
            .unwrap();
    }
    let mut expected = [
        machine_row(&env.pool, ids[0]).await,
        machine_row(&env.pool, ids[1]).await,
    ];
    let patch_name = |id, name: &str| forge::PatchExpectedMachineRequest {
        expected_machine: Some(forge::ExpectedMachine {
            id: rpc_id(id),
            metadata: Some(forge::Metadata {
                name: name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        update_mask: mask(&["metadata.name"]),
    };
    env.api
        .patch_expected_machines(Request::new(forge::PatchExpectedMachinesRequest {
            patches: ids
                .into_iter()
                .rev()
                .map(|id| patch_name(id, "batch updated"))
                .collect(),
        }))
        .await
        .unwrap();
    for (id, expected) in ids.into_iter().zip(&mut expected) {
        expected["metadata_name"] = json!("batch updated");
        assert_eq!(machine_row(&env.pool, id).await, *expected);
    }

    // The first write succeeds inside the transaction; a missing later row
    // must roll it back before any caller can observe the new credentials.
    let error = env
        .api
        .patch_expected_machines(Request::new(forge::PatchExpectedMachinesRequest {
            patches: vec![
                forge::PatchExpectedMachineRequest {
                    expected_machine: Some(forge::ExpectedMachine {
                        id: rpc_id(ids[0]),
                        bmc_username: "rollback-user".to_string(),
                        bmc_password: "rollback-password".to_string(),
                        ..Default::default()
                    }),
                    update_mask: mask(&["bmc_username", "bmc_password"]),
                },
                patch_name(Uuid::from_u128(3), "missing"),
            ],
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound, "{error}");
    for (id, expected) in ids.into_iter().zip(&expected) {
        assert_eq!(machine_row(&env.pool, id).await, *expected);
    }

    for (case, patches) in [
        (
            "incomplete credential pair",
            vec![
                patch_name(ids[0], "must not change"),
                forge::PatchExpectedMachineRequest {
                    expected_machine: Some(forge::ExpectedMachine {
                        id: rpc_id(ids[1]),
                        bmc_username: "only-username".to_string(),
                        ..Default::default()
                    }),
                    update_mask: mask(&["bmc_username"]),
                },
            ],
        ),
        ("empty batch", vec![]),
        (
            "duplicate machine IDs",
            vec![
                patch_name(ids[0], "first duplicate"),
                patch_name(ids[0], "second duplicate"),
            ],
        ),
    ] {
        let error = env
            .api
            .patch_expected_machines(Request::new(forge::PatchExpectedMachinesRequest {
                patches,
            }))
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            tonic::Code::InvalidArgument,
            "case {case}: {error}"
        );
        for (id, expected) in ids.into_iter().zip(&expected) {
            assert_eq!(machine_row(&env.pool, id).await, *expected, "case: {case}");
        }
    }
}

#[crate::sqlx_test]
async fn patch_expected_machine_merges_after_a_concurrent_writer_commits(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    env.api
        .add_expected_machine(Request::new(machine(id, 6)))
        .await?;
    let mut expected = machine_row(&env.pool, id).await;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("UPDATE expected_machines SET bmc_username=$1, bmc_password=$2 WHERE id=$3")
        .bind("concurrent-user")
        .bind("concurrent-password")
        .bind(id)
        .execute(&mut *blocker)
        .await?;
    let api = env.api.clone();
    let patch_task = tokio::spawn(async move {
        api.patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
            expected_machine: Some(forge::ExpectedMachine {
                id: rpc_id(id),
                metadata: Some(forge::Metadata {
                    name: "after lock".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["metadata.name"]),
        }))
        .await
    });
    wait_for_blocked_query(&env.pool, blocker_pid, "expected_machines").await;
    blocker.commit().await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), patch_task).await???;

    expected["bmc_username"] = json!("concurrent-user");
    expected["bmc_password"] = json!("concurrent-password");
    expected["metadata_name"] = json!("after lock");
    assert_eq!(machine_row(&env.pool, id).await, expected);
    Ok(())
}

#[crate::sqlx_test]
async fn expected_power_shelf_update_locks_inventory_before_interface(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let fixture = forge::ExpectedPowerShelf {
        expected_power_shelf_id: rpc_id(id),
        bmc_mac_address: "02:00:00:00:59:20".to_string(),
        bmc_username: "before".to_string(),
        bmc_password: "password".to_string(),
        shelf_serial_number: "SHELF-LOCK".to_string(),
        bmc_ip_address: "192.0.2.230".to_string(),
        ..Default::default()
    };
    env.api
        .add_expected_power_shelf(Request::new(fixture.clone()))
        .await?;
    let interface_id =
        addressless_interface(&env, &fixture.bmc_mac_address, &fixture.bmc_ip_address).await;
    let mut expected = power_shelf_row(&env.pool, id).await;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    db::expected_power_shelf::find_by_id_for_update(&mut blocker, id)
        .await?
        .expect("expected shelf exists");
    let api = env.api.clone();
    let update = tokio::spawn(async move {
        api.update_expected_power_shelf(Request::new(forge::ExpectedPowerShelf {
            bmc_username: "after lock".to_string(),
            ..fixture
        }))
        .await
    });
    wait_for_blocked_query(&env.pool, blocker_pid, "expected_power_shelves").await;

    // PATCH already owns the inventory lock. A legacy update waiting for
    // that lock must not hold the interface lock PATCH acquires next.
    let mut probe = env.pool.begin().await?;
    let interface_lock: Result<MachineInterfaceId, _> =
        sqlx::query_scalar("SELECT id FROM machine_interfaces WHERE id=$1 FOR UPDATE NOWAIT")
            .bind(interface_id)
            .fetch_one(&mut *probe)
            .await;
    probe.rollback().await?;
    blocker.commit().await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), update).await???;
    assert!(interface_lock.is_ok(), "{interface_lock:?}");

    expected["bmc_username"] = json!("after lock");
    assert_eq!(power_shelf_row(&env.pool, id).await, expected);
    let mut txn = env.pool.begin().await?;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(
        addresses[0].address,
        "192.0.2.230".parse::<std::net::IpAddr>()?
    );
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn expected_switch_update_locks_inventory_before_interface(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let fixture = forge::ExpectedSwitch {
        expected_switch_id: rpc_id(id),
        bmc_mac_address: "02:00:00:00:59:21".to_string(),
        bmc_username: "before".to_string(),
        bmc_password: "password".to_string(),
        switch_serial_number: "SWITCH-LOCK".to_string(),
        bmc_ip_address: "192.0.2.231".to_string(),
        ..Default::default()
    };
    env.api
        .add_expected_switch(Request::new(fixture.clone()))
        .await?;
    let interface_id =
        addressless_interface(&env, &fixture.bmc_mac_address, &fixture.bmc_ip_address).await;
    let mut expected = switch_row(&env.pool, id).await;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    db::expected_switch::find_for_update(
        &mut blocker,
        &model::expected_switch::ExpectedSwitchRequest {
            expected_switch_id: Some(id),
            bmc_mac_address: None,
        },
    )
    .await?
    .expect("expected switch exists");
    let api = env.api.clone();
    let update = tokio::spawn(async move {
        api.update_expected_switch(Request::new(forge::ExpectedSwitch {
            bmc_username: "after lock".to_string(),
            ..fixture
        }))
        .await
    });
    wait_for_blocked_query(&env.pool, blocker_pid, "expected_switches:write").await;

    let mut probe = env.pool.begin().await?;
    let interface_lock: Result<MachineInterfaceId, _> =
        sqlx::query_scalar("SELECT id FROM machine_interfaces WHERE id=$1 FOR UPDATE NOWAIT")
            .bind(interface_id)
            .fetch_one(&mut *probe)
            .await;
    probe.rollback().await?;
    blocker.commit().await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), update).await???;
    assert!(interface_lock.is_ok(), "{interface_lock:?}");

    expected["bmc_username"] = json!("after lock");
    assert_eq!(switch_row(&env.pool, id).await, expected);
    let mut txn = env.pool.begin().await?;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(
        addresses[0].address,
        "192.0.2.231".parse::<std::net::IpAddr>()?
    );
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn patch_expected_power_shelf_rolls_back_when_address_is_occupied(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let bmc_mac = "02:00:00:00:59:22";
    let occupied_address = "192.0.2.232";
    let owner_id = static_interface(&env.pool, "02:00:00:01:59:22", occupied_address).await;
    let target_id = addressless_interface(&env, bmc_mac, "192.0.2.233").await;
    env.api
        .add_expected_power_shelf(Request::new(forge::ExpectedPowerShelf {
            expected_power_shelf_id: rpc_id(id),
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "before".to_string(),
            bmc_password: "password".to_string(),
            shelf_serial_number: "SHELF-ROLLBACK".to_string(),
            ..Default::default()
        }))
        .await?;
    let original = power_shelf_row(&env.pool, id).await;

    let error = env
        .api
        .patch_expected_power_shelf(Request::new(forge::PatchExpectedPowerShelfRequest {
            expected_power_shelf: Some(forge::ExpectedPowerShelf {
                expected_power_shelf_id: rpc_id(id),
                bmc_ip_address: occupied_address.to_string(),
                metadata: Some(forge::Metadata {
                    name: "must roll back".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["bmc_ip_address", "metadata.name"]),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{error}");
    assert!(
        error.message().contains("address already in use"),
        "{error}"
    );
    assert_eq!(power_shelf_row(&env.pool, id).await, original);
    assert!(
        db::machine_interface::find_one(&env.pool, target_id)
            .await?
            .addresses
            .is_empty()
    );
    assert_eq!(
        db::machine_interface::find_one(&env.pool, owner_id)
            .await?
            .addresses,
        vec![occupied_address.parse::<std::net::IpAddr>()?]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn patch_expected_switch_rolls_back_bmc_allocation_when_nvos_address_is_occupied(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let bmc_mac = "02:00:00:00:59:23";
    let nvos_mac = "02:00:00:01:59:23";
    let occupied_address = "192.0.2.236";
    let owner_id = static_interface(&env.pool, "02:00:00:02:59:23", occupied_address).await;
    let bmc_id = addressless_interface(&env, bmc_mac, "192.0.2.234").await;
    let nvos_id = addressless_interface(&env, nvos_mac, "192.0.2.235").await;
    env.api
        .add_expected_switch(Request::new(forge::ExpectedSwitch {
            expected_switch_id: rpc_id(id),
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "before".to_string(),
            bmc_password: "password".to_string(),
            switch_serial_number: "SWITCH-ROLLBACK".to_string(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            ..Default::default()
        }))
        .await?;
    let original = switch_row(&env.pool, id).await;

    let error = env
        .api
        .patch_expected_switch(Request::new(forge::PatchExpectedSwitchRequest {
            expected_switch: Some(forge::ExpectedSwitch {
                expected_switch_id: rpc_id(id),
                bmc_ip_address: "192.0.2.234".to_string(),
                nvos_ip_address: Some(occupied_address.to_string()),
                metadata: Some(forge::Metadata {
                    name: "must roll back".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: mask(&["bmc_ip_address", "nvos_ip_address", "metadata.name"]),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{error}");
    assert!(error.message().contains(occupied_address), "{error}");
    assert_eq!(switch_row(&env.pool, id).await, original);
    for interface_id in [bmc_id, nvos_id] {
        assert!(
            db::machine_interface::find_one(&env.pool, interface_id)
                .await?
                .addresses
                .is_empty()
        );
    }
    assert_eq!(
        db::machine_interface::find_one(&env.pool, owner_id)
            .await?
            .addresses,
        vec![occupied_address.parse::<std::net::IpAddr>()?]
    );
    Ok(())
}
