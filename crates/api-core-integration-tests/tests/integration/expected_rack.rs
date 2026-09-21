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

use carbide_api_core::test_support::default_config;
use carbide_test_harness::CarbideConfig;
use carbide_test_harness::prelude::*;
use carbide_uuid::rack::{RackId, RackProfileId};
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackProductFamily, RackProfile, RackProfileConfig,
};
use rpc::forge::{ExpectedRackList, ExpectedRackRequest};

fn config_with_rack_profiles() -> CarbideConfig {
    let mut config = default_config::get();
    config.rack_profiles = RackProfileConfig {
        rack_profiles: [
            (
                "NVL72".to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    rack_capabilities: RackCapabilitiesSet {
                        compute: RackCapabilityCompute {
                            name: Some("GB200".to_string()),
                            count: 18,
                            vendor: Some("NVIDIA".to_string()),
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                        switch: RackCapabilitySwitch {
                            name: None,
                            count: 9,
                            vendor: None,
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                        power_shelf: RackCapabilityPowerShelf {
                            name: None,
                            count: 4,
                            vendor: None,
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                    },
                    ..Default::default()
                },
            ),
            (
                "NVL36".to_string(),
                RackProfile {
                    product_family: Some(RackProductFamily::Gb200),
                    rack_capabilities: RackCapabilitiesSet {
                        compute: RackCapabilityCompute {
                            name: None,
                            count: 9,
                            vendor: None,
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                        switch: RackCapabilitySwitch {
                            name: None,
                            count: 4,
                            vendor: None,
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                        power_shelf: RackCapabilityPowerShelf {
                            name: None,
                            count: 2,
                            vendor: None,
                            slot_ids: None,
                            attributes: Default::default(),
                        },
                    },
                    ..Default::default()
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    config
}

async fn env_with_rack_profiles(pool: PgPool) -> TestHarness {
    let config = config_with_rack_profiles();
    TestHarness::builder(pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(config.into()))
        .build()
        .await
}

fn new_rack_id() -> RackId {
    RackId::new(uuid::Uuid::new_v4().to_string())
}

#[sqlx_test]
async fn test_add_expected_rack(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: Some(RackProfileId::new("NVL72")),
        metadata: Some(rpc::forge::Metadata {
            name: "test-rack".to_string(),
            description: "A test NVL72 rack".to_string(),
            labels: vec![rpc::forge::Label {
                key: "env".to_string(),
                value: Some("test".to_string()),
            }],
        }),
    };

    env.api()
        .add_expected_rack(tonic::Request::new(expected_rack.clone()))
        .await
        .expect("unable to add expected rack");

    let retrieved = env
        .api()
        .get_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .expect("unable to retrieve expected rack")
        .into_inner();

    assert_eq!(retrieved.rack_id, Some(rack_id));
    assert_eq!(
        retrieved.rack_profile_id.as_ref().unwrap().as_str(),
        "NVL72"
    );
    assert_eq!(retrieved.metadata.as_ref().unwrap().name, "test-rack");
}

#[sqlx_test]
async fn test_add_expected_rack_invalid_type(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: Some(RackProfileId::new("INVALID_TYPE")),
        metadata: None,
    };

    let err = env
        .api()
        .add_expected_rack(tonic::Request::new(expected_rack))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("unknown rack_profile_id"),
        "Expected error about unknown rack_profile_id, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_add_expected_rack_empty_type(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: Some(RackProfileId::new("")),
        metadata: None,
    };

    let err = env
        .api()
        .add_expected_rack(tonic::Request::new(expected_rack))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("rack_profile_id is required"),
        "Expected error about empty rack_profile_id, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_add_expected_rack_missing_rack_id(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: None,
        rack_profile_id: Some(RackProfileId::new("NVL72")),
        metadata: None,
    };

    let err = env
        .api()
        .add_expected_rack(tonic::Request::new(expected_rack))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("rack_id"),
        "Expected error about missing rack_id, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_get_expected_rack_not_found(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let rack_id = new_rack_id();
    let err = env
        .api()
        .get_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("not found"),
        "Expected not found error, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_delete_expected_rack(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: Some(RackProfileId::new("NVL72")),
        metadata: None,
    };

    env.api()
        .add_expected_rack(tonic::Request::new(expected_rack))
        .await
        .expect("unable to add expected rack");

    env.api()
        .delete_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .expect("unable to delete expected rack");

    let err = env
        .api()
        .get_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .unwrap_err();

    assert!(err.message().contains("not found"));
}

#[sqlx_test]
async fn test_delete_expected_rack_not_found(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let rack_id = new_rack_id();
    let err = env
        .api()
        .delete_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("not found"),
        "Expected not found error, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_update_expected_rack(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();

    // Add a rack first.
    env.api()
        .add_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(RackProfileId::new("NVL72")),
            metadata: Some(rpc::forge::Metadata {
                name: "original".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .expect("unable to add expected rack");

    // Update it.
    env.api()
        .update_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(RackProfileId::new("NVL36")),
            metadata: Some(rpc::forge::Metadata {
                name: "updated".to_string(),
                description: "Updated rack".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .expect("unable to update expected rack");

    let retrieved = env
        .api()
        .get_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: rack_id.to_string(),
        }))
        .await
        .expect("unable to get expected rack")
        .into_inner();

    assert_eq!(
        retrieved.rack_profile_id.as_ref().unwrap().as_str(),
        "NVL36"
    );
    assert_eq!(retrieved.metadata.as_ref().unwrap().name, "updated");
    assert_eq!(
        retrieved.metadata.as_ref().unwrap().description,
        "Updated rack"
    );
}

#[sqlx_test]
async fn test_update_expected_rack_not_found(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let err = env
        .api()
        .update_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(RackProfileId::new("NVL72")),
            metadata: None,
        }))
        .await
        .unwrap_err();

    assert!(
        err.message().contains("not found"),
        "Expected not found error, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_get_all_expected_racks(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    // Start with none.
    let all = env
        .api()
        .get_all_expected_racks(tonic::Request::new(()))
        .await
        .expect("unable to get all expected racks")
        .into_inner();
    assert_eq!(all.expected_racks.len(), 0);

    // Add two.
    for i in 0..2 {
        let rack_id = new_rack_id();
        env.api()
            .add_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
                rack_id: Some(rack_id),
                rack_profile_id: Some(RackProfileId::new("NVL72")),
                metadata: Some(rpc::forge::Metadata {
                    name: format!("rack-{}", i),
                    ..Default::default()
                }),
            }))
            .await
            .expect("unable to add expected rack");
    }

    let all = env
        .api()
        .get_all_expected_racks(tonic::Request::new(()))
        .await
        .expect("unable to get all expected racks")
        .into_inner();
    assert_eq!(all.expected_racks.len(), 2);
}

