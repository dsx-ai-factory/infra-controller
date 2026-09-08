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

use carbide_uuid::machine::{DpuMachineId, MachineId};
use carbide_uuid::vpc::VpcId;
use carbide_uuid::vpc_peering::VpcPeeringId;
use futures_util::{FutureExt, TryFutureExt};
use model::metadata::Metadata;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    ManagedHostNetworkConfigRequest, VpcPeeringCreationRequest, VpcPeeringDeletionRequest,
    VpcPeeringList, VpcPeeringSearchFilter, VpcPeeringsByIdsRequest, VpcVirtualizationType,
};
use sqlx::PgPool;
use tonic::{IntoRequest, Request, Response, Status};
use uuid::Uuid;

use super::common::api_fixtures::{self, TestEnv, TestManagedHost};
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::instance::default_tenant_config;
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, create_tenant_network_segment,
};
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides,
};
use crate::tests::common::rpc_builder::VpcCreationRequest;

async fn create_test_vpcs(
    env: &TestEnv,
    count: i32,
    vtype: Option<VpcVirtualizationType>,
) -> Result<DpuMachineId, Box<dyn std::error::Error>> {
    let default_tenant = default_tenant_config();
    let tenant_organization_id =
        if matches!(vtype, Some(VpcVirtualizationType::Fnn)) && env.config.fnn.is_some() {
            create_fixture_tenant(env, default_tenant.tenant_organization_id.clone()).await?;
            default_tenant.tenant_organization_id
        } else {
            String::new()
        };

    let mut first_segment_id = None;
    for i in 0..count {
        let name = format!("test vpc {}", i + 1); // start from 1 for readability

        let vpc = match vtype {
            Some(vtype) => env
                .api
                .create_vpc(
                    VpcCreationRequest::builder(tenant_organization_id.clone())
                        .metadata(Metadata {
                            name,
                            ..Default::default()
                        })
                        .network_virtualization_type(vtype)
                        .tonic_request(),
                )
                .await
                .unwrap()
                .into_inner(),
            None => env
                .api
                .create_vpc(
                    VpcCreationRequest::builder("")
                        .metadata(Metadata {
                            name,
                            ..Default::default()
                        })
                        .tonic_request(),
                )
                .await
                .unwrap()
                .into_inner(),
        };

        let vpc_id = vpc.id.expect("Expected vpc_id to be present");
        let segment_id = create_tenant_network_segment(
            &env.api,
            Some(vpc_id),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[i as usize],
            &format!("TENANT{}", i + 1),
            true,
        )
        .await;

        if i == 0 {
            first_segment_id = Some(segment_id);
        }

        env.run_network_segment_controller_iteration().await;
    }

    // Create an instance on the first VPC
    let mh = create_managed_host(env).await;
    let instance_network = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(
                first_segment_id.expect("Expected first segment id to be present"),
            ),
            network_details: None,
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };
    mh.instance_builer(env)
        .network(instance_network)
        .build()
        .await;

    Ok(mh.dpu().id)
}

async fn release_instances_from_vpcs(
    env: &TestEnv,
    vpc_ids: &[VpcId],
) -> Result<(), Box<dyn std::error::Error>> {
    let instance_ids = futures_util::future::join_all(vpc_ids.iter().map(|vpc_id| {
        async move {
            env.api
                .find_instance_ids(
                    rpc::forge::InstanceSearchFilter {
                        vpc_id: Some(vpc_id.to_string()),
                        ..Default::default()
                    }
                    .into_request(),
                )
                .map_ok(|r| r.into_inner().instance_ids)
                .await
                .unwrap()
        }
        .boxed()
    }))
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();

    if instance_ids.is_empty() {
        return Ok(());
    }

    let instances = env
        .api
        .find_instances_by_ids(rpc::forge::InstancesByIdsRequest { instance_ids }.into_request())
        .await
        .expect("searching for instances should succeed")
        .into_inner()
        .instances;

    let mut machines: HashMap<MachineId, rpc::forge::Machine> = env
        .api
        .find_machines_by_ids(
            rpc::forge::MachinesByIdsRequest {
                machine_ids: instances.iter().filter_map(|i| i.machine_id).collect(),
                include_history: false,
            }
            .into_request(),
        )
        .await
        .expect("Finding machines should succeed")
        .into_inner()
        .machines
        .into_iter()
        .map(|m| (m.id.unwrap(), m))
        .collect();

    futures_util::future::join_all(instances.into_iter().map(|i| {
        let machine = machines
            .remove(&i.machine_id.unwrap())
            .expect("Should have found machine for instance");
        async move {
            TestManagedHost::from_rpc_machine(&machine, env.api.clone())
                .delete_instance(env, i.id.unwrap())
                .await
        }
        .boxed()
    }))
    .await;

    Ok(())
}

