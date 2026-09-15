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
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedInterfaceRole,
};
use prost_types::FieldMask;
use rpc::forge::forge_server::Forge;
use rpc::{common, forge};
use serde_json::{Value, json};
use sqlx::PgPool;
use tonic::Request;
use uuid::Uuid;

use crate::tests::common::api_fixtures::create_test_env;
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
        bmc_retain_credentials: Some(false),
        dpu_mode: Some(forge::DpuMode::NicMode as i32),
        host_lifecycle_profile: Some(forge::HostLifecycleProfile {
            disable_lockdown: Some(true),
        }),
        host_nics: vec![
            forge::ExpectedHostNic {
                mac_address: bmc_mac_address,
                role: Some(forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            },
            forge::ExpectedHostNic {
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
                host_lifecycle_profile: Some(forge::HostLifecycleProfile {
                    disable_lockdown: Some(false),
                }),
                ..Default::default()
            }),
            update_mask: mask(&[
                "fallback_dpu_serial_numbers",
                "metadata.labels",
                "is_dpf_enabled",
                "host_lifecycle_profile.disable_lockdown",
            ]),
        }))
        .await
        .unwrap();
    expected["fallback_dpu_serial_numbers"] = json!([]);
    expected["metadata_labels"] = json!({});
    expected["dpf_enabled"] = json!(false);
    expected["host_lifecycle_profile"] = json!({"disable_lockdown": false});
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
    env.api
        .add_expected_machine(Request::new(machine(id, 7)))
        .await
        .unwrap();
    let original = machine_row(&env.pool, id).await;
    assert_eq!(
        original["bmc_ip_allocation"],
        json!(BmcIpAllocationType::Retained)
    );

    for (case, address, expected_address, expected_policy) in [
        (
            "set",
            "192.0.2.240",
            Some("192.0.2.240"),
            ExpectedInterfaceIpAllocation::Fixed,
        ),
        ("clear", "", None, ExpectedInterfaceIpAllocation::Retained),
    ] {
        env.api
            .patch_expected_machine(Request::new(forge::PatchExpectedMachineRequest {
                expected_machine: Some(forge::ExpectedMachine {
                    id: rpc_id(id),
                    bmc_ip_address: Some(address.to_string()),
                    ..Default::default()
                }),
                update_mask: mask(&["bmc_ip_address"]),
            }))
            .await
            .unwrap();

        let stored = machine_row(&env.pool, id).await;
        assert_eq!(
            stored["bmc_ip_address"],
            json!(expected_address),
            "case: {case}"
        );
        assert_eq!(
            stored["bmc_ip_allocation"],
            json!(BmcIpAllocationType::Auto),
            "case: {case}"
        );
        let interfaces: Vec<ExpectedInterface> =
            serde_json::from_value(stored["host_nics"].clone()).unwrap();
        assert_eq!(interfaces.len(), 2, "case: {case}");
        let host_bmc = &interfaces[0];
        assert_eq!(
            host_bmc.role,
            ExpectedInterfaceRole::HostBmc,
            "case: {case}"
        );
        assert_eq!(host_bmc.ip_allocation, None, "case: {case}");
        assert_eq!(
            host_bmc.fixed_ip.map(|ip| ip.to_string()),
            expected_address.map(str::to_string),
            "case: {case}"
        );
        assert_eq!(
            host_bmc.resolved_ip_allocation(),
            expected_policy,
            "case: {case}"
        );
        assert_eq!(
            stored["host_nics"][1], original["host_nics"][1],
            "case: {case}"
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
            bmc_retain_credentials: Some(false),
            metadata: Some(forge::Metadata {
                description: "keep description".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await
        .unwrap();
    let query = "SELECT to_jsonb(expected_power_shelves) FROM expected_power_shelves WHERE expected_power_shelf_id=$1";
    let mut expected: Value = sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
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
    let stored: Value = sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(stored, expected);
}

#[crate::sqlx_test]
async fn patch_expected_switch_preserves_unselected_pairs(pool: PgPool) {
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
            bmc_retain_credentials: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
    let mut expected = switch_row(&env.pool, id).await;
    for (paths, patch, changed_columns) in [
        (
            vec!["metadata.name"],
            forge::ExpectedSwitch {
                metadata: Some(forge::Metadata {
                    name: "after".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![("metadata_name", "after")],
        ),
        (
            vec!["bmc_username", "bmc_password"],
            forge::ExpectedSwitch {
                bmc_username: "new-bmc-user".to_string(),
                bmc_password: "new-bmc-password".to_string(),
                ..Default::default()
            },
            vec![
                ("bmc_username", "new-bmc-user"),
                ("bmc_password", "new-bmc-password"),
            ],
        ),
        (
            vec!["nvos_username", "nvos_password"],
            forge::ExpectedSwitch {
                nvos_username: Some("new-nvos-user".to_string()),
                nvos_password: Some("new-nvos-password".to_string()),
                ..Default::default()
            },
            vec![
                ("nvos_username", "new-nvos-user"),
                ("nvos_password", "new-nvos-password"),
            ],
        ),
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
            expected[column] = json!(value);
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

    let error = env
        .api
        .patch_expected_machines(Request::new(forge::PatchExpectedMachinesRequest {
            patches: vec![
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
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    for (id, expected) in ids.into_iter().zip(&expected) {
        assert_eq!(machine_row(&env.pool, id).await, *expected);
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
