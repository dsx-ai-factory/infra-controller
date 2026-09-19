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
use std::time::{Duration, SystemTime};

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, HostMachineId, StableHostMachineId};
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use model::instance::config::network::DeviceLocator;
use model::machine::{InstanceState, ManagedHostState, RetryInfo};
use model::metadata::Metadata;
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::forge_server::Forge;
use rpc::health::HealthReport;
use tonic::Code;
use uuid::uuid;

use super::common::api_fixtures::TestEnv;
use crate::cfg::file::{
    FnnConfig, FnnRoutingProfileConfig, default_max_network_security_group_size,
};
use crate::test_support::fixture_config::FixtureDefault as _;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, interface_network_config_with_devices,
    single_interface_network_config,
};
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, create_tenant_network_segment,
};
use crate::tests::common::api_fixtures::test_managed_host::TestManagedHost;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides,
    get_config, populate_network_security_groups, site_explorer,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::{VpcCreationRequest, VpcUpdateRequest};

async fn update_network_status_observation(
    env: &TestEnv,
    instance_id: &InstanceId,
    good_network_security_group_id: &str,
    security_version: &str,
    dpu_machine_id: &DpuMachineId,
    source: rpc::forge::NetworkSecurityGroupSource,
    internal_uuid: &rpc::Uuid,
) {
    let _ = env
        .api
        .record_dpu_network_status(tonic::Request::new(rpc::forge::DpuNetworkStatus {
            instance_id: Some(*instance_id),
            observed_at: Some(SystemTime::now().into()),
            interfaces: vec![rpc::forge::InstanceInterfaceStatusObservation {
                gateways: vec!["10.180.125.1/27".to_string()],
                prefixes: vec![],
                addresses: vec!["10.180.125.5".to_string()],
                function_type: rpc::forge::InterfaceFunctionType::Physical.into(),
                mac_address: Some("A0:88:C2:4E:9B:78".to_string()),
                virtual_function_id: None,
                network_security_group: Some(rpc::forge::NetworkSecurityGroupStatus {
                    id: good_network_security_group_id.to_string(),
                    source: source.into(),
                    version: security_version.to_string(),
                }),
                internal_uuid: Some(internal_uuid.clone()),
            }],
            dpu_machine_id: Some(*dpu_machine_id),
            network_config_version: Some("V1-T1".to_string()),
            instance_network_config_version: Some("V1-T1".to_string()),
            network_config_error: None,
            dpu_agent_version: Some("V1-T1".to_string()),
            client_certificate_expiry_unix_epoch_secs: Some(10000000),
            dpu_health: Some(HealthReport {
                source: "dpu-agent".to_string(),
                triggered_by: None,
                observed_at: Some(SystemTime::now().into()),
                successes: vec![],
                alerts: vec![],
            }),
            instance_config_version: Some("V1-T1".to_string()),
            fabric_interfaces: vec![],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: Some("V1-T1".to_string()),
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .unwrap();
}

fn stable_host_id(host_id: HostMachineId) -> StableHostMachineId {
    host_id
        .try_into()
        .expect("site-explorer fixture should produce a stable host ID")
}

#[crate::sqlx_test]
async fn test_network_security_group_create(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let id = "can_i_see_some_id?";

    // Create tenant orgs
    let default_tenant_org = "Tenant1";
    let _ = env
        .api
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

    let tenant_org2 = "create_nsg_tenant2";
    let _ = env
        .api
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

    // Prepare some bad attributes for testing NSG size limits
    let too_many_src_ports = Some(rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: false,
        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("anything".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            ipv6: false,
            src_port_start: Some(80),
            src_port_end: Some(32768),
            dst_port_start: None,
            dst_port_end: None,
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            priority: 9001,
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
        }],
    });

    let too_many_dst_ports = Some(rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: false,
        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("anything".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            ipv6: false,
            src_port_start: None,
            src_port_end: None,
            dst_port_start: Some(80),
            dst_port_end: Some(32768),
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            priority: 9001,
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
        }],
    });

    let too_many_rules =
        Some(rpc::forge::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("anything".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            ipv6: false,
            src_port_start: Some(80),
            src_port_end: Some(80),
            dst_port_start: Some(80),
            dst_port_end: Some(80),
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            priority: 9001,
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
        }; (default_max_network_security_group_size()+1) as usize],
        });

    let duplicate_rule_ids =
        Some(rpc::forge::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(80),
                dst_port_start: Some(80),
                dst_port_end: Some(80),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            },rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(80),
                dst_port_start: Some(80),
                dst_port_end: Some(80),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            }],
        });

    // Prepare some attributes for creation and comparison later
    let network_security_group_attributes = Some(rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: true,
        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("anything".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            ipv6: false,
            src_port_start: Some(80),
            src_port_end: Some(80),
            dst_port_start: Some(90),
            dst_port_end: Some(90),
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            priority: 9001,
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
        }],
    });

    let metadata = Some(rpc::forge::Metadata {
        name: "the best NSG".to_string(),
        description: "".to_string(),
        labels: vec![],
    });

    // First, attempt to create a new NSG that's too big
    // because it will expand into too many rules.
    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(id.to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: too_many_src_ports,
            },
        ))
        .await
        .unwrap_err();

    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(id.to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: too_many_dst_ports,
            },
        ))
        .await
        .unwrap_err();

    // Then, attempt to create a new NSG that's too big because
    // it just has too many explicit rules.
    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(id.to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: too_many_rules,
            },
        ))
        .await
        .unwrap_err();

    // Then, attempt to create a new NSG that'has duplicate
    // rule IDs
    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(id.to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: duplicate_rule_ids,
            },
        ))
        .await
        .unwrap_err();

    // Next, successfully create a new NSG
    let forge_network_security_group = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(id.to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: network_security_group_attributes.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group
        .unwrap();

    // Check that we're on our first version.
    let version: ConfigVersion = forge_network_security_group.version.parse()?;
    assert_eq!(version.version_nr(), 1);

    // Verify that the attributes we sent in are the attributes we got back out.
    assert_eq!(
        forge_network_security_group.attributes,
        network_security_group_attributes
    );

    //Verify the metadata
    assert_eq!(forge_network_security_group.metadata, metadata);

    // Next, try to create a duplicate with a new ID but the same name.
    // This should fail.
    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some("any_other_id".to_string()),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: network_security_group_attributes.clone(),
            },
        ))
        .await
        .unwrap_err();

    // Next, try to create a duplicate with a new ID and the same name
    // but for a different tenant.
    // This should pass.
    let _ = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some("any_other_id".to_string()),
                tenant_organization_id: tenant_org2.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: network_security_group_attributes.clone(),
            },
        ))
        .await
        .unwrap();

    // Next, we'll find all the network security group IDs in the system.
    // There should two: one for each tenant.
    let forge_network_security_group_ids = env
        .api
        .find_network_security_group_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupIdsRequest {
                name: None,
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group_ids;

    // We should have exactly two new ones
    assert_eq!(forge_network_security_group_ids.len(), 2);

    // Next, we'll use query options to search for our specific
    // network security group.
    let forge_network_security_group_ids = env
        .api
        .find_network_security_group_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupIdsRequest {
                name: Some("the best NSG".to_string()),
                tenant_organization_id: Some(default_tenant_org.to_string()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group_ids;

    // We should have exactly one.
    assert_eq!(forge_network_security_group_ids.len(), 1);

    // Next, we'll retrieve the previously created network security group
    // and make sure everything still matches.
    let forge_network_security_groups = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                network_security_group_ids: vec![id.to_string()],
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_groups;

    // We should have exactly one.
    assert_eq!(forge_network_security_groups.len(), 1);

    let network_security_group = forge_network_security_groups[0].clone();

    // The ID should be the one we started with.
    assert_eq!(network_security_group.id, id);

    // Verify that the attributes we sent in are the attributes we got back out.
    assert_eq!(
        network_security_group.attributes,
        network_security_group_attributes
    );

    //Verify the metadata
    assert_eq!(network_security_group.metadata, metadata);

    Ok(())
}

/// Verifies disabled site support rejects new stateful intent without mutating persistence.
///
/// This prevents accepted NSG state from promising statefulness the site cannot provide.
#[crate::sqlx_test]
async fn test_network_security_group_stateful_egress_requires_site_support(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.tenant_prefix_overlap_enabled = false;
    config.network_security_group.stateful_acls_enabled = false;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let tenant_organization_id = "stateful_nsg_tenant";
    let rejected_id = "08b4c3fa-bf88-4a61-b435-a2f398545652";
    let stateless_id = "c34d8b63-2189-4bda-9340-266bf762442d";
    let metadata = Some(rpc::forge::Metadata {
        name: "stateful NSG gate".to_string(),
        description: String::new(),
        labels: vec![],
    });

    // Create the tenant required by the public NSG API.
    env.api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: tenant_organization_id.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: tenant_organization_id.to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect("tenant creation should succeed");

    // Reject a stateful create before it can persist an unusable group.
    let error = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(rejected_id.to_string()),
                tenant_organization_id: tenant_organization_id.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: Some(
                    rpc::forge::NetworkSecurityGroupAttributes {
                        stateful_egress: true,
                        rules: vec![],
                    },
                ),
            },
        ))
        .await
        .expect_err("stateful creation should require site support");
    assert_eq!(error.code(), Code::InvalidArgument);

    // Reload through the public API to prove the rejected create left no record.
    let rejected_groups = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                network_security_group_ids: vec![rejected_id.to_string()],
                tenant_organization_id: Some(tenant_organization_id.to_string()),
            },
        ))
        .await?
        .into_inner()
        .network_security_groups;
    assert!(rejected_groups.is_empty());

    // Stateless NSGs remain valid while site-level stateful ACLs are disabled.
    let stateless_network_security_group = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(stateless_id.to_string()),
                tenant_organization_id: tenant_organization_id.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: Some(
                    rpc::forge::NetworkSecurityGroupAttributes {
                        stateful_egress: false,
                        rules: vec![],
                    },
                ),
            },
        ))
        .await?
        .into_inner()
        .network_security_group
        .expect("create response should contain the NSG");
    assert!(
        !stateless_network_security_group
            .attributes
            .as_ref()
            .expect("created NSG should contain attributes")
            .stateful_egress
    );

    // Reject the unsupported false-to-true transition.
    let error = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: stateless_id.to_string(),
                tenant_organization_id: tenant_organization_id.to_string(),
                metadata,
                network_security_group_attributes: Some(
                    rpc::forge::NetworkSecurityGroupAttributes {
                        stateful_egress: true,
                        rules: vec![],
                    },
                ),
                if_version_match: Some(stateless_network_security_group.version.clone()),
            },
        ))
        .await
        .expect_err("enabling stateful egress should require site support");
    assert_eq!(error.code(), Code::InvalidArgument);

    // Reload the NSG to prove the rejected update changed neither data nor version.
    let persisted_groups = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                network_security_group_ids: vec![stateless_id.to_string()],
                tenant_organization_id: Some(tenant_organization_id.to_string()),
            },
        ))
        .await?
        .into_inner()
        .network_security_groups;
    assert_eq!(persisted_groups, vec![stateless_network_security_group]);

    // A stateful group can outlive the site setting. Once another writer
    // disables it, an unconditional update must not re-enable it using the
    // exception for an existing stateful group.
    sqlx::query("UPDATE network_security_groups SET stateful_egress = true WHERE id = $1")
        .bind(stateless_id)
        .execute(&env.pool)
        .await?;
    let id = stateless_id.parse()?;
    let mut writer = env.db_txn().await;
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;
    let old =
        db::network_security_group::find_by_ids(&mut writer, std::slice::from_ref(&id), None, true)
            .await?
            .pop()
            .unwrap();
    let restricted = db::network_security_group::update(
        &mut writer,
        &id,
        &old.tenant_organization_id,
        &old.metadata,
        false,
        &[],
        old.version,
        None,
    )
    .await?;
    let update = env.api.update_network_security_group(tonic::Request::new(
        rpc::forge::UpdateNetworkSecurityGroupRequest {
            id: stateless_id.to_string(),
            tenant_organization_id: tenant_organization_id.to_string(),
            metadata: persisted_groups[0].metadata.clone(),
            network_security_group_attributes: Some(rpc::forge::NetworkSecurityGroupAttributes {
                stateful_egress: true,
                rules: vec![],
            }),
            if_version_match: None,
        },
    ));
    let release = async {
        wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
        writer.commit().await
    };
    let (result, released) = tokio::join!(update, release);
    released?;
    let error = result.expect_err("stateful enablement must use the locked policy");
    assert_eq!(error.code(), Code::InvalidArgument, "{error}");
    let mut txn = env.db_txn().await;
    assert_eq!(
        db::network_security_group::find_by_ids(&mut txn, &[id], None, false).await?,
        vec![restricted]
    );
    txn.rollback().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_network_security_group_update(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    populate_network_security_groups(env.api.clone()).await;

    let existing_network_security_groups = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                // Provided by fixtures
                network_security_group_ids: vec![
                    "fd3ab096-d811-11ef-8fe9-7be4b2483448".to_string(),
                    "b65b13d6-d81c-11ef-9252-b346dc360bd4".to_string(),
                ],
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_groups;

    let id = existing_network_security_groups[0].id.clone();
    let version = existing_network_security_groups[0].version.clone();

    // Provided by fixtures
    let default_tenant_org = "Tenant1";

    // Prepare some bad attributes for testing NSG size limits
    let too_many_ports = Some(rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: false,
        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("anything".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            ipv6: false,
            src_port_start: Some(80),
            src_port_end: Some(32768),
            dst_port_start: Some(80),
            dst_port_end: Some(32768),
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
            priority: 9001,
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
        }],
    });

    let too_many_rules =
        Some(rpc::forge::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(80),
                dst_port_start: Some(80),
                dst_port_end: Some(80),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            }; (default_max_network_security_group_size()+1) as usize],
        });

    let duplicate_rule_ids =
        Some(rpc::forge::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(80),
                dst_port_start: Some(80),
                dst_port_end: Some(80),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            },rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(80),
                dst_port_start: Some(80),
                dst_port_end: Some(80),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            }],
        });

    let update_network_security_group_attributes =
        Some(rpc::forge::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: None,
                src_port_end: None,
                dst_port_start: Some(800),
                dst_port_end: Some(900),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
                priority: 9002,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "1.1.1.1/1".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "2.2.2.2/2".to_string(),
                    ),
                ),
            }],
        });

    let metadata = Some(rpc::forge::Metadata {
        name: "fixture_test_network_security_group_1".to_string(),
        description: "".to_string(),
        labels: vec![],
    });

    // Try to update the network security group with the wrong tenant org.  This should fail.
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: "this_is_a_bad_org".to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: update_network_security_group_attributes.clone(),
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    // Now update the network security group again.  This time it should
    // fail because we are trying to add too many implicit rules
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: too_many_ports,
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    // One more update, and this time it should
    // fail because we are trying to add too many explicit rules
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: too_many_rules,
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    // One more update, and this time it should
    // fail because we are trying to add duplicate rule IDs
    // in the NSG.
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: duplicate_rule_ids,
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    // Now update the network security group again.  This time it should
    // pass because the tenant org is correct and we are adding a valid
    // amount of rules.
    let forge_network_security_group = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: update_network_security_group_attributes.clone(),
                if_version_match: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group
        .unwrap();

    // Make sure we didn't somehow end up with a new id.
    assert_eq!(forge_network_security_group.id, id.to_string());

    // Check that we're on the second version.
    let next_version: ConfigVersion = forge_network_security_group.version.parse()?;
    assert_eq!(next_version.version_nr(), 2);

    // Verify that the attributes we sent in are the attributes we got back out.
    assert_eq!(
        forge_network_security_group.attributes,
        update_network_security_group_attributes
    );

    //Verify the metadata
    assert_eq!(forge_network_security_group.metadata, metadata);

    // Now update the network security group again but only if it's still on the first version.
    // This should fail.
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: update_network_security_group_attributes.clone(),
                if_version_match: Some(version.to_string()),
            },
        ))
        .await
        .unwrap_err();

    // Now update the network security group AGAIN but only if its on the second version.
    // This should pass.
    let forge_network_security_group = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: metadata.clone(),
                network_security_group_attributes: update_network_security_group_attributes.clone(),
                if_version_match: Some(next_version.to_string()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group
        .unwrap();

    // Check that we're on the third version.
    let next_version: ConfigVersion = forge_network_security_group.version.parse()?;

    // Make sure we didn't somehow end up with a new id.
    assert_eq!(forge_network_security_group.id, id.to_string());

    assert_eq!(next_version.version_nr(), 3);
    // Verify that the attributes we sent in are the attributes we got back out.
    assert_eq!(
        forge_network_security_group.attributes,
        update_network_security_group_attributes
    );

    //Verify the metadata
    assert_eq!(forge_network_security_group.metadata, metadata);

    // Next, we'll retrieve the updated network security group
    // and make sure everything still matches and that we
    // didn't screw-up the DB update and lie to ourselves.
    let forge_network_security_groups = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                network_security_group_ids: vec![forge_network_security_group.id.to_string()],
                tenant_organization_id: Some(default_tenant_org.to_string()),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_groups;

    // We should have exactly one.
    assert_eq!(forge_network_security_groups.len(), 1);

    let network_security_group = forge_network_security_groups[0].clone();

    // The ID should be the one we started with.
    assert_eq!(network_security_group.id, id.to_string());

    // Verify that the attributes we sent in are the attributes we got back out.
    assert_eq!(
        network_security_group.attributes,
        update_network_security_group_attributes
    );

    //Verify the metadata
    assert_eq!(network_security_group.metadata, metadata);

    // Now update the network security group again, but use
    // the name of an existing type.  This should fail.
    let _ = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: id.to_string(),
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: Some(rpc::forge::Metadata {
                    name: existing_network_security_groups[1]
                        .metadata
                        .clone()
                        .unwrap()
                        .name,
                    description: "".to_string(),
                    labels: vec![],
                }),
                network_security_group_attributes: update_network_security_group_attributes.clone(),
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    Ok(())
}

