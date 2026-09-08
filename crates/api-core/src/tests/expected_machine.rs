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
use std::default::Default;

use common::api_fixtures::{
    TestEnvOverrides, create_test_env, create_test_env_with_overrides, get_config,
};
use db::{self};
use mac_address::MacAddress;
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterfaceIpAllocation, ExpectedInterfaceRole, ExpectedMachine,
    ExpectedMachineData,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::{ExpectedMachineList, ExpectedMachineRequest};
use uuid::Uuid;

use crate::CarbideError;
use crate::test_support::fixture_config::FixtureDefault as _;
use crate::test_support::metadata;
use crate::tests::common;

async fn create_fixture_expected_machines(pool: &sqlx::PgPool) {
    let mut txn = pool.begin().await.unwrap();
    for (bmc_mac_address, serial_number, fallback_dpu_serial_numbers) in [
        ("0a:0b:0c:0d:0e:0f", "VVG121GG", vec![]),
        ("1a:1b:1c:1d:1e:1f", "VVG121GH", vec![]),
        ("2a:2b:2c:2d:2e:2f", "VVG121GI", vec![]),
        ("3a:3b:3c:3d:3e:3f", "VVG121GJ", vec!["dpu_serial1"]),
        (
            "4a:4b:4c:4d:4e:4f",
            "VVG121GK",
            vec!["dpu_serial2", "dpu_serial3"],
        ),
        ("5a:5b:5c:5d:5e:5f", "VVG121GL", vec![]),
    ] {
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: bmc_mac_address.parse().unwrap(),
                data: ExpectedMachineData {
                    bmc_username: "ADMIN".into(),
                    bmc_password: "Pwd2023x0x0x0x0x7".into(),
                    serial_number: serial_number.into(),
                    fallback_dpu_serial_numbers: fallback_dpu_serial_numbers
                        .into_iter()
                        .map(ToString::to_string)
                        .collect(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();
}

// Test API functionality
/*
  // Expected Machine Management
  // Replace all expected machines in site
  rpc ReplaceAllExpectedMachines(ExpectedMachineList) returns (google.protobuf.Empty);
*/
#[crate::sqlx_test()]
async fn test_add_expected_machine(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    for (idx, expected_machine) in [
        rpc::forge::ExpectedMachine {
            bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "VVG121GI".into(),
            metadata: None,
            sku_id: None,
            id: Some(::rpc::common::Uuid {
                value: Uuid::new_v4().to_string(),
            }),
            default_pause_ingestion_and_poweron: Some(true),
            is_dpf_enabled: Some(false),
            ..Default::default()
        },
        rpc::forge::ExpectedMachine {
            bmc_mac_address: "3A:3B:3C:3D:3E:40".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "VVG121GI".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            sku_id: Some("sku_id".to_string()),
            id: Some(::rpc::common::Uuid {
                value: Uuid::new_v4().to_string(),
            }),
            default_pause_ingestion_and_poweron: Some(false),
            is_dpf_enabled: Some(true),
            #[allow(deprecated)]
            dpf_enabled: true,
            ..Default::default()
        },
        rpc::forge::ExpectedMachine {
            bmc_mac_address: "3A:3B:3C:3D:3E:41".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "VVG121GI".into(),
            metadata: Some(rpc::forge::Metadata {
                name: "a".to_string(),
                description: "desc".to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: "k1".to_string(),
                        value: None,
                    },
                    rpc::forge::Label {
                        key: "k2".to_string(),
                        value: Some("v2".to_string()),
                    },
                ],
            }),
            id: Some(::rpc::common::Uuid {
                value: Uuid::new_v4().to_string(),
            }),
            sku_id: Some("sku_id".to_string()),
            default_pause_ingestion_and_poweron: None,
            is_dpf_enabled: Some(false),
            ..Default::default()
        },
    ]
    .iter_mut()
    .enumerate()
    {
        env.api
            .add_expected_machine(tonic::Request::new(expected_machine.clone()))
            .await
            .expect("unable to add expected machine ");

        let expected_machine_query = rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: expected_machine.bmc_mac_address.clone(),
            id: None,
        };

        let mut retrieved_expected_machine = env
            .api
            .get_expected_machine(tonic::Request::new(expected_machine_query))
            .await
            .expect("unable to retrieve expected machine ")
            .into_inner();
        retrieved_expected_machine
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        if expected_machine.metadata.is_none() {
            expected_machine.metadata = Some(Default::default());
        }
        if expected_machine
            .default_pause_ingestion_and_poweron
            .is_none()
        {
            expected_machine.default_pause_ingestion_and_poweron = Some(false);
        }
        assert_eq!(retrieved_expected_machine, expected_machine.clone());

        if idx != 1 {
            assert!(
                !retrieved_expected_machine
                    .is_dpf_enabled
                    .unwrap_or_default()
            );
        } else {
            assert!(
                retrieved_expected_machine
                    .is_dpf_enabled
                    .unwrap_or_default()
            );
        }
    }
}

#[crate::sqlx_test]
async fn test_delete_expected_machine(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;

    let expected_machine_count = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines
        .len();

    let expected_machine_query = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
        id: None,
    };
    env.api
        .delete_expected_machine(tonic::Request::new(expected_machine_query))
        .await
        .expect("unable to delete expected machine ")
        .into_inner();

    let new_expected_machine_count = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines
        .len();

    assert_eq!(new_expected_machine_count, expected_machine_count - 1);
}

#[crate::sqlx_test()]
async fn test_delete_expected_machine_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_machine_request = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        id: None,
    };

    let err = env
        .api
        .delete_expected_machine(tonic::Request::new(expected_machine_request))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        CarbideError::NotFoundError {
            kind: "expected_machine",
            id: bmc_mac_address.to_string(),
        }
        .to_string()
    );
}

#[crate::sqlx_test]
async fn test_update_expected_machine(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;

    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    for mut updated_machine in [
        rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            chassis_serial_number: "VVG121GI".into(),
            metadata: None,
            default_pause_ingestion_and_poweron: Some(true),
            is_dpf_enabled: Some(false),
            ..Default::default()
        },
        rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            chassis_serial_number: "VVG121GJ".into(),
            metadata: Some(Default::default()),
            default_pause_ingestion_and_poweron: Some(false),
            is_dpf_enabled: Some(false),
            ..Default::default()
        },
        rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE1".into(),
            bmc_password: "PASS_UPDATE1".into(),
            chassis_serial_number: "VVG121GN".into(),
            metadata: Some(rpc::forge::Metadata {
                name: "a".to_string(),
                description: "desc".to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: "k1".to_string(),
                        value: None,
                    },
                    rpc::forge::Label {
                        key: "k2".to_string(),
                        value: Some("v2".to_string()),
                    },
                ],
            }),
            default_pause_ingestion_and_poweron: None,
            is_dpf_enabled: Some(false),
            ..Default::default()
        },
    ] {
        // ensure MAC-based update; id is ignored by update path
        updated_machine.id = None;
        env.api
            .update_expected_machine(tonic::Request::new(updated_machine.clone()))
            .await
            .expect("unable to update expected machine ")
            .into_inner();

        let mut retrieved_expected_machine = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: bmc_mac_address.to_string(),
                id: None,
            }))
            .await
            .expect("unable to fetch expected machine ")
            .into_inner();
        retrieved_expected_machine
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        // Ignore id field in comparison; MAC-based update path doesn't care about id
        retrieved_expected_machine.id = None;
        if updated_machine.metadata.is_none() {
            updated_machine.metadata = Some(Default::default());
        }

        if updated_machine
            .default_pause_ingestion_and_poweron
            .is_none()
        {
            updated_machine.default_pause_ingestion_and_poweron = Some(false);
        }

        assert_eq!(retrieved_expected_machine, updated_machine);
    }
}

#[crate::sqlx_test()]
async fn test_update_expected_machine_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN_UPDATE".into(),
        bmc_password: "PASS_UPDATE".into(),
        chassis_serial_number: "VVG121GI".into(),
        ..Default::default()
    };

    let err = env
        .api
        .update_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        CarbideError::NotFoundError {
            kind: "expected_machine",
            id: bmc_mac_address.to_string(),
        }
        .to_string()
    );
}

#[crate::sqlx_test]
async fn test_delete_all_expected_machines(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;
    let mut expected_machine_count = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines
        .len();

    assert_eq!(expected_machine_count, 6);

    env.api
        .delete_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner();

    expected_machine_count = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines
        .len();

    assert_eq!(expected_machine_count, 0);
}

#[crate::sqlx_test]
async fn test_replace_all_expected_machines(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;
    let expected_machine_count = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines
        .len();

    assert_eq!(expected_machine_count, 6);

    let mut expected_machine_list = ExpectedMachineList {
        expected_machines: Vec::new(),
    };

    let expected_machine_1 = rpc::forge::ExpectedMachine {
        bmc_mac_address: "4A:4B:4C:4D:4E:4F".into(),
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        chassis_serial_number: "SERIAL_NEW".into(),
        metadata: Some(rpc::Metadata::default()),
        default_pause_ingestion_and_poweron: Some(true),
        is_dpf_enabled: Some(false),
        ..Default::default()
    };

    let expected_machine_2 = rpc::forge::ExpectedMachine {
        bmc_mac_address: "5A:5B:5C:5D:5E:5F".into(),
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        chassis_serial_number: "SERIAL_NEW".into(),
        metadata: Some(rpc::Metadata::default()),
        default_pause_ingestion_and_poweron: Some(false),
        is_dpf_enabled: Some(false),
        ..Default::default()
    };

    let expected_machine_3 = rpc::forge::ExpectedMachine {
        bmc_mac_address: "6A:6B:6C:6D:6E:6F".into(),
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        chassis_serial_number: "SERIAL_NEW".into(),
        metadata: Some(rpc::Metadata::default()),
        default_pause_ingestion_and_poweron: None,
        is_dpf_enabled: Some(false),
        ..Default::default()
    };

    expected_machine_list
        .expected_machines
        .push(expected_machine_1.clone());
    expected_machine_list
        .expected_machines
        .push(expected_machine_2.clone());
    expected_machine_list
        .expected_machines
        .push(expected_machine_3.clone());

    env.api
        .replace_all_expected_machines(tonic::Request::new(expected_machine_list))
        .await
        .expect("unable to get all expected machines")
        .into_inner();

    let mut expected_machines = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("unable to get all expected machines")
        .into_inner()
        .expected_machines;
    expected_machines.sort_by_key(|e| e.bmc_mac_address.clone());

    assert_eq!(expected_machines.len(), 3);
    let mut resulting_machine_1 = expected_machines[0].clone();
    resulting_machine_1.id = None;
    let mut resulting_machine_2 = expected_machines[1].clone();
    resulting_machine_2.id = None;
    let mut resulting_machine_3 = expected_machines[2].clone();
    resulting_machine_3.id = None;

    // None will become Some(false), so we have to make the adjustment
    let mut expected_machine_3_clone = expected_machine_3.clone();
    expected_machine_3_clone.default_pause_ingestion_and_poweron = Some(false);
    assert_eq!(expected_machine_1, resulting_machine_1);
    assert_eq!(expected_machine_2, resulting_machine_2);
    assert_eq!(expected_machine_3_clone, resulting_machine_3);
}

#[crate::sqlx_test]
async fn test_replace_all_prevalidates_batch_before_clearing_existing_machines(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    fn replacement(
        bmc_mac_address: &str,
        chassis_serial_number: &str,
    ) -> rpc::forge::ExpectedMachine {
        rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.into(),
            bmc_username: "ADMIN_NEW".into(),
            bmc_password: "PASS_NEW".into(),
            chassis_serial_number: chassis_serial_number.into(),
            ..Default::default()
        }
    }

    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;
    let mut original = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await?
        .into_inner()
        .expected_machines;
    original.sort_by_key(|machine| machine.bmc_mac_address.clone());

    let mut invalid_policy = replacement("6A:6B:6C:6D:6E:71", "SERIAL-FIXED");
    invalid_policy.host_nics = vec![rpc::forge::ExpectedInterface {
        mac_address: "6A:6B:6C:6D:6E:72".into(),
        ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
        ..Default::default()
    }];

    let duplicate_id = Uuid::new_v4().to_string();
    let mut duplicate_id_a = replacement("6A:6B:6C:6D:6E:74", "SERIAL-DUP-ID-A");
    duplicate_id_a.id = Some(::rpc::common::Uuid {
        value: duplicate_id.clone(),
    });
    let mut duplicate_id_b = replacement("6A:6B:6C:6D:6E:75", "SERIAL-DUP-ID-B");
    duplicate_id_b.id = Some(::rpc::common::Uuid {
        value: duplicate_id,
    });

    for (case, replacements, expected_error) in [
        (
            "invalid interface policy",
            vec![
                replacement("6A:6B:6C:6D:6E:70", "SERIAL-VALID"),
                invalid_policy,
            ],
            "ip_allocation=fixed requires fixed_ip",
        ),
        (
            "duplicate BMC MAC",
            vec![
                replacement("6A:6B:6C:6D:6E:73", "SERIAL-DUP-MAC-A"),
                replacement("6A:6B:6C:6D:6E:73", "SERIAL-DUP-MAC-B"),
            ],
            "duplicate expected machine BMC MAC address",
        ),
        (
            "duplicate id",
            vec![duplicate_id_a, duplicate_id_b],
            "duplicate expected machine id",
        ),
    ] {
        let error = env
            .api
            .replace_all_expected_machines(tonic::Request::new(ExpectedMachineList {
                expected_machines: replacements,
            }))
            .await
            .expect_err("invalid replacement should be rejected");
        assert_eq!(error.code(), tonic::Code::InvalidArgument, "case: {case}");
        assert!(
            error.message().contains(expected_error),
            "case: {case}; unexpected rejection reason: {}",
            error.message(),
        );

        let mut remaining = env
            .api
            .get_all_expected_machines(tonic::Request::new(()))
            .await?
            .into_inner()
            .expected_machines;
        remaining.sort_by_key(|machine| machine.bmc_mac_address.clone());
        assert_eq!(remaining, original, "case: {case}");
    }

    Ok(())
}

