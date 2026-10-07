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

use carbide_uuid::rack::RackId;
use common::api_fixtures::create_test_env;
use common::api_fixtures::site_explorer::create_expected_switches;
use mac_address::MacAddress;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ExpectedSwitchList, ExpectedSwitchRequest};
use rpc::forge_api_client::EXPECTED_SWITCH_UPDATE_MASK_HEADER;

use crate::tests::common;

#[crate::sqlx_test()]
async fn test_add_expected_switch(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    for mut expected_switch in [
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
            nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:4F".to_string()],
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-TEST-001".into(),
            ..Default::default()
        },
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: "3A:3B:3C:3D:3E:40".to_string(),
            nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:40".to_string()],
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-TEST-002".into(),
            nvos_username: Some("nvos_user".into()),
            nvos_password: Some("nvos_pass".into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        },
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: "3A:3B:3C:3D:3E:41".to_string(),
            nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:41".to_string()],
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-TEST-003".into(),
            nvos_username: Some("nvos_user2".into()),
            nvos_password: Some("nvos_pass2".into()),
            metadata: Some(rpc::forge::Metadata {
                name: "switch-a".to_string(),
                description: "Test switch".to_string(),
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
            ..Default::default()
        },
    ] {
        env.api
            .add_expected_switch(tonic::Request::new(expected_switch.clone()))
            .await
            .expect("unable to add expected switch ");

        let expected_switch_query = rpc::forge::ExpectedSwitchRequest {
            bmc_mac_address: expected_switch.bmc_mac_address.clone(),
            expected_switch_id: None,
        };

        let mut retrieved_expected_switch = env
            .api
            .get_expected_switch(tonic::Request::new(expected_switch_query))
            .await
            .expect("unable to retrieve expected switch ")
            .into_inner();
        retrieved_expected_switch
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        if expected_switch.metadata.is_none() {
            expected_switch.metadata = Some(Default::default());
        }
        // The server generates an ID if one wasn't provided.
        expected_switch.expected_switch_id = retrieved_expected_switch.expected_switch_id.clone();

        assert_eq!(retrieved_expected_switch, expected_switch);
    }
}

#[crate::sqlx_test()]
async fn test_add_expected_switch_validates_nvos_credentials_pair(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    for (nvos_username, nvos_password, bmc_mac_address, expected_error) in [
        (None, None, "3A:3B:3C:3D:3E:44", None),
        (
            Some("admin".to_string()),
            None,
            "3A:3B:3C:3D:3E:42",
            Some("nvos_username and nvos_password must be set together"),
        ),
        (
            None,
            Some("nvos-pass".to_string()),
            "3A:3B:3C:3D:3E:43",
            Some("nvos_username and nvos_password must be set together"),
        ),
        (
            Some(String::new()),
            Some("nvos-pass".to_string()),
            "3A:3B:3C:3D:3E:45",
            Some("nvos_username must not be empty"),
        ),
        (
            Some("admin".to_string()),
            Some(String::new()),
            "3A:3B:3C:3D:3E:46",
            Some("nvos_password must not be empty"),
        ),
    ] {
        let result = env
            .api
            .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
                bmc_mac_address: bmc_mac_address.into(),
                nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:42".to_string()],
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                switch_serial_number: "SW-NVOS-PARTIAL".into(),
                nvos_username,
                nvos_password,
                ..Default::default()
            }))
            .await;

        if let Some(expected_error) = expected_error {
            let err = result.expect_err("invalid NVOS credentials should be rejected");

            assert_eq!(err.code(), tonic::Code::InvalidArgument);
            assert_eq!(err.message(), expected_error);
        } else {
            result.expect("absent NVOS credentials should be accepted");
        }
    }
}