/// Policy checks follow current and pending tenant interfaces, not address
/// collisions or only the VPC's default NSG attachment.
#[crate::sqlx_test]
async fn tenant_prefix_reuse_checks_effective_active_policy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.tenant_prefix_overlap_enabled = true;
    config.default_tenant_routing_profile_type = "INTERNAL".to_string();
    config.network_security_group.stateful_acls_enabled = true;
    let fnn = FnnConfig {
        admin_vpc: None,
        common_internal_route_target: None,
        additional_route_target_imports: vec![],
        routing_profiles: HashMap::from([
            (
                "INTERNAL".to_string(),
                FnnRoutingProfileConfig {
                    tenant_prefix_overlap_eligible: true,
                    internal: Some(true),
                    ..Default::default()
                },
            ),
            (
                "EXTERNAL".to_string(),
                FnnRoutingProfileConfig {
                    internal: Some(false),
                    ..Default::default()
                },
            ),
        ]),
        use_vpc_vrf_loopback: false,
    };
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_fnn_config(Some(fnn)),
    )
    .await;
    let tenant = "overlap-policy";
    env.api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: tenant.to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: tenant.to_string(),
                ..Default::default()
            }),
        }))
        .await?;
    let permit = rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: false,
        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
            id: Some("permit-http".to_string()),
            direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                .into(),
            protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
            action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
            priority: 100,
            dst_port_start: Some(80),
            dst_port_end: Some(80),
            source_net: Some(
                rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            ..Default::default()
        }],
    };
    let safe_id = uuid::Uuid::new_v4().to_string();
    let permit_id = uuid::Uuid::new_v4().to_string();
    let override_id = uuid::Uuid::new_v4().to_string();
    for (id, name, attributes) in [
        (
            &safe_id,
            "vpc-policy",
            rpc::forge::NetworkSecurityGroupAttributes::default(),
        ),
        (&permit_id, "unused-permit-policy", permit.clone()),
        (
            &override_id,
            "instance-policy",
            rpc::forge::NetworkSecurityGroupAttributes::default(),
        ),
    ] {
        env.api
            .create_network_security_group(tonic::Request::new(
                rpc::forge::CreateNetworkSecurityGroupRequest {
                    id: Some(id.clone()),
                    tenant_organization_id: tenant.to_string(),
                    metadata: Some(rpc::forge::Metadata {
                        name: name.to_string(),
                        ..Default::default()
                    }),
                    network_security_group_attributes: Some(attributes),
                },
            ))
            .await?;
    }
    let active_vpc_id = VpcId::new();
    let segment_ids = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(tenant)
                .id(active_vpc_id)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .routing_profile_type("INTERNAL".to_string())
                .metadata(Metadata::new_with_default_name())
                .rpc(),
            1,
        )
        .await;
    let pending_vpc_id = VpcId::new();
    env.api
        .create_vpc(
            VpcCreationRequest::builder(tenant)
                .id(pending_vpc_id)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .routing_profile_type("INTERNAL".to_string())
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await?;
    let vpc_update = |nsg: &str| {
        VpcUpdateRequest::builder()
            .id(pending_vpc_id)
            .metadata(Metadata {
                name: "pending-vpc".to_string(),
                ..Default::default()
            })
            .network_security_group_id(nsg.to_string())
            .rpc()
    };
    // An unused VPC can hold an unsafe policy definition. It is not a
    // tenant-serving path until an Instance references it.
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&permit_id)))
        .await?;
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&safe_id)))
        .await?;
    let mh = create_managed_host(&env).await;
    let instance = mh
        .instance_builer(&env)
        .tenant_org(tenant)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    let pending_segment_id = create_tenant_network_segment(
        &env.api,
        Some(pending_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "PENDING",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let current = instance.rpc_instance().await;
    let mut pending = current.config().inner().clone();
    pending.network = Some(single_interface_network_config(pending_segment_id));
    env.api
        .update_instance_config(
            rpc::forge::InstanceConfigUpdateRequest::builder()
                .instance_id(instance.id)
                .config(pending)
                .metadata(current.metadata().clone())
                .tonic_request(),
        )
        .await?;

    let nsg_update = |attributes| rpc::forge::UpdateNetworkSecurityGroupRequest {
        id: safe_id.clone(),
        tenant_organization_id: tenant.to_string(),
        metadata: Some(rpc::forge::Metadata {
            name: "vpc-policy".to_string(),
            ..Default::default()
        }),
        network_security_group_attributes: Some(attributes),
        if_version_match: None,
    };
    let nsg_query = rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
        network_security_group_ids: vec![safe_id.clone()],
        tenant_organization_id: None,
    };
    let before_nsg = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(nsg_query.clone()))
        .await?
        .into_inner();
    let error = env
        .api
        .update_network_security_group(tonic::Request::new(nsg_update(permit.clone())))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );
    assert_eq!(
        env.api
            .find_network_security_groups_by_ids(tonic::Request::new(nsg_query.clone()))
            .await?
            .into_inner(),
        before_nsg
    );
    let mut inline_update = vpc_update(&safe_id);
    inline_update.routing_profile_overrides = Some(rpc::forge::VpcRoutingProfileOverrides {
        leak_default_route_from_underlay: Some(true),
        ..Default::default()
    });
    let active_and_pending_query = rpc::forge::VpcsByIdsRequest {
        vpc_ids: vec![pending_vpc_id, active_vpc_id],
    };
    let before_active_and_pending_vpcs = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(active_and_pending_query.clone()))
        .await?
        .into_inner();
    for (case, request) in [
        ("VPC NSG replacement", vpc_update(&permit_id)),
        ("inline profile", inline_update.clone()),
    ] {
        let error = env
            .api
            .update_vpc(tonic::Request::new(request))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition, "{case}: {error}");
        assert!(
            error.message().contains("not safe for tenant prefix reuse"),
            "{case}: {error}"
        );
    }
    let active_vpc = before_active_and_pending_vpcs
        .vpcs
        .iter()
        .find(|vpc| vpc.id == Some(active_vpc_id))
        .unwrap();
    let error = env
        .api
        .change_vpc_routing_profile(tonic::Request::new(
            rpc::forge::VpcChangeRoutingProfileRequest {
                id: Some(active_vpc_id),
                if_version_match: Some(active_vpc.version.clone()),
                routing_profile_type: "EXTERNAL".to_string(),
                vni: None,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );
    assert_eq!(
        env.api
            .find_vpcs_by_ids(tonic::Request::new(active_and_pending_query))
            .await?
            .into_inner(),
        before_active_and_pending_vpcs
    );

    // Assignment advances without another API request, even if release was
    // requested before activation. Deletion intent is not network withdrawal.
    for (state, use_admin_network, deleted) in [
        (ManagedHostState::Ready, true, false),
        (ManagedHostState::Ready, true, true),
        (
            ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForNetworkSegmentToBeReady,
            },
            false,
            false,
        ),
    ] {
        let mut txn = env.db_txn().await;
        let (mut network, version) = db::machine::get_network_config(txn.as_mut(), &mh.id.into())
            .await?
            .take();
        network.use_admin_network = Some(use_admin_network);
        assert_eq!(
            db::machine::try_update_network_config(&mut txn, &mh.id.into(), version, &network)
                .await?,
            db::ConditionalWrite::Applied(())
        );
        db::machine::update_state(&mut txn, &mh.id.into(), &state).await?;
        sqlx::query(
            "UPDATE instances SET deleted = CASE WHEN $1 THEN NOW() ELSE NULL END WHERE id = $2",
        )
        .bind(deleted)
        .bind(instance.id)
        .execute(txn.as_mut())
        .await?;
        txn.commit().await?;
        let error = env
            .api
            .update_network_security_group(tonic::Request::new(nsg_update(permit.clone())))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition, "{state:?}: {error}");
        for request in [vpc_update(&permit_id), inline_update.clone()] {
            let error = env
                .api
                .update_vpc(tonic::Request::new(request))
                .await
                .unwrap_err();
            assert_eq!(error.code(), Code::FailedPrecondition, "{state:?}: {error}");
        }
    }
    assert_eq!(
        env.api
            .find_network_security_groups_by_ids(tonic::Request::new(nsg_query.clone()))
            .await?
            .into_inner(),
        before_nsg
    );
    assert_eq!(
        env.api
            .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
                vpc_ids: vec![pending_vpc_id, active_vpc_id],
            }))
            .await?
            .into_inner(),
        before_active_and_pending_vpcs
    );

    // The Instance's NSG replaces the VPC's default. Changing the shadowed
    // VPC policy is permitted; it does not change this host's effective NSG.
    sqlx::query("UPDATE instances SET network_security_group_id = $1 WHERE id = $2")
        .bind(&permit_id)
        .bind(instance.id)
        .execute(&env.pool)
        .await?;
    env.api
        .update_network_security_group(tonic::Request::new(nsg_update(permit.clone())))
        .await?;
    env.api
        .update_network_security_group(tonic::Request::new(nsg_update(
            rpc::forge::NetworkSecurityGroupAttributes::default(),
        )))
        .await?;
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&permit_id)))
        .await?;
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&safe_id)))
        .await?;
    sqlx::query("UPDATE instances SET network_security_group_id = NULL WHERE id = $1")
        .bind(instance.id)
        .execute(&env.pool)
        .await?;

    // Discovery suppresses NSGs, but it still uses the VPC routing profile.
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        &mut txn,
        &mh.id.into(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: RetryInfo { count: 0 },
            },
        },
    )
    .await?;
    txn.commit().await?;
    env.api
        .update_network_security_group(tonic::Request::new(nsg_update(permit.clone())))
        .await?;
    let error = env
        .api
        .update_vpc(tonic::Request::new(inline_update))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );
    env.api
        .update_network_security_group(tonic::Request::new(nsg_update(
            rpc::forge::NetworkSecurityGroupAttributes::default(),
        )))
        .await?;

    // Deletion intent does not withdraw an Instance's tenant network or its
    // direct NSG attachment. Keep it visible until the controller does so.
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        &mut txn,
        &mh.id.into(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await?;
    sqlx::query(
        "UPDATE instances SET network_security_group_id = $1, deleted = NOW() WHERE id = $2",
    )
    .bind(&override_id)
    .bind(instance.id)
    .execute(txn.as_mut())
    .await?;
    txn.commit().await?;
    let mut request = nsg_update(rpc::forge::NetworkSecurityGroupAttributes {
        stateful_egress: true,
        rules: vec![],
    });
    request.id = override_id;
    request.metadata.as_mut().unwrap().name = "instance-policy".to_string();
    let error = env
        .api
        .update_network_security_group(tonic::Request::new(request))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );

    let unused_update = |attributes| rpc::forge::UpdateNetworkSecurityGroupRequest {
        id: permit_id.clone(),
        tenant_organization_id: tenant.to_string(),
        metadata: Some(rpc::forge::Metadata {
            name: "unused-permit-policy".to_string(),
            ..Default::default()
        }),
        network_security_group_attributes: Some(attributes),
        if_version_match: None,
    };
    let mut deny = permit.clone();
    deny.rules[0].action = rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into();
    let mut unchanged_permit = permit.clone();
    unchanged_permit.rules[0].id = None;
    let mut overlap_lock = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut overlap_lock).await?;
    for (case, attributes) in [
        ("unchanged permit without a rule ID", unchanged_permit),
        ("deny-only replacement", deny.clone()),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            env.api
                .update_network_security_group(tonic::Request::new(unused_update(
                    attributes.clone(),
                ))),
        )
        .await
        .expect("nonexpanding NSG policy must not wait for the overlap lock")?;
        let mut returned = result
            .into_inner()
            .network_security_group
            .unwrap()
            .attributes
            .unwrap();
        if attributes.rules[0].id.is_none() {
            assert!(returned.rules[0].id.take().is_some_and(|id| !id.is_empty()));
        }
        assert_eq!(returned, attributes, "{case}");
    }
    overlap_lock.rollback().await?;
    env.api
        .update_network_security_group(tonic::Request::new(unused_update(permit.clone())))
        .await?;
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&permit_id)))
        .await?;

    // A request that classified its old permit rules as unchanged cannot put
    // them back after another writer commits a restrictive replacement.
    let mut writer = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut writer).await?;
    let writer_pid = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;
    let id = permit_id.parse()?;
    let old =
        db::network_security_group::find_by_ids(&mut writer, std::slice::from_ref(&id), None, true)
            .await?
            .pop()
            .unwrap();
    let deny_rules = deny
        .rules
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
    let restricted = db::network_security_group::update(
        &mut writer,
        &id,
        &old.tenant_organization_id,
        &old.metadata,
        false,
        &deny_rules,
        old.version,
        None,
    )
    .await?;
    let mut metadata_update = vpc_update(&permit_id);
    metadata_update.metadata.as_mut().unwrap().description =
        "unchanged NSG does not wait".to_string();
    tokio::time::timeout(
        Duration::from_secs(10),
        env.api.update_vpc(tonic::Request::new(metadata_update)),
    )
    .await
    .expect("metadata with the current NSG must not wait for that NSG row")?;
    let pending_vpc_query = rpc::forge::VpcsByIdsRequest {
        vpc_ids: vec![pending_vpc_id],
    };
    let updated_pending_vpc = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(pending_vpc_query.clone()))
        .await?
        .into_inner()
        .vpcs
        .pop()
        .unwrap();
    assert_eq!(
        updated_pending_vpc.metadata.unwrap().description,
        "unchanged NSG does not wait"
    );
    let waiter = env
        .api
        .update_network_security_group(tonic::Request::new(unused_update(permit.clone())));
    let release = async {
        wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
        writer.commit().await
    };
    let (result, released) = tokio::join!(waiter, release);
    released?;
    let error = result.unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert!(error.message().contains("version"), "{error}");
    let mut txn = env.db_txn().await;
    assert_eq!(
        db::network_security_group::find_by_ids(&mut txn, std::slice::from_ref(&id), None, false)
            .await?,
        vec![restricted.clone()]
    );
    txn.rollback().await?;
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&safe_id)))
        .await?;

    // VPC assignment classifies the target NSG too. A safe NSG may gain
    // permits while the assignment waits for its row lock.
    let before_pending_vpc = env
        .api
        .find_vpcs_by_ids(tonic::Request::new(pending_vpc_query.clone()))
        .await?
        .into_inner();
    let mut writer = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut writer).await?;
    let writer_pid = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;
    let permit_rules = permit
        .rules
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
    db::network_security_group::update(
        &mut writer,
        &id,
        &restricted.tenant_organization_id,
        &restricted.metadata,
        false,
        &permit_rules,
        restricted.version,
        None,
    )
    .await?;
    let assignment = env
        .api
        .update_vpc(tonic::Request::new(vpc_update(&permit_id)));
    let release = async {
        wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
        writer.commit().await
    };
    let (result, released) = tokio::join!(assignment, release);
    released?;
    let error = result.expect_err("VPC assignment must reject the changed NSG");
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert!(error.message().contains("version"), "{error}");
    assert_eq!(
        env.api
            .find_vpcs_by_ids(tonic::Request::new(pending_vpc_query))
            .await?
            .into_inner(),
        before_pending_vpc
    );

    // After deletion has entered network teardown, changing policy must not
    // prevent the controller from withdrawing the old tenant configuration.
    let mut txn = env.db_txn().await;
    let (mut network, version) = db::machine::get_network_config(txn.as_mut(), &mh.id.into())
        .await?
        .take();
    network.use_admin_network = Some(true);
    assert_eq!(
        db::machine::try_update_network_config(&mut txn, &mh.id.into(), version, &network).await?,
        db::ConditionalWrite::Applied(())
    );
    db::machine::update_state(
        &mut txn,
        &mh.id.into(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForNetworkReconfig,
        },
    )
    .await?;
    sqlx::query("UPDATE instances SET network_security_group_id = NULL WHERE id = $1")
        .bind(instance.id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;
    let updated = env
        .api
        .update_network_security_group(tonic::Request::new(nsg_update(
            rpc::forge::NetworkSecurityGroupAttributes {
                stateful_egress: true,
                rules: vec![],
            },
        )))
        .await?
        .into_inner();
    assert!(
        updated
            .network_security_group
            .unwrap()
            .attributes
            .unwrap()
            .stateful_egress
    );
    env.api
        .update_vpc(tonic::Request::new(vpc_update(&permit_id)))
        .await?;
    Ok(())
}