async fn find_vpc_id_by_name(
    env: &TestEnv,
    vpc_name: &str,
) -> Result<VpcId, Box<dyn std::error::Error>> {
    let vpc_id = db::vpc::find_by_name(&env.pool, vpc_name)
        .await?
        .into_iter()
        .next()
        .unwrap()
        .id;
    Ok(vpc_id)
}

async fn get_vpc_peerings(
    env: &TestEnv,
    vpc_id: VpcId,
) -> Result<Response<VpcPeeringList>, Status> {
    let find_ids_request = Request::new(VpcPeeringSearchFilter {
        vpc_id: Some(vpc_id),
    });
    let ids = env
        .api
        .find_vpc_peering_ids(find_ids_request)
        .await?
        .into_inner()
        .vpc_peering_ids;

    let find_by_ids_request = Request::new(VpcPeeringsByIdsRequest {
        vpc_peering_ids: ids,
    });
    env.api.find_vpc_peerings_by_ids(find_by_ids_request).await
}

#[crate::sqlx_test]

async fn test_create_vpc_peering(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    create_test_vpcs(&env, 2, None).await?;

    let vpc_id_1 = find_vpc_id_by_name(&env, "test vpc 1").await?;
    let vpc_id_2 = find_vpc_id_by_name(&env, "test vpc 2").await?;

    let vpc_peering_request = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id: None,
    });

    let response = env.api.create_vpc_peering(vpc_peering_request).await;

    assert!(response.is_ok());

    Ok(())
}

#[crate::sqlx_test]
// Test creation, get, and deletion of vpc_peer
async fn test_vpc_peering_full(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    create_test_vpcs(&env, 3, None).await?;

    let vpc_id_1 = find_vpc_id_by_name(&env, "test vpc 1").await?;
    let vpc_id_2 = find_vpc_id_by_name(&env, "test vpc 2").await?;
    let vpc_id_3 = find_vpc_id_by_name(&env, "test vpc 3").await?;

    let id = Some(VpcPeeringId::from(Uuid::new_v4()));
    let vpc_peering_request_12 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id,
    });
    let response_12 = env.api.create_vpc_peering(vpc_peering_request_12).await;
    assert!(response_12.is_ok());
    let vpc_peering_12_id = response_12.unwrap().into_inner().id;

    // Recreate should fail
    let vpc_peering_request_12 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id: None,
    });
    let response_12 = env.api.create_vpc_peering(vpc_peering_request_12).await;
    assert!(response_12.is_err());

    // This should fail because the id is already in use
    let vpc_peering_request_same_id = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_3),
        peer_vpc_id: Some(vpc_id_1),
        id,
    });
    let response_same_id = env
        .api
        .create_vpc_peering(vpc_peering_request_same_id)
        .await;
    assert!(response_same_id.is_err());
    println!("response_same_id: {:?}", response_same_id);

    let vpc_peering_request_13 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_3),
        id: None,
    });
    let response_13 = env.api.create_vpc_peering(vpc_peering_request_13).await;
    assert!(response_13.is_ok());
    let vpc_peering_13_id = response_13.unwrap().into_inner().id;

    let get_response = get_vpc_peerings(&env, vpc_id_1).await;
    assert!(get_response.is_ok());
    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 2);

    let vpc_peering_delete_request = Request::new(VpcPeeringDeletionRequest {
        id: vpc_peering_12_id,
    });
    let delete_response = env.api.delete_vpc_peering(vpc_peering_delete_request).await;
    assert!(delete_response.is_ok());

    let get_response = get_vpc_peerings(&env, vpc_id_1).await;
    assert!(get_response.is_ok());
    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 1);

    let vpc_peering_delete_request = Request::new(VpcPeeringDeletionRequest {
        id: vpc_peering_13_id,
    });
    let delete_response = env.api.delete_vpc_peering(vpc_peering_delete_request).await;
    assert!(delete_response.is_ok());

    let get_response = get_vpc_peerings(&env, vpc_id_1).await;
    assert!(get_response.is_ok());
    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 0);

    // Recreate
    let vpc_peering_request_12 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id: None,
    });
    let response_12 = env.api.create_vpc_peering(vpc_peering_request_12).await;
    assert!(response_12.is_ok());

    let vpc_peering_request_13 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_3),
        id: None,
    });
    let _ = env.api.create_vpc_peering(vpc_peering_request_13).await;

    let vpc_peering_list = get_vpc_peerings(&env, vpc_id_1).await.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 2);

    release_instances_from_vpcs(&env, &[vpc_id_1, vpc_id_2, vpc_id_3]).await?;

    let vpc_delete_response = env
        .api
        .delete_vpc(tonic::Request::new(rpc::forge::VpcDeletionRequest {
            id: Some(vpc_id_1),
        }))
        .await;
    assert!(vpc_delete_response.is_ok());

    let get_response = get_vpc_peerings(&env, vpc_id_1).await;
    assert!(get_response.is_ok());

    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 0);

    Ok(())
}

