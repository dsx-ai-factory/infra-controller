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
use std::ops::DerefMut;

use ::rpc::forge as rpc;
use carbide_test_harness::prelude::{PgPool, ResourcePoolBuilder, TestHarness, sqlx_test};
use carbide_test_harness::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use carbide_uuid::vpc::VpcId;
use model::resource_pool::Range;
use model::resource_pool::common::VPC_VNI;
use rpc::forge_server::Forge;

async fn test_harness(pool: PgPool) -> TestHarness {
    let mut resource_pools = ResourcePoolBuilder::default().build();
    resource_pools
        .get_mut(VPC_VNI)
        .expect("VPC VNI resource pool must exist")
        .ranges = vec![
        Range {
            start: "20001".to_string(),
            end: "20005".to_string(),
            auto_assign: true,
        },
        Range {
            start: "60001".to_string(),
            end: "60005".to_string(),
            auto_assign: false,
        },
    ];
    TestHarness::builder(pool)
        .with_resource_pools(resource_pools)
        .build()
        .await
}

async fn create_vpc(env: &TestHarness, name: String) -> (VpcId, rpc::Vpc) {
    let vpc_id = VpcId::new();
    let vpc = env
        .api()
        .create_vpc(
            rpc::VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .id(vpc_id)
                .metadata(rpc::Metadata {
                    name,
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();
    (vpc_id, vpc)
}

#[sqlx_test]
async fn test_find_vpc_ids(pool: PgPool) {
    let env = test_harness(pool).await;

    for i in 0..4 {
        let (_vpc_id, _vpc) = create_vpc(&env, format!("vpc_{i}")).await;
    }

    // test getting all ids
    let request_all = tonic::Request::new(rpc::VpcSearchFilter {
        name: None,
        tenant_org_id: None,
        label: None,
    });

    let vpc_ids_all = env
        .api()
        .find_vpc_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_ids_all.vpc_ids.len(), 4);

    // test getting ids based on name
    let request_name = tonic::Request::new(rpc::VpcSearchFilter {
        name: Some("vpc_2".to_string()),
        tenant_org_id: None,
        label: None,
    });

    let vpc_ids_name = env
        .api()
        .find_vpc_ids(request_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_ids_name.vpc_ids.len(), 1);

    // test search by tenant_org_id
    let request_tenant = tonic::Request::new(rpc::VpcSearchFilter {
        name: None,
        tenant_org_id: Some(FIXTURE_TENANT_ORG_ID.to_string()),
        label: None,
    });

    let vpc_ids_tenant = env
        .api()
        .find_vpc_ids(request_tenant)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_ids_tenant.vpc_ids.len(), 4);

    // test search by tenant_org_id and name
    let request_tenant_name = tonic::Request::new(rpc::VpcSearchFilter {
        name: Some("vpc_2".to_string()),
        tenant_org_id: Some(FIXTURE_TENANT_ORG_ID.to_string()),
        label: None,
    });

    let vpc_ids_tenant_name = env
        .api()
        .find_vpc_ids(request_tenant_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_ids_tenant_name.vpc_ids.len(), 1);
}

#[sqlx_test]
async fn test_find_vpcs_by_ids(pool: PgPool) {
    let env = test_harness(pool).await;

    let mut vpc3 = rpc::Vpc::default();
    for i in 0..4 {
        let (_vpc_id, vpc) = create_vpc(&env, format!("vpc_{i}")).await;
        if i == 3 {
            vpc3 = vpc;
        }
    }

    let request_ids = tonic::Request::new(rpc::VpcSearchFilter {
        name: Some("vpc_3".to_string()),
        tenant_org_id: None,
        label: None,
    });

    let vpc_ids_list = env
        .api()
        .find_vpc_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_ids_list.vpc_ids.len(), 1);

    let request_vpcs = tonic::Request::new(rpc::VpcsByIdsRequest {
        vpc_ids: vpc_ids_list.vpc_ids.clone(),
    });

    let vpc_list = env
        .api()
        .find_vpcs_by_ids(request_vpcs)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(vpc_list.vpcs.len(), 1);

    assert_eq!(vpc3, vpc_list.vpcs[0]);
}

// The empty-list and over-max guards for `find_vpcs_by_ids` are shared API-layer
// code, proven once across representative RPCs in `tests::find_by_ids_guards`.

