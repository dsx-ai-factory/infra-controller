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
use carbide_uuid::vpc::VpcId;
use model::tenant::TenantOrganizationId;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request, Status};

use crate::tests::common::api_fixtures::instance::{
    TestInstance, default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{
    TestEnv, TestManagedHost, create_managed_host, create_test_env,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::VpcCreationRequest;

const TENANT: &str = "Tenant1";
const OTHER_TENANT: &str = "Tenant2";

/// Creates an option set for `tenant` whose only value is an IPv4 boot URL.
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

/// Returns the option sets the public Find RPC shows to `tenant`.
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

/// Deletes an option set through the public RPC as `tenant`.
async fn delete_option_set(env: &TestEnv, id: DhcpOptionSetId, tenant: &str) -> Result<(), Status> {
    env.api
        .delete_dhcp_option_set(Request::new(rpc::forge::DeleteDhcpOptionSetRequest {
            id: Some(id),
            tenant_organization_id: tenant.to_string(),
        }))
        .await
        .map(|_| ())
}

/// Creates a named VPC for `TENANT`, attached to `dhcp_option_set_id` when given.
async fn create_vpc(
    env: &TestEnv,
    name: &str,
    dhcp_option_set_id: Option<String>,
) -> Result<VpcId, Status> {
    let mut request = VpcCreationRequest::builder(TENANT)
        .metadata(rpc::forge::Metadata {
            name: name.to_string(),
            ..Default::default()
        })
        .rpc();
    request.dhcp_option_set_id = dhcp_option_set_id;
    Ok(env
        .api
        .create_vpc(Request::new(request))
        .await?
        .into_inner()
        .id
        .unwrap())
}

/// Returns the option set IDs the public Find RPC lists for `tenant`.
async fn find_option_set_ids(env: &TestEnv, tenant: &str) -> Vec<DhcpOptionSetId> {
    env.api
        .find_dhcp_option_set_ids(Request::new(rpc::forge::FindDhcpOptionSetIdsRequest {
            name: None,
            tenant_organization_id: Some(tenant.to_string()),
        }))
        .await
        .unwrap()
        .into_inner()
        .ids
}

/// Returns the VPC's `dhcp_option_set_id` as the public Find RPC reports it.
async fn vpc_dhcp_option_set(env: &TestEnv, vpc_id: VpcId) -> Option<String> {
    let vpcs = env
        .api
        .find_vpcs_by_ids(Request::new(rpc::forge::VpcsByIdsRequest {
            vpc_ids: vec![vpc_id],
        }))
        .await
        .unwrap()
        .into_inner()
        .vpcs;
    vpcs[0].config.as_ref().unwrap().dhcp_option_set_id.clone()
}

/// Updates only the VPC's `dhcp_option_set_id`, then returns the value the public Find RPC reports.
async fn update_vpc_dhcp_option_set(
    env: &TestEnv,
    vpc_id: VpcId,
    dhcp_option_set_id: Option<String>,
) -> Result<Option<String>, Status> {
    env.api
        .update_vpc(Request::new(rpc::forge::VpcUpdateRequest {
            id: Some(vpc_id),
            dhcp_option_set_id,
            ..Default::default()
        }))
        .await?;
    Ok(vpc_dhcp_option_set(env, vpc_id).await)
}

/// Sends the instance's current config with `dhcp_option_set_id` replaced, then returns the stored value.
async fn update_instance_dhcp_option_set(
    env: &TestEnv,
    instance: &TestInstance<'_, '_>,
    dhcp_option_set_id: Option<String>,
) -> Result<Option<DhcpOptionSetId>, Status> {
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
        .await?;
    Ok(db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .unwrap()
        .config
        .dhcp_option_set_id)
}

/// Tenants manage their option sets through create, find, update and delete,
/// and every step must store exactly what was sent and stay within the tenant.
#[crate::sqlx_test]
async fn test_dhcp_option_set_lifecycle(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    create_fixture_tenant(&env, OTHER_TENANT).await.unwrap();

    // Invalid input and unknown tenants are client errors, not server faults.
    let err = create_option_set(&env, TENANT, "ftp://boot.example.com/x.efi")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    let err = create_option_set(&env, "UnknownTenant", "http://boot.example.com/x.efi")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    // The stored set holds the requested values and is hidden from other tenants.
    let created = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap();
    let id = created.id.unwrap();
    assert_eq!(created.metadata.as_ref().unwrap().name, "provider boot");
    assert_eq!(
        created.options.as_ref().unwrap().v4,
        Some(rpc::forge::DhcpOptionsV4 {
            boot_url: Some("http://boot.example.com/x.efi".to_string()),
            dns_servers: vec![],
        })
    );
    assert_eq!(
        find_option_sets(&env, id, TENANT).await,
        vec![created.clone()]
    );
    assert!(find_option_sets(&env, id, OTHER_TENANT).await.is_empty());
    assert_eq!(find_option_set_ids(&env, TENANT).await, vec![id]);
    assert!(find_option_set_ids(&env, OTHER_TENANT).await.is_empty());

    // Updating replaces the options and moves the set to a new version.
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
    assert_eq!(
        updated.options.as_ref().unwrap().v4,
        Some(rpc::forge::DhcpOptionsV4 {
            boot_url: Some("http://boot.example.com/v2.efi".to_string()),
            dns_servers: vec!["10.0.0.53".to_string()],
        })
    );
    assert_eq!(
        find_option_sets(&env, id, TENANT).await,
        vec![updated.clone()]
    );

    // An update based on the old version must not overwrite the newer one.
    let err = env
        .api
        .update_dhcp_option_set(Request::new(update(&created.version)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(find_option_sets(&env, id, TENANT).await, vec![updated]);

    // Only the owning tenant can delete the set.
    let err = delete_option_set(&env, id, OTHER_TENANT).await.unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    delete_option_set(&env, id, TENANT).await.unwrap();
    assert!(find_option_sets(&env, id, TENANT).await.is_empty());
    assert!(find_option_set_ids(&env, TENANT).await.is_empty());
}

/// The database only checks that an attached set exists, not who owns it, so
/// every attach path must reject another tenant's set itself.
#[crate::sqlx_test]
async fn test_attaching_other_tenants_dhcp_option_set_is_rejected(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    create_fixture_tenant(&env, OTHER_TENANT).await.unwrap();
    let own_set = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap()
        .to_string();
    let other_set = create_option_set(&env, OTHER_TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap()
        .to_string();

    // VPC create and VPC update.
    let err = create_vpc(&env, "rejected vpc", Some(other_set.clone()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let vpc_id = create_vpc(&env, "tenant vpc", None).await.unwrap();
    let err = update_vpc_dhcp_option_set(&env, vpc_id, Some(other_set.clone()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    // Instance update.
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host = create_managed_host(&env).await;
    let instance = host
        .instance_builer(&env)
        .config(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            ..Default::default()
        })
        .build()
        .await;
    let err = update_instance_dhcp_option_set(&env, &instance, Some(other_set.clone()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    // Batch allocation fails as a whole when any request names another tenant's set.
    let allocation = |host: &TestManagedHost, set: &str| rpc::forge::InstanceAllocationRequest {
        machine_id: Some(host.host().id),
        config: Some(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            dhcp_option_set_id: Some(set.to_string()),
            ..Default::default()
        }),
        metadata: Some(rpc::forge::Metadata {
            name: format!("batch-{}", host.host().id),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (own_host, other_host) = (
        create_managed_host(&env).await,
        create_managed_host(&env).await,
    );
    let err = env
        .api
        .allocate_instances(Request::new(rpc::forge::BatchInstanceAllocationRequest {
            instance_requests: vec![
                allocation(&own_host, &own_set),
                allocation(&other_host, &other_set),
            ],
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
}

/// Instance updates replace the whole config, so omitting `dhcp_option_set_id`
/// removes the attached set, as it does for the instance's NSG.
#[crate::sqlx_test]
async fn test_instance_update_without_dhcp_option_set_removes_it(pool: sqlx::PgPool) {
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

    assert_eq!(
        update_instance_dhcp_option_set(&env, &instance, None)
            .await
            .unwrap(),
        None
    );
}

/// VPC updates replace the attached set: a new ID swaps it in, and omitting
/// `dhcp_option_set_id` removes it, as it does for the VPC's NSG.
#[crate::sqlx_test]
async fn test_vpc_update_replaces_or_removes_dhcp_option_set(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let [set_a, set_b] = [
        create_option_set(&env, TENANT, "http://boot.example.com/a.efi").await,
        create_option_set(&env, TENANT, "http://boot.example.com/b.efi").await,
    ]
    .map(|set| set.unwrap().id.unwrap().to_string());
    let vpc_id = create_vpc(&env, "tenant vpc", Some(set_a.clone()))
        .await
        .unwrap();

    // Each update applies to the state the previous one left.
    for (sent, expected) in [(Some(set_b.clone()), Some(&set_b)), (None, None)] {
        assert_eq!(
            update_vpc_dhcp_option_set(&env, vpc_id, sent.clone())
                .await
                .unwrap()
                .as_ref(),
            expected,
            "after sending {sent:?}"
        );
    }
}

/// A set in use by a live instance or VPC must not be deleted; each kind of
/// user blocks the delete on its own.
#[crate::sqlx_test]
async fn test_dhcp_option_set_delete_refused_while_in_use(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let set_id = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap();

    // Attach the set to both a VPC and an instance.
    let vpc_id = create_vpc(&env, "tenant vpc", Some(set_id.to_string()))
        .await
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
    let err = delete_option_set(&env, set_id, TENANT).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    // With only the instance left attached, the delete is still refused.
    update_vpc_dhcp_option_set(&env, vpc_id, Some(String::new()))
        .await
        .unwrap();
    let err = delete_option_set(&env, set_id, TENANT).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);

    // Once nothing uses the set, it can be deleted.
    update_instance_dhcp_option_set(&env, &instance, Some(String::new()))
        .await
        .unwrap();
    delete_option_set(&env, set_id, TENANT).await.unwrap();
}

/// Attaching must keep the set locked until commit; otherwise a concurrent
/// delete could remove it, and another tenant could reuse its ID.
#[crate::sqlx_test]
async fn test_attach_waits_for_concurrent_dhcp_option_set_delete(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let set_id = create_option_set(&env, TENANT, "http://boot.example.com/x.efi")
        .await
        .unwrap()
        .id
        .unwrap();
    let vpc_id = create_vpc(&env, "tenant vpc", None).await.unwrap();

    // Hold the set's row lock, as a delete in progress does.
    let mut delete_txn = env.db_txn().await;
    db::dhcp_option_set::find_by_ids(delete_txn.as_mut(), &[set_id], None, true)
        .await
        .unwrap();
    let delete_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(delete_txn.as_mut())
        .await
        .unwrap();

    // The attach must wait on that lock instead of reading past it.
    let attach = update_vpc_dhcp_option_set(&env, vpc_id, Some(set_id.to_string()));
    tokio::pin!(attach);
    tokio::select! {
        result = &mut attach => panic!("attach did not wait for the option set lock: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, delete_pid, "FROM dhcp_option_sets") => {}
    }

    // Once the delete commits, the attach fails cleanly and leaves the VPC unattached.
    let tenant: TenantOrganizationId = TENANT.parse().unwrap();
    db::dhcp_option_set::delete(delete_txn.as_mut(), &set_id, &tenant)
        .await
        .unwrap();
    delete_txn.commit().await.unwrap();
    let err = attach.await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(vpc_dhcp_option_set(&env, vpc_id).await, None);
}