#[crate::sqlx_test()]
async fn test_get_expected_machine_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_machine_query = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        id: None,
    };

    let err = env
        .api
        .get_expected_machine(tonic::Request::new(expected_machine_query))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        CarbideError::NotFoundError {
            kind: "expected_machine",
            id: bmc_mac_address.to_string(),
        }
        .to_string()
    );
}

#[crate::sqlx_test]
async fn test_get_linked_expected_machines_unseen(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;
    let out = env
        .api
        .get_all_expected_machines_linked(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.expected_machines.len(), 6);
    // They are sorted by MAC server-side
    let em = out.expected_machines.first().unwrap();
    assert_eq!(em.chassis_serial_number, "VVG121GG");
    assert!(
        em.interface_id.is_none(),
        "expected_machines fixture should have no linked interface"
    );
    assert!(
        em.explored_endpoint_address.is_none(),
        "expected_machines fixture should have no linked explored endpoint"
    );
    assert!(
        em.machine_id.is_none(),
        "expected_machines fixture should have no machine"
    );
    assert!(
        em.expected_machine_id.is_some(),
        "expected_machine_id should be populated from the expected_machines table"
    );
}

#[crate::sqlx_test]
async fn test_get_linked_expected_machines_completed(pool: sqlx::PgPool) {
    // Prep the data

    let env = create_test_env(pool.clone()).await;
    let host_config = model::test_support::ManagedHostConfig::default();
    let bmc_mac = host_config.bmc_mac_address;

    let provided_id = Uuid::new_v4();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "GKTEST".into(),
        id: Some(::rpc::common::Uuid {
            value: provided_id.to_string(),
        }),
        ..Default::default()
    };
    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine");

    let (host_machine_id, _dpu_machine_id) =
        common::api_fixtures::create_managed_host_with_config(&env, host_config)
            .await
            .into();
    let host_machine = env.find_machine(&host_machine_id).await.remove(0);
    let bmc_ip = host_machine.bmc_info.as_ref().unwrap().ip();

    // The test

    let mut out = env
        .api
        .get_all_expected_machines_linked(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.expected_machines.len(), 1);

    let mut em = out.expected_machines.remove(0);
    assert_eq!(em.chassis_serial_number, "GKTEST");
    assert!(em.interface_id.is_some(), "interface not found");
    assert_eq!(
        em.explored_endpoint_address.take().unwrap(),
        bmc_ip,
        "BMC MAC should match"
    );
    assert_eq!(
        em.machine_id.take().unwrap().to_string(),
        host_machine_id.to_string(),
        "machine id should match via bmc_mac"
    );
    assert!(
        em.expected_machine_id.is_some(),
        "expected_machine_id should be populated"
    );
    assert_eq!(
        em.expected_machine_id.unwrap().value,
        provided_id.to_string(),
        "expected_machine_id should match the ID we provided"
    );
}

#[crate::sqlx_test()]
async fn test_add_expected_machine_dpu_serials(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "3A:3B:3C:3D:3E:3F".parse().unwrap();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "VVG121GI".into(),
        fallback_dpu_serial_numbers: vec!["dpu_serial1".to_string()],
        metadata: Some(rpc::Metadata::default()),
        sku_id: None,
        id: None,
        default_pause_ingestion_and_poweron: Some(true),
        host_nics: vec![],
        rack_id: None,
        is_dpf_enabled: Some(true),
        bmc_ip_address: None,
        bmc_retain_credentials: None,
        dpu_mode: None,
        bmc_ip_allocation: None,
        replace_host_nics: false,
        host_lifecycle_profile: None,
        #[allow(deprecated)]
        dpf_enabled: true,
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine ");

    let expected_machine_query = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        id: None,
    };

    let mut retrieved_expected_machine = env
        .api
        .get_expected_machine(tonic::Request::new(expected_machine_query))
        .await
        .expect("unable to retrieve expected machine ")
        .into_inner();
    // Zero id for equality test
    retrieved_expected_machine.id = None;
    assert_eq!(retrieved_expected_machine, expected_machine);
}

#[crate::sqlx_test()]
async fn test_add_and_update_expected_machine_with_invalid_metadata(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "3A:3B:3C:3D:3E:3F".parse().unwrap();
    // Start adding an expected-machine with invalid metadata
    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(false) {
        let expected_machine = rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "VVG121GI".into(),
            fallback_dpu_serial_numbers: vec![],
            metadata: Some(invalid_metadata.clone()),
            sku_id: None,
            id: None,
            default_pause_ingestion_and_poweron: None,
            host_nics: vec![],
            rack_id: None,
            is_dpf_enabled: Some(true),
            ..Default::default()
        };

        let err = env
            .api
            .add_expected_machine(tonic::Request::new(expected_machine.clone()))
            .await
            .expect_err(&format!(
                "Invalid metadata of type should not be accepted: {invalid_metadata:?}"
            ));
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains(&expected_err),
            "Testcase: {:?}\nMessage is \"{}\".\nMessage should contain: \"{}\"",
            invalid_metadata,
            err.message(),
            expected_err
        );
    }

    // Create one with valid metadata, and try to update it to invalid
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "VVG121GI".into(),
        fallback_dpu_serial_numbers: vec![],
        metadata: None,
        sku_id: None,
        id: None,
        default_pause_ingestion_and_poweron: None,
        host_nics: vec![],
        rack_id: None,
        is_dpf_enabled: Some(true),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("Expected addition to succeed");

    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(false) {
        let expected_machine = rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "VVG121GI".into(),
            fallback_dpu_serial_numbers: vec![],
            metadata: Some(invalid_metadata.clone()),
            sku_id: None,
            id: None,
            default_pause_ingestion_and_poweron: None,
            host_nics: vec![],
            rack_id: None,
            is_dpf_enabled: Some(true),
            ..Default::default()
        };

        let err = env
            .api
            .update_expected_machine(tonic::Request::new(expected_machine.clone()))
            .await
            .expect_err(&format!(
                "Invalid metadata of type should not be accepted: {invalid_metadata:?}"
            ));
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains(&expected_err),
            "Testcase: {:?}\nMessage is \"{}\".\nMessage should contain: \"{}\"",
            invalid_metadata,
            err.message(),
            expected_err
        );
    }
}

#[crate::sqlx_test()]
async fn test_add_expected_machine_duplicate_dpu_serials(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "3A:3B:3C:3D:3E:3F".parse().unwrap();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "VVG121GI".into(),
        fallback_dpu_serial_numbers: vec!["dpu_serial1".to_string(), "dpu_serial1".to_string()],
        metadata: None,
        sku_id: None,
        id: None,
        default_pause_ingestion_and_poweron: None,
        host_nics: vec![],
        rack_id: None,
        is_dpf_enabled: Some(true),
        ..Default::default()
    };

    assert!(
        env.api
            .add_expected_machine(tonic::Request::new(expected_machine.clone()))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_update_expected_machine_add_dpu_serial(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;

    let env = create_test_env(pool).await;

    let mut ee1 = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
            id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    ee1.fallback_dpu_serial_numbers = vec!["dpu_serial".to_string()];

    env.api
        .update_expected_machine(tonic::Request::new(ee1.clone()))
        .await
        .expect("unable to update")
        .into_inner();

    let ee2 = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
            id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    assert_eq!(ee1, ee2);
}
#[crate::sqlx_test]
async fn test_update_expected_machine_add_duplicate_dpu_serial(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;

    let mut ee1 = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
            id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    ee1.fallback_dpu_serial_numbers = vec![
        "dpu_serial1".to_string(),
        "dpu_serial2".to_string(),
        "dpu_serial1".to_string(),
    ];

    assert!(
        env.api
            .update_expected_machine(tonic::Request::new(ee1.clone()))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_update_expected_machine_add_sku(pool: sqlx::PgPool) {
    create_fixture_expected_machines(&pool).await;
    let env = create_test_env(pool).await;

    let mut ee1 = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
            id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    ee1.sku_id = Some("sku_id".to_string());

    env.api
        .update_expected_machine(tonic::Request::new(ee1.clone()))
        .await
        .expect("unable to update")
        .into_inner();

    let ee2 = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "2A:2B:2C:2D:2E:2F".into(),
            id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    assert_eq!(ee1, ee2);
}

#[crate::sqlx_test()]
async fn test_add_expected_machine_with_id_and_get_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let provided_id = Uuid::new_v4().to_string();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "AA:BB:CC:DD:EE:01".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "SERIAL-ID".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine with id");

    // Get by id
    let get_req = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(get_req))
        .await
        .expect("unable to retrieve by id")
        .into_inner();

    assert_eq!(
        retrieved.id,
        Some(::rpc::common::Uuid { value: provided_id })
    );
    assert_eq!(retrieved.bmc_mac_address, "AA:BB:CC:DD:EE:01");
}

#[crate::sqlx_test()]
async fn test_update_expected_machine_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // Create with id
    let provided_id = Uuid::new_v4().to_string();
    let mut expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "AA:BB:CC:DD:EE:02".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "SERIAL-1".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("add with id");

    // Update by id (change username)
    expected_machine.bmc_username = "ADMIN_UPDATED".into();
    env.api
        .update_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("update by id");

    // Fetch by id and verify
    let get_req = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(get_req))
        .await
        .expect("get after update by id")
        .into_inner();

    assert_eq!(
        retrieved.id,
        Some(::rpc::common::Uuid { value: provided_id })
    );
    assert_eq!(retrieved.bmc_username, "ADMIN_UPDATED");
}

#[crate::sqlx_test()]
async fn test_delete_expected_machine_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // Create with id
    let provided_id = Uuid::new_v4().to_string();
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "AA:BB:CC:DD:EE:03".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "SERIAL-DEL".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("add with id");

    // Delete by id
    let del_req = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    env.api
        .delete_expected_machine(tonic::Request::new(del_req))
        .await
        .expect("delete by id");

    // Verify NotFound by id
    let get_req = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let err = env
        .api
        .get_expected_machine(tonic::Request::new(get_req))
        .await
        .unwrap_err();
    assert_eq!(
        err.message().to_string(),
        CarbideError::NotFoundError {
            kind: "expected_machine",
            id: provided_id
        }
        .to_string()
    );
}

#[crate::sqlx_test()]
async fn test_batch_create_expected_machines_all_or_nothing_success(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();

    let request = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:01".to_string(),
                    bmc_username: "admin1".to_string(),
                    bmc_password: "pass1".to_string(),
                    chassis_serial_number: "SERIAL-001".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id2.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:02".to_string(),
                    bmc_username: "admin2".to_string(),
                    bmc_password: "pass2".to_string(),
                    chassis_serial_number: "SERIAL-002".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: false,
    };

    let response = env
        .api
        .create_expected_machines(tonic::Request::new(request))
        .await
        .expect("batch create should succeed");

    let results = response.into_inner().results;
    assert_eq!(results.len(), 2);
    assert!(results[0].success);
    assert!(results[1].success);

    // Verify both machines were created
    let get_req1 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id1.to_string(),
        }),
    };
    let machine1 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req1))
        .await
        .expect("should find machine 1");
    assert_eq!(machine1.into_inner().bmc_username, "admin1");

    let get_req2 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id2.to_string(),
        }),
    };
    let machine2 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req2))
        .await
        .expect("should find machine 2");
    assert_eq!(machine2.into_inner().bmc_username, "admin2");
}