#[crate::sqlx_test]
// Test creation, get, and deletion of vpc_peering
async fn test_vpc_peering_constraint(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    create_test_vpcs(&env, 3, None).await?;

    let vpc_id_1 = find_vpc_id_by_name(&env, "test vpc 1").await?;
    let vpc_id_2 = find_vpc_id_by_name(&env, "test vpc 2").await?;

    let vpc_peering_request_12 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id: Some(VpcPeeringId::from(Uuid::new_v4())),
    });
    let response_12 = env.api.create_vpc_peering(vpc_peering_request_12).await;
    assert!(response_12.is_ok());

    // Create should fail for same pair of VPC in different order
    let vpc_peering_request_21 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_2),
        peer_vpc_id: Some(vpc_id_1),
        id: None,
    });
    let response_21 = env.api.create_vpc_peering(vpc_peering_request_21).await;
    assert!(response_21.is_err());

    let fake_vpc_id: VpcId = "deadbeef-dead-beef-dead-beefdeadbeef".parse().unwrap();

    // Create should fail if two VPC ids provided are identical
    let dup_vpc_id_request = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_1),
        id: None,
    });
    let response = env.api.create_vpc_peering(dup_vpc_id_request).await;
    assert!(response.is_err());

    // Test foreign key constraint: create should fail if either VPC id does not exist in 'vpcs' table
    let fake_vpc_id_request = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(fake_vpc_id),
        id: None,
    });
    let response = env.api.create_vpc_peering(fake_vpc_id_request).await;
    assert!(response.is_err());

    Ok(())
}