#[crate::sqlx_test]
async fn test_add_expected_switch_rejects_claimed_nvos_mac(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let mut expected_switch = rpc::forge::ExpectedSwitch {
        bmc_mac_address: "3A:3B:3C:3D:3E:50".to_string(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:50".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-NVOS-DUP-001".into(),
        ..Default::default()
    };
    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch");

    // A second switch claiming the same NVOS MAC -- spelled with a different
    // separator and case, which macaddr comparison canonicalizes -- is rejected.
    expected_switch.bmc_mac_address = "3A:3B:3C:3D:3E:51".to_string();
    expected_switch.switch_serial_number = "SW-NVOS-DUP-002".into();
    expected_switch.nvos_mac_addresses = vec!["4a-4b-4c-4d-4e-50".to_string()];
    let status = env
        .api
        .add_expected_switch(tonic::Request::new(expected_switch))
        .await
        .expect_err("adding a switch with a claimed NVOS MAC should fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[crate::sqlx_test]
async fn test_update_expected_switch_rejects_claimed_nvos_mac(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();

    // switches[1] tries to claim an NVOS MAC that switches[0] already holds.
    let update = rpc::forge::ExpectedSwitch {
        bmc_mac_address: switches[1].bmc_mac_address.to_string(),
        nvos_mac_addresses: switches[0]
            .nvos_mac_addresses
            .iter()
            .map(|mac| mac.to_string())
            .collect(),
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: switches[1].serial_number.clone(),
        ..Default::default()
    };
    let status = env
        .api
        .update_expected_switch(tonic::Request::new(update))
        .await
        .expect_err("claiming another switch's NVOS MAC should fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[crate::sqlx_test]
async fn test_replace_all_expected_switches_rejects_intra_list_nvos_dup(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let switch = |bmc: &str, serial: &str| rpc::forge::ExpectedSwitch {
        bmc_mac_address: bmc.to_string(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:60".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: serial.into(),
        ..Default::default()
    };

    // Two switches in one replace list claiming the same NVOS MAC surface a
    // config conflict, not an internal server fault.
    let status = env
        .api
        .replace_all_expected_switches(tonic::Request::new(rpc::forge::ExpectedSwitchList {
            expected_switches: vec![
                switch("3A:3B:3C:3D:3E:60", "SW-REPLACE-001"),
                switch("3A:3B:3C:3D:3E:61", "SW-REPLACE-002"),
            ],
        }))
        .await
        .expect_err("intra-list NVOS MAC duplicate should fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

#[crate::sqlx_test]
async fn test_delete_expected_switch(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();
    let expected_switch_count = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches
        .len();

    let expected_switch_query = rpc::forge::ExpectedSwitchRequest {
        bmc_mac_address: switches[1].bmc_mac_address.to_string(),
        expected_switch_id: None,
    };
    env.api
        .delete_expected_switch(tonic::Request::new(expected_switch_query))
        .await
        .expect("unable to delete expected switch ")
        .into_inner();

    let new_expected_switch_count = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches
        .len();

    assert_eq!(new_expected_switch_count, expected_switch_count - 1);
}

#[crate::sqlx_test]
async fn test_update_expected_switch(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    txn.commit().await.unwrap();

    let bmc_mac_address: MacAddress = switches[1].bmc_mac_address;

    let nvos_mac_addresses: Vec<String> = switches[1]
        .nvos_mac_addresses
        .iter()
        .map(|m| m.to_string())
        .collect();
    for mut updated_switch in [
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac_address.to_string(),
            nvos_mac_addresses: nvos_mac_addresses.clone(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            switch_serial_number: "SW-UPD-001".into(),
            ..Default::default()
        },
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac_address.to_string(),
            nvos_mac_addresses: nvos_mac_addresses.clone(),
            bmc_username: "ADMIN_UPDATE".into(),
            bmc_password: "PASS_UPDATE".into(),
            switch_serial_number: "SW-UPD-002".into(),
            nvos_username: Some("nvos_upd_user".into()),
            nvos_password: Some("nvos_upd_pass".into()),
            metadata: Some(Default::default()),
            ..Default::default()
        },
        rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac_address.to_string(),
            nvos_mac_addresses: nvos_mac_addresses.clone(),
            bmc_username: "ADMIN_UPDATE1".into(),
            bmc_password: "PASS_UPDATE1".into(),
            switch_serial_number: "SW-UPD-003".into(),
            nvos_username: Some("nvos_upd_user2".into()),
            nvos_password: Some("nvos_upd_pass2".into()),
            metadata: Some(rpc::forge::Metadata {
                name: "updated-switch".to_string(),
                description: "Updated switch".to_string(),
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
            ..Default::default()
        },
    ] {
        env.api
            .update_expected_switch(tonic::Request::new(updated_switch.clone()))
            .await
            .expect("unable to update expected switch ")
            .into_inner();

        let mut retrieved_expected_switch = env
            .api
            .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
                bmc_mac_address: bmc_mac_address.to_string(),
                expected_switch_id: None,
            }))
            .await
            .expect("unable to fetch expected switch ")
            .into_inner();
        retrieved_expected_switch
            .metadata
            .as_mut()
            .unwrap()
            .labels
            .sort_by(|l1, l2| l1.key.cmp(&l2.key));
        if updated_switch.metadata.is_none() {
            updated_switch.metadata = Some(Default::default());
        }
        // The server returns the ID from the database.
        updated_switch.expected_switch_id = retrieved_expected_switch.expected_switch_id.clone();

        assert_eq!(retrieved_expected_switch, updated_switch);
    }
}

#[crate::sqlx_test()]
async fn test_sparse_update_expected_switch_preserves_omitted_fields(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    txn.commit().await.unwrap();

    let bmc_mac_address: MacAddress = switches[1].bmc_mac_address;
    let original_serial_number = switches[1].serial_number.clone();
    let original_nvos_mac_addresses = switches[1].nvos_mac_addresses.clone();

    let mut request = tonic::Request::new(rpc::forge::ExpectedSwitch {
        bmc_mac_address: bmc_mac_address.to_string(),
        nvos_username: Some("nvos_upd_user".into()),
        nvos_password: Some("nvos_upd_pass".into()),
        ..Default::default()
    });

    request.metadata_mut().insert(
        EXPECTED_SWITCH_UPDATE_MASK_HEADER,
        "nvos_username,nvos_password".parse().unwrap(),
    );

    env.api
        .update_expected_switch(request)
        .await
        .expect("unable to update expected switch");

    let updated = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: bmc_mac_address.to_string(),
            expected_switch_id: None,
        }))
        .await
        .expect("unable to read updated expected switch")
        .into_inner();

    assert_eq!(updated.switch_serial_number, original_serial_number);

    assert_eq!(
        updated.nvos_mac_addresses,
        original_nvos_mac_addresses
            .into_iter()
            .map(|mac| mac.to_string())
            .collect::<Vec<_>>()
    );

    assert_eq!(updated.nvos_username.as_deref(), Some("nvos_upd_user"));
    assert_eq!(updated.nvos_password.as_deref(), Some("nvos_upd_pass"));
}

#[crate::sqlx_test()]
async fn test_get_expected_switch_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let explicit_id = uuid::Uuid::new_v4();
    let expected_switch = rpc::forge::ExpectedSwitch {
        expected_switch_id: Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        }),
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["3A:3B:3C:3D:3E:40".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-ID-001".into(),
        nvos_username: Some("nvos_user".into()),
        nvos_password: Some("nvos_pass".into()),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch");

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "".into(),
            expected_switch_id: Some(rpc::common::Uuid {
                value: explicit_id.to_string(),
            }),
        }))
        .await
        .expect("unable to get expected switch by id")
        .into_inner();

    assert_eq!(retrieved, expected_switch);
}