#[crate::sqlx_test]
async fn network_security_group_gate_off_concurrent_updates(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.tenant_prefix_overlap_enabled = false;
    config.network_security_group.stateful_acls_enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let tenant = "concurrent-nsg-updates";
    crate::tests::common::api_fixtures::tenant::create_fixture_tenant(&env, tenant).await?;

    for (scenario, stateful_egress, match_version) in [
        ("unconditional metadata", false, false),
        ("unconditional expansion", true, false),
        ("explicit version", false, true),
    ] {
        let nsg = env
            .api
            .create_network_security_group(tonic::Request::new(
                rpc::forge::CreateNetworkSecurityGroupRequest {
                    id: Some(uuid::Uuid::new_v4().to_string()),
                    tenant_organization_id: tenant.to_string(),
                    metadata: Some(rpc::forge::Metadata {
                        name: scenario.to_string(),
                        ..Default::default()
                    }),
                    network_security_group_attributes: Some(Default::default()),
                },
            ))
            .await?
            .into_inner()
            .network_security_group
            .unwrap();
        let id = nsg.id.parse()?;
        let mut writer = env.db_txn().await;
        let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await?;
        let old = db::network_security_group::find_by_ids(
            &mut writer,
            std::slice::from_ref(&id),
            None,
            true,
        )
        .await?
        .pop()
        .unwrap();
        let mut metadata = old.metadata.clone();
        metadata.description = "competing update".to_string();
        let competing = db::network_security_group::update(
            &mut writer,
            &id,
            &old.tenant_organization_id,
            &metadata,
            false,
            &[],
            old.version,
            None,
        )
        .await?;
        let mut overlap_blocker = env.db_txn().await;
        if !stateful_egress {
            db::tenant_prefix_overlap::lock_checks(&mut overlap_blocker).await?;
        }
        let request = rpc::forge::UpdateNetworkSecurityGroupRequest {
            id: nsg.id,
            tenant_organization_id: tenant.to_string(),
            metadata: Some(rpc::forge::Metadata {
                name: scenario.to_string(),
                description: "requested update".to_string(),
                ..Default::default()
            }),
            network_security_group_attributes: Some(rpc::forge::NetworkSecurityGroupAttributes {
                stateful_egress,
                rules: vec![],
            }),
            if_version_match: match_version.then_some(nsg.version),
        };
        let update = env
            .api
            .update_network_security_group(tonic::Request::new(request));
        let release = async {
            wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
            writer.commit().await
        };
        let (result, released) = tokio::time::timeout(Duration::from_secs(40), async {
            tokio::join!(update, release)
        })
        .await?;
        released?;
        overlap_blocker.rollback().await?;
        let mut txn = env.db_txn().await;
        let persisted = db::network_security_group::find_by_ids(
            &mut txn,
            std::slice::from_ref(&id),
            None,
            false,
        )
        .await?
        .pop()
        .unwrap();
        txn.rollback().await?;
        if match_version {
            let error = result.expect_err(scenario);
            assert_eq!(
                error.code(),
                Code::FailedPrecondition,
                "{scenario}: {error}"
            );
            assert_eq!(persisted, competing, "{scenario}");
        } else {
            let returned = result?.into_inner().network_security_group.unwrap();
            assert_eq!(
                persisted.metadata.description, "requested update",
                "{scenario}"
            );
            assert_eq!(persisted.stateful_egress, stateful_egress, "{scenario}");
            assert_eq!(
                persisted.version.version_nr(),
                competing.version.version_nr() + 1,
                "{scenario}"
            );
            let expected: rpc::forge::NetworkSecurityGroup = persisted.try_into()?;
            assert_eq!(returned, expected, "{scenario}");
        }
    }
    Ok(())
}