#[crate::sqlx_test()]
async fn test_batch_create_expected_machines_all_or_nothing_failure(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();

    let request = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:03".to_string(),
                    bmc_username: "admin1".to_string(),
                    bmc_password: "pass1".to_string(),
                    chassis_serial_number: "SERIAL-003".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id2.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:03".to_string(), // Duplicate MAC
                    bmc_username: "admin2".to_string(),
                    bmc_password: "pass2".to_string(),
                    chassis_serial_number: "SERIAL-004".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: false,
    };

    let result = env
        .api
        .create_expected_machines(tonic::Request::new(request))
        .await;

    // Should fail due to duplicate MAC
    assert!(result.is_err());

    // Verify neither machine was created (transaction rollback)
    let get_req1 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id1.to_string(),
        }),
    };
    let result1 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req1))
        .await;
    assert!(result1.is_err());
}

#[crate::sqlx_test()]
async fn test_batch_create_expected_machines_partial_results(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();

    let request = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:05".to_string(),
                    bmc_username: "admin1".to_string(),
                    bmc_password: "pass1".to_string(),
                    chassis_serial_number: "SERIAL-005".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id2.to_string(),
                    }),
                    bmc_mac_address: "INVALID-MAC".to_string(), // Invalid MAC
                    bmc_username: "admin2".to_string(),
                    bmc_password: "pass2".to_string(),
                    chassis_serial_number: "SERIAL-006".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id3.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:07".to_string(),
                    bmc_username: "admin3".to_string(),
                    bmc_password: "pass3".to_string(),
                    chassis_serial_number: "SERIAL-007".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: true,
    };

    let response = env
        .api
        .create_expected_machines(tonic::Request::new(request))
        .await
        .expect("batch create should succeed with partial results");

    let results = response.into_inner().results;
    assert_eq!(results.len(), 3);
    assert!(results[0].success, "First machine should succeed");
    assert!(!results[1].success, "Second machine should fail");
    assert!(results[2].success, "Third machine should succeed");

    // Verify first machine was created
    let get_req1 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id1.to_string(),
        }),
    };
    let machine1 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req1))
        .await
        .expect("should find machine 1");
    assert_eq!(machine1.into_inner().bmc_username, "admin1");

    // Verify second machine was NOT created
    let get_req2 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id2.to_string(),
        }),
    };
    let result2 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req2))
        .await;
    assert!(result2.is_err());

    // Verify third machine was created
    let get_req3 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id3.to_string(),
        }),
    };
    let machine3 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req3))
        .await
        .expect("should find machine 3");
    assert_eq!(machine3.into_inner().bmc_username, "admin3");
}

#[crate::sqlx_test()]
async fn test_batch_create_missing_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let request = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![rpc::forge::ExpectedMachine {
                id: None, // Missing ID
                bmc_mac_address: "AA:BB:CC:DD:EE:08".to_string(),
                bmc_username: "admin".to_string(),
                bmc_password: "pass".to_string(),
                chassis_serial_number: "SERIAL-008".to_string(),
                metadata: Some(rpc::forge::Metadata::default()),
                ..Default::default()
            }],
        }),
        accept_partial_results: false,
    };

    let result = env
        .api
        .create_expected_machines(tonic::Request::new(request))
        .await;

    assert!(result.is_err(), "Should fail when id is missing");
}

#[crate::sqlx_test()]
async fn test_batch_update_expected_machines_all_or_nothing_failure(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();

    // Create initial machines
    let create_req = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id1.to_string(),
                }),
                bmc_mac_address: "AA:BB:CC:DD:EE:12".to_string(),
                bmc_username: "admin1".to_string(),
                bmc_password: "pass1".to_string(),
                chassis_serial_number: "SERIAL-012".to_string(),
                metadata: Some(rpc::forge::Metadata::default()),
                ..Default::default()
            }],
        }),
        accept_partial_results: false,
    };

    env.api
        .create_expected_machines(tonic::Request::new(create_req))
        .await
        .expect("create should succeed");

    // Try to update with one valid and one invalid (non-existent id)
    let update_req = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:12".to_string(),
                    bmc_username: "admin1_updated".to_string(),
                    bmc_password: "pass1_updated".to_string(),
                    chassis_serial_number: "SERIAL-012".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id2.to_string(), // Non-existent ID
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:13".to_string(),
                    bmc_username: "admin2".to_string(),
                    bmc_password: "pass2".to_string(),
                    chassis_serial_number: "SERIAL-013".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: false,
    };

    let result = env
        .api
        .update_expected_machines(tonic::Request::new(update_req))
        .await;

    // Should fail
    assert!(result.is_err());

    // Verify first machine was NOT updated (transaction rollback)
    let get_req1 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id1.to_string(),
        }),
    };
    let machine1 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req1))
        .await
        .expect("should find machine 1");
    assert_eq!(
        machine1.into_inner().bmc_username,
        "admin1",
        "Should still have original username due to rollback"
    );
}

#[crate::sqlx_test()]
async fn test_batch_update_expected_machines_partial_results(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();

    // Create initial machines
    let create_req = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:14".to_string(),
                    bmc_username: "admin1".to_string(),
                    bmc_password: "pass1".to_string(),
                    chassis_serial_number: "SERIAL-014".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id3.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:16".to_string(),
                    bmc_username: "admin3".to_string(),
                    bmc_password: "pass3".to_string(),
                    chassis_serial_number: "SERIAL-016".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: false,
    };

    env.api
        .create_expected_machines(tonic::Request::new(create_req))
        .await
        .expect("create should succeed");

    // Try to update with partial results
    let update_req = rpc::forge::BatchExpectedMachineOperationRequest {
        expected_machines: Some(rpc::forge::ExpectedMachineList {
            expected_machines: vec![
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id1.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:14".to_string(),
                    bmc_username: "admin1_updated".to_string(),
                    bmc_password: "pass1_updated".to_string(),
                    chassis_serial_number: "SERIAL-014".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id2.to_string(), // Non-existent ID
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:15".to_string(),
                    bmc_username: "admin2".to_string(),
                    bmc_password: "pass2".to_string(),
                    chassis_serial_number: "SERIAL-015".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
                rpc::forge::ExpectedMachine {
                    id: Some(::rpc::common::Uuid {
                        value: id3.to_string(),
                    }),
                    bmc_mac_address: "AA:BB:CC:DD:EE:16".to_string(),
                    bmc_username: "admin3_updated".to_string(),
                    bmc_password: "pass3_updated".to_string(),
                    chassis_serial_number: "SERIAL-016".to_string(),
                    metadata: Some(rpc::forge::Metadata::default()),
                    ..Default::default()
                },
            ],
        }),
        accept_partial_results: true,
    };

    let response = env
        .api
        .update_expected_machines(tonic::Request::new(update_req))
        .await
        .expect("batch update should succeed with partial results");

    let results = response.into_inner().results;
    assert_eq!(results.len(), 3);
    assert!(results[0].success, "First update should succeed");
    assert!(!results[1].success, "Second update should fail");
    assert!(results[2].success, "Third update should succeed");

    // Verify first machine was updated
    let get_req1 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id1.to_string(),
        }),
    };
    let machine1 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req1))
        .await
        .expect("should find machine 1");
    assert_eq!(machine1.into_inner().bmc_username, "admin1_updated");

    // Verify second machine does not exist
    let get_req2 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id2.to_string(),
        }),
    };
    let result2 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req2))
        .await;
    assert!(result2.is_err());

    // Verify third machine was updated
    let get_req3 = rpc::forge::ExpectedMachineRequest {
        bmc_mac_address: "".to_string(),
        id: Some(::rpc::common::Uuid {
            value: id3.to_string(),
        }),
    };
    let machine3 = env
        .api
        .get_expected_machine(tonic::Request::new(get_req3))
        .await
        .expect("should find machine 3");
    assert_eq!(machine3.into_inner().bmc_username, "admin3_updated");
}

/// Wait until an expected-machine writer is blocked by the test transaction.
async fn wait_until_expected_machine_write_is_blocked(pool: &sqlx::PgPool, blocker_pid: i32) {
    for _ in 0..300 {
        let blocked: bool = sqlx::query_scalar(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM pg_stat_activity AS activity
                    WHERE activity.datname = current_database()
                      AND activity.wait_event_type = 'Lock'
                      AND $1 = ANY(pg_blocking_pids(activity.pid))
                      AND activity.query ILIKE '%' || $2 || '%'
                )
            "#,
        )
        .bind(blocker_pid)
        .bind("expected_machines")
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    panic!("expected-machine write never waited for the database lock");
}

#[crate::sqlx_test]
async fn test_concurrent_older_client_update_preserves_interface_fields(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    for (case, suffix, use_batch) in [
        ("single update by MAC", 0x78, false),
        ("batch update by ID", 0x7a, true),
    ] {
        let id = Uuid::new_v4();
        let bmc_mac_address = format!("7A:7B:7C:7D:81:{suffix:02X}");
        let interface_mac_address = format!("7A:7B:7C:7D:81:{:02X}", suffix + 1);
        let chassis_serial_number = format!("EM-LOCK-{suffix:02X}");

        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac_address.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: chassis_serial_number.clone(),
                host_nics: vec![rpc::forge::ExpectedInterface {
                    mac_address: interface_mac_address.clone(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .await?;

        let mut blocker = env.pool.begin().await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let mut newer = db::expected_machine::find_by_id(&mut *blocker, id)
            .await?
            .expect("expected machine should exist");
        newer.data.interfaces[0].role = ExpectedInterfaceRole::DpuBmc;
        newer.data.interfaces[0].ip_allocation = Some(ExpectedInterfaceIpAllocation::Retained);
        db::expected_machine::update(&mut blocker, &newer).await?;

        let update = rpc::forge::ExpectedMachine {
            id: use_batch.then(|| ::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac_address.clone(),
            bmc_username: "UPDATED_ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number,
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: interface_mac_address,
                ..Default::default()
            }],
            ..Default::default()
        };
        let api = env.api.clone();
        let update_task = tokio::spawn(async move {
            if use_batch {
                let response = api
                    .update_expected_machines(tonic::Request::new(
                        rpc::forge::BatchExpectedMachineOperationRequest {
                            expected_machines: Some(ExpectedMachineList {
                                expected_machines: vec![update],
                            }),
                            accept_partial_results: false,
                        },
                    ))
                    .await?
                    .into_inner();
                assert!(
                    response.results[0].success,
                    "batch update failed: {:?}",
                    response.results[0].error_message
                );
            } else {
                api.update_expected_machine(tonic::Request::new(update))
                    .await?;
            }
            Ok::<(), tonic::Status>(())
        });

        wait_until_expected_machine_write_is_blocked(&env.pool, blocker_pid).await;
        blocker.commit().await?;
        let update_result =
            tokio::time::timeout(std::time::Duration::from_secs(10), update_task).await?;
        update_result??;

        let stored = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: String::new(),
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
            }))
            .await?
            .into_inner();
        assert_eq!(stored.bmc_username, "UPDATED_ADMIN", "case: {case}");
        assert_eq!(
            stored.host_nics[0].role,
            Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
            "case: {case}"
        );
        assert_eq!(
            stored.host_nics[0].ip_allocation,
            Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
            "case: {case}"
        );
    }

    Ok(())
}