#[crate::sqlx_test()]
async fn test_delete_expected_switch_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let explicit_id = uuid::Uuid::new_v4();
    let expected_switch = rpc::forge::ExpectedSwitch {
        expected_switch_id: Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        }),
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["3A:3B:3C:3D:3E:40".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-DEL-ID-001".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch");

    env.api
        .delete_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "".into(),
            expected_switch_id: Some(rpc::common::Uuid {
                value: explicit_id.to_string(),
            }),
        }))
        .await
        .expect("unable to delete expected switch by id");

    let err = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "".into(),
            expected_switch_id: Some(rpc::common::Uuid {
                value: explicit_id.to_string(),
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_switch not found: {}", explicit_id)
    );
}

#[crate::sqlx_test()]
async fn test_update_expected_switch_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let explicit_id = uuid::Uuid::new_v4();
    let expected_switch = rpc::forge::ExpectedSwitch {
        expected_switch_id: Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        }),
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["3A:3B:3C:3D:3E:40".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-UPD-ID-001".into(),
        nvos_username: Some("nvos_user".into()),
        nvos_password: Some("nvos_pass".into()),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch");

    let updated_switch = rpc::forge::ExpectedSwitch {
        expected_switch_id: Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        }),
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["3A:3B:3C:3D:3E:40".to_string()],
        bmc_username: "ADMIN_UPDATED".into(),
        bmc_password: "PASS_UPDATED".into(),
        switch_serial_number: "SW-UPD-ID-002".into(),
        nvos_username: Some("nvos_updated".into()),
        nvos_password: Some("nvos_upd_pass".into()),
        metadata: Some(rpc::forge::Metadata {
            name: "updated-switch".to_string(),
            description: "Updated via id".to_string(),
            labels: vec![rpc::forge::Label {
                key: "env".to_string(),
                value: Some("staging".to_string()),
            }],
        }),
        ..Default::default()
    };

    env.api
        .update_expected_switch(tonic::Request::new(updated_switch.clone()))
        .await
        .expect("unable to update expected switch by id");

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "".into(),
            expected_switch_id: Some(rpc::common::Uuid {
                value: explicit_id.to_string(),
            }),
        }))
        .await
        .expect("unable to get expected switch by id")
        .into_inner();

    assert_eq!(retrieved, updated_switch);
}