#[crate::sqlx_test]
async fn tenant_prefix_overlap_gate_off_preserves_policy_contraction(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.tenant_prefix_overlap_enabled = false;
    config.default_tenant_routing_profile_type = "INTERNAL".to_string();
    config.network_security_group.stateful_acls_enabled = true;
    let fnn = FnnConfig {
        admin_vpc: None,
        common_internal_route_target: None,
        additional_route_target_imports: vec![],
        routing_profiles: HashMap::from([(
            "INTERNAL".to_string(),
            FnnRoutingProfileConfig {
                tenant_prefix_overlap_eligible: true,
                internal: Some(true),
                ..Default::default()
            },
        )]),
        use_vpc_vrf_loopback: false,
    };
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            ..TestEnvOverrides::with_config(config).with_fnn_config(Some(fnn))
        },
    )
    .await;
    let tenant = "gate-off-policy";
    crate::tests::common::api_fixtures::tenant::create_fixture_tenant(&env, tenant).await?;
    let nsg_id = uuid::Uuid::new_v4().to_string();
    env.api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(nsg_id.clone()),
                tenant_organization_id: tenant.to_string(),
                metadata: Some(rpc::forge::Metadata {
                    name: nsg_id.clone(),
                    ..Default::default()
                }),
                network_security_group_attributes: Some(Default::default()),
            },
        ))
        .await?;
    let vpc_id = VpcId::new();
    let mut request = VpcCreationRequest::builder(tenant)
        .id(vpc_id)
        .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
        .routing_profile_type("INTERNAL".to_string())
        .metadata(Metadata::new_with_default_name())
        .rpc();
    request.network_security_group_id = Some(nsg_id.clone());
    env.api.create_vpc(tonic::Request::new(request)).await?;
    let segment_id = create_tenant_network_segment(
        &env.api,
        Some(vpc_id),
        "10.117.1.1/24".parse()?,
        "gate-off-policy",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let host = create_managed_host(&env).await;
    let _instance = host
        .instance_builer(&env)
        .tenant_org(tenant)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let nsg_update = |stateful_egress| {
        tonic::Request::new(rpc::forge::UpdateNetworkSecurityGroupRequest {
            id: nsg_id.clone(),
            tenant_organization_id: tenant.to_string(),
            metadata: Some(rpc::forge::Metadata {
                name: nsg_id.clone(),
                ..Default::default()
            }),
            network_security_group_attributes: Some(rpc::forge::NetworkSecurityGroupAttributes {
                stateful_egress,
                rules: vec![],
            }),
            if_version_match: None,
        })
    };
    let read_nsg = || {
        env.api
            .find_network_security_groups_by_ids(tonic::Request::new(
                rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                    network_security_group_ids: vec![nsg_id.clone()],
                    tenant_organization_id: None,
                },
            ))
    };
    let vpc_update = |leak_default_route_from_underlay| {
        tonic::Request::new(rpc::forge::VpcUpdateRequest {
            id: Some(vpc_id),
            metadata: Some(rpc::forge::Metadata {
                name: "gate-off-policy".to_string(),
                ..Default::default()
            }),
            network_security_group_id: Some(nsg_id.clone()),
            routing_profile_overrides: Some(rpc::forge::VpcRoutingProfileOverrides {
                leak_default_route_from_underlay: Some(leak_default_route_from_underlay),
                ..Default::default()
            }),
            ..Default::default()
        })
    };
    // The disabled gate preserves ordinary policy changes without duplicates.
    env.api
        .update_network_security_group(nsg_update(true))
        .await?;
    env.api
        .update_network_security_group(nsg_update(false))
        .await?;
    env.api.update_vpc(vpc_update(true)).await?;
    env.api.update_vpc(vpc_update(false)).await?;

    let update_after_restriction = async || {
        let id = nsg_id.parse()?;
        let mut writer = env.db_txn().await;
        let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await?;
        let old = db::network_security_group::find_by_ids(
            &mut writer,
            std::slice::from_ref(&id),
            None,
            true,
        )
        .await?
        .pop()
        .unwrap();
        assert!(old.stateful_egress);
        let restricted = db::network_security_group::update(
            &mut writer,
            &id,
            &old.tenant_organization_id,
            &old.metadata,
            false,
            &[],
            old.version,
            None,
        )
        .await?;
        let mut overlap_blocker = env.db_txn().await;
        db::tenant_prefix_overlap::lock_checks(&mut overlap_blocker).await?;
        let overlap_blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(overlap_blocker.as_mut())
            .await?;
        let update = env.api.update_network_security_group(nsg_update(true));
        let release = async {
            // The request first sees unchanged policy, then discovers the
            // restriction after waiting for the NSG row.
            wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
            writer.commit().await?;
            wait_for_blocked_query(
                &env.pool,
                overlap_blocker_pid,
                "tenant_prefix_overlap:checks",
            )
            .await;
            // The overlap wait must not keep the first transaction's row lock.
            let available = tokio::time::timeout(
                Duration::from_secs(10),
                db::network_security_group::find_by_ids(
                    &mut overlap_blocker,
                    std::slice::from_ref(&id),
                    None,
                    true,
                ),
            )
            .await??;
            assert_eq!(available, vec![restricted.clone()]);
            overlap_blocker.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (result, released) = tokio::join!(update, release);
        released?;
        Ok::<_, Box<dyn std::error::Error>>((result, restricted))
    };
    env.api
        .update_network_security_group(nsg_update(true))
        .await?;
    let (result, restricted) = update_after_restriction().await?;
    let updated = result?.into_inner().network_security_group.unwrap();
    assert!(updated.attributes.as_ref().unwrap().stateful_egress);
    assert_eq!(
        updated.version.parse::<ConfigVersion>()?.version_nr(),
        restricted.version.version_nr() + 1
    );
    assert_eq!(
        read_nsg().await?.into_inner().network_security_groups,
        vec![updated]
    );
    env.api
        .update_network_security_group(nsg_update(false))
        .await?;

    let other = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .routing_profile_type("INTERNAL".to_string())
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await?
        .into_inner();
    let other_id = other.id.unwrap();
    let mut txn = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    let root = db::site_prefix::create_tenant_managed(
        model::site_prefix::NewTenantManagedSitePrefix {
            id: carbide_uuid::site_prefix::SitePrefixId::new(),
            tenant_organization_id: tenant.parse()?,
            prefix: "10.117.0.0/16".parse()?,
            metadata: Metadata::new_with_default_name(),
        },
        env.config.max_site_prefixes_per_tenant,
        &mut txn,
    )
    .await?
    .site_prefix;
    sqlx::query("UPDATE site_prefixes SET lifecycle_state = 'ready' WHERE id = $1")
        .bind(root.id)
        .execute(txn.as_mut())
        .await?;
    // Mixed tables allow a retained duplicate fixture without removing either
    // exclusion constraint. Deletion intent must not hide its address space.
    let prefix_id = carbide_uuid::vpc::VpcPrefixId::new();
    db::vpc_prefix::persist(
        model::vpc_prefix::NewVpcPrefix {
            id: prefix_id,
            site_prefix_id: Some(root.id),
            vpc_id: other_id,
            overlap_vpc_id: None,
            config: model::vpc_prefix::VpcPrefixConfig {
                prefix: "10.117.1.0/24".parse()?,
            },
            metadata: Metadata::new_with_default_name(),
        },
        other.version.parse()?,
        &mut txn,
    )
    .await?;
    let version = sqlx::query_scalar("SELECT version FROM vpcs WHERE id = $1")
        .bind(other_id)
        .fetch_one(txn.as_mut())
        .await?;
    db::vpc_prefix::mark_as_deleted(
        &model::vpc_prefix::DeleteVpcPrefix { id: prefix_id },
        version,
        &mut txn,
    )
    .await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(txn.as_mut())
        .await?;
    let before_nsg = read_nsg().await?.into_inner();
    let expansion = env.api.update_network_security_group(nsg_update(true));
    let release = async {
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
        txn.commit().await
    };
    let (result, released) = tokio::join!(expansion, release);
    released?;
    let error = result.expect_err("gate-off expansion must see the newly committed duplicate");
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert_eq!(read_nsg().await?.into_inner(), before_nsg);
    let read_vpc = || {
        env.api
            .find_vpcs_by_ids(tonic::Request::new(rpc::forge::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
    };
    let before_vpc = read_vpc().await?.into_inner();
    let error = env.api.update_vpc(vpc_update(true)).await.unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert_eq!(read_vpc().await?.into_inner(), before_vpc);

    // Assignment must recheck an NSG that gained permissions while the VPC
    // request waited for its row lock.
    let target_id = uuid::Uuid::new_v4().to_string();
    env.api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: Some(target_id.clone()),
                tenant_organization_id: tenant.to_string(),
                metadata: Some(rpc::forge::Metadata {
                    name: "concurrent assignment policy".to_string(),
                    ..Default::default()
                }),
                network_security_group_attributes: Some(Default::default()),
            },
        ))
        .await?;
    let id = target_id.parse()?;
    let mut writer = env.db_txn().await;
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;
    let old =
        db::network_security_group::find_by_ids(&mut writer, std::slice::from_ref(&id), None, true)
            .await?
            .pop()
            .unwrap();
    let expanded = db::network_security_group::update(
        &mut writer,
        &id,
        &old.tenant_organization_id,
        &old.metadata,
        true,
        &[],
        old.version,
        None,
    )
    .await?;
    let mut overlap_blocker = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut overlap_blocker).await?;
    let overlap_blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(overlap_blocker.as_mut())
        .await?;
    let assignment = env
        .api
        .update_vpc(tonic::Request::new(rpc::forge::VpcUpdateRequest {
            network_security_group_id: Some(target_id),
            ..vpc_update(false).into_inner()
        }));
    let release = async {
        wait_for_blocked_query(&env.pool, writer_pid, "network_security_groups").await;
        writer.commit().await?;
        wait_for_blocked_query(
            &env.pool,
            overlap_blocker_pid,
            "tenant_prefix_overlap:checks",
        )
        .await;
        let available = tokio::time::timeout(
            Duration::from_secs(10),
            db::network_security_group::find_by_ids(&mut overlap_blocker, &[id], None, true),
        )
        .await??;
        assert_eq!(available, vec![expanded]);
        overlap_blocker.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (result, released) = tokio::join!(assignment, release);
    released?;
    let error = result.expect_err("assignment must recheck the expanded NSG");
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );
    assert_eq!(read_vpc().await?.into_inner(), before_vpc);

    // With retained duplicates, the same concurrent restriction must not be
    // undone after the request reacquires the overlap lock.
    sqlx::query("UPDATE network_security_groups SET stateful_egress = true WHERE id = $1")
        .bind(&nsg_id)
        .execute(&env.pool)
        .await?;
    let (result, restricted) = update_after_restriction().await?;
    let error = result.expect_err("reclassified expansion must check retained duplicates");
    assert_eq!(error.code(), Code::FailedPrecondition, "{error}");
    assert!(
        error.message().contains("not safe for tenant prefix reuse"),
        "{error}"
    );
    let expected: rpc::forge::NetworkSecurityGroup = restricted.try_into()?;
    assert_eq!(
        read_nsg().await?.into_inner().network_security_groups,
        vec![expected]
    );

    // Restore the unsafe policy so recovery removes permissions rather than
    // resubmitting an already safe configuration.
    sqlx::query("UPDATE network_security_groups SET stateful_egress = true WHERE id = $1")
        .bind(&nsg_id)
        .execute(&env.pool)
        .await?;
    sqlx::query(
        "UPDATE vpcs SET routing_profile_overrides = \
         jsonb_build_object('leak_default_route_from_underlay', true) WHERE id = $1",
    )
    .bind(vpc_id)
    .execute(&env.pool)
    .await?;
    let mut blocker = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut blocker).await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        env.api.update_network_security_group(nsg_update(false)),
    )
    .await??;
    tokio::time::timeout(
        Duration::from_secs(10),
        env.api.update_vpc(vpc_update(false)),
    )
    .await??;
    blocker.rollback().await?;
    assert!(
        !read_nsg().await?.into_inner().network_security_groups[0]
            .attributes
            .as_ref()
            .unwrap()
            .stateful_egress
    );
    assert_eq!(
        read_vpc().await?.into_inner().vpcs[0]
            .config
            .as_ref()
            .unwrap()
            .routing_profile_overrides
            .as_ref()
            .unwrap()
            .leak_default_route_from_underlay,
        Some(false)
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_network_security_group_propagation_one_dpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 1, 1).await
}