/// Replace-all must preserve the latest interface fields when an older client
/// races with a single-machine update.
#[crate::sqlx_test]
async fn test_concurrent_replace_all_preserves_latest_interface_fields(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let bmc_mac_address = "7A:7B:7C:7D:81:7C".to_string();
    let interface_mac_address = "7A:7B:7C:7D:81:7D".to_string();
    let chassis_serial_number = "EM-REPLACE-LOCK-7C".to_string();

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac_address.clone(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: chassis_serial_number.clone(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: interface_mac_address.clone(),
                role: Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let mut newer = db::expected_machine::find_for_update(
        &mut blocker,
        &model::expected_machine::ExpectedMachineRequest {
            id: Some(id),
            bmc_mac_address: None,
        },
    )
    .await?
    .expect("expected machine should exist");
    newer.data.interfaces[0].role = ExpectedInterfaceRole::DpuBmc;
    newer.data.interfaces[0].ip_allocation = Some(ExpectedInterfaceIpAllocation::Retained);

    let replacement = rpc::forge::ExpectedMachine {
        id: Some(::rpc::common::Uuid {
            value: id.to_string(),
        }),
        bmc_mac_address: bmc_mac_address.clone(),
        bmc_username: "OLDER_CLIENT".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number,
        host_nics: vec![rpc::forge::ExpectedInterface {
            mac_address: interface_mac_address,
            ..Default::default()
        }],
        ..Default::default()
    };
    let api = env.api.clone();
    let replace_task = tokio::spawn(async move {
        api.replace_all_expected_machines(tonic::Request::new(ExpectedMachineList {
            expected_machines: vec![replacement],
        }))
        .await?;
        Ok::<(), tonic::Status>(())
    });

    wait_until_expected_machine_write_is_blocked(&env.pool, blocker_pid).await;
    db::expected_machine::update(&mut blocker, &newer).await?;
    blocker.commit().await?;
    let replace_result =
        tokio::time::timeout(std::time::Duration::from_secs(10), replace_task).await?;
    replace_result??;

    let stored = env
        .api
        .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
            bmc_mac_address: String::new(),
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
        }))
        .await?
        .into_inner();
    assert_eq!(stored.bmc_username, "OLDER_CLIENT");
    assert_eq!(
        stored.host_nics[0].role,
        Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
    );
    assert_eq!(
        stored.host_nics[0].ip_allocation,
        Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_replace_all_preserves_interface_fields_omitted_by_older_client(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    struct Case {
        scenario: &'static str,
        suffix: u8,
        role: Option<i32>,
        allocation: Option<i32>,
        omit_id: bool,
        expected_role: Option<i32>,
        expected_allocation: Option<i32>,
    }

    for Case {
        scenario,
        suffix,
        role,
        allocation,
        omit_id,
        expected_role,
        expected_allocation,
    } in [
        Case {
            scenario: "omitted fields and id",
            suffix: 0x72,
            role: None,
            allocation: None,
            omit_id: true,
            expected_role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
            expected_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
        },
        Case {
            scenario: "Unspecified fields",
            suffix: 0x74,
            role: Some(rpc::forge::ExpectedInterfaceRole::Unspecified as i32),
            allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Unspecified as i32),
            omit_id: false,
            expected_role: None,
            expected_allocation: None,
        },
    ] {
        let id = Uuid::new_v4();
        let bmc_mac = format!("7A:7B:7C:7D:80:{suffix:02X}");
        let interface_mac = format!("7A:7B:7C:7D:80:{:02X}", suffix + 1);
        let serial = format!("EM-REPLACE-COMPAT-{suffix:02X}");
        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                host_nics: vec![rpc::forge::ExpectedInterface {
                    mac_address: interface_mac.clone(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .await?;

        env.api
            .replace_all_expected_machines(tonic::Request::new(ExpectedMachineList {
                expected_machines: vec![rpc::forge::ExpectedMachine {
                    id: (!omit_id).then(|| ::rpc::common::Uuid {
                        value: id.to_string(),
                    }),
                    bmc_mac_address: bmc_mac.clone(),
                    bmc_username: "UPDATED".into(),
                    bmc_password: "PASS".into(),
                    chassis_serial_number: serial,
                    host_nics: vec![rpc::forge::ExpectedInterface {
                        mac_address: interface_mac,
                        role,
                        ip_allocation: allocation,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }))
            .await?;

        let stored = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: if omit_id { bmc_mac } else { String::new() },
                id: (!omit_id).then(|| ::rpc::common::Uuid {
                    value: id.to_string(),
                }),
            }))
            .await?
            .into_inner();
        assert_eq!(stored.bmc_username, "UPDATED", "case: {scenario}");
        assert_eq!(stored.host_nics[0].role, expected_role, "case: {scenario}",);
        assert_eq!(
            stored.host_nics[0].ip_allocation, expected_allocation,
            "case: {scenario}",
        );
    }

    Ok(())
}

/// The stable protobuf replacement marker makes an interface list
/// authoritative, so an empty list or one without HostBmc removes nested-only
/// BMC settings. Older writers leave the marker false and retain the
/// compatibility behavior covered by
/// `test_replace_all_distinguishes_legacy_and_nested_host_bmc_omission`.
#[crate::sqlx_test]
async fn test_authoritative_interface_replacement_removes_nested_host_bmc(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    enum Operation {
        SingleUpdate,
        BatchUpdate,
        ReplaceAll,
    }

    for (scenario, suffix, operation, keep_host_interface) in [
        (
            "single update with an empty list",
            0x88,
            Operation::SingleUpdate,
            false,
        ),
        (
            "single update with a Host-only list",
            0x8a,
            Operation::SingleUpdate,
            true,
        ),
        (
            "batch update with an empty list",
            0x8c,
            Operation::BatchUpdate,
            false,
        ),
        (
            "batch update with a Host-only list",
            0x8e,
            Operation::BatchUpdate,
            true,
        ),
        (
            "replace-all with an empty list",
            0x90,
            Operation::ReplaceAll,
            false,
        ),
        (
            "replace-all with a Host-only list",
            0x92,
            Operation::ReplaceAll,
            true,
        ),
    ] {
        let id = Uuid::new_v4();
        let bmc_mac = format!("7A:7B:7C:7D:82:{suffix:02X}");
        let host_mac = format!("7A:7B:7C:7D:82:{:02X}", suffix + 1);
        let serial = format!("EM-AUTHORITATIVE-INTERFACES-{suffix:02X}");

        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                host_nics: vec![rpc::forge::ExpectedInterface {
                    mac_address: bmc_mac.clone(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .await?;

        let interfaces = keep_host_interface
            .then(|| rpc::forge::ExpectedInterface {
                mac_address: host_mac.clone(),
                role: Some(rpc::forge::ExpectedInterfaceRole::Host as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                ..Default::default()
            })
            .into_iter()
            .collect();
        let replacement = rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac.clone(),
            bmc_username: "UPDATED".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: serial,
            host_nics: interfaces,
            bmc_ip_allocation: Some(rpc::forge::BmcIpAllocationType::Retained as i32),
            replace_host_nics: true,
            ..Default::default()
        };

        match operation {
            Operation::SingleUpdate => {
                env.api
                    .update_expected_machine(tonic::Request::new(replacement))
                    .await?;
            }
            Operation::BatchUpdate => {
                let response = env
                    .api
                    .update_expected_machines(tonic::Request::new(
                        rpc::forge::BatchExpectedMachineOperationRequest {
                            expected_machines: Some(ExpectedMachineList {
                                expected_machines: vec![replacement],
                            }),
                            accept_partial_results: false,
                        },
                    ))
                    .await?
                    .into_inner();
                assert!(response.results[0].success, "case: {scenario}");
            }
            Operation::ReplaceAll => {
                env.api
                    .replace_all_expected_machines(tonic::Request::new(ExpectedMachineList {
                        expected_machines: vec![replacement],
                    }))
                    .await?;
            }
        }

        let stored = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: String::new(),
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
            }))
            .await?
            .into_inner();
        assert!(
            stored.interfaces().iter().all(|interface| {
                interface.role != Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32)
            }),
            "case {scenario}: the nested HostBmc should be removed",
        );
        assert_eq!(
            stored.interfaces().len(),
            usize::from(keep_host_interface),
            "case: {scenario}",
        );
        assert_eq!(
            stored.bmc_ip_allocation,
            Some(rpc::forge::BmcIpAllocationType::Retained as i32),
            "case {scenario}: compatibility allocation should remain available",
        );
    }

    Ok(())
}

// test_patch_dpf_enabled_none_to_true verifies that when an expected machine is
// added with is_dpf_enabled: None, the value defaults to true on insert, and a
// subsequent update with is_dpf_enabled: None preserves that value.
#[crate::sqlx_test()]
async fn test_patch_dpf_enabled_none_to_true(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address = "AA:BB:CC:DD:EE:F0";

    // Create machine with dpf_enabled = null (is_dpf_enabled: None)
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "SN-DPF-NULL".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            is_dpf_enabled: None,
            ..Default::default()
        }))
        .await
        .expect("unable to add expected machine");

    // Patch (update) with is_dpf_enabled: None — should keep dpf_enabled as NULL
    let mut updated = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            id: None,
        }))
        .await
        .expect("unable to fetch expected machine")
        .into_inner();

    // default should be true
    assert_eq!(updated.is_dpf_enabled, Some(true),);

    updated.id = None;
    updated.bmc_username = "ADMIN_PATCHED".into();
    updated.is_dpf_enabled = None;

    env.api
        .update_expected_machine(tonic::Request::new(updated))
        .await
        .expect("unable to update expected machine");

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            id: None,
        }))
        .await
        .expect("unable to fetch expected machine after update")
        .into_inner();

    assert_eq!(retrieved.is_dpf_enabled, Some(true),);
}

// test_patch_dpf_enabled_true_stays_true_when_patched_with_null verifies that when
// dpf_enabled is true in the DB and an update is applied with is_dpf_enabled: None,
// the value remains true (not overwritten to NULL).
#[crate::sqlx_test()]
async fn test_patch_dpf_enabled_true_stays_true_when_patched_with_null(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address = "AA:BB:CC:DD:EE:F1";

    // Create machine with dpf_enabled = true
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "SN-DPF-TRUE".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            is_dpf_enabled: Some(true),
            ..Default::default()
        }))
        .await
        .expect("unable to add expected machine");

    // Patch (update) with is_dpf_enabled: None — should preserve dpf_enabled = true
    let mut updated = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            id: None,
        }))
        .await
        .expect("unable to fetch expected machine")
        .into_inner();

    assert_eq!(updated.is_dpf_enabled, Some(true),);

    updated.id = None;
    updated.bmc_username = "ADMIN_PATCHED".into();
    updated.is_dpf_enabled = None;

    env.api
        .update_expected_machine(tonic::Request::new(updated))
        .await
        .expect("unable to update expected machine");

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            id: None,
        }))
        .await
        .expect("unable to fetch expected machine after update")
        .into_inner();

    assert_eq!(retrieved.is_dpf_enabled, Some(true),);
}

// --- Optional `ExpectedMachine.bmc_ip_address`: persists configured BMC IP and exercises API
// pre-allocation (`preallocate_machine_interface` / `update_preallocated_machine_interface`). ---
#[crate::sqlx_test()]
async fn test_add_expected_machine_with_static_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "5A:5B:5C:5D:5E:60".to_string(),
        bmc_username: "root".into(),
        bmc_password: "testpass".into(),
        chassis_serial_number: "STATIC-IP-TEST".into(),
        bmc_ip_address: Some("10.0.0.100".to_string()),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: uuid::Uuid::new_v4().to_string(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine with static IP");

    let retrieved_machine = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "5A:5B:5C:5D:5E:60".to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine")
        .into_inner();

    assert_eq!(
        retrieved_machine.bmc_ip_address,
        Some("10.0.0.100".to_string())
    );
    assert_eq!(retrieved_machine.bmc_username, "root");
}

#[crate::sqlx_test()]
async fn test_update_expected_machine_add_static_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // Create machine without static IP
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "5A:5B:5C:5D:5E:62".to_string(),
        bmc_username: "root".into(),
        bmc_password: "testpass".into(),
        chassis_serial_number: "UPDATE-STATIC-IP".into(),
        bmc_ip_address: None,
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: uuid::Uuid::new_v4().to_string(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine");

    // Update to add static IP
    let mut updated_machine = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "5A:5B:5C:5D:5E:62".to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine")
        .into_inner();

    updated_machine.id = None;
    updated_machine.bmc_ip_address = Some("192.168.1.50".to_string());

    env.api
        .update_expected_machine(tonic::Request::new(updated_machine.clone()))
        .await
        .expect("unable to update expected machine with static IP");

    let retrieved_machine = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "5A:5B:5C:5D:5E:62".to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine after update")
        .into_inner();

    assert_eq!(
        retrieved_machine.bmc_ip_address,
        Some("192.168.1.50".to_string())
    );
}

/// Single and batch updates use the same fixed expected-interface
/// reconciliation, including the role-derived interface settings.
#[crate::sqlx_test]
async fn test_expected_machine_update_fixed_interface_single_batch_parity(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    for (case, suffix, fixed_ip, use_batch) in [
        ("single update", 0x70, "192.0.2.220", false),
        ("batch update", 0x72, "192.0.2.221", true),
    ] {
        let fixed_ip: std::net::IpAddr = fixed_ip.parse()?;
        let id = Uuid::new_v4();
        let bmc_mac: MacAddress = format!("5A:5B:5C:5D:60:{suffix:02X}").parse()?;
        let interface_mac: MacAddress = format!("5A:5B:5C:5D:60:{:02X}", suffix + 1).parse()?;
        let serial = format!("FIXED-INTERFACE-{suffix:02X}");

        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac.to_string(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                ..Default::default()
            }))
            .await?;

        let update = rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: serial,
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: interface_mac.to_string(),
                role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
                fixed_ip: Some(fixed_ip.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        if use_batch {
            let response = env
                .api
                .update_expected_machines(tonic::Request::new(
                    rpc::forge::BatchExpectedMachineOperationRequest {
                        expected_machines: Some(ExpectedMachineList {
                            expected_machines: vec![update],
                        }),
                        accept_partial_results: false,
                    },
                ))
                .await?
                .into_inner();
            assert!(response.results[0].success, "case: {case}");
        } else {
            env.api
                .update_expected_machine(tonic::Request::new(update))
                .await?;
        }

        let mut txn = env.pool.begin().await?;
        let interfaces =
            db::machine_interface::find_by_mac_address(&mut *txn, interface_mac).await?;
        assert_eq!(interfaces.len(), 1, "case: {case}");
        assert_eq!(
            interfaces[0].interface_type,
            model::machine_interface::InterfaceType::Bmc,
            "case: {case}",
        );
        assert!(!interfaces[0].primary_interface, "case: {case}");
        assert_eq!(interfaces[0].addresses, vec![fixed_ip], "case: {case}",);

        let addresses =
            db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
        assert_eq!(addresses.len(), 1, "case: {case}");
        assert_eq!(
            addresses[0].allocation_type,
            model::allocation_type::AllocationType::Static,
            "case: {case}",
        );
        txn.rollback().await?;
    }

    Ok(())
}

