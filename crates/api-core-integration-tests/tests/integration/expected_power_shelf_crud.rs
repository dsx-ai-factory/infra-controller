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

use carbide_test_harness::prelude::*;
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ExpectedPowerShelfList, ExpectedPowerShelfRequest};
use uuid::Uuid;

use crate::expected_power_shelf::{create_expected_power_shelves, create_test_env};

#[sqlx_test]
async fn test_add_expected_power_shelf(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    for mut expected_power_shelf in [
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-TEST-001".into(),
            bmc_ip_address: "".into(),
            metadata: None,
            rack_id: None,
            bmc_retain_credentials: None,
        },
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: "3A:3B:3C:3D:3E:40".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-TEST-002".into(),
            bmc_ip_address: "192.168.1.200".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        },
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: "3A:3B:3C:3D:3E:41".to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-TEST-003".into(),
            bmc_ip_address: "192.168.1.201".into(),
            metadata: Some(rpc::forge::Metadata {
                name: "power-shelf-a".to_string(),
                description: "Test power shelf".to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: "location".to_string(),
                        value: Some("datacenter-1".to_string()),
                    },
                    rpc::forge::Label {
                        key: "rack".to_string(),
                        value: Some("A1".to_string()),
                    },
                ],
            }),
            rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
            bmc_retain_credentials: None,
        },
    ] {
        env.api()
            .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
            .await
            .expect("unable to add expected power shelf ");

        let expected_power_shelf_query = rpc::forge::ExpectedPowerShelfRequest {
            bmc_mac_address: expected_power_shelf.bmc_mac_address.clone(),
            expected_power_shelf_id: None,
        };

        let mut retrieved_expected_power_shelf = env
            .api()
            .get_expected_power_shelf(tonic::Request::new(expected_power_shelf_query))
            .await
            .expect("unable to retrieve expected power shelf ")
            .into_inner();
        retrieved_expected_power_shelf
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        if expected_power_shelf.metadata.is_none() {
            expected_power_shelf.metadata = Some(Default::default());
        }
        // The server generates an ID if one wasn't provided.
        expected_power_shelf.expected_power_shelf_id = retrieved_expected_power_shelf
            .expected_power_shelf_id
            .clone();

        assert_eq!(retrieved_expected_power_shelf, expected_power_shelf);
    }
}

#[sqlx_test]
async fn test_delete_expected_power_shelf(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shelves = create_expected_power_shelves(&mut conn).await;
    drop(conn);
    let env = create_test_env(pool).await;

    let expected_power_shelf_count = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves
        .len();

    let expected_power_shelf_query = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: shelves[1].bmc_mac_address.to_string(),
        expected_power_shelf_id: None,
    };
    env.api()
        .delete_expected_power_shelf(tonic::Request::new(expected_power_shelf_query))
        .await
        .expect("unable to delete expected power shelf ")
        .into_inner();

    let new_expected_power_shelf_count = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves
        .len();

    assert_eq!(
        new_expected_power_shelf_count,
        expected_power_shelf_count - 1
    );
}

#[sqlx_test]
async fn test_delete_expected_power_shelf_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_power_shelf_request = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        expected_power_shelf_id: None,
    };

    let err = env
        .api()
        .delete_expected_power_shelf(tonic::Request::new(expected_power_shelf_request))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_power_shelf not found: {}", bmc_mac_address)
    );
}

#[sqlx_test]
async fn test_update_expected_power_shelf(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shelves = create_expected_power_shelves(&mut conn).await;
    drop(conn);
    let env = create_test_env(pool).await;

    let bmc_mac_address: MacAddress = shelves[1].bmc_mac_address;
    for mut updated_power_shelf in [
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            shelf_serial_number: "PS-UPD-001".into(),
            bmc_ip_address: "".into(),
            metadata: None,
            rack_id: None,
            bmc_retain_credentials: None,
        },
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            shelf_serial_number: "PS-UPD-002".into(),
            bmc_ip_address: "192.168.2.100".into(),
            metadata: Some(Default::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        },
        rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac_address.to_string(),
            bmc_username: "ADMIN_UPDATE1".into(),
            bmc_password: "PASS_UPDATE1".into(),
            shelf_serial_number: "PS-UPD-003".into(),
            bmc_ip_address: "192.168.2.100".into(),
            metadata: Some(rpc::forge::Metadata {
                name: "updated-shelf".to_string(),
                description: "Updated power shelf".to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: "env".to_string(),
                        value: Some("production".to_string()),
                    },
                    rpc::forge::Label {
                        key: "zone".to_string(),
                        value: Some("zone-a".to_string()),
                    },
                ],
            }),
            rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
            bmc_retain_credentials: None,
        },
    ] {
        env.api()
            .update_expected_power_shelf(tonic::Request::new(updated_power_shelf.clone()))
            .await
            .expect("unable to update expected power shelf ")
            .into_inner();

        let mut retrieved_expected_power_shelf = env
            .api()
            .get_expected_power_shelf(tonic::Request::new(ExpectedPowerShelfRequest {
                bmc_mac_address: bmc_mac_address.to_string(),
                expected_power_shelf_id: None,
            }))
            .await
            .expect("unable to fetch expected power shelf ")
            .into_inner();
        retrieved_expected_power_shelf
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        if updated_power_shelf.metadata.is_none() {
            updated_power_shelf.metadata = Some(Default::default());
        }
        // The server returns the ID from the database.
        updated_power_shelf.expected_power_shelf_id = retrieved_expected_power_shelf
            .expected_power_shelf_id
            .clone();

        assert_eq!(retrieved_expected_power_shelf, updated_power_shelf);
    }
}