async fn create_vpc_peering(
    env: &TestEnv,
    vtype1: VpcVirtualizationType,
    vtype2: VpcVirtualizationType,
) -> Result<(VpcId, VpcId, u32, u32, DpuMachineId), Box<dyn std::error::Error>> {
    let default_tenant = default_tenant_config();
    let peer_tenant_organization_id = "Tenant2";
    let use_fixture_tenants = env.config.fnn.is_some()
        && (vtype1 == VpcVirtualizationType::Fnn || vtype2 == VpcVirtualizationType::Fnn);

    if use_fixture_tenants {
        create_fixture_tenant(env, default_tenant.tenant_organization_id.clone()).await?;
        create_fixture_tenant(env, peer_tenant_organization_id).await?;
    }

    let (vpc_id, vpc_vni, segment_id, peer_vpc_id, peer_vpc_vni, _peer_segment_id) =
        if use_fixture_tenants {
            env.create_vpc_and_peer_vpc_with_tenant_segments_for_tenants(
                &default_tenant.tenant_organization_id,
                vtype1,
                peer_tenant_organization_id,
                vtype2,
            )
            .await
        } else {
            env.create_vpc_and_peer_vpc_with_tenant_segments(vtype1, vtype2)
                .await
        };
    let vpc_id = vpc_id.expect("Expected vpc_id to be Some, but was None");
    let peer_vpc_id = peer_vpc_id.expect("Expected peer_vpc_id to be Some, but was None");
    let vpc_vni = vpc_vni.expect("Expected vpc_vni to be Some, but was None");
    let peer_vpc_vni = peer_vpc_vni.expect("Expected vpc_vni to be Some, but was None");

    let mh = create_managed_host(env).await;

    // Creating VPC peering between two VPCs
    let vpc_peering_request = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id),
        peer_vpc_id: Some(peer_vpc_id),
        id: None,
    });
    let _ = env.api.create_vpc_peering(vpc_peering_request).await?;

    // Add an instance
    let instance_network = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: None,
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    mh.instance_builer(env)
        .network(instance_network)
        .build()
        .await;

    Ok((vpc_id, peer_vpc_id, vpc_vni, peer_vpc_vni, mh.dpu().id))
}

#[crate::sqlx_test]
async fn test_vpc_peering_network_config(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let (_, _, _, peer_vpc_vni, dpu_machine_id) =
        create_vpc_peering(&env, VpcVirtualizationType::Fnn, VpcVirtualizationType::Fnn).await?;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.tenant_interfaces.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_prefixes.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_vnis.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_vnis[0], peer_vpc_vni);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_peering_network_config_mixed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let response = create_vpc_peering(
        &env,
        VpcVirtualizationType::Fnn,
        VpcVirtualizationType::EthernetVirtualizer,
    )
    .await;

    assert!(response.is_err());

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_peering_network_config_exclusive_etv(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;

    let (_, _, _, _, dpu_machine_id) = create_vpc_peering(
        &env,
        VpcVirtualizationType::EthernetVirtualizer,
        VpcVirtualizationType::EthernetVirtualizer,
    )
    .await?;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.tenant_interfaces.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_prefixes.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_vnis.len(), 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_peering_deletion_upon_vpc_deletion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let (vpc_id, peer_vpc_id, _, _, dpu_machine_id) = create_vpc_peering(
        &env,
        VpcVirtualizationType::EthernetVirtualizer,
        VpcVirtualizationType::EthernetVirtualizer,
    )
    .await?;

    let get_response = get_vpc_peerings(&env, vpc_id).await;
    assert!(get_response.is_ok());
    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 1);

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.tenant_interfaces.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_prefixes.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_vnis.len(), 0);

    let vpc_delete_response = env
        .api
        .delete_vpc(tonic::Request::new(rpc::forge::VpcDeletionRequest {
            id: Some(peer_vpc_id),
        }))
        .await;
    assert!(vpc_delete_response.is_ok());

    let get_response = get_vpc_peerings(&env, vpc_id).await;
    assert!(get_response.is_ok());
    let vpc_peering_list = get_response.unwrap().into_inner();
    assert_eq!(vpc_peering_list.vpc_peerings.len(), 0);

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.tenant_interfaces.len(), 1);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_prefixes.len(), 0);
    assert_eq!(response.tenant_interfaces[0].vpc_peer_vnis.len(), 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_peering_network_config_ordered_peerings(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;

    let dpu_machine_id = create_test_vpcs(&env, 4, Some(VpcVirtualizationType::Fnn)).await?;
    let vpc_id_1 = find_vpc_id_by_name(&env, "test vpc 1").await?;
    let vpc_id_2 = find_vpc_id_by_name(&env, "test vpc 2").await?;
    let vpc_id_3 = find_vpc_id_by_name(&env, "test vpc 3").await?;
    let vpc_id_4 = find_vpc_id_by_name(&env, "test vpc 4").await?;

    let peer_vpc_vni_2 = db::vpc::find_by_name(&env.pool, "test vpc 2")
        .await?
        .into_iter()
        .next()
        .and_then(|vpc| vpc.status.vni)
        .expect("Expected peer vpc 2 vni to be present") as u32;
    let peer_vpc_vni_3 = db::vpc::find_by_name(&env.pool, "test vpc 3")
        .await?
        .into_iter()
        .next()
        .and_then(|vpc| vpc.status.vni)
        .expect("Expected peer vpc 3 vni to be present") as u32;
    let peer_vpc_vni_4 = db::vpc::find_by_name(&env.pool, "test vpc 4")
        .await?
        .into_iter()
        .next()
        .and_then(|vpc| vpc.status.vni)
        .expect("Expected peer vpc 4 vni to be present") as u32;

    // Create VPC Peering between VPC 1 and VPC 2
    let vpc_peering_request_12 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_2),
        id: None,
    });
    let _ = env.api.create_vpc_peering(vpc_peering_request_12).await?;

    // Create VPC Peering between VPC 1 and VPC 3
    let vpc_peering_request_13 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_3),
        id: None,
    });
    let _ = env.api.create_vpc_peering(vpc_peering_request_13).await?;

    // Create VPC Peering between VPC 1 and VPC 4
    let vpc_peering_request_14 = Request::new(VpcPeeringCreationRequest {
        vpc_id: Some(vpc_id_1),
        peer_vpc_id: Some(vpc_id_4),
        id: None,
    });
    let _ = env.api.create_vpc_peering(vpc_peering_request_14).await?;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await?
        .into_inner();

    assert_eq!(response.tenant_interfaces.len(), 1);
    let peer_vnis = &response.tenant_interfaces[0].vpc_peer_vnis;
    assert_eq!(peer_vnis.len(), 3);
    assert!(peer_vnis.contains(&peer_vpc_vni_2));
    assert!(peer_vnis.contains(&peer_vpc_vni_3));
    assert!(peer_vnis.contains(&peer_vpc_vni_4));

    let mut expected_peer_vnis = peer_vnis.clone();
    expected_peer_vnis.sort_unstable();
    assert_eq!(*peer_vnis, expected_peer_vnis);

    let peer_prefixes = &response.tenant_interfaces[0].vpc_peer_prefixes;
    assert_eq!(peer_prefixes.len(), 3);
    let mut expected_peer_prefixes = peer_prefixes.clone();
    expected_peer_prefixes.sort_unstable();
    assert_eq!(*peer_prefixes, expected_peer_prefixes);

    Ok(())
}

