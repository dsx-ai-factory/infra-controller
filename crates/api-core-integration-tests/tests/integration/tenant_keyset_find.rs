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

use ::rpc::forge as rpc;
use carbide_test_harness::prelude::*;

async fn create_tenant_keyset(
    env: &TestHarness,
    organization_id: impl Into<String>,
) -> rpc::TenantKeyset {
    let request = rpc::CreateTenantKeysetRequest {
        keyset_identifier: Some(rpc::TenantKeysetIdentifier {
            organization_id: organization_id.into(),
            keyset_id: uuid::Uuid::new_v4().to_string(),
        }),
        keyset_content: Some(rpc::TenantKeysetContent {
            public_keys: vec![rpc::TenantPublicKey {
                public_key: "public key".to_string(),
                comment: Some("key comment".to_string()),
            }],
        }),
        version: uuid::Uuid::new_v4().to_string(),
    };

    env.api()
        .create_tenant_keyset(tonic::Request::new(request))
        .await
        .expect("tenant keyset creation should succeed")
        .into_inner()
        .keyset
        .expect("created tenant keyset response must include the keyset")
}

#[sqlx_test]
async fn test_find_tenant_keyset_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    for i in 0..4 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        create_tenant_keyset(&env, tenant_org_id.to_string()).await;
    }

    // test getting all ids
    let request_all = tonic::Request::new(rpc::TenantKeysetSearchFilter {
        tenant_org_id: None,
    });

    let ids_all = env
        .api()
        .find_tenant_keyset_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.keyset_ids.len(), 4);

    // test search by tenant_org_id
    let request_tenant = tonic::Request::new(rpc::TenantKeysetSearchFilter {
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids_tenant = env
        .api()
        .find_tenant_keyset_ids(request_tenant)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_tenant.keyset_ids.len(), 2);
}

#[sqlx_test]
async fn test_find_tenant_keysets_by_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let mut keyset1 = rpc::TenantKeyset::default();
    let mut keyset3 = rpc::TenantKeyset::default();
    for i in 0..4 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        let keyset = create_tenant_keyset(&env, tenant_org_id.to_string()).await;
        if i == 1 {
            keyset1 = keyset
        } else if i == 3 {
            keyset3 = keyset;
        }
    }

    // test search by tenant_org_id
    let request_ids = tonic::Request::new(rpc::TenantKeysetSearchFilter {
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids = env
        .api()
        .find_tenant_keyset_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids.keyset_ids.len(), 2);

    let request_keysets = tonic::Request::new(rpc::TenantKeysetsByIdsRequest {
        keyset_ids: ids.keyset_ids.clone(),
        include_key_data: false,
    });

    let keysets = env
        .api()
        .find_tenant_keysets_by_ids(request_keysets)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(keysets.keyset.len(), 2);

    let mut keyset1_valid = false;
    let mut keyset3_valid = false;
    for keyset in keysets.keyset {
        if keyset.keyset_identifier.eq(&keyset1.keyset_identifier) {
            keyset1_valid = true;
        } else if keyset.keyset_identifier.eq(&keyset3.keyset_identifier) {
            keyset3_valid = true;
        }
    }
    assert!(keyset1_valid);
    assert!(keyset3_valid);
}

// The empty-list and over-max guards for `find_tenant_keysets_by_ids` are shared
// API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.