#[sqlx_test]
async fn test_update_expected_power_shelf_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN_UPDATE".into(),
        bmc_password: "PASS_UPDATE".into(),
        shelf_serial_number: "PS-UPD-001".into(),
        bmc_ip_address: "".into(),
        metadata: None,
        rack_id: None,
        bmc_retain_credentials: None,
    };

    let err = env
        .api()
        .update_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .unwrap_err();

    assert!(
        err.message().contains(&bmc_mac_address.to_string()),
        "Error should reference the MAC address: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_delete_all_expected_power_shelves(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    create_expected_power_shelves(&mut conn).await;
    drop(conn);
    let env = create_test_env(pool).await;
    let mut expected_power_shelf_count = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves
        .len();

    assert_eq!(expected_power_shelf_count, 6);

    env.api()
        .delete_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to delete all expected power shelves")
        .into_inner();

    expected_power_shelf_count = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves
        .len();

    assert_eq!(expected_power_shelf_count, 0);
}

#[sqlx_test]
async fn test_replace_all_expected_power_shelves(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    create_expected_power_shelves(&mut conn).await;
    drop(conn);
    let env = create_test_env(pool).await;
    let expected_power_shelf_count = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves
        .len();

    assert_eq!(expected_power_shelf_count, 6);

    let mut expected_power_shelf_list = ExpectedPowerShelfList {
        expected_power_shelves: Vec::new(),
    };

    let expected_power_shelf_1 = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: "6A:6B:6C:6D:6E:6F".into(),
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        shelf_serial_number: "PS-NEW-001".into(),
        bmc_ip_address: "192.168.100.1".into(),
        metadata: Some(rpc::Metadata::default()),
        rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
        bmc_retain_credentials: None,
    };

    let expected_power_shelf_2 = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: "7A:7B:7C:7D:7E:7F".into(),
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        shelf_serial_number: "PS-NEW-002".into(),
        bmc_ip_address: "192.168.100.2".into(),
        metadata: Some(rpc::Metadata::default()),
        rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
        bmc_retain_credentials: None,
    };

    expected_power_shelf_list
        .expected_power_shelves
        .push(expected_power_shelf_1.clone());
    expected_power_shelf_list
        .expected_power_shelves
        .push(expected_power_shelf_2.clone());

    env.api()
        .replace_all_expected_power_shelves(tonic::Request::new(expected_power_shelf_list))
        .await
        .expect("unable to replace all expected power shelves")
        .into_inner();

    let expected_power_shelves = env
        .api()
        .get_all_expected_power_shelves(tonic::Request::new(()))
        .await
        .expect("unable to get all expected power shelves")
        .into_inner()
        .expected_power_shelves;

    assert_eq!(expected_power_shelves.len(), 2);
    // Server generates IDs, so compare by serial number.
    assert!(
        expected_power_shelves
            .iter()
            .any(|ps| ps.shelf_serial_number == expected_power_shelf_1.shelf_serial_number)
    );
    assert!(
        expected_power_shelves
            .iter()
            .any(|ps| ps.shelf_serial_number == expected_power_shelf_2.shelf_serial_number)
    );
}