#[crate::sqlx_test]
async fn flat_vpc_can_peer_with_etv_under_exclusive_policy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Flat VPCs short-circuit the ETV<->FNN exclusion under Exclusive policy
    // because Flat VPCs do not own a Carbide-managed data plane.
    let env = api_fixtures::create_test_env(pool).await;

    let (_, etv_vpc) = api_fixtures::vpc::create_vpc(
        &env,
        "etv".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
        None,
    )
    .await;
    let (_, flat_vpc) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;

    env.api
        .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
            vpc_id: etv_vpc.id,
            peer_vpc_id: flat_vpc.id,
            id: None,
        }))
        .await
        .expect("Flat <-> ETV peering must be allowed under Exclusive policy");

    // The create returning Ok only says the RPC didn't error. Read the peering back --
    // and from *both* sides, because `find_vpc_peering_ids` filters on a single
    // `vpc_id` while the row stores an ordered (vpc_id, peer_vpc_id) pair, so whether
    // the flat side sees its own peering is a separate question.
    let peerings = get_vpc_peerings(&env, etv_vpc.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(peerings.len(), 1);
    // The stored row does not preserve the order the peering was created in, so assert
    // the pair connects the two VPCs without assuming which side landed in `vpc_id`.
    let pair = (peerings[0].vpc_id, peerings[0].peer_vpc_id);
    assert!(
        pair == (etv_vpc.id, flat_vpc.id) || pair == (flat_vpc.id, etv_vpc.id),
        "peering should connect the two VPCs, got {pair:?}"
    );

    let from_flat = get_vpc_peerings(&env, flat_vpc.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(
        from_flat.len(),
        1,
        "the flat side should see the peering too"
    );
    let pair = (from_flat[0].vpc_id, from_flat[0].peer_vpc_id);
    assert!(
        pair == (etv_vpc.id, flat_vpc.id) || pair == (flat_vpc.id, etv_vpc.id),
        "the reverse lookup should name the same pair, got {pair:?}"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn flat_vpc_can_peer_with_fnn_under_exclusive_policy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Same short-circuit as the ETV case, but on the FNN side: Flat VPCs are
    // peer-policy-neutral.
    let env = api_fixtures::create_test_env(pool).await;

    let fnn_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "fnn".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(VpcVirtualizationType::Fnn)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let (_, flat_vpc) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;

    env.api
        .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
            vpc_id: fnn_vpc.id,
            peer_vpc_id: flat_vpc.id,
            id: None,
        }))
        .await
        .expect("Flat <-> FNN peering must be allowed under Exclusive policy");

    // The create returning Ok only says the RPC didn't error. Read the peering back --
    // and from *both* sides, because `find_vpc_peering_ids` filters on a single
    // `vpc_id` while the row stores an ordered (vpc_id, peer_vpc_id) pair, so whether
    // the flat side sees its own peering is a separate question.
    let peerings = get_vpc_peerings(&env, fnn_vpc.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(peerings.len(), 1);
    // The stored row does not preserve the order the peering was created in, so assert
    // the pair connects the two VPCs without assuming which side landed in `vpc_id`.
    let pair = (peerings[0].vpc_id, peerings[0].peer_vpc_id);
    assert!(
        pair == (fnn_vpc.id, flat_vpc.id) || pair == (flat_vpc.id, fnn_vpc.id),
        "peering should connect the two VPCs, got {pair:?}"
    );

    let from_flat = get_vpc_peerings(&env, flat_vpc.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(
        from_flat.len(),
        1,
        "the flat side should see the peering too"
    );
    let pair = (from_flat[0].vpc_id, from_flat[0].peer_vpc_id);
    assert!(
        pair == (fnn_vpc.id, flat_vpc.id) || pair == (flat_vpc.id, fnn_vpc.id),
        "the reverse lookup should name the same pair, got {pair:?}"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn flat_vpc_can_peer_with_flat_under_exclusive_policy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Flat <-> Flat is structurally identical: no overlay state to mediate.
    let env = api_fixtures::create_test_env(pool).await;

    let (_, flat_a) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat-a".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;
    let (_, flat_b) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat-b".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;

    env.api
        .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
            vpc_id: flat_a.id,
            peer_vpc_id: flat_b.id,
            id: None,
        }))
        .await
        .expect("Flat <-> Flat peering must be allowed under Exclusive policy");

    // The create returning Ok only says the RPC didn't error. Read the peering back --
    // and from *both* sides, because `find_vpc_peering_ids` filters on a single
    // `vpc_id` while the row stores an ordered (vpc_id, peer_vpc_id) pair, so whether
    // the flat side sees its own peering is a separate question.
    let peerings = get_vpc_peerings(&env, flat_a.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(peerings.len(), 1);
    // The stored row does not preserve the order the peering was created in, so assert
    // the pair connects the two VPCs without assuming which side landed in `vpc_id`.
    let pair = (peerings[0].vpc_id, peerings[0].peer_vpc_id);
    assert!(
        pair == (flat_a.id, flat_b.id) || pair == (flat_b.id, flat_a.id),
        "peering should connect the two VPCs, got {pair:?}"
    );

    let from_peer = get_vpc_peerings(&env, flat_b.id.unwrap())
        .await?
        .into_inner()
        .vpc_peerings;
    assert_eq!(from_peer.len(), 1, "the peer side should see it too");
    let pair = (from_peer[0].vpc_id, from_peer[0].peer_vpc_id);
    assert!(
        pair == (flat_a.id, flat_b.id) || pair == (flat_b.id, flat_a.id),
        "the reverse lookup should name the same pair, got {pair:?}"
    );

    Ok(())
}