/// Explicit fixed policies require a managed prefix in both update APIs, while
/// legacy Host entries retain their `static-assignments` fallback.
#[crate::sqlx_test]
async fn test_expected_machine_update_fixed_interface_requires_managed_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    /// One update API and allocation-policy combination.
    struct Case {
        name: &'static str,
        suffix: u8,
        fixed_ip: &'static str,
        use_batch: bool,
        explicit_policy: bool,
    }

    for case in [
        Case {
            name: "single legacy Host update",
            suffix: 0x74,
            fixed_ip: "203.0.113.240",
            use_batch: false,
            explicit_policy: false,
        },
        Case {
            name: "batch legacy Host update",
            suffix: 0x76,
            fixed_ip: "203.0.113.241",
            use_batch: true,
            explicit_policy: false,
        },
        Case {
            name: "single explicit Fixed update",
            suffix: 0x78,
            fixed_ip: "203.0.113.242",
            use_batch: false,
            explicit_policy: true,
        },
        Case {
            name: "batch explicit Fixed update",
            suffix: 0x7a,
            fixed_ip: "203.0.113.243",
            use_batch: true,
            explicit_policy: true,
        },
    ] {
        let fixed_ip: std::net::IpAddr = case.fixed_ip.parse()?;
        let id = Uuid::new_v4();
        let bmc_mac: MacAddress = format!("5A:5B:5C:5D:61:{:02X}", case.suffix).parse()?;
        let interface_mac: MacAddress =
            format!("5A:5B:5C:5D:61:{:02X}", case.suffix + 1).parse()?;
        let serial = format!("FIXED-PREFIX-{:02X}", case.suffix);

        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac.to_string(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                ..Default::default()
            }))
            .await?;

        let update = rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: serial,
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: interface_mac.to_string(),
                ip_allocation: case
                    .explicit_policy
                    .then_some(rpc::forge::ExpectedInterfaceIpAllocation::Fixed as i32),
                fixed_ip: Some(fixed_ip.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let result = if case.use_batch {
            env.api
                .update_expected_machines(tonic::Request::new(
                    rpc::forge::BatchExpectedMachineOperationRequest {
                        expected_machines: Some(ExpectedMachineList {
                            expected_machines: vec![update],
                        }),
                        accept_partial_results: false,
                    },
                ))
                .await
                .map(|_| ())
        } else {
            env.api
                .update_expected_machine(tonic::Request::new(update))
                .await
                .map(|_| ())
        };

        if case.explicit_policy {
            let error = result.expect_err(case.name);
            assert_eq!(
                error.code(),
                tonic::Code::InvalidArgument,
                "case: {}",
                case.name,
            );
            assert!(
                error
                    .message()
                    .contains("not within a configured network segment"),
                "case {}: {error}",
                case.name,
            );
        } else {
            result?;
        }

        let expected_interface_count = if case.explicit_policy { 0 } else { 1 };
        let mut txn = env.pool.begin().await?;
        let interfaces =
            db::machine_interface::find_by_mac_address(&mut *txn, interface_mac).await?;
        assert_eq!(
            interfaces.len(),
            expected_interface_count,
            "case: {}",
            case.name,
        );
        if let Some(interface) = interfaces.first() {
            assert_eq!(interface.addresses, vec![fixed_ip], "case: {}", case.name,);
            let static_assignments = db::network_segment::static_assignments(txn.as_mut()).await?;
            assert_eq!(
                interface.segment_id, static_assignments.id,
                "case: {}",
                case.name,
            );
        }
        txn.rollback().await?;

        let stored = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: String::new(),
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
            }))
            .await?
            .into_inner();
        assert_eq!(
            stored.host_nics.len(),
            expected_interface_count,
            "case {}: a rejected update must leave expected configuration unchanged",
            case.name,
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_legacy_bmc_update_preserves_interface_behavior_and_restores_naming(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "5A:5B:5C:5D:5E:61".parse()?;
    let initial_ip: std::net::IpAddr = "192.0.2.210".parse()?;
    let configured_ip: std::net::IpAddr = "192.0.2.211".parse()?;

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "root".into(),
            bmc_password: "testpass".into(),
            chassis_serial_number: "UPDATE-ADDRESSLESS-BMC".into(),
            ..Default::default()
        }))
        .await?;

    let mut txn = env.pool.begin().await?;
    db::machine_interface::preallocate_bmc_machine_interface(&mut txn, bmc_mac, initial_ip, None)
        .await?;
    let interface = db::machine_interface::find_by_mac_address(txn.as_mut(), bmc_mac)
        .await?
        .pop()
        .expect("BMC preallocation should create an interface");
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    db::machine_interface::sync_hostname_after_address_change(&mut txn, interface.id).await?;
    let addressless = db::machine_interface::find_one(txn.as_mut(), interface.id).await?;
    assert_eq!(
        addressless.interface_type,
        model::machine_interface::InterfaceType::Bmc,
    );
    assert!(!addressless.primary_interface);
    assert!(addressless.hostname.starts_with("noip-"));
    assert!(addressless.domain_id.is_none());
    txn.commit().await?;

    let mut update = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac.to_string(),
            id: None,
        }))
        .await?
        .into_inner();
    update.id = None;
    update.bmc_ip_address = Some(configured_ip.to_string());
    env.api
        .update_expected_machine(tonic::Request::new(update))
        .await?;

    let mut txn = env.pool.begin().await?;
    let updated = db::machine_interface::find_one(txn.as_mut(), interface.id).await?;
    txn.rollback().await?;
    assert_eq!(
        updated.interface_type,
        model::machine_interface::InterfaceType::Bmc,
    );
    assert!(!updated.primary_interface);
    assert_eq!(updated.addresses, vec![configured_ip]);
    assert!(!updated.hostname.starts_with("noip-"));
    assert_eq!(updated.domain_id, Some(env.domain.into()));

    // Once addressed, legacy BMC updates remain decoupled from the managed
    // interface. This also needs to hold on upgraded sites that do not have
    // the newer static-assignments anchor.
    let external_ip: std::net::IpAddr = "203.0.113.211".parse()?;
    let mut txn = env.pool.begin().await?;
    let static_assignments = db::network_segment::static_assignments(txn.as_mut()).await?;
    db::network_segment::final_delete(static_assignments.id, &mut txn).await?;
    assert!(
        db::network_segment::for_prefix_containing_address(txn.as_mut(), external_ip)
            .await?
            .is_none(),
    );
    txn.commit().await?;

    let mut update = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac.to_string(),
            id: None,
        }))
        .await?
        .into_inner();
    update.id = None;
    update.bmc_ip_address = Some(external_ip.to_string());
    env.api
        .update_expected_machine(tonic::Request::new(update))
        .await?;

    let expected = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: bmc_mac.to_string(),
            id: None,
        }))
        .await?
        .into_inner();
    assert_eq!(expected.bmc_ip_address, Some(external_ip.to_string()));

    let mut txn = env.pool.begin().await?;
    let unchanged = db::machine_interface::find_one(txn.as_mut(), interface.id).await?;
    txn.rollback().await?;
    assert_eq!(unchanged.addresses, vec![configured_ip]);

    Ok(())
}

#[crate::sqlx_test()]
async fn test_update_expected_machine_change_static_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // Create machine with static IP
    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "5A:5B:5C:5D:5E:63".to_string(),
        bmc_username: "root".into(),
        bmc_password: "testpass".into(),
        chassis_serial_number: "CHANGE-STATIC-IP".into(),
        bmc_ip_address: Some("10.0.0.200".to_string()),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: uuid::Uuid::new_v4().to_string(),
        }),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine");

    // Update to change static IP
    let mut updated_machine = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "5A:5B:5C:5D:5E:63".to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine")
        .into_inner();

    updated_machine.id = None;
    updated_machine.bmc_ip_address = Some("10.0.0.201".to_string());

    env.api
        .update_expected_machine(tonic::Request::new(updated_machine.clone()))
        .await
        .expect("unable to update expected machine IP");

    let retrieved_machine = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "5A:5B:5C:5D:5E:63".to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine after IP change")
        .into_inner();

    assert_eq!(
        retrieved_machine.bmc_ip_address,
        Some("10.0.0.201".to_string())
    );
}

#[crate::sqlx_test()]
async fn test_add_expected_machine_with_invalid_static_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: "5A:5B:5C:5D:5E:64".to_string(),
        bmc_username: "root".into(),
        bmc_password: "testpass".into(),
        chassis_serial_number: "INVALID-IP".into(),
        bmc_ip_address: Some("not-a-valid-ip".to_string()),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: uuid::Uuid::new_v4().to_string(),
        }),
        ..Default::default()
    };

    let result = env
        .api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await;

    assert!(
        result.is_err(),
        "Should fail when adding machine with invalid IP address"
    );
}

/// Adding an expected machine with `interfaces[].fixed_ip` should result in a static
/// `machine_interface` for that NIC. The materialization is deferred: site-explorer's
/// reconciliation pass (or the DHCP discover hook) is what creates the row. The test
/// triggers that reconciliation after add to verify the end-to-end flow.
#[crate::sqlx_test]
async fn test_add_with_host_nic_fixed_ip_creates_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:01".parse().unwrap();
    let nic_mac: MacAddress = "7A:7B:7C:7D:7E:02".parse().unwrap();
    let fixed_ip: std::net::IpAddr = "192.0.2.230".parse()?;
    let expected_interface = model::expected_machine::ExpectedInterface {
        mac_address: nic_mac,
        nic_type: Some("onboard".into()),
        fixed_ip: Some(fixed_ip),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-FIXEDIP-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: nic_mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // Add doesn't preallocate inline; run the same allocation-policy helper
    // that Site Explorer uses on its next iteration.
    carbide_site_explorer::try_apply_expected_interface(&env.pool, &expected_interface, None).await;

    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, nic_mac).await?;
    assert_eq!(
        interfaces.len(),
        1,
        "should have one interface for the host NIC MAC"
    );
    assert!(
        interfaces[0].addresses.contains(&fixed_ip),
        "interface should have the fixed IP"
    );

    let addrs =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert_eq!(addrs.len(), 1);
    assert_eq!(
        addrs[0].allocation_type,
        model::allocation_type::AllocationType::Static
    );

    Ok(())
}

/// When a device DHCPs with a MAC that has a fixed_ip in the expected
/// machine's interfaces, it should get the fixed IP (not a pool allocation).
#[crate::sqlx_test]
async fn test_dhcp_discover_uses_fixed_ip_from_interfaces(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:03".parse().unwrap();
    let nic_mac: MacAddress = "7A:7B:7C:7D:7E:04".parse().unwrap();
    let fixed_ip = "192.0.2.231";

    // Register the expected machine with an interface fixed_ip.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCP-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: nic_mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.into()),
                fixed_mask: None,
                fixed_gateway: None,
                primary: None,
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // DHCP discover with the host NIC MAC -- should get the fixed IP.
    let nic_mac_str = nic_mac.to_string();
    let response = env
        .api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &nic_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.address, fixed_ip,
        "DHCP should return the fixed IP from the expected interface"
    );

    Ok(())
}

/// First DHCPDISCOVER for an `expected_machines` BMC: discover() consults
/// `find_by_bmc_mac_address`, preallocates from `bmc_ip_address`, and the existing
/// find_or_create path serves that static IP. Add-time doesn't preallocate; row materialization
/// is deferred until this hook fires (for in-network MACs that DHCPDISCOVER) or until
/// site-explorer's reconciliation pass runs (for everything, including external
/// static-assignments IPs).
#[crate::sqlx_test]
async fn test_dhcp_discover_preallocates_bmc_ip_for_unknown_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:41".parse().unwrap();
    let bmc_ip = "192.0.2.245";

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-RECOVERY-001".into(),
            bmc_ip_address: Some(bmc_ip.into()),
            ..Default::default()
        }))
        .await?;

    // Add no longer preallocates -- the row should be absent until DHCPDISCOVER fires.
    let mut txn = env.db_txn().await;
    let before = db::machine_interface::find_by_mac_address(txn.as_mut(), bmc_mac).await?;
    assert!(
        before.is_empty(),
        "add does not preallocate inline; the interface should only appear after discover()"
    );
    txn.commit().await?;

    let bmc_mac_str = bmc_mac.to_string();
    let response = env
        .api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &bmc_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.address, bmc_ip,
        "BMC DHCP should serve the configured bmc_ip_address, not a dynamic-pool allocation"
    );

    let mut txn = env.db_txn().await;
    let after = db::machine_interface::find_by_mac_address(txn.as_mut(), bmc_mac).await?;
    assert_eq!(after.len(), 1, "interface should be created by discover()");
    assert!(
        after[0].addresses.contains(&bmc_ip.parse().unwrap()),
        "preallocated interface should carry the configured static IP"
    );
    assert_eq!(
        after[0].interface_type,
        model::machine_interface::InterfaceType::Bmc,
        "BMC discover hook should mark the interface as InterfaceType::Bmc, not Data"
    );

    Ok(())
}

