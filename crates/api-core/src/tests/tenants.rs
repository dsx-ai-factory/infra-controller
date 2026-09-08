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
use common::api_fixtures::{TestEnv, create_test_env};
use rpc::forge::forge_server::Forge;
use rpc::forge::{CreateTenantKeysetResponse, TenantKeysetIdentifier};
use tonic::Code;

use crate::tests::common;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env_with_overrides,
};

#[crate::sqlx_test]
async fn test_tenant(pool: sqlx::PgPool) {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        }
        .with_fnn_config(None),
    )
    .await;

    // Reject generally invalid metadata with just a name that is too short
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "x".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(tenant_create.code(), Code::InvalidArgument);

    // Reject metadata that is invalid specifically for a tenant
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "Name".to_string(),
                description: "should not be stored".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "aaa".to_string(),
                    value: Some("bbb".to_string()),
                }],
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(tenant_create.code(), Code::InvalidArgument);
    assert!(tenant_create.message().contains("description"));

    // Test the case of creating a tenant by using a known bad
    // routing-profile name.
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "Organic".to_string(),
            routing_profile_type: Some("ADMIN".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "Name".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(tenant_create.code(), Code::NotFound);
    assert!(
        tenant_create
            .message()
            .contains("RoutingProfile not found: ADMIN")
    );

    // Now perform a good create
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "Name".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant = tenant_create.tenant.unwrap();

    assert_eq!(tenant.organization_id, "Org");
    assert_eq!(
        tenant.metadata.unwrap(),
        rpc::forge::Metadata {
            name: "Name".to_string(),
            // Until/unless we actually start using labels and descriptions for Tenant,
            // these should come back empty
            labels: vec![],
            description: "".to_string(),
        }
    );

    let find_tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: "Org".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        find_tenant.permitted_routing_profile_types,
        vec!["EXTERNAL"]
    );
    let tenant = find_tenant.tenant.unwrap();

    // This fixture enables the default FNN config, so the tenant should
    // receive the default routing profile.
    assert_eq!(tenant.routing_profile_type.as_deref(), Some("EXTERNAL"));

    assert_eq!(tenant.organization_id, "Org");
    assert_eq!(
        tenant.metadata.unwrap(),
        rpc::forge::Metadata {
            name: "Name".to_string(),
            // Until/unless we actually start using labels and descriptions for Tenant,
            // these should come back empty
            labels: vec![],
            description: "".to_string(),
        }
    );

    let version = tenant.version;
    let tenant_org = tenant.organization_id;

    // Reject generally invalid metadata with just a name that is too short
    let update_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "x".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            if_version_match: Some(version.clone()),
        }))
        .await
        .unwrap_err();

    assert_eq!(update_tenant.code(), Code::InvalidArgument);

    // Reject metadata that is invalid specifically for a tenant
    let update_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "AnotherName".to_string(),
                description: "should not be stored".to_string(),
                labels: vec![rpc::forge::Label {
                    key: "aaa".to_string(),
                    value: Some("bbb".to_string()),
                }],
            }),
            if_version_match: Some(version.clone()),
        }))
        .await
        .unwrap_err();

    assert_eq!(update_tenant.code(), Code::InvalidArgument);
    assert!(update_tenant.message().contains("description"));

    // Reject an unknown routing profile name on update.
    let update_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: Some("ADMIN".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "AnotherName".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            if_version_match: Some(version.clone()),
        }))
        .await
        .unwrap_err();

    assert_eq!(update_tenant.code(), Code::NotFound);
    assert!(
        update_tenant
            .message()
            .contains("RoutingProfile not found: ADMIN")
    );

    // Create a VPC for the tenant
    // No network_virtualization_type, should default.
    let new_vpc = env
        .api
        .create_vpc(
            common::rpc_builder::VpcCreationRequest::builder(tenant_org)
                .metadata(rpc::forge::Metadata {
                    name: "Forge".to_string(),
                    description: "".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Now try to update the routing profile type and fail
    assert!(
        env.api
            .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
                organization_id: "Org".to_string(),
                routing_profile_type: Some("INTERNAL".to_string()),
                metadata: Some(rpc::forge::Metadata {
                    name: "AnotherName".to_string(),
                    description: "".to_string(),
                    labels: vec![],
                }),
                if_version_match: Some(version.clone()),
            }))
            .await
            .unwrap_err()
            .message()
            .contains("cannot update tenant routing profile type")
    );

    //
    // Make sure we get back an error if metadata isn't sent.
    //
    let update_tenant_err = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            metadata: None,
            routing_profile_type: None,
            if_version_match: Some(version.clone()),
        }))
        .await
        .unwrap_err();

    assert_eq!(update_tenant_err.code(), tonic::Code::InvalidArgument);
    assert!(update_tenant_err.message().contains("metadata"));

    // Now perform a good update that doesn't change the profile and so should
    // pass.
    let update_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            // No change from whatever it was given on create.
            routing_profile_type: tenant.routing_profile_type,
            metadata: Some(rpc::forge::Metadata {
                name: "AnotherName".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            if_version_match: Some(version.clone()),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant = update_tenant.tenant.unwrap();

    assert_eq!(tenant.organization_id, "Org");
    assert_eq!(
        tenant.metadata.unwrap(),
        rpc::forge::Metadata {
            // Make sure the name changed.
            name: "AnotherName".to_string(),
            // Until/unless we actually start using labels and descriptions for Tenant,
            // these should come back empty
            labels: vec![],
            description: "".to_string(),
        }
    );

    // Now delete the VPC we created
    let _ = env
        .api
        .delete_vpc(
            common::rpc_builder::VpcDeletionRequest::builder()
                .id(new_vpc.id.unwrap())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "Org".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "AnotherName".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            if_version_match: Some(tenant.version),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    assert_eq!(tenant.routing_profile_type.as_deref(), Some("INTERNAL"));

    let find_tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: "Org".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        find_tenant.permitted_routing_profile_types,
        vec!["EXTERNAL", "INTERNAL"]
    );

    // Now perform one more good create just to confirm that we can set
    // the routing profile to something other than default
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "Org2".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "Name".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant = tenant_create.tenant.unwrap();

    assert_eq!(tenant.routing_profile_type.as_deref(), Some("INTERNAL"));
    assert_eq!(tenant.organization_id, "Org2");

    // A profile can disappear from FNN config after it was persisted. Tenant
    // lookup must remain usable and expose no selectable profiles in that
    // stale state.
    sqlx::query("UPDATE tenants SET routing_profile_type = $1 WHERE organization_id = $2")
        .bind("REMOVED_PROFILE")
        .bind("Org2")
        .execute(&env.pool)
        .await
        .unwrap();

    let find_tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: "Org2".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(find_tenant.permitted_routing_profile_types.is_empty());
    assert_eq!(
        find_tenant.tenant.unwrap().routing_profile_type.as_deref(),
        Some("REMOVED_PROFILE")
    );
}

#[crate::sqlx_test]
async fn test_find_tenant_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    for x in 0..10 {
        let _tenant_create = env
            .api
            .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
                organization_id: format!("fh{x}{x}abcdw"),
                routing_profile_type: None,
                metadata: Some(rpc::forge::Metadata {
                    name: format!("tenant_{x}"),
                    description: "".to_string(),
                    labels: vec![],
                }),
            }))
            .await;
    }

    let find_tenant = env
        .api
        .find_tenant_organization_ids(tonic::Request::new(rpc::forge::TenantSearchFilter {
            tenant_organization_name: Some("tenant_3".to_string()),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant_id = find_tenant.tenant_organization_ids;

    assert_eq!(tenant_id.len(), 1);
    assert_eq!(tenant_id.first().cloned(), Some("fh33abcdw".to_string()));

    let tenant_object = env
        .api
        .find_tenants_by_organization_ids(tonic::Request::new(
            rpc::forge::TenantByOrganizationIdsRequest {
                organization_ids: vec!["fh33abcdw".to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(tenant_object.tenants.len(), 1);
    assert_eq!(
        tenant_object.tenants.first().unwrap().metadata,
        Some(rpc::forge::Metadata {
            name: "tenant_3".to_string(),
            description: "".to_string(),
            labels: vec![],
        })
    );

    let find_all_tenants = env
        .api
        .find_tenant_organization_ids(tonic::Request::new(rpc::forge::TenantSearchFilter {
            tenant_organization_name: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(find_all_tenants.tenant_organization_ids.len(), 10);
}

#[crate::sqlx_test]
async fn test_tenant_create_without_fnn(pool: sqlx::PgPool) {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            ..Default::default()
        },
    )
    .await;

    // Make sure this test is actually exercising the pre-FNN path.
    assert!(env.api.runtime_config.fnn.is_none());

    // Create a tenant without a routing profile.
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "PreFnnOrg".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: "PreFnnOrg".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant = tenant_create.tenant.unwrap();
    assert_eq!(tenant.organization_id, "PreFnnOrg");
    assert_eq!(tenant.routing_profile_type, None);

    // Look up the tenant to verify the pre-FNN create path does not persist a profile.
    let find_tenant = env
        .api
        .find_tenant(tonic::Request::new(rpc::forge::FindTenantRequest {
            tenant_organization_id: "PreFnnOrg".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(find_tenant.permitted_routing_profile_types.is_empty());
    let tenant = find_tenant.tenant.unwrap();
    assert_eq!(tenant.organization_id, "PreFnnOrg");
    assert_eq!(tenant.routing_profile_type, None);

    // Updating a tenant with a routing profile while FNN is disabled should fail.
    let update_tenant = env
        .api
        .update_tenant(tonic::Request::new(rpc::forge::UpdateTenantRequest {
            organization_id: "PreFnnOrg".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "PreFnnOrg".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            if_version_match: Some(tenant.version.clone()),
        }))
        .await
        .unwrap_err();

    assert_eq!(update_tenant.code(), Code::NotFound);
    assert!(
        update_tenant
            .message()
            .contains("RoutingProfile not found: INTERNAL")
    );

    // Creating a tenant with a routing profile while FNN is disabled should fail.
    let tenant_create = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "PreFnnOrgWithProfile".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "PreFnnOrgWithProfile".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap_err();

    assert_eq!(tenant_create.code(), Code::NotFound);
    assert!(
        tenant_create
            .message()
            .contains("RoutingProfile not found: INTERNAL")
    );
}

async fn create_keyset(
    env: &TestEnv,
    organization_id: String,
    keyset_id: String,
    version: String,
    keyset_content: rpc::forge::TenantKeysetContent,
) -> CreateTenantKeysetResponse {
    env.api
        .create_tenant_keyset(tonic::Request::new(rpc::forge::CreateTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id,
                keyset_id,
            }),
            keyset_content: Some(keyset_content),
            version,
        }))
        .await
        .unwrap()
        .into_inner()
}

#[crate::sqlx_test]
async fn test_tenant_create_keyset(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let keyset = create_keyset(
        &env,
        "Org1".to_string(),
        "keyset1".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![],
        },
    )
    .await
    .keyset
    .unwrap();

    assert_eq!(
        keyset.keyset_identifier.as_ref().unwrap().organization_id,
        "Org1"
    );

    assert_eq!(
        keyset.keyset_identifier.as_ref().unwrap().keyset_id,
        "keyset1"
    );

    assert!(keyset.keyset_content.unwrap().public_keys.is_empty());
}

#[crate::sqlx_test]
async fn test_tenant_delete_keyset(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let _ = create_keyset(
        &env,
        "Org1".to_string(),
        "keyset1".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![],
        },
    )
    .await;

    let err = env
        .api
        .delete_tenant_keyset(tonic::Request::new(rpc::forge::DeleteTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset_id".to_string(),
            }),
        }))
        .await
        .expect_err("Deletion should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);

    env.api
        .delete_tenant_keyset(tonic::Request::new(rpc::forge::DeleteTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }),
        }))
        .await
        .unwrap();

    let find_result = env
        .api
        .find_tenant_keyset_ids(tonic::Request::new(rpc::forge::TenantKeysetSearchFilter {
            tenant_org_id: Some("Org1".to_string()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(find_result.keyset_ids.is_empty());
}

#[crate::sqlx_test]
async fn test_tenant_update_keyset(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let _ = create_keyset(
        &env,
        "Org1".to_string(),
        "keyset1".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![],
        },
    )
    .await;

    let find_result = env
        .api
        .find_tenant_keysets_by_ids(tonic::Request::new(rpc::forge::TenantKeysetsByIdsRequest {
            keyset_ids: vec![TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }],
            include_key_data: true,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        find_result.keyset[0]
            .keyset_content
            .as_ref()
            .unwrap()
            .public_keys
            .is_empty()
    );

    // Update to invalid version fails
    let err = env
        .api
        .update_tenant_keyset(tonic::Request::new(rpc::forge::UpdateTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }),
            keyset_content: Some(rpc::forge::TenantKeysetContent {
                public_keys: vec![
                    rpc::forge::TenantPublicKey {
                        public_key: "mypublickey1".to_string(),
                        comment: Some("comment1".to_string()),
                    },
                    rpc::forge::TenantPublicKey {
                        public_key: "mypublickey2".to_string(),
                        comment: Some("comment2".to_string()),
                    },
                ],
            }),
            version: "V2-T1691517639501030".to_string(),
            if_version_match: Some("V1-T1691517639501900".to_string()),
        }))
        .await
        .expect_err("Update should not be processed due to invalid version");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // Update to valid version and invalid keyset ID returns NotFound
    let err = env
        .api
        .update_tenant_keyset(tonic::Request::new(rpc::forge::UpdateTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset2".to_string(),
            }),
            keyset_content: Some(rpc::forge::TenantKeysetContent {
                public_keys: vec![rpc::forge::TenantPublicKey {
                    public_key: "mypublickey1".to_string(),
                    comment: Some("comment1".to_string()),
                }],
            }),
            version: "V2-T1691517639501030".to_string(),
            if_version_match: Some("V1-T1691517639501025".to_string()),
        }))
        .await
        .expect_err("Keyset should not be found");
    assert_eq!(err.code(), tonic::Code::NotFound);

    // Update to valid version succeeds
    env.api
        .update_tenant_keyset(tonic::Request::new(rpc::forge::UpdateTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }),
            keyset_content: Some(rpc::forge::TenantKeysetContent {
                public_keys: vec![rpc::forge::TenantPublicKey {
                    public_key: "mypublickey1".to_string(),
                    comment: Some("comment1".to_string()),
                }],
            }),
            version: "V2-T1691517639501030".to_string(),
            if_version_match: Some("V1-T1691517639501025".to_string()),
        }))
        .await
        .unwrap();

    let find_result = env
        .api
        .find_tenant_keysets_by_ids(tonic::Request::new(rpc::forge::TenantKeysetsByIdsRequest {
            keyset_ids: vec![TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }],
            include_key_data: true,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        find_result.keyset[0]
            .keyset_content
            .as_ref()
            .unwrap()
            .public_keys
            .len(),
        1
    );

    env.api
        .update_tenant_keyset(tonic::Request::new(rpc::forge::UpdateTenantKeysetRequest {
            keyset_identifier: Some(rpc::forge::TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }),
            keyset_content: Some(rpc::forge::TenantKeysetContent {
                public_keys: vec![
                    rpc::forge::TenantPublicKey {
                        public_key: "mypublickey1".to_string(),
                        comment: Some("comment1".to_string()),
                    },
                    rpc::forge::TenantPublicKey {
                        public_key: "mypublickey2".to_string(),
                        comment: Some("comment2".to_string()),
                    },
                ],
            }),
            version: "V3-T1691517639501030".to_string(),
            if_version_match: None,
        }))
        .await
        .unwrap();

    let find_result = env
        .api
        .find_tenant_keysets_by_ids(tonic::Request::new(rpc::forge::TenantKeysetsByIdsRequest {
            keyset_ids: vec![TenantKeysetIdentifier {
                organization_id: "Org1".to_string(),
                keyset_id: "keyset1".to_string(),
            }],
            include_key_data: true,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        find_result.keyset[0]
            .keyset_content
            .as_ref()
            .unwrap()
            .public_keys
            .len(),
        2
    );

    assert_eq!(
        "V3-T1691517639501030".to_string(),
        find_result.keyset[0].version
    );
}

#[crate::sqlx_test]
async fn test_tenant_validate_keyset(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let _keyset = create_keyset(
        &env,
        "Tenant1".to_string(),
        "keyset1".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![rpc::forge::TenantPublicKey {
                public_key: "ssh-rsa some_long_key_base64_encoded test@myname".to_string(),
                comment: Some("some random comment".to_string()),
            }],
        },
    )
    .await
    .keyset
    .unwrap();

    let _keyset = create_keyset(
        &env,
        "Tenant1".to_string(),
        "keyset2".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![rpc::forge::TenantPublicKey {
                public_key: "ssh-rsa my_another_key test@myname".to_string(),
                comment: Some("some random comment".to_string()),
            }],
        },
    )
    .await
    .keyset
    .unwrap();

    let _keyset = create_keyset(
        &env,
        "Tenant1".to_string(),
        "keyset3".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![rpc::forge::TenantPublicKey {
                public_key: "ssh-rsa my_another_keyset3 test@myname".to_string(),
                comment: Some("some random comment".to_string()),
            }],
        },
    )
    .await
    .keyset
    .unwrap();

    let _keyset = create_keyset(
        &env,
        "org1".to_string(),
        "keyset2".to_string(),
        "V1-T1691517639501025".to_string(),
        rpc::forge::TenantKeysetContent {
            public_keys: vec![rpc::forge::TenantPublicKey {
                public_key: "ssh-rsa some_long_key_base64_encoded_1 test@myname".to_string(),
                comment: Some("some random comment".to_string()),
            }],
        },
    )
    .await
    .keyset
    .unwrap();

    // Create instance
    let mh = create_managed_host(&env).await;
    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .keyset_ids(&["keyset1", "keyset2"])
        .build()
        .await;

    // Test that key set validation NOT ok with ssh keys passed with instance.
    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "mykey1".to_string()
                },
            ))
            .await
            .is_err()
    );

    // Only key associated with Tenant1 and keyset1, keyset2 are accepted.
    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "some_long_key_base64_encoded".to_string()
                },
            ))
            .await
            .is_ok()
    );

    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "my_another_key".to_string()
                },
            ))
            .await
            .is_ok()
    );

    // Any other keyset except mentioned in keyset_ids is not accepted.
    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "my_another_keyset3".to_string()
                },
            ))
            .await
            .is_err()
    );

    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "some_long_key_base64_encoded_1".to_string()
                },
            ))
            .await
            .is_err()
    );

    assert!(
        env.api
            .validate_tenant_public_key(tonic::Request::new(
                rpc::forge::ValidateTenantPublicKeyRequest {
                    instance_id: tinstance.id.to_string(),
                    tenant_public_key: "unknown_key1".to_string()
                },
            ))
            .await
            .is_err()
    );
}

#[crate::sqlx_test]
async fn test_keyset_in_instance(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .keyset_ids(&["keyset1", "keyset2"])
        .build()
        .await;

    let instance = env.one_instance(tinstance.id).await;

    let tenant = instance.config().tenant();

    assert_eq!(
        tenant.tenant_keyset_ids,
        vec!["keyset1".to_string(), "keyset2".to_string()]
    );
}