#[crate::sqlx_test]
async fn test_network_security_group_propagation_one_of_two_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 2, 1).await
}

#[crate::sqlx_test]
async fn test_network_security_group_propagation_two_of_two_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 2, 2).await
}

#[crate::sqlx_test]
async fn test_network_security_group_propagation_one_of_ten_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 10, 1).await
}
#[crate::sqlx_test]
async fn test_network_security_group_propagation_five_of_ten_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 10, 5).await
}
#[crate::sqlx_test]
async fn test_network_security_group_propagation_ten_of_ten_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    test_network_security_group_propagation_impl(pool, 10, 10).await
}

/// VPC propagation follows persisted interface ownership even when SLAAC has
/// no concrete row in `instance_addresses`.
#[crate::sqlx_test]
async fn test_vpc_network_security_group_propagation_includes_addressless_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    populate_network_security_groups(env.api.clone()).await;

    let tenant_organization_id = "Tenant1";
    let network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";
    let vpc_id = VpcId::new();
    let instance_id = InstanceId::new();
    let segment_ids = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(tenant_organization_id)
                .id(vpc_id)
                .metadata(Metadata::new_with_default_name())
                .network_security_group_id(network_security_group_id)
                .rpc(),
            1,
        )
        .await;
    let [segment_id] = segment_ids.as_slice() else {
        panic!("expected one tenant segment");
    };
    let segment_id = *segment_id;
    let managed_host = site_explorer::new_host(&env, ManagedHostConfig::default()).await?;
    env.api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(stable_host_id(managed_host.host_snapshot.id)),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: Some(instance_id),
            instance_type_id: None,
            metadata: Some(rpc::Metadata {
                name: "addressless-vpc-nsg-instance".to_string(),
                ..Default::default()
            }),
            allow_unhealthy_machine: false,
        }))
        .await?;

    // Model the missing normalized address row that matters for SLAAC while
    // retaining the resolved VPC and segment references written by allocation.
    let mut txn = env.db_txn().await;
    sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1")
        .bind(instance_id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;

    let propagation = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await?
        .into_inner();
    let [vpc_status] = propagation.vpcs.as_slice() else {
        panic!("expected one VPC propagation result");
    };
    assert_eq!(vpc_status.id, vpc_id.to_string());
    assert_eq!(
        vpc_status.status,
        rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone as i32,
    );
    assert_eq!(
        vpc_status.related_instance_ids,
        vec![instance_id.to_string()]
    );
    assert_eq!(
        vpc_status.unpropagated_instance_ids,
        vec![instance_id.to_string()],
    );

    Ok(())
}