/// First DHCPDISCOVER for an `ExpectedInterface.fixed_ip`. discover() passes the matched NIC
/// through to `validate_existing_mac_and_create`, which honors `fixed_ip` via
/// `AddressSelectionStrategy::StaticAddress`. Pins the deferred preallocation path for host NICs.
#[crate::sqlx_test]
async fn test_dhcp_discover_preallocates_host_nic_fixed_ip_for_unknown_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:51".parse().unwrap();
    let nic_mac: MacAddress = "7A:7B:7C:7D:7E:52".parse().unwrap();
    let fixed_ip = "192.0.2.246";

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-RECOVERY-002".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: nic_mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.into()),
                fixed_mask: None,
                fixed_gateway: None,
                primary: None,
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let before = db::machine_interface::find_by_mac_address(txn.as_mut(), nic_mac).await?;
    assert!(
        before.is_empty(),
        "add does not preallocate inline; the interface should only appear after discover()"
    );
    txn.commit().await?;

    let nic_mac_str = nic_mac.to_string();
    let response = env
        .api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &nic_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.address, fixed_ip,
        "host NIC re-DHCP should serve the configured fixed_ip"
    );

    let mut txn = env.db_txn().await;
    let after = db::machine_interface::find_by_mac_address(txn.as_mut(), nic_mac).await?;
    assert_eq!(after.len(), 1, "interface should be created by discover()");
    assert_eq!(
        after[0].interface_type,
        model::machine_interface::InterfaceType::Data,
        "host NIC discover hook should mark the interface as InterfaceType::Data, not Bmc"
    );

    Ok(())
}

/// When `bmc_retain_credentials` is set to true, the value should persist through
/// add -> get round-trip via the RPC API.
#[crate::sqlx_test()]
async fn test_add_expected_machine_with_bmc_retain_credentials(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "5A:5B:5C:5D:5E:70".parse().unwrap();

    let expected_machine = rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "RETAIN-CREDS-001".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        id: Some(::rpc::common::Uuid {
            value: Uuid::new_v4().to_string(),
        }),
        bmc_retain_credentials: Some(true),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(expected_machine.clone()))
        .await
        .expect("unable to add expected machine");

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
            bmc_mac_address: bmc_mac.to_string(),
            id: None,
        }))
        .await
        .expect("unable to retrieve expected machine")
        .into_inner();

    assert_eq!(
        retrieved.bmc_retain_credentials,
        Some(true),
        "bmc_retain_credentials should be true after round-trip"
    );
}

/// Verify that updating an expected machine without specifying `bmc_retain_credentials`
/// preserves the existing value (and making sure COALESCE works).
#[crate::sqlx_test()]
async fn test_update_preserves_bmc_retain_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "5A:5B:5C:5D:5E:71".parse().unwrap();

    // Create with bmc_retain_credentials = true.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "RETAIN-UPDATE-001".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            id: Some(::rpc::common::Uuid {
                value: Uuid::new_v4().to_string(),
            }),
            bmc_retain_credentials: Some(true),
            ..Default::default()
        }))
        .await?;

    // Update without setting bmc_retain_credentials (None).
    env.api
        .update_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "NEW-ADMIN".into(),
            bmc_password: "NEW-PASS".into(),
            chassis_serial_number: "RETAIN-UPDATE-001".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            bmc_retain_credentials: None,
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
            bmc_mac_address: bmc_mac.to_string(),
            id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(
        retrieved.bmc_retain_credentials,
        Some(true),
        "bmc_retain_credentials should be preserved after update with None"
    );

    Ok(())
}

/// When an ExpectedMachine's `interfaces` entry is flagged `primary: true`,
/// the matching NIC's DHCP should land as `machine_interfaces.primary_interface=true`.
#[crate::sqlx_test]
async fn test_dhcp_honors_primary_host_nic(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // rack_management_enabled is required for discover_dhcp to consult
    // ExpectedMachine records for unknown MACs -- that's the path that
    // reads the matched interface's `primary` flag.
    let env = {
        let mut config = get_config();
        config.rack_management_enabled = true;
        create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await
    };
    let bmc_mac: MacAddress = "9A:9B:9C:9D:9E:01".parse().unwrap();
    let primary_mac: MacAddress = "9A:9B:9C:9D:9E:02".parse().unwrap();

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-PRIMARY-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: primary_mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: None,
                fixed_mask: None,
                fixed_gateway: None,
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // DHCP discover with the declared primary MAC.
    let primary_mac_str = primary_mac.to_string();
    env.api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &primary_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?;

    // Verify the created machine_interface is flagged primary=true.
    let mut txn = env.pool.begin().await?;
    let ifaces = db::machine_interface::find_by_mac_address(&mut *txn, primary_mac).await?;
    assert_eq!(ifaces.len(), 1);
    assert!(
        ifaces[0].primary_interface,
        "interface primary=true should flow to machine_interfaces.primary_interface"
    );

    Ok(())
}

