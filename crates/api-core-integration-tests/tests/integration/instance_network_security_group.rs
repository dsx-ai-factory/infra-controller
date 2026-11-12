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

use std::collections::HashMap;
use std::sync::Arc;

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use db::network_security_group::create as create_network_security_group;
use model::metadata::Metadata;
use model::network_security_group;
use model::tenant::TenantOrganizationId;
use model::test_support::ManagedHostConfig;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig};

use super::instance_common::{
    InstanceEnv, default_os_config, default_tenant_config, host_id, single_interface_network_config,
};

#[sqlx_test]
async fn test_allocate_and_update_with_network_security_group(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = InstanceEnv::new(pool, default_config::get()).await;

    populate_network_security_groups(env.harness.api_arc()).await;

    // NSG ID of and NSG for the default tenant provided by fixtures.
    let good_network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    // NSG ID of not-the-default-tenant provided by fixtures.
    let bad_network_security_group_id = "ddfcabc4-92dc-41e2-874e-2c7eeb9fa156";

    // Create a new managed host in the DB and get the snapshot.
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let host_machine_id = host_id(&mh);

    let segment_id = env.segment_id;

    // Try to create an instance, but send in a valid and
    // existing NSG ID that doesn't match the tenant of
    // instance being created.
    // This should fail.
    let _ = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(bad_network_security_group_id)
                        .rpc(),
                )
                .metadata(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                })
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Try that once more, but with an NSG ID
    // that has the same tenant as the instance.
    let i = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(good_network_security_group_id)
                        .rpc(),
                )
                .metadata(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Check that the instance actually has the ID we expect
    assert_eq!(
        i.config.unwrap().network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    let instance_id = i.id.unwrap();

    // Now update to remove the NSG attachment.
    let i = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id))
                        .into(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Check that the instance no longer has an NSG ID
    assert!(i.config.unwrap().network_security_group_id.is_none());

    // Now try to update it again and try to add the NSG with the mismatched tenant org
    // Now update to remove the NSG attachment.
    let _ = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(bad_network_security_group_id)
                        .rpc(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap_err();

    // Now try to update it again and but with a good NSG
    let i = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(good_network_security_group_id)
                        .into(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Check that the instance actually has the ID we expect
    assert_eq!(
        i.config.unwrap().network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    Ok(())
}

async fn populate_network_security_groups(api: Arc<Api>) {
    // Create tenant orgs
    let default_tenant_org = "Tenant1";
    let _ = api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: default_tenant_org.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: default_tenant_org.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    let tenant_org2 = "Tenant2";
    let _ = api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: tenant_org2.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: tenant_org2.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Create default network security groups.
    let mut txn = api.database_connection.begin().await.unwrap();

    // Just a default ID for group and single rule.
    let uid = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    let rules = vec![network_security_group::NetworkSecurityGroupRule {
        id: Some(uid.to_string()),
        direction: network_security_group::NetworkSecurityGroupRuleDirection::Ingress,
        ipv6: false,
        src_port_start: Some(80),
        src_port_end: Some(32768),
        dst_port_start: Some(80),
        dst_port_end: Some(32768),
        protocol: network_security_group::NetworkSecurityGroupRuleProtocol::Any,
        action: network_security_group::NetworkSecurityGroupRuleAction::Deny,
        priority: 9001,
        src_net: network_security_group::NetworkSecurityGroupRuleNet::Prefix(
            "0.0.0.0/0".parse().unwrap(),
        ),
        dst_net: network_security_group::NetworkSecurityGroupRuleNet::Prefix(
            "0.0.0.0/0".parse().unwrap(),
        ),
    }];

    let metadata = Metadata {
        name: "default_network_security_group_1".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };

    let id = uid.parse().unwrap();

    let tenant_org = default_tenant_org.parse::<TenantOrganizationId>().unwrap();

    let _it =
        create_network_security_group(&mut txn, &id, &tenant_org, None, &metadata, false, &rules)
            .await
            .unwrap();

    // Create one more NSG with a different name.
    // The rules can be the same.
    // Just another default ID for group and single rule.
    let uid = "b65b13d6-d81c-11ef-9252-b346dc360bd4";
    let metadata = Metadata {
        name: "default_network_security_group_2".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };
    let id = uid.parse().unwrap();

    let _it =
        create_network_security_group(&mut txn, &id, &tenant_org, None, &metadata, false, &rules)
            .await
            .unwrap();

    // One more for the second tenant
    let uid = "ddfcabc4-92dc-41e2-874e-2c7eeb9fa156";
    let metadata = Metadata {
        name: "default_network_security_group_3".to_string(),
        description: "".to_string(),
        labels: HashMap::new(),
    };
    let id = uid.parse().unwrap();

    let _it = create_network_security_group(
        &mut txn,
        &id,
        &tenant_org2.parse::<TenantOrganizationId>().unwrap(),
        None,
        &metadata,
        false,
        &rules,
    )
    .await
    .unwrap();

    txn.commit().await.unwrap();
}
