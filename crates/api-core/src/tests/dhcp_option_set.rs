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

use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{TestEnv, create_test_env};

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

/// The DPU's DHCP server gets tenant values only for a Ready instance whose OS
/// boots via its option set; any other OS keeps NICo's values even with a set attached.
#[crate::sqlx_test]
async fn test_host_network_config_carries_tenant_dhcp_values_for_option_set_os(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, TENANT).await.unwrap();
    let set_id = env
        .api
        .create_dhcp_option_set(Request::new(rpc::forge::CreateDhcpOptionSetRequest {
            id: None,
            tenant_organization_id: TENANT.to_string(),
            metadata: Some(rpc::forge::Metadata {
                name: "tenant boot".to_string(),
                ..Default::default()
            }),
            options: Some(rpc::forge::DhcpOptions {
                v4: Some(rpc::forge::DhcpOptionsV4 {
                    boot_url: Some("http://boot.example.com/x.efi".to_string()),
                    dns_servers: vec!["10.0.0.53".to_string()],
                }),
                v6: Some(rpc::forge::DhcpOptionsV6 {
                    boot_url: Some("http://[2001:db8::5]/x.efi".to_string()),
                    dns_servers: vec![],
                }),
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .dhcp_option_set
        .and_then(|set| set.id)
        .unwrap();
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let option_set_os = rpc::forge::InstanceOperatingSystemConfig {
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::DhcpOptionSet(
                rpc::forge::DhcpOptionSetBoot {},
            ),
        ),
        ..default_os_config()
    };

    // Each row allocates a Ready instance with the set attached, then reads the DPU's config.
    for (os, expected) in [
        (
            option_set_os,
            (
                Some("http://boot.example.com/x.efi".to_string()),
                Some("http://[2001:db8::5]/x.efi".to_string()),
                vec!["10.0.0.53".to_string()],
            ),
        ),
        (default_os_config(), (None, None, vec![])),
    ] {
        let host = create_managed_host(&env).await;
        host.instance_builer(&env)
            .config(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(os.clone()),
                network: Some(single_interface_network_config(segment_id)),
                dhcp_option_set_id: Some(set_id.to_string()),
                ..Default::default()
            })
            .build()
            .await;
        let config = env
            .api
            .get_managed_host_network_config(Request::new(
                rpc::forge::ManagedHostNetworkConfigRequest {
                    dpu_machine_id: Some(host.dpu().id),
                },
            ))
            .await
            .unwrap()
            .into_inner();
        let interface = &config.tenant_interfaces[0];
        assert_eq!(
            (
                interface.booturl_v4.clone(),
                interface.booturl_v6.clone(),
                interface.dns_servers_v4.clone(),
            ),
            expected,
            "os = {os:?}"
        );
    }
}