#[sqlx_test]
async fn find_vpc_by_name(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let vpc_id = VpcId::new();

    sqlx::query(r#"
        INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, 'test vpc 1', $2, 'V1-T1666644937952267');
    "#).bind(vpc_id).bind(FIXTURE_TENANT_ORG_ID).execute(txn.deref_mut()).await?;

    let some_vpc = db::vpc::find_by_name(txn.as_mut(), "test vpc 1").await?;

    assert_eq!(1, some_vpc.len());

    let first = some_vpc.first();

    assert!(matches!(first, Some(x) if x.id == vpc_id));
    txn.commit().await?;

    Ok(())
}

async fn find_vpc_by_request(
    env: &TestHarness,
    label: rpc::Label,
    name: Option<String>,
) -> rpc::VpcList {
    let request = tonic::Request::new(rpc::VpcSearchFilter {
        name,
        tenant_org_id: None,
        label: Some(label),
    });

    let mut vpc_id = env
        .api()
        .find_vpc_ids(request)
        .await
        .map(|response| response.into_inner())
        .unwrap()
        .vpc_ids;

    if !vpc_id.is_empty() {
        env.api()
            .find_vpcs_by_ids(tonic::Request::new(rpc::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id.remove(0)],
            }))
            .await
            .map(|response| response.into_inner())
            .unwrap()
    } else {
        rpc::VpcList { vpcs: vec![] }
    }
}

#[sqlx_test]
async fn test_vpc_search_based_on_labels(pool: PgPool) {
    let env = test_harness(pool).await;

    for i in 0..=3 {
        env.api()
            .create_vpc(
                rpc::VpcCreationRequest::builder("")
                    .metadata(rpc::Metadata {
                        name: format!("VPC_{i}{i}{i}").to_string(),
                        description: format!("VPC_{i}{i}{i} have labels").to_string(),
                        labels: vec![
                            rpc::Label {
                                key: format!("key_A_{i}{i}{i}").to_string(),
                                value: Some(format!("value_A_{i}{i}{i}").to_string()),
                            },
                            rpc::Label {
                                key: format!("key_B_{i}{i}{i}").to_string(),
                                value: None,
                            },
                        ],
                    })
                    .tonic_request(),
            )
            .await
            .unwrap()
            .into_inner();
    }

    // Test searching based on value.
    let search_label = rpc::Label {
        key: "".to_string(),
        value: Some("value_A_000".to_string()),
    };

    let vpc_matched_by_label = find_vpc_by_request(&env, search_label, None)
        .await
        .vpcs
        .remove(0);
    assert_eq!(vpc_matched_by_label.metadata.unwrap().name, "VPC_000");

    // Test searching based on key.
    let search_label = rpc::Label {
        key: "key_A_111".to_string(),
        value: None,
    };

    let vpc_matched_by_label = find_vpc_by_request(&env, search_label, None)
        .await
        .vpcs
        .remove(0);
    assert_eq!(vpc_matched_by_label.metadata.unwrap().name, "VPC_111");

    // Test searching based on key and value.
    let search_label = rpc::Label {
        key: "key_A_222".to_string(),
        value: Some("value_A_222".to_string()),
    };

    let vpc_matched_by_label = find_vpc_by_request(&env, search_label, None)
        .await
        .vpcs
        .remove(0);
    assert_eq!(vpc_matched_by_label.metadata.unwrap().name, "VPC_222");

    // Test searching based on key and name.
    let search_label = rpc::Label {
        key: "key_A_222".to_string(),
        value: None,
    };

    let vpc_matched_by_label = find_vpc_by_request(&env, search_label, Some("VPC_222".to_string()))
        .await
        .vpcs
        .remove(0);
    assert_eq!(vpc_matched_by_label.metadata.unwrap().name, "VPC_222");

    // Test searching based on key and name.
    let search_label = rpc::Label {
        key: "key_A_333".to_string(),
        value: None,
    };

    let vpc_matched_by_label =
        find_vpc_by_request(&env, search_label, Some("VPC_222".to_string())).await;
    assert_eq!(vpc_matched_by_label.vpcs.len(), 0);
}

#[sqlx_test]
async fn test_vpc_find_by_segment(pool: PgPool) {
    let env = test_harness(pool).await;
    let (created_vpc_id, _) = create_vpc(&env, "test vpc 1".to_string()).await;
    let domain = env.test_domain().await;
    let segment_id = env
        .network_controller()
        .create_tenant_segment(&domain, created_vpc_id)
        .await
        .id;

    let mut txn = env.db_txn().await;
    let vpc_id = db::vpc::find_by_name(txn.as_mut(), "test vpc 1")
        .await
        .unwrap()
        .first()
        .unwrap()
        .id;
    let vpc = db::vpc::find_by_segment(txn.as_mut(), segment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vpc.id.to_string(), vpc_id.to_string());
    txn.rollback().await.unwrap();
}