/// When one `interfaces` entry is flagged `primary: true`, a DHCP from a
/// *different* MAC on the same host should land as `primary_interface: false`.
/// Verifies the "operator declared some other NIC primary, so this one
/// must not inherit the default primary=true" branch, protecting the DB's
/// one_primary_interface_per_machine unique constraint once the primary
/// MAC's interface eventually lands.
#[crate::sqlx_test]
async fn test_dhcp_marks_non_primary_mac_as_non_primary(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = {
        let mut config = get_config();
        config.rack_management_enabled = true;
        create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await
    };
    let bmc_mac: MacAddress = "9A:9B:9C:9D:9E:10".parse().unwrap();
    let primary_mac: MacAddress = "9A:9B:9C:9D:9E:11".parse().unwrap();
    let other_mac: MacAddress = "9A:9B:9C:9D:9E:12".parse().unwrap();

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-PRIMARY-002".into(),
            host_nics: vec![
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: primary_mac.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: Some(true),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: other_mac.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .await?;

    // DHCP for the non-primary MAC on this machine.
    let other_mac_str = other_mac.to_string();
    env.api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &other_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?;

    let mut txn = env.pool.begin().await?;
    let ifaces = db::machine_interface::find_by_mac_address(&mut *txn, other_mac).await?;
    assert_eq!(ifaces.len(), 1);
    assert!(
        !ifaces[0].primary_interface,
        "a MAC that isn't the declared primary should not land as primary_interface=true"
    );

    Ok(())
}

/// An ExpectedMachine with two `interfaces` entries both flagged `primary: true`
/// must be rejected at the API boundary -- the handler enforces at most one
/// primary NIC per machine (anchoring the DB's `one_primary_interface_per_machine`
/// unique constraint to a single declaration).
#[crate::sqlx_test]
async fn test_add_rejects_multiple_primary_host_nics(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "9A:9B:9C:9D:9E:20".parse().unwrap();
    let mac_a: MacAddress = "9A:9B:9C:9D:9E:21".parse().unwrap();
    let mac_b: MacAddress = "9A:9B:9C:9D:9E:22".parse().unwrap();

    let result = env
        .api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DUPLICATE-PRIMARY-001".into(),
            host_nics: vec![
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: mac_a.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: Some(true),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: mac_b.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: Some(true),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .await;

    let err = result.expect_err("multi-primary ExpectedMachine should be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// Batch updates use the same primary-interface validation as single writes.
#[crate::sqlx_test]
async fn test_batch_update_rejects_multiple_primary_host_nics(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let bmc_mac = "9A:9B:9C:9D:9E:30";

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac.into(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-BATCH-DUPLICATE-PRIMARY-001".into(),
            ..Default::default()
        }))
        .await?;

    let update = rpc::forge::ExpectedMachine {
        id: Some(::rpc::common::Uuid {
            value: id.to_string(),
        }),
        bmc_mac_address: bmc_mac.into(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "EM-BATCH-DUPLICATE-PRIMARY-001".into(),
        host_nics: ["9A:9B:9C:9D:9E:31", "9A:9B:9C:9D:9E:32"]
            .into_iter()
            .map(|mac_address| rpc::forge::ExpectedInterface {
                mac_address: mac_address.into(),
                primary: Some(true),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let result = env
        .api
        .update_expected_machines(tonic::Request::new(
            rpc::forge::BatchExpectedMachineOperationRequest {
                expected_machines: Some(rpc::forge::ExpectedMachineList {
                    expected_machines: vec![update],
                }),
                accept_partial_results: false,
            },
        ))
        .await;

    let err = result.expect_err("batch update with multiple primary interfaces should fail");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// The declared primary survives whichever order its NICs DHCP in: leasing the
/// non-primary NIC first, then the declared primary, still lands the declared
/// primary as `primary_interface` and the other as non-primary.
#[crate::sqlx_test]
async fn test_declared_primary_survives_dhcp_arrival_order(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = {
        let mut config = get_config();
        config.rack_management_enabled = true;
        create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await
    };
    let bmc_mac: MacAddress = "9A:9B:9C:9D:9F:10".parse().unwrap();
    let primary_mac: MacAddress = "9A:9B:9C:9D:9F:11".parse().unwrap();
    let other_mac: MacAddress = "9A:9B:9C:9D:9F:12".parse().unwrap();

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-PRIMARY-003".into(),
            host_nics: vec![
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: primary_mac.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: Some(true),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    network_segment_type: None,
                    mac_address: other_mac.to_string(),
                    nic_type: Some("onboard".into()),
                    fixed_ip: None,
                    fixed_mask: None,
                    fixed_gateway: None,
                    primary: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .await?;

    // The non-primary NIC leases first, then the declared primary.
    for mac in [other_mac, primary_mac] {
        let mac_str = mac.to_string();
        env.api
            .discover_dhcp(
                common::rpc_builder::DhcpDiscovery::builder(
                    &mac_str,
                    common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
                )
                .tonic_request(),
            )
            .await?;
    }

    let mut txn = env.pool.begin().await?;
    let primary = db::machine_interface::find_by_mac_address(&mut *txn, primary_mac).await?;
    let other = db::machine_interface::find_by_mac_address(&mut *txn, other_mac).await?;
    assert_eq!(primary.len(), 1);
    assert_eq!(other.len(), 1);
    assert!(
        primary[0].primary_interface,
        "the declared primary NIC should be primary even when it leases last"
    );
    assert!(
        !other[0].primary_interface,
        "the non-declared NIC should not be primary"
    );

    Ok(())
}

/// The stable Forge DPU policy field round-trips through the database.
#[crate::sqlx_test]
async fn test_dpu_mode_round_trip_for_non_default_values(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    for (idx, mode) in [rpc::forge::DpuMode::NicMode, rpc::forge::DpuMode::NoDpu]
        .iter()
        .enumerate()
    {
        let mac = format!("5A:5B:5C:5D:5E:{idx:02X}");
        let request = rpc::forge::ExpectedMachine {
            bmc_mac_address: mac.clone(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: format!("EM-DPU-MODE-{idx}"),
            dpu_mode: Some(*mode as i32),
            ..Default::default()
        };

        env.api
            .add_expected_machine(tonic::Request::new(request))
            .await?;

        let retrieved = env
            .api
            .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: mac.clone(),
                id: None,
            }))
            .await?
            .into_inner();

        assert_eq!(
            retrieved.dpu_mode,
            Some(*mode as i32),
            "DPU policy mode {mode:?} should survive DB round-trip unchanged"
        );
    }

    Ok(())
}

/// The default host DPU policy is omitted on the wire, preserving existing
/// clients' absent-field behavior.
#[crate::sqlx_test]
async fn test_dpu_mode_default_value_omitted_on_wire(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mac = "5A:5B:5C:5D:5E:FF";
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: mac.into(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DPU-DEFAULT".into(),
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: mac.into(),
            id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(
        retrieved.dpu_mode, None,
        "default HostDpuPolicy should not be emitted on the Forge compatibility field"
    );

    Ok(())
}

/// Verify the update RPC (for update/patch flows) changes the DPU policy.
#[crate::sqlx_test]
async fn test_update_changes_dpu_mode(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mac = "5A:5B:5C:5D:5E:80";
    let base = rpc::forge::ExpectedMachine {
        bmc_mac_address: mac.into(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "EM-DPU-UPDATE".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_machine(tonic::Request::new(base.clone()))
        .await?;

    for mode in [
        rpc::forge::DpuMode::NicMode,
        rpc::forge::DpuMode::NoDpu,
        rpc::forge::DpuMode::DpuMode,
    ] {
        env.api
            .update_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                dpu_mode: Some(mode as i32),
                ..base.clone()
            }))
            .await?;

        let retrieved = env
            .api
            .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: mac.into(),
                id: None,
            }))
            .await?
            .into_inner();

        // Manage is the column default and the wire-default; the model
        // collapses it to `None` on the way out (see `From<ExpectedMachine>
        // for rpc::forge::ExpectedMachine`), so compare accordingly.
        let expected_wire = match mode {
            rpc::forge::DpuMode::DpuMode | rpc::forge::DpuMode::Unspecified => None,
            other => Some(other as i32),
        };
        assert_eq!(
            retrieved.dpu_mode, expected_wire,
            "update to {mode:?} should persist and round-trip on the wire"
        );
    }

    Ok(())
}

/// Every non-default `ExpectedMachine.bmc_ip_allocation` value persists and
/// reads back unchanged. Fixed includes its required compatibility address;
/// the default/unset case is covered separately below.
#[crate::sqlx_test]
async fn test_bmc_ip_allocation_round_trip_for_non_default_values(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    for (idx, (mode, bmc_ip_address)) in [
        (rpc::forge::BmcIpAllocationType::Dynamic, None),
        (rpc::forge::BmcIpAllocationType::Retained, None),
        (rpc::forge::BmcIpAllocationType::Fixed, Some("192.0.2.25")),
    ]
    .into_iter()
    .enumerate()
    {
        let mac = format!("5A:5B:5C:5D:5F:{idx:02X}");
        let request = rpc::forge::ExpectedMachine {
            bmc_mac_address: mac.clone(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: format!("EM-BMC-ALLOC-{idx}"),
            bmc_ip_allocation: Some(mode as i32),
            bmc_ip_address: bmc_ip_address.map(str::to_string),
            ..Default::default()
        };

        env.api
            .add_expected_machine(tonic::Request::new(request))
            .await?;

        let retrieved = env
            .api
            .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: mac.clone(),
                id: None,
            }))
            .await?
            .into_inner();

        assert_eq!(
            retrieved.bmc_ip_allocation,
            Some(mode as i32),
            "bmc_ip_allocation {mode:?} should survive DB round-trip unchanged"
        );
        assert_eq!(
            retrieved.bmc_ip_address.as_deref(),
            bmc_ip_address,
            "bmc_ip_address for {mode:?} should survive DB round-trip unchanged"
        );
    }

    Ok(())
}

/// Default-case round-trip for `bmc_ip_allocation`: when the operator omits it on
/// the wire, the server persists the Postgres default (`Auto`) and returns `None`
/// on the wire, so old clients see exactly what they sent.
#[crate::sqlx_test]
async fn test_bmc_ip_allocation_default_value_omitted_on_wire(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mac = "5A:5B:5C:5D:5F:FF";
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: mac.into(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-BMC-ALLOC-DEFAULT".into(),
            bmc_ip_allocation: None,
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: mac.into(),
            id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(
        retrieved.bmc_ip_allocation, None,
        "default bmc_ip_allocation should not be emitted on the wire for stable round-trips"
    );

    Ok(())
}

/// Every `bmc_ip_allocation` x `bmc_ip_address` combination driven through the
/// real handlers: `add_expected_machine` accepts the six valid pairings and
/// refuses the three invalid ones, and `update_expected_machine` refuses the
/// same invalid pairings on an existing machine. Rejection is `InvalidArgument`
/// and names `bmc_ip_allocation` so the operator knows which knob to fix, and a
/// rejected update leaves the stored machine untouched. The combination table
/// itself is unit-tested in api-model -- these rows pin the handler wiring that
/// enforces it at the API boundary.
#[crate::sqlx_test]
async fn test_bmc_ip_allocation_combinations_enforced_at_the_api_boundary(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, check_cases_async};
    use rpc::forge::BmcIpAllocationType::{Auto, Dynamic, Fixed, Retained};

    let env = create_test_env(pool).await;

    /// One `add_expected_machine` request: the allocation/address pairing under
    /// test, plus the unique per-machine identity the API requires.
    struct AddRequest {
        mode: Option<rpc::forge::BmcIpAllocationType>,
        bmc_ip_address: Option<&'static str>,
        mac: &'static str,
        serial: &'static str,
    }

    // Rejections are projected to (status code, "the error names
    // bmc_ip_allocation") so each failing row pins both.
    check_cases_async(
        [
            Case {
                scenario: "add: unset with no address is accepted (defaults to auto)",
                input: AddRequest {
                    mode: None,
                    bmc_ip_address: None,
                    mac: "5A:5B:5C:5D:61:01",
                    serial: "EM-BMC-ALLOC-API-01",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: auto with no address is accepted (retains)",
                input: AddRequest {
                    mode: Some(Auto),
                    bmc_ip_address: None,
                    mac: "5A:5B:5C:5D:61:02",
                    serial: "EM-BMC-ALLOC-API-02",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: auto with an address is accepted (fixed)",
                input: AddRequest {
                    mode: Some(Auto),
                    bmc_ip_address: Some("192.0.2.61"),
                    mac: "5A:5B:5C:5D:61:03",
                    serial: "EM-BMC-ALLOC-API-03",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: dynamic with no address is accepted",
                input: AddRequest {
                    mode: Some(Dynamic),
                    bmc_ip_address: None,
                    mac: "5A:5B:5C:5D:61:04",
                    serial: "EM-BMC-ALLOC-API-04",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: fixed with an address is accepted",
                input: AddRequest {
                    mode: Some(Fixed),
                    bmc_ip_address: Some("192.0.2.62"),
                    mac: "5A:5B:5C:5D:61:05",
                    serial: "EM-BMC-ALLOC-API-05",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: retained with no address is accepted",
                input: AddRequest {
                    mode: Some(Retained),
                    bmc_ip_address: None,
                    mac: "5A:5B:5C:5D:61:06",
                    serial: "EM-BMC-ALLOC-API-06",
                },
                expect: Yields(()),
            },
            Case {
                scenario: "add: fixed with no address is rejected",
                input: AddRequest {
                    mode: Some(Fixed),
                    bmc_ip_address: None,
                    mac: "5A:5B:5C:5D:61:07",
                    serial: "EM-BMC-ALLOC-API-07",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "add: dynamic with an address is rejected",
                input: AddRequest {
                    mode: Some(Dynamic),
                    bmc_ip_address: Some("192.0.2.63"),
                    mac: "5A:5B:5C:5D:61:08",
                    serial: "EM-BMC-ALLOC-API-08",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "add: retained with an address is rejected",
                input: AddRequest {
                    mode: Some(Retained),
                    bmc_ip_address: Some("192.0.2.64"),
                    mac: "5A:5B:5C:5D:61:09",
                    serial: "EM-BMC-ALLOC-API-09",
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
        ],
        |req| {
            let env = &env;
            async move {
                env.api
                    .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                        bmc_mac_address: req.mac.into(),
                        bmc_username: "ADMIN".into(),
                        bmc_password: "PASS".into(),
                        chassis_serial_number: req.serial.into(),
                        bmc_ip_allocation: req.mode.map(|m| m as i32),
                        bmc_ip_address: req.bmc_ip_address.map(Into::into),
                        ..Default::default()
                    }))
                    .await
                    .map(|_| ())
                    .map_err(|status| {
                        (
                            status.code(),
                            status.message().contains("bmc_ip_allocation"),
                        )
                    })
            }
        },
    )
    .await;

    // The same invalid pairings through update_expected_machine, against one
    // existing machine.
    let mac = "5A:5B:5C:5D:61:10";
    let base = rpc::forge::ExpectedMachine {
        bmc_mac_address: mac.into(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "EM-BMC-ALLOC-API-10".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };
    env.api
        .add_expected_machine(tonic::Request::new(base.clone()))
        .await?;

    /// One `update_expected_machine` request: the invalid pairing sent for the
    /// machine created above.
    struct UpdateRequest {
        mode: rpc::forge::BmcIpAllocationType,
        bmc_ip_address: Option<&'static str>,
    }

    check_cases_async(
        [
            Case {
                scenario: "update: fixed with no address is rejected",
                input: UpdateRequest {
                    mode: Fixed,
                    bmc_ip_address: None,
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "update: dynamic with an address is rejected",
                input: UpdateRequest {
                    mode: Dynamic,
                    bmc_ip_address: Some("192.0.2.65"),
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
            Case {
                scenario: "update: retained with an address is rejected",
                input: UpdateRequest {
                    mode: Retained,
                    bmc_ip_address: Some("192.0.2.66"),
                },
                expect: FailsWith((tonic::Code::InvalidArgument, true)),
            },
        ],
        |req| {
            let env = &env;
            let base = &base;
            async move {
                env.api
                    .update_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                        bmc_ip_allocation: Some(req.mode as i32),
                        bmc_ip_address: req.bmc_ip_address.map(Into::into),
                        ..base.clone()
                    }))
                    .await
                    .map(|_| ())
                    .map_err(|status| {
                        (
                            status.code(),
                            status.message().contains("bmc_ip_allocation"),
                        )
                    })
            }
        },
    )
    .await;

    // Every rejected update happened before any write: the stored machine still
    // has the default allocation and no address.
    let retrieved = env
        .api
        .get_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: mac.into(),
            id: None,
        }))
        .await?
        .into_inner();
    assert_eq!(
        retrieved.bmc_ip_allocation, None,
        "rejected updates should leave the stored allocation untouched"
    );
    assert_eq!(
        retrieved.bmc_ip_address, None,
        "rejected updates should not store a bmc_ip_address"
    );

    Ok(())
}

/// Host BMC declarations keep the top-level BMC MAC as the machine identity.
///
/// Clients that serialize `primary=false` for every interface remain accepted,
/// while ambiguous declarations are rejected before normalization or storage.
#[crate::sqlx_test]
async fn test_host_bmc_declaration_validation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    /// One Host BMC declaration accepted or rejected by the API boundary.
    struct Case {
        name: &'static str,
        suffix: u8,
        interfaces: Vec<rpc::forge::ExpectedInterface>,
        expected_error: Option<&'static str>,
    }

    for case in [
        Case {
            name: "one declaration with compatibility primary false",
            suffix: 0x20,
            interfaces: vec![rpc::forge::ExpectedInterface {
                mac_address: "5A:5B:5C:5D:62:20".into(),
                role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                primary: Some(false),
                ..Default::default()
            }],
            expected_error: None,
        },
        Case {
            name: "two Host BMC declarations",
            suffix: 0x21,
            interfaces: vec![
                rpc::forge::ExpectedInterface {
                    mac_address: "5A:5B:5C:5D:62:21".into(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    mac_address: "5A:5B:5C:5D:62:21".into(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    ..Default::default()
                },
            ],
            expected_error: Some("at most one role=host_bmc interface"),
        },
        Case {
            name: "Host BMC MAC differs from the machine key",
            suffix: 0x22,
            interfaces: vec![rpc::forge::ExpectedInterface {
                mac_address: "5A:5B:5C:5D:62:FF".into(),
                role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            }],
            expected_error: Some("must match expected machine BMC MAC"),
        },
        Case {
            name: "Host BMC declares itself primary",
            suffix: 0x23,
            interfaces: vec![rpc::forge::ExpectedInterface {
                mac_address: "5A:5B:5C:5D:62:23".into(),
                role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                primary: Some(true),
                ..Default::default()
            }],
            expected_error: Some("cannot set primary=true"),
        },
        Case {
            name: "one MAC has conflicting roles",
            suffix: 0x24,
            interfaces: vec![
                rpc::forge::ExpectedInterface {
                    mac_address: "5A:5B:5C:5D:62:A4".into(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    mac_address: "5A:5B:5C:5D:62:A4".into(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                },
            ],
            expected_error: Some("must use the same role"),
        },
        Case {
            name: "machine BMC MAC uses a non-HostBmc role",
            suffix: 0x25,
            interfaces: vec![rpc::forge::ExpectedInterface {
                mac_address: "5A:5B:5C:5D:62:25".into(),
                role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            }],
            expected_error: Some("may only be configured with role=host_bmc"),
        },
    ] {
        let bmc_mac = format!("5A:5B:5C:5D:62:{:02X}", case.suffix);
        let result = env
            .api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                bmc_mac_address: bmc_mac.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: format!("HOST-BMC-VALIDATION-{:02X}", case.suffix),
                host_nics: case.interfaces,
                ..Default::default()
            }))
            .await;

        match case.expected_error {
            Some(expected_error) => {
                let error = result.expect_err("invalid Host BMC declaration should be rejected");
                assert_eq!(
                    error.code(),
                    tonic::Code::InvalidArgument,
                    "case: {}",
                    case.name,
                );
                assert!(
                    error.message().contains(expected_error),
                    "case {}: unexpected rejection reason: {}",
                    case.name,
                    error.message(),
                );
            }
            None => {
                result?;
                let stored = env
                    .api
                    .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                        bmc_mac_address: bmc_mac,
                        id: None,
                    }))
                    .await?
                    .into_inner();
                let host_bmcs = stored
                    .host_nics
                    .iter()
                    .filter(|interface| {
                        interface.role == Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(host_bmcs.len(), 1, "case: {}", case.name);
                assert_eq!(
                    host_bmcs[0].primary, None,
                    "case {}: primary=false should normalize to omission",
                    case.name,
                );
            }
        }
    }

    Ok(())
}

/// Compatibility columns remain authoritative for rows changed by an older
/// writer. Reads expose their value without losing nested-only settings, and
/// the next ordinary update repairs the stored nested declaration.
#[crate::sqlx_test]
async fn test_host_bmc_compatibility_drift_is_read_and_healed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let id = Uuid::new_v4();
    let bmc_mac: MacAddress = "5A:5B:5C:5D:65:50".parse()?;
    let base = rpc::forge::ExpectedMachine {
        id: Some(::rpc::common::Uuid {
            value: id.to_string(),
        }),
        bmc_mac_address: bmc_mac.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        chassis_serial_number: "HOST-BMC-DRIFT".into(),
        ..Default::default()
    };
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: bmc_mac.to_string(),
                role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                ..Default::default()
            }],
            ..base.clone()
        }))
        .await?;

    // Simulate an older writer that knows only the compatibility columns.
    let mut txn = env.pool.begin().await?;
    let mut drifted = db::expected_machine::find_by_id(&mut *txn, id)
        .await?
        .expect("expected machine should exist");
    drifted.data.bmc_ip_allocation = BmcIpAllocationType::Dynamic;
    drifted.data.bmc_ip_address = None;
    db::expected_machine::update(&mut txn, &drifted).await?;
    txn.commit().await?;

    let read = env
        .api
        .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
            bmc_mac_address: String::new(),
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
        }))
        .await?
        .into_inner();
    let effective = read
        .interfaces()
        .iter()
        .find(|interface| interface.role == Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32))
        .expect("read should synthesize the effective Host BMC");
    assert_eq!(
        effective.ip_allocation,
        Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
    );
    assert_eq!(
        effective.network_segment_type,
        Some(rpc::forge::NetworkSegmentType::Admin as i32),
        "nested-only settings should survive compatibility drift",
    );

    // An old client can omit the nested Host BMC during an unrelated update.
    env.api
        .update_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_username: "UPDATED_ADMIN".into(),
            ..base
        }))
        .await?;

    let mut txn = env.pool.begin().await?;
    let healed = db::expected_machine::find_by_id(&mut *txn, id)
        .await?
        .expect("expected machine should exist");
    let nested = healed
        .data
        .interfaces
        .iter()
        .find(|interface| interface.role.is_host_bmc())
        .expect("ordinary update should retain and heal the Host BMC");
    assert_eq!(
        nested.ip_allocation,
        Some(ExpectedInterfaceIpAllocation::Dynamic),
    );
    assert_eq!(
        nested.network_segment_type,
        Some(model::network_segment::NetworkSegmentType::Admin),
    );
    assert_eq!(healed.data.bmc_ip_allocation, BmcIpAllocationType::Dynamic,);
    assert_eq!(healed.data.bmc_username, "UPDATED_ADMIN");

    Ok(())
}