#[sqlx_test]
async fn test_add_expected_rack_duplicate(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    let rack_id = new_rack_id();
    let expected_rack = rpc::forge::ExpectedRack {
        rack_id: Some(rack_id.clone()),
        rack_profile_id: Some(RackProfileId::new("NVL72")),
        metadata: None,
    };

    env.api()
        .add_expected_rack(tonic::Request::new(expected_rack.clone()))
        .await
        .expect("unable to add expected rack");

    // Adding the same rack again should fail.
    let err = env
        .api()
        .add_expected_rack(tonic::Request::new(expected_rack))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::AlreadyExists);
    assert!(
        err.message().contains("already exists"),
        "Expected already exists error, got: {}",
        err.message()
    );
}

#[sqlx_test]
async fn test_replace_all_expected_racks(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    // Add one initial rack.
    let initial_rack_id = new_rack_id();
    env.api()
        .add_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
            rack_id: Some(initial_rack_id.clone()),
            rack_profile_id: Some(RackProfileId::new("NVL72")),
            metadata: None,
        }))
        .await
        .expect("unable to add expected rack");

    // Replace all with two new racks.
    let rack_id_1 = new_rack_id();
    let rack_id_2 = new_rack_id();
    let replacement = ExpectedRackList {
        expected_racks: vec![
            rpc::forge::ExpectedRack {
                rack_id: Some(rack_id_1),
                rack_profile_id: Some(RackProfileId::new("NVL72")),
                metadata: Some(rpc::forge::Metadata {
                    name: "replacement-1".to_string(),
                    ..Default::default()
                }),
            },
            rpc::forge::ExpectedRack {
                rack_id: Some(rack_id_2),
                rack_profile_id: Some(RackProfileId::new("NVL36")),
                metadata: Some(rpc::forge::Metadata {
                    name: "replacement-2".to_string(),
                    ..Default::default()
                }),
            },
        ],
    };

    env.api()
        .replace_all_expected_racks(tonic::Request::new(replacement))
        .await
        .expect("unable to replace all expected racks");

    let all = env
        .api()
        .get_all_expected_racks(tonic::Request::new(()))
        .await
        .expect("unable to get all expected racks")
        .into_inner();
    assert_eq!(all.expected_racks.len(), 2);

    // The initial rack should be gone.
    let err = env
        .api()
        .get_expected_rack(tonic::Request::new(ExpectedRackRequest {
            rack_id: initial_rack_id.to_string(),
        }))
        .await
        .unwrap_err();
    assert!(err.message().contains("not found"));
}

#[sqlx_test]
async fn test_delete_all_expected_racks(pool: PgPool) {
    let env = env_with_rack_profiles(pool).await;

    // Add two racks.
    for _ in 0..2 {
        let rack_id = new_rack_id();
        env.api()
            .add_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
                rack_id: Some(rack_id),
                rack_profile_id: Some(RackProfileId::new("NVL72")),
                metadata: None,
            }))
            .await
            .expect("unable to add expected rack");
    }

    let all = env
        .api()
        .get_all_expected_racks(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(all.expected_racks.len(), 2);

    // Delete all.
    env.api()
        .delete_all_expected_racks(tonic::Request::new(()))
        .await
        .expect("unable to delete all expected racks");

    let all = env
        .api()
        .get_all_expected_racks(tonic::Request::new(()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(all.expected_racks.len(), 0);
}

#[sqlx_test]
async fn test_add_expected_rack_creates_rack_entry(pool: PgPool) {
    let env = env_with_rack_profiles(pool.clone()).await;

    let rack_id = new_rack_id();
    env.api()
        .add_expected_rack(tonic::Request::new(rpc::forge::ExpectedRack {
            rack_id: Some(rack_id.clone()),
            rack_profile_id: Some(RackProfileId::new("NVL72")),
            metadata: None,
        }))
        .await
        .expect("unable to add expected rack");

    // Verify the expected_rack entry was created (racks row is created lazily
    // by ensure_rack_exists when the first device is discovered).
    let mut txn = pool.acquire().await.unwrap();
    let expected = db::expected_rack::find_by_rack_id(&mut txn, &rack_id)
        .await
        .unwrap();
    assert!(expected.is_some(), "expected_rack entry should exist");
    assert_eq!(expected.unwrap().rack_profile_id.as_str(), "NVL72");
}