#[sqlx_test]
async fn test_get_expected_power_shelf_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_power_shelf_query = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        expected_power_shelf_id: None,
    };

    let err = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(expected_power_shelf_query))
        .await
        .unwrap_err();

    assert!(
        err.message().contains(&bmc_mac_address.to_string()),
        "Error should reference the MAC address: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_get_linked_expected_power_shelves_unseen(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    create_expected_power_shelves(&mut conn).await;
    drop(conn);
    let env = create_test_env(pool).await;
    let out = env
        .api()
        .get_all_expected_power_shelves_linked(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.expected_power_shelves.len(), 6);
    // They are sorted by MAC server-side
    let eps = out.expected_power_shelves.first().unwrap();
    assert_eq!(eps.shelf_serial_number, "PS-SN-001");
    assert!(
        eps.power_shelf_id.is_none(),
        "expected_power_shelves fixture should have no linked power shelf"
    );
}

#[sqlx_test]
async fn test_add_expected_power_shelf_with_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "3A:3B:3C:3D:3E:3F".parse().unwrap();
    let mut expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: bmc_mac_address.to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-IP-001".into(),
        bmc_ip_address: "10.0.0.100".into(),
        metadata: Some(rpc::Metadata::default()),
        rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf ");

    let expected_power_shelf_query = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        expected_power_shelf_id: None,
    };

    let retrieved_expected_power_shelf = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(expected_power_shelf_query))
        .await
        .expect("unable to retrieve expected power shelf ")
        .into_inner();

    // The server generates an ID if one wasn't provided.
    expected_power_shelf.expected_power_shelf_id = retrieved_expected_power_shelf
        .expected_power_shelf_id
        .clone();
    assert_eq!(retrieved_expected_power_shelf, expected_power_shelf);
    assert_eq!(retrieved_expected_power_shelf.bmc_ip_address, "10.0.0.100");
}

#[sqlx_test]
async fn test_update_expected_power_shelf_ip_address(pool: sqlx::PgPool) {
    let mut conn = pool.acquire().await.unwrap();
    let shelves = create_expected_power_shelves(&mut conn).await;

    drop(conn);
    let env = create_test_env(pool).await;

    let shelf_mac = shelves[1].bmc_mac_address.to_string();
    let mut eps1 = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelfRequest {
            bmc_mac_address: shelf_mac.clone(),
            expected_power_shelf_id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    eps1.bmc_ip_address = "172.16.0.50".to_string();

    env.api()
        .update_expected_power_shelf(tonic::Request::new(eps1.clone()))
        .await
        .expect("unable to update")
        .into_inner();

    let eps2 = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelfRequest {
            bmc_mac_address: shelf_mac,
            expected_power_shelf_id: None,
        }))
        .await
        .expect("unable to get")
        .into_inner();

    assert_eq!(eps1, eps2);
    assert_eq!(eps2.bmc_ip_address, "172.16.0.50");
}

#[sqlx_test]
async fn test_get_expected_power_shelf_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let provided_id = Uuid::new_v4().to_string();
    let expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        bmc_mac_address: "AA:BB:CC:DD:EE:01".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-ID-001".into(),
        bmc_ip_address: "10.0.0.50".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        rack_id: None,
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf");

    // Get by id
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "".to_string(),
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .expect("unable to retrieve by id")
        .into_inner();

    assert_eq!(
        retrieved.expected_power_shelf_id,
        Some(::rpc::common::Uuid { value: provided_id })
    );
    assert_eq!(retrieved.bmc_mac_address, "AA:BB:CC:DD:EE:01");
    assert_eq!(retrieved.shelf_serial_number, "PS-ID-001");
    assert_eq!(retrieved.bmc_ip_address, "10.0.0.50");
}

#[sqlx_test]
async fn test_delete_expected_power_shelf_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let provided_id = Uuid::new_v4().to_string();
    let expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        bmc_mac_address: "AA:BB:CC:DD:EE:02".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-DEL-001".into(),
        bmc_ip_address: "".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        rack_id: None,
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf");

    // Delete by id
    let del_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "".to_string(),
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    env.api()
        .delete_expected_power_shelf(tonic::Request::new(del_req))
        .await
        .expect("unable to delete by id");

    // Verify it's gone by trying to get by id
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "".to_string(),
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let err = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_power_shelf not found: {}", provided_id)
    );
}