/// Single and batch updates preserve an omitted legacy Fixed policy while
/// allowing nested-only settings to change. Explicit Unspecified still resets
/// that compatibility policy even when the stored HostBmc role is omitted.
#[crate::sqlx_test]
async fn test_host_bmc_updates_preserve_and_reset_compatibility_fixed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    struct Case {
        name: &'static str,
        suffix: u8,
        use_batch: bool,
    }

    for case in [
        Case {
            name: "single update",
            suffix: 0x54,
            use_batch: false,
        },
        Case {
            name: "batch update",
            suffix: 0x56,
            use_batch: true,
        },
    ] {
        let id = Uuid::new_v4();
        let bmc_mac = format!("5A:5B:5C:5D:65:{:02X}", case.suffix);
        let serial = format!("HOST-BMC-FIXED-PRESENCE-{:02X}", case.suffix);
        let address = format!("192.0.2.{}", case.suffix);
        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: bmc_mac.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                bmc_ip_address: Some(address.clone()),
                bmc_ip_allocation: Some(rpc::forge::BmcIpAllocationType::Fixed as i32),
                host_nics: vec![rpc::forge::ExpectedInterface {
                    mac_address: bmc_mac.clone(),
                    role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                    fixed_ip: Some(address.clone()),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .await?;

        let update = |ip_allocation| rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: bmc_mac.clone(),
            bmc_username: "UPDATED_ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: serial.clone(),
            bmc_ip_address: Some(address.clone()),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: bmc_mac.clone(),
                // Omitted role and policy model clients that do not know the
                // stored HostBmc details.
                role: None,
                ip_allocation,
                fixed_ip: Some(address.clone()),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                ..Default::default()
            }],
            ..Default::default()
        };
        let apply_update = |machine: rpc::forge::ExpectedMachine| async {
            if case.use_batch {
                env.api
                    .update_expected_machines(tonic::Request::new(
                        rpc::forge::BatchExpectedMachineOperationRequest {
                            expected_machines: Some(ExpectedMachineList {
                                expected_machines: vec![machine],
                            }),
                            accept_partial_results: false,
                        },
                    ))
                    .await
                    .map(|_| ())
            } else {
                env.api
                    .update_expected_machine(tonic::Request::new(machine))
                    .await
                    .map(|_| ())
            }
        };

        apply_update(update(None)).await?;

        let mut txn = env.pool.begin().await?;
        let preserved = db::expected_machine::find_by_id(&mut *txn, id)
            .await?
            .expect("expected machine should exist");
        txn.rollback().await?;
        let preserved_host_bmc = preserved
            .data
            .interfaces
            .iter()
            .find(|interface| interface.role.is_host_bmc())
            .expect("nested HostBmc should remain present");
        assert_eq!(
            preserved.data.bmc_ip_allocation,
            BmcIpAllocationType::Fixed,
            "case: {}",
            case.name,
        );
        assert_eq!(
            preserved_host_bmc.ip_allocation, None,
            "case: {}",
            case.name,
        );
        assert_eq!(
            preserved_host_bmc.network_segment_type,
            Some(model::network_segment::NetworkSegmentType::Admin),
            "case: {}",
            case.name,
        );

        apply_update(update(Some(
            rpc::forge::ExpectedInterfaceIpAllocation::Unspecified as i32,
        )))
        .await?;

        let mut txn = env.pool.begin().await?;
        let reset = db::expected_machine::find_by_id(&mut *txn, id)
            .await?
            .expect("expected machine should exist");
        txn.rollback().await?;
        assert_eq!(
            reset.data.bmc_ip_allocation,
            BmcIpAllocationType::Auto,
            "case: {}",
            case.name,
        );
        assert_eq!(
            reset.data.bmc_ip_address.map(|ip| ip.to_string()),
            Some(address.clone()),
            "case: {}",
            case.name,
        );
    }

    Ok(())
}

/// Both update APIs reject changing the BMC MAC selected by an ExpectedMachine
/// ID, leaving the original alternate key intact.
#[crate::sqlx_test]
async fn test_expected_machine_update_rejects_bmc_mac_change(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    struct Case {
        name: &'static str,
        suffix: u8,
        use_batch: bool,
    }

    for case in [
        Case {
            name: "single update",
            suffix: 0x60,
            use_batch: false,
        },
        Case {
            name: "batch update",
            suffix: 0x62,
            use_batch: true,
        },
    ] {
        let id = Uuid::new_v4();
        let original_mac = format!("5A:5B:5C:5D:66:{:02X}", case.suffix);
        let changed_mac = format!("5A:5B:5C:5D:66:{:02X}", case.suffix + 1);
        let serial = format!("HOST-BMC-KEY-{:02X}", case.suffix);
        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: Some(::rpc::common::Uuid {
                    value: id.to_string(),
                }),
                bmc_mac_address: original_mac.clone(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: serial.clone(),
                ..Default::default()
            }))
            .await?;

        let update = rpc::forge::ExpectedMachine {
            id: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
            bmc_mac_address: changed_mac,
            bmc_username: "UPDATED_ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: serial,
            ..Default::default()
        };
        let error = if case.use_batch {
            env.api
                .update_expected_machines(tonic::Request::new(
                    rpc::forge::BatchExpectedMachineOperationRequest {
                        expected_machines: Some(ExpectedMachineList {
                            expected_machines: vec![update],
                        }),
                        accept_partial_results: false,
                    },
                ))
                .await
                .expect_err("batch BMC MAC change should be rejected")
        } else {
            env.api
                .update_expected_machine(tonic::Request::new(update))
                .await
                .expect_err("BMC MAC change should be rejected")
        };
        assert_eq!(
            error.code(),
            tonic::Code::InvalidArgument,
            "case: {}",
            case.name,
        );
        assert!(
            error.message().contains("cannot change BMC MAC address"),
            "case {}: unexpected rejection reason: {}",
            case.name,
            error.message(),
        );

        let stored = env
            .api
            .get_expected_machine(tonic::Request::new(ExpectedMachineRequest {
                bmc_mac_address: original_mac,
                id: None,
            }))
            .await?
            .into_inner();
        assert_eq!(stored.bmc_username, "ADMIN", "case: {}", case.name);
    }

    Ok(())
}

/// Make sure expected_machines.json, which uses create_missing_from,
/// follows the shared codepath for handling interface allocation.
#[crate::sqlx_test]
async fn test_create_missing_from_preallocates_interfaces(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "AA:BB:CC:DD:EE:01".parse().unwrap();
    let nic_mac: MacAddress = "AA:BB:CC:DD:EE:02".parse().unwrap();
    let bmc_ip: std::net::IpAddr = "203.0.113.240".parse().unwrap();
    let host_ip: std::net::IpAddr = "192.0.2.241".parse().unwrap();

    let machine = ExpectedMachine {
        id: None,
        bmc_mac_address: bmc_mac,
        data: ExpectedMachineData {
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            serial_number: "EM-JSON-SEED-001".into(),
            bmc_ip_address: Some(bmc_ip),
            interfaces: vec![model::expected_machine::ExpectedInterface {
                network_segment_type: None,
                mac_address: nic_mac,
                nic_type: Some("onboard".into()),
                fixed_ip: Some(host_ip),
                fixed_mask: None,
                fixed_gateway: None,
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        },
    };

    let mut txn = env.pool.begin().await?;
    crate::handlers::expected_machine::create_missing_from(
        &mut txn,
        std::slice::from_ref(&machine),
    )
    .await?;
    txn.commit().await?;

    // Run the same effective-interface path as Site Explorer. The legacy BMC
    // address is intentionally outside every managed prefix, so its inferred
    // HostBmc declaration must retain the static-assignments fallback.
    let mut txn = env.pool.begin().await?;
    let stored = db::expected_machine::find_by_bmc_mac_address(&mut *txn, bmc_mac)
        .await?
        .expect("expected machine should exist");
    txn.rollback().await?;
    carbide_site_explorer::try_apply_expected_interface(
        &env.pool,
        &stored.effective_host_bmc(),
        None,
    )
    .await;
    for interface in stored
        .data
        .interfaces
        .iter()
        .filter(|interface| interface.mac_address != stored.bmc_mac_address)
    {
        carbide_site_explorer::try_apply_expected_interface(&env.pool, interface, None).await;
    }

    let mut txn = env.pool.begin().await?;
    for (mac, expected_ip) in [(bmc_mac, bmc_ip), (nic_mac, host_ip)] {
        let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
        assert_eq!(
            interfaces.len(),
            1,
            "expected one machine_interface for MAC {mac}"
        );
        assert!(
            interfaces[0].addresses.contains(&expected_ip),
            "machine_interface for MAC {mac} should carry static IP {expected_ip}, got {:?}",
            interfaces[0].addresses,
        );
        if mac == bmc_mac {
            assert_eq!(
                interfaces[0].interface_type,
                model::machine_interface::InterfaceType::Bmc,
            );
            assert!(!interfaces[0].primary_interface);
            let static_assignments = db::network_segment::static_assignments(txn.as_mut()).await?;
            assert_eq!(interfaces[0].segment_id, static_assignments.id);
        }
    }

    // Re-running create_missing_from with the same input must be a no-op (idempotent).
    let mut txn = env.pool.begin().await?;
    crate::handlers::expected_machine::create_missing_from(
        &mut txn,
        std::slice::from_ref(&machine),
    )
    .await?;
    txn.commit().await?;

    Ok(())
}