async fn test_network_security_group_propagation_impl(
    pool: sqlx::PgPool,
    dpu_count: usize,
    mut instance_interface_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    populate_network_security_groups(env.api.clone()).await;

    // Provided by fixtures
    let default_tenant_org = "Tenant1";

    // Our known fixture network security group
    let good_network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    let vpc_id: VpcId = uuid!("2ff5ba26-da6a-11ef-9c48-5b78e547a5e7").into();
    let instance_id: InstanceId = uuid!("46c555e0-da6a-11ef-b86d-db132142d068").into();

    let good_network_security_group = env
        .api
        .find_network_security_groups_by_ids(tonic::Request::new(
            rpc::forge::FindNetworkSecurityGroupsByIdsRequest {
                // Provided by fixtures
                network_security_group_ids: vec![good_network_security_group_id.to_string()],
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_groups
        .pop()
        .unwrap();

    // Check propagation status before doing anything else,
    // but make a bad request.
    let err = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap_err();

    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains("at least one"));

    // Check propagation status again before doing anything else.
    // There should be no objects with any attached NSG
    // and no status should have been reported yet,
    // so there should be no results for any instance or VPC.
    // The results should be empty arrays.
    let prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![instance_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now create some objects with NSGs attached.

    let segment_ids = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(default_tenant_org)
                .id(vpc_id)
                .metadata(Metadata::new_with_default_name())
                .network_security_group_id(good_network_security_group_id)
                .rpc(),
            dpu_count,
        )
        .await;

    let mut dpus = Vec::with_capacity(dpu_count);
    for _ in 0..dpu_count {
        // default handles making the dpu unique
        dpus.push(DpuConfig::default());
    }
    let managed_host_config = ManagedHostConfig {
        dpus,
        ..ManagedHostConfig::default()
    };

    // Create a new managed host in the DB and get the snapshot.
    let mh = site_explorer::new_host(&env, managed_host_config)
        .await
        .unwrap();

    let device_maps = mh.host_snapshot.get_dpu_device_and_id_mappings().unwrap();
    let mut device_locators = Vec::default();
    for (device, device_count) in device_maps
        .1
        .iter()
        .map(|(device, id_list)| (device, id_list.len()))
    {
        for device_instance in 0..device_count {
            instance_interface_count -= 1;
            device_locators.push(DeviceLocator {
                device: device.clone(),
                device_instance,
            });
            if instance_interface_count == 0 {
                break;
            }
        }
        if instance_interface_count == 0 {
            break;
        }
    }
    // Create an Instance
    let _ = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(stable_host_id(mh.host_snapshot.id)),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(interface_network_config_with_devices(
                    &segment_ids,
                    &device_locators,
                )),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                network_security_group_id: Some(good_network_security_group_id.to_string()),
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: Some(instance_id),
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance".to_string(),
                description: "desc".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .unwrap();

    let prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![instance_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // No status should have been reported yet, so we
    // should see the instance as having no propagation.
    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![],
        instances: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: instance_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone.into(),
            details: None,
            related_instance_ids: vec![instance_id.to_string()],
            unpropagated_instance_ids: vec![instance_id.to_string()],
        }],
    };

    assert_eq!(prop_status, expected_results);

    // peek into the db to get the internal id.  note that the state machine has not processed the instance yet
    // so getting the network via the api will not work.
    let instance = db::instance::find_by_id(&pool, instance_id)
        .await
        .unwrap()
        .unwrap();

    let internal_interface_ids: Vec<_> = instance
        .config
        .network
        .interfaces
        .iter()
        .map(|i| i.internal_uuid.into())
        .collect();

    // Now make a call to report status.
    tracing::info!("updating network obs");
    for (internal_id, dpu_machine_id) in internal_interface_ids
        .iter()
        .zip(mh.dpu_snapshots.iter().map(|s| s.id))
    {
        update_network_status_observation(
            &env,
            &instance_id,
            good_network_security_group_id,
            &good_network_security_group.version,
            &dpu_machine_id,
            rpc::forge::NetworkSecurityGroupSource::NsgSourceInstance,
            internal_id,
        )
        .await;
    }
    // Now that DPU status has been reported,
    // check propagation status again.
    let prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![instance_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Up to now, the VPC and instance both have an NSG configured.
    // The instance should take precedence, so we should only see
    // that in the list, and the VPC has no children who need propagation.
    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![],
        instances: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: instance_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull.into(),
            details: None,
            related_instance_ids: vec![instance_id.to_string()],
            unpropagated_instance_ids: vec![],
        }],
    };

    assert_eq!(prop_status, expected_results);

    // Now update the instance to remove the NSG attachment
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(interface_network_config_with_devices(
                        &segment_ids,
                        &device_locators,
                    )),
                    infiniband: None,
                    nvlink: None,
                    spxconfig: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    power_profile: None,
                }),
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

    // Confirm that the security ID has been removed from the instance.
    assert_eq!(instance.config.unwrap().network_security_group_id, None);

    // Now check status again and we should see the VPC reported with
    // no propagation
    let prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone.into(),
            details: None,
            related_instance_ids: vec![instance_id.to_string()],
            unpropagated_instance_ids: vec![instance_id.to_string()],
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now send an observation update to make it look like
    // the DPU updated and has the NSG with the VPC source
    for (internal_id, dpu_machine_id) in internal_interface_ids
        .iter()
        .zip(mh.dpu_snapshots.iter().map(|s| s.id))
    {
        update_network_status_observation(
            &env,
            &instance_id,
            good_network_security_group_id,
            &good_network_security_group.version,
            &dpu_machine_id,
            rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc,
            internal_id,
        )
        .await;
    }

    // Now check status again, and we should see the VPC with full propagation.
    let prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull.into(),
            details: None,
            related_instance_ids: vec![instance_id.to_string()],
            unpropagated_instance_ids: vec![],
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now add another machine and instance with no NSG
    // attached for the same VPC.
    let mh2 = site_explorer::new_host(&env, ManagedHostConfig::default())
        .await
        .unwrap();

    let instance_id2: InstanceId = uuid!("16faf95e-dcb9-11ef-96b1-d3941046d310").into();
    // Create an Instance
    let _ = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(stable_host_id(mh2.host_snapshot.id)),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(
                    *segment_ids.first().unwrap(),
                )),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: Some(instance_id2),
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance2".to_string(),
                description: "desc2".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .unwrap();

    // peek into the db to get the internal id.  note that the state machine has not processed the instance yet
    // so getting the network via the api will not work.
    let instance = db::instance::find_by_id(&pool, instance_id2)
        .await
        .unwrap()
        .unwrap();

    let internal_interface_ids2: Vec<_> = instance
        .config
        .network
        .interfaces
        .iter()
        .map(|i| i.internal_uuid.into())
        .collect();

    // Now check status again and we should see the VPC with partial propagation
    let mut prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let mut both_instances_sorted = vec![instance_id.to_string(), instance_id2.to_string()];
    both_instances_sorted.sort();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusPartial.into(),
            details: None,
            related_instance_ids: both_instances_sorted.clone(),
            unpropagated_instance_ids: vec![instance_id2.to_string()],
        }],
        instances: vec![],
    };

    prop_status.vpcs[0].related_instance_ids.sort();
    prop_status.vpcs[0].unpropagated_instance_ids.sort();

    assert_eq!(prop_status, expected_results);

    // Now send an observation update to make it look like
    // the DPU of the other instance updated and has the NSG
    // with the VPC source.
    for (internal_id, dpu_machine_id) in internal_interface_ids2
        .iter()
        .zip(mh2.dpu_snapshots.iter().map(|s| s.id))
    {
        update_network_status_observation(
            &env,
            &instance_id2,
            good_network_security_group_id,
            &good_network_security_group.version,
            &dpu_machine_id,
            rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc,
            internal_id,
        )
        .await;
    }

    // Now check status again and we should see the VPC with full propagation again.
    let mut prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    prop_status.vpcs[0].related_instance_ids.sort();
    prop_status.vpcs[0].unpropagated_instance_ids.sort();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull.into(),
            details: None,
            related_instance_ids: both_instances_sorted.clone(),
            unpropagated_instance_ids: vec![],
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now we update the NSG itself, and this should send everything
    // back into an unpropagated state.

    let nsg_version = env
        .api
        .update_network_security_group(tonic::Request::new(
            rpc::forge::UpdateNetworkSecurityGroupRequest {
                id: good_network_security_group_id.to_string(),
                if_version_match: None,
                tenant_organization_id: default_tenant_org.to_string(),
                metadata: Some(rpc::forge::Metadata {
                    name: "irrelevant".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
                network_security_group_attributes: Some(
                    rpc::forge::NetworkSecurityGroupAttributes {
                        stateful_egress: false,
                        rules: vec![rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: None,
                src_port_end: None,
                dst_port_start: Some(800),
                dst_port_end: Some(900),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
                priority: 9002,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "1.1.1.1/1".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "2.2.2.2/2".to_string(),
                    ),
                ),
            }],
                    },
                ),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group
        .unwrap()
        .version;

    // Now check status again and we should see the VPC with no propagation again.
    let mut prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    prop_status.vpcs[0].related_instance_ids.sort();
    prop_status.vpcs[0].unpropagated_instance_ids.sort();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone.into(),
            details: None,
            related_instance_ids: both_instances_sorted.clone(),
            unpropagated_instance_ids: both_instances_sorted.clone(),
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now another observation with the new version.
    for (internal_id, dpu_machine_id) in internal_interface_ids
        .iter()
        .zip(mh.dpu_snapshots.iter().map(|s| s.id))
    {
        update_network_status_observation(
            &env,
            &instance_id,
            good_network_security_group_id,
            &nsg_version,
            &dpu_machine_id,
            rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc,
            internal_id,
        )
        .await;
    }

    // Now check status again and we should see the VPC with partial propagation again.
    let mut prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    prop_status.vpcs[0].related_instance_ids.sort();
    prop_status.vpcs[0].unpropagated_instance_ids.sort();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusPartial.into(),
            details: None,
            related_instance_ids: both_instances_sorted.clone(),
            unpropagated_instance_ids: vec![instance_id2.to_string()],
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    // Now send an observation update for the second instance
    for (internal_id, dpu_machine_id) in internal_interface_ids2
        .iter()
        .zip(mh2.dpu_snapshots.iter().map(|s| s.id))
    {
        update_network_status_observation(
            &env,
            &instance_id2,
            good_network_security_group_id,
            &nsg_version,
            &dpu_machine_id,
            rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc,
            internal_id,
        )
        .await;
    }

    // Now check status again and we should see the VPC with full propagation.
    let mut prop_status = env
        .api
        .get_network_security_group_propagation_status(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupPropagationStatusRequest {
                network_security_group_ids: None,
                vpc_ids: vec![vpc_id.to_string()],
                instance_ids: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    prop_status.vpcs[0].related_instance_ids.sort();
    prop_status.vpcs[0].unpropagated_instance_ids.sort();

    let expected_results = rpc::forge::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vec![rpc::forge::NetworkSecurityGroupPropagationObjectStatus {
            id: vpc_id.to_string(),
            status: rpc::forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull.into(),
            details: None,
            related_instance_ids: both_instances_sorted.clone(),
            unpropagated_instance_ids: vec![],
        }],
        instances: vec![],
    };

    assert_eq!(prop_status, expected_results);

    Ok(())
}

#[crate::sqlx_test]
async fn test_network_security_group_get_attachments(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    populate_network_security_groups(env.api.clone()).await;

    // Provided by fixtures
    let default_tenant_org = "Tenant1";

    // Our known fixture network security group
    let good_network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    let vpc_id: VpcId = uuid!("2ff5ba26-da6a-11ef-9c48-5b78e547a5e7").into();

    // Check attachments before doing anything else.
    // There should be no objects with any attached NSG.
    let prop_status = env
        .api
        .get_network_security_group_attachments(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupAttachmentsRequest {
                network_security_group_ids: vec![good_network_security_group_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupAttachmentsResponse {
        attachments: vec![rpc::forge::NetworkSecurityGroupAttachments {
            network_security_group_id: good_network_security_group_id.to_string(),
            vpc_ids: vec![],
            instance_ids: vec![],
        }],
    };

    assert_eq!(prop_status, expected_results);

    // Now create some objects with NSGs attached.

    // Create a VPC
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(default_tenant_org)
                .id(vpc_id)
                .metadata(Metadata {
                    name: "test vpc 1".to_string(),
                    ..Default::default()
                })
                .network_security_group_id(good_network_security_group_id)
                .rpc(),
        )
        .await;

    // Create a new managed host in the DB and get the snapshot.
    let mh = site_explorer::new_host(&env, ManagedHostConfig::default())
        .await
        .unwrap();

    let test_managed_host =
        TestManagedHost::from_rpc_machine(&mh.host_snapshot.clone().into(), env.api.clone());
    // Create an Instance
    let instance = test_managed_host
        .instance_builer(&env)
        .config(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            infiniband: None,
            nvlink: None,
            spxconfig: None,
            network_security_group_id: Some(good_network_security_group_id.to_string()),
            dpu_extension_services: None,
            power_profile: None,
        })
        .build()
        .await;

    // Check attachments
    let prop_status = env
        .api
        .get_network_security_group_attachments(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupAttachmentsRequest {
                network_security_group_ids: vec![good_network_security_group_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupAttachmentsResponse {
        attachments: vec![rpc::forge::NetworkSecurityGroupAttachments {
            network_security_group_id: good_network_security_group_id.to_string(),
            vpc_ids: vec![vpc_id.to_string()],
            instance_ids: vec![instance.id.to_string()],
        }],
    };

    assert_eq!(prop_status, expected_results);

    // Delete the instance and wait for it to be fully gone
    test_managed_host.delete_instance(&env, instance.id).await;

    // Delete the VPC
    env.api
        .delete_vpc(tonic::Request::new(rpc::forge::VpcDeletionRequest {
            id: Some(vpc_id),
        }))
        .await
        .unwrap();

    // Check attachments.  We should see none again.
    let prop_status = env
        .api
        .get_network_security_group_attachments(tonic::Request::new(
            rpc::forge::GetNetworkSecurityGroupAttachmentsRequest {
                network_security_group_ids: vec![good_network_security_group_id.to_string()],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let expected_results = rpc::forge::GetNetworkSecurityGroupAttachmentsResponse {
        attachments: vec![rpc::forge::NetworkSecurityGroupAttachments {
            network_security_group_id: good_network_security_group_id.to_string(),
            vpc_ids: vec![],
            instance_ids: vec![],
        }],
    };

    assert_eq!(prop_status, expected_results);

    Ok(())
}