#[sqlx_test]
async fn test_update_expected_power_shelf_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let provided_id = Uuid::new_v4().to_string();
    let mut expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        bmc_mac_address: "AA:BB:CC:DD:EE:03".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-UPD-ID-001".into(),
        bmc_ip_address: "".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        rack_id: None,
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf");

    // Update by id (change username and serial number)
    expected_power_shelf.bmc_username = "ADMIN_UPDATED".into();
    expected_power_shelf.shelf_serial_number = "PS-UPD-ID-002".into();
    expected_power_shelf.bmc_ip_address = "172.16.0.99".into();
    env.api()
        .update_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to update by id");

    // Fetch by id and verify
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "".to_string(),
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
    };
    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .expect("unable to get after update by id")
        .into_inner();

    assert_eq!(
        retrieved.expected_power_shelf_id,
        Some(::rpc::common::Uuid { value: provided_id })
    );
    assert_eq!(retrieved.bmc_username, "ADMIN_UPDATED");
    assert_eq!(retrieved.shelf_serial_number, "PS-UPD-ID-002");
    assert_eq!(retrieved.bmc_ip_address, "172.16.0.99");
}

#[sqlx_test]
async fn test_create_expected_power_shelf_with_explicit_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let provided_id = Uuid::new_v4().to_string();
    let expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: provided_id.clone(),
        }),
        bmc_mac_address: "AA:BB:CC:DD:EE:04".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-EXPLICIT-001".into(),
        bmc_ip_address: "".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        rack_id: None,
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf with explicit id");

    // Retrieve by MAC and verify the ID matches what we provided
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "AA:BB:CC:DD:EE:04".to_string(),
        expected_power_shelf_id: None,
    };
    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .expect("unable to retrieve expected power shelf")
        .into_inner();

    assert_eq!(
        retrieved.expected_power_shelf_id,
        Some(::rpc::common::Uuid { value: provided_id })
    );
}

#[sqlx_test]
async fn test_create_expected_power_shelf_auto_generates_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let expected_power_shelf = rpc::forge::ExpectedPowerShelf {
        expected_power_shelf_id: None,
        bmc_mac_address: "AA:BB:CC:DD:EE:05".to_string(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        shelf_serial_number: "PS-AUTO-001".into(),
        bmc_ip_address: "".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        rack_id: None,
        bmc_retain_credentials: None,
    };

    env.api()
        .add_expected_power_shelf(tonic::Request::new(expected_power_shelf.clone()))
        .await
        .expect("unable to add expected power shelf without id");

    // Retrieve by MAC and verify an ID was auto-generated
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "AA:BB:CC:DD:EE:05".to_string(),
        expected_power_shelf_id: None,
    };
    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .expect("unable to retrieve expected power shelf")
        .into_inner();

    assert!(
        retrieved.expected_power_shelf_id.is_some(),
        "expected_power_shelf_id should be auto-generated when not provided"
    );
    assert!(
        !retrieved
            .expected_power_shelf_id
            .as_ref()
            .unwrap()
            .value
            .is_empty(),
        "auto-generated expected_power_shelf_id should not be empty"
    );
}

#[sqlx_test]
async fn test_get_expected_power_shelf_by_id_not_found(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let non_existent_id = Uuid::new_v4().to_string();
    let get_req = rpc::forge::ExpectedPowerShelfRequest {
        bmc_mac_address: "".to_string(),
        expected_power_shelf_id: Some(::rpc::common::Uuid {
            value: non_existent_id.clone(),
        }),
    };

    let err = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(get_req))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_power_shelf not found: {}", non_existent_id)
    );
}

/// When `bmc_retain_credentials` is set to true, the value should persist through
/// add -> get round-trip via the RPC API.
#[sqlx_test]
async fn test_add_expected_power_shelf_with_bmc_retain_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:80".parse().unwrap();

    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-RETAIN-001".into(),
            bmc_ip_address: String::new(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: Some(true),
        }))
        .await?;

    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelfRequest {
            bmc_mac_address: bmc_mac.to_string(),
            expected_power_shelf_id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(
        retrieved.bmc_retain_credentials,
        Some(true),
        "bmc_retain_credentials should be true after round-trip"
    );

    Ok(())
}

/// Verify that updating an expected power shelf without specifying `bmc_retain_credentials`
/// preserves the existing value (and that COALESCE works).
#[sqlx_test]
async fn test_update_expected_power_shelf_preserves_bmc_retain_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:81".parse().unwrap();

    // Create with bmc_retain_credentials = true.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-RETAIN-UPD-001".into(),
            bmc_ip_address: String::new(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: Some(true),
        }))
        .await?;

    // Update without setting bmc_retain_credentials (None).
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "NEW-ADMIN".into(),
            bmc_password: "NEW-PASS".into(),
            shelf_serial_number: "PS-RETAIN-UPD-001".into(),
            bmc_ip_address: String::new(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    let retrieved = env
        .api()
        .get_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelfRequest {
            bmc_mac_address: bmc_mac.to_string(),
            expected_power_shelf_id: None,
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