#[crate::sqlx_test()]
async fn test_create_expected_switch_with_explicit_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let explicit_id = uuid::Uuid::new_v4();
    let expected_switch = rpc::forge::ExpectedSwitch {
        expected_switch_id: Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        }),
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:3F".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-EXPLICIT-001".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch with explicit id");

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "3A:3B:3C:3D:3E:3F".into(),
            expected_switch_id: None,
        }))
        .await
        .expect("unable to retrieve expected switch")
        .into_inner();

    assert_eq!(
        retrieved.expected_switch_id,
        Some(rpc::common::Uuid {
            value: explicit_id.to_string(),
        })
    );
    assert_eq!(retrieved, expected_switch);
}

#[crate::sqlx_test()]
async fn test_create_expected_switch_auto_generates_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let expected_switch = rpc::forge::ExpectedSwitch {
        bmc_mac_address: "3A:3B:3C:3D:3E:3F".to_string(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:3F".to_string()],
        bmc_username: "ADMIN".into(),
        bmc_password: "PASS".into(),
        switch_serial_number: "SW-AUTO-001".into(),
        metadata: Some(rpc::forge::Metadata::default()),
        ..Default::default()
    };

    env.api
        .add_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .expect("unable to add expected switch");

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "3A:3B:3C:3D:3E:3F".into(),
            expected_switch_id: None,
        }))
        .await
        .expect("unable to retrieve expected switch")
        .into_inner();

    // Server should have auto-generated an id
    assert!(
        retrieved.expected_switch_id.is_some(),
        "expected_switch_id should be auto-generated when not provided"
    );
    // Verify the auto-generated id is a valid UUID
    let auto_id = retrieved.expected_switch_id.unwrap();
    uuid::Uuid::parse_str(&auto_id.value).expect("auto-generated id should be a valid UUID");
}

