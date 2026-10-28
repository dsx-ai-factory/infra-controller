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

use carbide_uuid::dhcp_option_set::DhcpOptionSetId;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request, Status};

use crate::tests::common::api_fixtures::instance::{
    TestInstance, default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{TestEnv, create_managed_host, create_test_env};
use crate::tests::common::rpc_builder::VpcCreationRequest;

const TENANT: &str = "Tenant1";
const OTHER_TENANT: &str = "Tenant2";

async fn create_option_set(
    env: &TestEnv,
    tenant: &str,
    boot_url: &str,
) -> Result<rpc::forge::DhcpOptionSet, Status> {
    Ok(env
        .api
        .create_dhcp_option_set(Request::new(rpc::forge::CreateDhcpOptionSetRequest {
            id: None,
            tenant_organization_id: tenant.to_string(),
            metadata: Some(rpc::forge::Metadata {
                name: "provider boot".to_string(),
                ..Default::default()
            }),
            options: Some(rpc::forge::DhcpOptions {
                v4: Some(rpc::forge::DhcpOptionsV4 {
                    boot_url: Some(boot_url.to_string()),
                    dns_servers: vec![],
                }),
                v6: None,
            }),
        }))
        .await?
        .into_inner()
        .dhcp_option_set
        .unwrap())
}

async fn find_option_sets(
    env: &TestEnv,
    id: DhcpOptionSetId,
    tenant: &str,
) -> Vec<rpc::forge::DhcpOptionSet> {
    env.api
        .find_dhcp_option_sets_by_ids(Request::new(rpc::forge::FindDhcpOptionSetsByIdsRequest {
            ids: vec![id],
            tenant_organization_id: Some(tenant.to_string()),
        }))
        .await
        .unwrap()
        .into_inner()
        .dhcp_option_sets
}

async fn delete_option_set(env: &TestEnv, id: DhcpOptionSetId, tenant: &str) -> Result<(), Status> {
    env.api
        .delete_dhcp_option_set(Request::new(rpc::forge::DeleteDhcpOptionSetRequest {
            id: Some(id),
            tenant_organization_id: tenant.to_string(),
        }))
        .await
        .map(|_| ())
}

/// Sends the instance's current config with `dhcp_option_set_id` replaced, then returns the stored value.
async fn update_instance_dhcp_option_set(
    env: &TestEnv,
    instance: &TestInstance<'_, '_>,
    dhcp_option_set_id: Option<String>,
) -> Option<DhcpOptionSetId> {
    let before = instance.rpc_instance().await.into_inner();
    let mut config = before.config.unwrap();
    config.dhcp_option_set_id = dhcp_option_set_id;
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: before.id,
            if_version_match: None,
            config: Some(config),
            metadata: before.metadata,
        }))
        .await
        .unwrap();
    db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .unwrap()
        .config
        .dhcp_option_set_id
}

#[crate::sqlx_test]
async fn test_dhcp_option_set_lifecycle(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    create_fixture_tenant(&env, OTHER_TENANT).await.unwrap();

    let err = create_option_set(&env, TENANT, "ftp://boot.example.com/x.efi")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    let err = create_option_set(&env, "UnknownTenant", "http://boot.example.com/x.efi")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    let created = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap();
    let id = created.id.unwrap();
    assert_eq!(
        find_option_sets(&env, id, TENANT).await,
        vec![created.clone()]
    );
    assert!(find_option_sets(&env, id, OTHER_TENANT).await.is_empty());

    let update = |version: &str| rpc::forge::UpdateDhcpOptionSetRequest {
        id: Some(id),
        tenant_organization_id: TENANT.to_string(),
        metadata: created.metadata.clone(),
        options: Some(rpc::forge::DhcpOptions {
            v4: Some(rpc::forge::DhcpOptionsV4 {
                boot_url: Some("http://boot.example.com/v2.efi".to_string()),
                dns_servers: vec!["10.0.0.53".to_string()],
            }),
            v6: None,
        }),
        if_version_match: Some(version.to_string()),
    };
    let updated = env
        .api
        .update_dhcp_option_set(Request::new(update(&created.version)))
        .await
        .unwrap()
        .into_inner()
        .dhcp_option_set
        .unwrap();
    assert_ne!(updated.version, created.version);
    assert_eq!(find_option_sets(&env, id, TENANT).await, vec![updated]);

    // An update based on the old version must not overwrite the newer one.
    let err = env
        .api
        .update_dhcp_option_set(Request::new(update(&created.version)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    let err = delete_option_set(&env, id, OTHER_TENANT).await.unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    delete_option_set(&env, id, TENANT).await.unwrap();
    assert!(find_option_sets(&env, id, TENANT).await.is_empty());
}

#[crate::sqlx_test]
async fn test_vpc_rejects_other_tenants_dhcp_option_set(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, OTHER_TENANT).await.unwrap();
    let other_set = create_option_set(&env, OTHER_TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap();

    let mut request = VpcCreationRequest::builder(TENANT).rpc();
    request.dhcp_option_set_id = Some(other_set.id.unwrap().to_string());
    let err = env.api.create_vpc(Request::new(request)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
}

#[crate::sqlx_test]
async fn test_instance_update_keeps_dhcp_option_set_when_omitted(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let set_id = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let instance = host
        .instance_builer(&env)
        .config(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            dhcp_option_set_id: Some(set_id.to_string()),
            ..Default::default()
        })
        .build()
        .await;

    // Older clients omit the field, so the stored option set must survive.
    assert_eq!(
        update_instance_dhcp_option_set(&env, &instance, None).await,
        Some(set_id)
    );
    assert_eq!(
        update_instance_dhcp_option_set(&env, &instance, Some(String::new())).await,
        None
    );
}

#[crate::sqlx_test]
async fn test_dhcp_option_set_delete_refused_while_in_use(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let set_id = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap();

    let mut request = VpcCreationRequest::builder(TENANT).rpc();
    request.dhcp_option_set_id = Some(set_id.to_string());
    let vpc = env
        .api
        .create_vpc(Request::new(request))
        .await
        .unwrap()
        .into_inner();

    let err = delete_option_set(&env, set_id, TENANT).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    env.api
        .update_vpc(Request::new(rpc::forge::VpcUpdateRequest {
            id: vpc.id,
            dhcp_option_set_id: Some(String::new()),
            ..Default::default()
        }))
        .await
        .unwrap();
    delete_option_set(&env, set_id, TENANT).await.unwrap();
}