#[crate::sqlx_test()]
async fn test_get_expected_switch_by_id_not_found(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let nonexistent_id = uuid::Uuid::new_v4();
    let err = env
        .api
        .get_expected_switch(tonic::Request::new(ExpectedSwitchRequest {
            bmc_mac_address: "".into(),
            expected_switch_id: Some(rpc::common::Uuid {
                value: nonexistent_id.to_string(),
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_switch not found: {}", nonexistent_id)
    );
}

#[crate::sqlx_test]
async fn test_get_linked_expected_switches(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.begin().await.unwrap();
    let _ = create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();

    let out = env
        .api
        .get_all_expected_switches_linked(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(out.expected_switches.len(), 6);
    // They are sorted by MAC server-side
    let es = out.expected_switches.first().unwrap();
    assert_eq!(es.switch_serial_number, "SW-SN-001");
    assert!(
        es.switch_id.is_none(),
        "expected_switches fixture should have no linked switch"
    );
}

#[crate::sqlx_test()]
async fn test_delete_expected_switch_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_switch_request = rpc::forge::ExpectedSwitchRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        expected_switch_id: None,
    };

    let err = env
        .api
        .delete_expected_switch(tonic::Request::new(expected_switch_request))
        .await
        .unwrap_err();

    assert_eq!(
        err.message().to_string(),
        format!("expected_switch not found: {}", bmc_mac_address)
    );
}

#[crate::sqlx_test()]
async fn test_update_expected_switch_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_switch = rpc::forge::ExpectedSwitch {
        bmc_mac_address: bmc_mac_address.to_string(),
        nvos_mac_addresses: vec!["3A:3B:3C:3D:3E:3F".to_string()],
        bmc_username: "ADMIN_UPDATE".into(),
        bmc_password: "PASS_UPDATE".into(),
        switch_serial_number: "SW-UPD-001".into(),
        ..Default::default()
    };

    let err = env
        .api
        .update_expected_switch(tonic::Request::new(expected_switch.clone()))
        .await
        .unwrap_err();

    assert!(
        err.message().contains(&bmc_mac_address.to_string()),
        "Error should reference the MAC address: {}",
        err.message()
    );
}

#[crate::sqlx_test()]
async fn test_get_expected_switch_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let bmc_mac_address: MacAddress = "2A:2B:2C:2D:2E:2F".parse().unwrap();
    let expected_switch_query = rpc::forge::ExpectedSwitchRequest {
        bmc_mac_address: bmc_mac_address.to_string(),
        expected_switch_id: None,
    };

    let err = env
        .api
        .get_expected_switch(tonic::Request::new(expected_switch_query))
        .await
        .unwrap_err();

    assert!(
        err.message().contains(&bmc_mac_address.to_string()),
        "Error should reference the MAC address: {}",
        err.message()
    );
}

#[crate::sqlx_test]
async fn test_delete_all_expected_switches(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let _ = create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();

    let mut expected_switch_count = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches
        .len();

    assert_eq!(expected_switch_count, 6);

    env.api
        .delete_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to delete all expected switches")
        .into_inner();

    expected_switch_count = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches
        .len();

    assert_eq!(expected_switch_count, 0);
}

#[crate::sqlx_test]
async fn test_replace_all_expected_switches(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
    create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();

    let expected_switch_count = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches
        .len();

    assert_eq!(expected_switch_count, 6);

    let mut expected_switch_list = ExpectedSwitchList {
        expected_switches: Vec::new(),
    };

    let expected_switch_1 = rpc::forge::ExpectedSwitch {
        bmc_mac_address: "6A:6B:6C:6D:6E:6F".into(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:6F".to_string()],
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        switch_serial_number: "SW-NEW-001".into(),
        nvos_username: Some("nvos_new".into()),
        nvos_password: Some("nvos_new_pass".into()),
        metadata: Some(rpc::Metadata::default()),
        rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
        ..Default::default()
    };

    let expected_switch_2 = rpc::forge::ExpectedSwitch {
        bmc_mac_address: "7A:7B:7C:7D:7E:7F".into(),
        nvos_mac_addresses: vec!["4A:4B:4C:4D:4E:7F".to_string()],
        bmc_username: "ADMIN_NEW".into(),
        bmc_password: "PASS_NEW".into(),
        switch_serial_number: "SW-NEW-002".into(),
        metadata: Some(rpc::Metadata::default()),
        rack_id: Some(RackId::new(uuid::Uuid::new_v4().to_string())),
        ..Default::default()
    };

    expected_switch_list
        .expected_switches
        .push(expected_switch_1.clone());
    expected_switch_list
        .expected_switches
        .push(expected_switch_2.clone());

    env.api
        .replace_all_expected_switches(tonic::Request::new(expected_switch_list))
        .await
        .expect("unable to replace all expected switches")
        .into_inner();

    let expected_switches = env
        .api
        .get_all_expected_switches(tonic::Request::new(()))
        .await
        .expect("unable to get all expected switches")
        .into_inner()
        .expected_switches;

    assert_eq!(expected_switches.len(), 2);
    // Server generates IDs, so compare by serial number.
    assert!(
        expected_switches
            .iter()
            .any(|s| s.switch_serial_number == expected_switch_1.switch_serial_number)
    );
    assert!(
        expected_switches
            .iter()
            .any(|s| s.switch_serial_number == expected_switch_2.switch_serial_number)
    );
}

/// Verify that find_all_linked joins on bmc_mac_address (not serial_number = config.name).
#[crate::sqlx_test]
async fn test_find_all_linked_joins_on_bmc_mac(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    // new_switch creates expected switches and a managed switch linked by bmc_mac_address.
    // The managed switch config.name is the expected switch's metadata name, NOT the serial number.
    let switch_id = common::api_fixtures::site_explorer::new_switch(&env, None, None)
        .await
        .unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let linked = db::expected_switch::find_all_linked(&mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // At least one expected switch should be linked to the managed switch.
    let linked_switch = linked.iter().find(|l| l.switch_id.is_some());
    assert!(
        linked_switch.is_some(),
        "expected at least one linked switch, but all were unlinked"
    );
    assert_eq!(
        linked_switch.unwrap().switch_id.unwrap().to_string(),
        switch_id.to_string(),
    );
}

#[crate::sqlx_test]
async fn test_find_one_linked_returns_explored_endpoint_address(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.begin().await.unwrap();
    create_expected_switches(&mut txn).await;

    let linked = db::expected_switch::find_one_linked(&mut txn)
        .await
        .unwrap()
        .expect("expected a linked switch row");
    assert_eq!(linked.address, None);

    let address =
        db::machine_interface::lookup_bmc_ip_by_mac_address(&mut *txn, linked.bmc_mac_address)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("expected a BMC interface address");

    db::explored_endpoints::insert(address, &Default::default(), false, &mut txn)
        .await
        .unwrap();

    let linked = db::expected_switch::find_one_linked(&mut txn)
        .await
        .unwrap()
        .expect("expected a linked switch row");
    assert_eq!(linked.address, Some(address));

    txn.commit().await.unwrap();
}

/// Verify that update persists nvos_mac_addresses.
#[crate::sqlx_test]
async fn test_update_persists_nvos_mac_addresses(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    txn.commit().await.unwrap();

    let original = &switches[0];
    let new_nvos_mac: MacAddress = "AA:BB:CC:DD:EE:FF".parse().unwrap();

    // Update with new nvos_mac_addresses.
    let mut updated = original.clone();
    updated.nvos_mac_addresses = vec![new_nvos_mac];

    let mut txn = env.pool.begin().await.unwrap();
    db::expected_switch::update(&mut txn, &updated)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Read back and verify.
    let mut txn = env.pool.begin().await.unwrap();
    let fetched = db::expected_switch::find_by_bmc_mac_address(&mut txn, original.bmc_mac_address)
        .await
        .unwrap()
        .expect("expected switch should exist");
    txn.commit().await.unwrap();

    assert_eq!(fetched.nvos_mac_addresses, vec![new_nvos_mac]);
}

/// When an expected switch is registered with a bmc_ip_address, the static `machine_interface`
/// gets materialized lazily by site-explorer's per-iteration sweep (the gRPC `add` handler
/// doesn't preallocate inline). Verify that flow end-to-end.
#[crate::sqlx_test()]
async fn test_add_with_bmc_ip_creates_static_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:70".parse().unwrap();
    let bmc_ip = "192.0.1.180";

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-STATIC-001".into(),
            nvos_mac_addresses: vec![],
            bmc_ip_address: bmc_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    // Add doesn't preallocate inline; mimic what site-explorer does on the next iteration --
    // materialize the static BMC interface for this entity.
    carbide_site_explorer::try_preallocate_one(
        &env.pool,
        bmc_mac,
        bmc_ip.parse().unwrap(),
        model::machine_interface::InterfaceType::Bmc,
        "expected_switch BMC",
        None,
    )
    .await;

    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(
        interfaces.len(),
        1,
        "should have one interface for the BMC MAC"
    );

    let iface = &interfaces[0];
    assert!(
        iface.addresses.contains(&bmc_ip.parse().unwrap()),
        "interface should have the static BMC IP"
    );

    let addrs = db::machine_interface_address::find_for_interface(&mut txn, iface.id).await?;
    assert_eq!(addrs.len(), 1);
    assert_eq!(
        addrs[0].address,
        bmc_ip.parse::<std::net::IpAddr>().unwrap()
    );
    assert_eq!(
        addrs[0].allocation_type,
        model::allocation_type::AllocationType::Static
    );

    Ok(())
}

/// When an expected switch is created WITHOUT a bmc_ip_address,
/// no machine_interface should be created.
#[crate::sqlx_test()]
async fn test_add_without_bmc_ip_creates_no_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:71".parse().unwrap();

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NO-IP-001".into(),
            nvos_mac_addresses: vec![],
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert!(
        interfaces.is_empty(),
        "should not create interface without bmc_ip_address"
    );

    Ok(())
}

/// When `bmc_retain_credentials` is set to true, the value should persist through
/// add -> get round-trip via the RPC API.
#[crate::sqlx_test()]
async fn test_add_expected_switch_with_bmc_retain_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:80".parse().unwrap();

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-RETAIN-001".into(),
            nvos_mac_addresses: vec![],
            metadata: Some(rpc::forge::Metadata::default()),
            bmc_retain_credentials: Some(true),
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitchRequest {
            bmc_mac_address: bmc_mac.to_string(),
            expected_switch_id: None,
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

/// Verify that updating an expected switch without specifying `bmc_retain_credentials`
/// preserves the existing value (and that COALESCE works).
#[crate::sqlx_test()]
async fn test_update_expected_switch_preserves_bmc_retain_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:81".parse().unwrap();

    // Create with bmc_retain_credentials = true.
    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-RETAIN-UPD-001".into(),
            nvos_mac_addresses: vec![],
            metadata: Some(rpc::forge::Metadata::default()),
            bmc_retain_credentials: Some(true),
            ..Default::default()
        }))
        .await?;

    // Update without setting bmc_retain_credentials (None).
    env.api
        .update_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "NEW-ADMIN".into(),
            bmc_password: "NEW-PASS".into(),
            switch_serial_number: "SW-RETAIN-UPD-001".into(),
            nvos_mac_addresses: vec![],
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitchRequest {
            bmc_mac_address: bmc_mac.to_string(),
            expected_switch_id: None,
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

/// `nvos_ip_address` is paired with the single wired NVOS port. Setting it without any
/// `nvos_mac_addresses` would leave the (mac, ip) pairing undefined, so the handler must
/// reject it with `InvalidArgument`.
#[crate::sqlx_test]
async fn test_add_expected_switch_rejects_nvos_ip_without_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:60".parse().unwrap();

    let err = env
        .api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-NO-MAC".into(),
            nvos_mac_addresses: vec![],
            nvos_ip_address: Some("192.0.2.250".into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await
        .expect_err("add must reject nvos_ip_address with no nvos_mac_addresses");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// Setting `nvos_ip_address` alongside multiple `nvos_mac_addresses` is ambiguous (which MAC
/// gets the IP?), so the handler must reject it instead of silently picking one.
#[crate::sqlx_test]
async fn test_add_expected_switch_rejects_nvos_ip_with_multiple_macs(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:61".parse().unwrap();

    let err = env
        .api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-MULTI-MAC".into(),
            nvos_mac_addresses: vec![
                "8A:8B:8C:8D:8E:01".to_string(),
                "8A:8B:8C:8D:8E:02".to_string(),
            ],
            nvos_ip_address: Some("192.0.2.251".into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await
        .expect_err("add must reject nvos_ip_address with multiple nvos_mac_addresses");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// Happy path: `nvos_ip_address` paired with exactly one `nvos_mac_addresses` entry is
/// accepted by `add`, and the field round-trips through `get`.
#[crate::sqlx_test]
async fn test_add_expected_switch_with_nvos_ip_round_trips(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:62".parse().unwrap();
    let nvos_mac: MacAddress = "8A:8B:8C:8D:8E:10".parse().unwrap();
    let nvos_ip = "192.0.2.252";

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-OK".into(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            nvos_ip_address: Some(nvos_ip.into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let retrieved = env
        .api
        .get_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitchRequest {
            bmc_mac_address: bmc_mac.to_string(),
            expected_switch_id: None,
        }))
        .await?
        .into_inner();

    assert_eq!(
        retrieved.nvos_ip_address.as_deref(),
        Some(nvos_ip),
        "nvos_ip_address must survive the add -> get round-trip"
    );

    Ok(())
}

/// Symmetric to the add validation: update must also reject `nvos_ip_address` when the
/// `nvos_mac_addresses` count is not exactly one. Covers the case where an operator
/// adds a valid switch and then mutates it into an invalid pairing.
#[crate::sqlx_test]
async fn test_update_expected_switch_rejects_invalid_nvos_pairing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:63".parse().unwrap();
    let nvos_mac: MacAddress = "8A:8B:8C:8D:8E:20".parse().unwrap();

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-UPDATE".into(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let err = env
        .api
        .update_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-UPDATE".into(),
            // Drop the NVOS MAC but try to keep the IP -- pairing now invalid.
            nvos_mac_addresses: vec![],
            nvos_ip_address: Some("192.0.2.253".into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await
        .expect_err("update must reject nvos_ip_address with no nvos_mac_addresses");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    Ok(())
}

/// First DHCPDISCOVER for an `ExpectedSwitch.nvos_ip_address`: discover() consults
/// `find_by_nvos_mac_address`, preallocates from `nvos_ip_address`, and the existing
/// find_or_create path serves that static IP. Mirrors the host-NIC fixed_ip flow. Add-time
/// doesn't preallocate; row materialization is deferred until this hook fires or until
/// site-explorer's reconciliation pass runs.
#[crate::sqlx_test]
async fn test_dhcp_discover_preallocates_nvos_ip_for_unknown_mac(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:64".parse().unwrap();
    let nvos_mac: MacAddress = "8A:8B:8C:8D:8E:30".parse().unwrap();
    let nvos_ip = "192.0.2.254";

    env.api
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-NVOS-DHCP".into(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            nvos_ip_address: Some(nvos_ip.into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let before = db::machine_interface::find_by_mac_address(txn.as_mut(), nvos_mac).await?;
    assert!(
        before.is_empty(),
        "add does not preallocate inline; the interface should only appear after discover()"
    );
    txn.commit().await?;

    let nvos_mac_str = nvos_mac.to_string();
    let response = env
        .api
        .discover_dhcp(
            common::rpc_builder::DhcpDiscovery::builder(
                &nvos_mac_str,
                common::api_fixtures::FIXTURE_DHCP_RELAY_ADDRESS,
            )
            .tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.address, nvos_ip,
        "NVOS DHCP should serve the configured nvos_ip_address"
    );

    let mut txn = env.db_txn().await;
    let after = db::machine_interface::find_by_mac_address(txn.as_mut(), nvos_mac).await?;
    assert_eq!(after.len(), 1, "interface should be created by discover()");
    assert_eq!(
        after[0].interface_type,
        model::machine_interface::InterfaceType::Data,
        "NVOS discover hook should mark the interface as InterfaceType::Data, not Bmc"
    );
    assert!(
        after[0].addresses.contains(&nvos_ip.parse().unwrap()),
        "preallocated row should carry the configured nvos_ip_address"
    );

    Ok(())
}
