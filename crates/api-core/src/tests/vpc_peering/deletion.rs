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

use model::controller_outcome::PersistentStateHandlerOutcome;

use super::*;
use crate::tests::common::api_fixtures::instance::single_interface_network_config;
use crate::tests::common::api_fixtures::{
    create_managed_host_multi_dpu, network_configured_with_health,
};

async fn stored_peering(env: &TestEnv, id: VpcPeeringId) -> model::vpc::VpcPeering {
    let mut txn = env.pool.begin().await.unwrap();
    let peering = db::vpc_peering::find_by_ids(&mut txn, vec![id])
        .await
        .unwrap()
        .pop()
        .unwrap();
    txn.commit().await.unwrap();
    peering
}

async fn stored_wait(env: &TestEnv, id: VpcPeeringId) -> String {
    let result = sqlx::query_scalar::<_, sqlx::types::Json<PersistentStateHandlerOutcome>>(
        "SELECT controller_state_outcome FROM vpc_peerings WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
    .0;
    let PersistentStateHandlerOutcome::Wait { reason, .. } = result else {
        panic!("the retained peering must record its missing DPU acknowledgement");
    };
    reason
}

/// Permission removal and its first version request survive retries/restarts;
/// neither one host's receipt nor one DPU's receipt completes the deletion.
#[crate::sqlx_test]
async fn fnn_deletion_waits_for_every_receiver_and_dpu(pool: PgPool) {
    Box::pin(async move {
        let env =
            create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
                .await;
        let tenant_organization_id = default_tenant_config().tenant_organization_id;
        create_fixture_tenant(&env, tenant_organization_id.clone())
            .await
            .unwrap();
        let (vpc, _, segment, peer, peer_vni, peer_segment) = env
            .create_vpc_and_peer_vpc_with_tenant_segments_for_tenants(
                &tenant_organization_id,
                VpcVirtualizationType::Fnn,
                &tenant_organization_id,
                VpcVirtualizationType::Fnn,
            )
            .await;
        let vpc = vpc.unwrap();
        let peer = peer.unwrap();
        let first = create_managed_host_multi_dpu(&env, 2).await;
        let second = create_managed_host(&env).await;
        first
            .instance_builer(&env)
            .network(single_interface_network_config(segment))
            .build()
            .await;
        second
            .instance_builer(&env)
            .network(single_interface_network_config(peer_segment))
            .build()
            .await;
        let peering = env
            .api
            .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
                id: None,
                vpc_id: Some(vpc),
                peer_vpc_id: Some(peer),
            }))
            .await
            .unwrap()
            .into_inner();
        let id = peering.id.unwrap();
        assert_eq!(peering.state(), rpc::forge::VpcPeeringState::Ready);
        first.network_configured(&env).await;
        second.network_configured(&env).await;
        let before = env
            .api
            .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(first.dpu().id),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            before.tenant_interfaces[0].vpc_peer_vnis,
            vec![peer_vni.unwrap()]
        );

        let request = VpcPeeringDeletionRequest { id: Some(id) };
        env.api
            .delete_vpc_peering(Request::new(request.clone()))
            .await
            .unwrap();
        let deletion_version = stored_peering(&env, id).await.deletion_version.unwrap();
        let mut txn = env.pool.begin().await.unwrap();
        let first_target = first
            .snapshot(&mut txn)
            .await
            .host_snapshot
            .network_config
            .version;
        let second_target = second
            .snapshot(&mut txn)
            .await
            .host_snapshot
            .network_config
            .version;
        txn.commit().await.unwrap();
        let response = env
            .api
            .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(first.dpu().id),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_ne!(
            response.managed_host_config_version,
            before.managed_host_config_version
        );
        assert!(response.tenant_interfaces[0].vpc_peer_vnis.is_empty());
        assert!(response.tenant_interfaces[0].vpc_peer_prefixes.is_empty());

        env.api
            .delete_vpc_peering(Request::new(request))
            .await
            .unwrap();
        deletion_controller(&env)
            .run_single_iteration_ext(false)
            .await;
        assert_eq!(
            stored_peering(&env, id).await.deletion_version,
            Some(deletion_version)
        );
        stored_wait(&env, id).await;
        let mut txn = env.pool.begin().await.unwrap();
        assert_eq!(
            first
                .snapshot(&mut txn)
                .await
                .host_snapshot
                .network_config
                .version,
            first_target
        );
        assert_eq!(
            second
                .snapshot(&mut txn)
                .await
                .host_snapshot
                .network_config
                .version,
            second_target
        );
        txn.commit().await.unwrap();

        let error = env
            .api
            .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
                id: None,
                vpc_id: Some(vpc),
                peer_vpc_id: Some(peer),
            }))
            .await
            .expect_err("Deleting still reserves the endpoint pair");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert!(error.message().contains("VpcPeering already exists"));
        for endpoint in [vpc, peer] {
            let error = env
                .api
                .delete_vpc(Request::new(rpc::forge::VpcDeletionRequest {
                    id: Some(endpoint),
                }))
                .await
                .expect_err("retained peering prevents VPC deletion");
            assert!(error.message().contains("delete its peerings"));
        }

        second.network_configured(&env).await;
        network_configured_with_health(&env, &first.dpu_ids[0], None).await;
        // A fresh controller proves the wait does not depend on an in-memory
        // receiver list. The second DPU has not applied the removed permission.
        deletion_controller(&env)
            .run_single_iteration_ext(false)
            .await;
        let reason = stored_wait(&env, id).await;
        assert!(reason.contains(&first.id.to_string()));
        assert!(reason.contains(&first_target.to_string()));
        network_configured_with_health(&env, &first.dpu_ids[1], None).await;
        deletion_controller(&env)
            .run_single_iteration_ext(false)
            .await;
        assert!(
            get_vpc_peerings(&env, vpc)
                .await
                .unwrap()
                .into_inner()
                .vpc_peerings
                .is_empty()
        );
    })
    .await;
}

/// Admission keeps a deleting peering's source visible until the controller
/// removes it, even though the renderer already omits that permission.
#[crate::sqlx_test]
async fn deleting_peering_still_blocks_overlapping_sibling(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, vpcs) = create_peering_overlap_fixture(
        pool,
        true,
        None,
        VpcVirtualizationType::EthernetVirtualizer,
    )
    .await?;
    let receiver = vpcs[0].id.unwrap();
    let first = env
        .api
        .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
            id: None,
            vpc_id: Some(receiver),
            peer_vpc_id: vpcs[1].id,
        }))
        .await?
        .into_inner()
        .id;
    let mut txn = env.pool.begin().await?;
    retain_peering_overlap_prefix(&mut txn, &vpcs[2]).await?;
    txn.commit().await?;
    env.api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest { id: first }))
        .await?;
    let conflicting = VpcPeeringCreationRequest {
        id: None,
        vpc_id: Some(receiver),
        peer_vpc_id: vpcs[2].id,
    };
    let error = env
        .api
        .create_vpc_peering(Request::new(conflicting.clone()))
        .await
        .expect_err("the first permission has not finished deleting");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    deletion_controller(&env)
        .run_single_iteration_ext(false)
        .await;
    env.api
        .create_vpc_peering(Request::new(conflicting))
        .await?;
    Ok(())
}

/// Concurrent drains sharing a receiver retain both relations until the DPU
/// applies the configuration without either permission.
#[crate::sqlx_test]
async fn concurrent_peering_deletions_wait_for_the_receiver(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu = create_test_vpcs(&env, 3, None).await?;
    let receiver = find_vpc_id_by_name(&env, "test vpc 1").await?;
    let mut peering_ids = Vec::new();
    for name in ["test vpc 2", "test vpc 3"] {
        let peer = find_vpc_id_by_name(&env, name).await?;
        peering_ids.push(
            env.api
                .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
                    id: None,
                    vpc_id: Some(receiver),
                    peer_vpc_id: Some(peer),
                }))
                .await?
                .into_inner()
                .id
                .unwrap(),
        );
    }
    network_configured_with_health(&env, &dpu, None).await;
    let first = env
        .api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest {
            id: Some(peering_ids[0]),
        }));
    let second = env
        .api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest {
            id: Some(peering_ids[1]),
        }));
    let (first, second) = tokio::join!(first, second);
    first?;
    second?;
    deletion_controller(&env)
        .run_single_iteration_ext(false)
        .await;
    for id in &peering_ids {
        stored_wait(&env, *id).await;
    }
    let config = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu),
        }))
        .await?
        .into_inner();
    assert!(config.tenant_interfaces[0].vpc_peer_prefixes.is_empty());
    network_configured_with_health(&env, &dpu, None).await;
    deletion_controller(&env)
        .run_single_iteration_ext(false)
        .await;
    assert!(
        get_vpc_peerings(&env, receiver)
            .await?
            .into_inner()
            .vpc_peerings
            .is_empty()
    );
    Ok(())
}

/// Receiver selection must follow retained updates, not only the current
/// interface list. SLAAC-style fixtures deliberately have no address rows.
#[crate::sqlx_test]
async fn deletion_selects_old_and_pending_receiver_attachments(pool: PgPool) {
    Box::pin(async move {
        let env = create_test_env(pool).await;
        let (vpc, _, _, _, _) = create_vpc_peering(
            &env,
            VpcVirtualizationType::EthernetVirtualizer,
            VpcVirtualizationType::EthernetVirtualizer,
        )
        .await
        .unwrap();
        let peering = get_vpc_peerings(&env, vpc)
            .await
            .unwrap()
            .into_inner()
            .vpc_peerings
            .pop()
            .unwrap();
        let id = peering.id.unwrap();
        let mut txn = env.pool.begin().await.unwrap();
        let instance_ids = db::instance::find_ids(
            &mut *txn,
            model::instance::InstanceSearchFilter {
                vpc_id: Some(vpc.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(instance_ids.len(), 1);
        let instance = db::instance::find(
            &mut *txn,
            db::ObjectColumnFilter::List(db::instance::IdColumn, &instance_ids),
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
        // The shared Instance query already tests its JSON representation.
        // Move this real receiver out of the current config to prove deletion
        // uses that complete query rather than only current interfaces.
        sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1")
            .bind(instance_ids[0])
            .execute(&mut *txn)
            .await
            .unwrap();
        let cases = [
            (
                "old_config",
                model::instance::config::network::InstanceNetworkConfigUpdate {
                    old_config: instance.config.network.clone(),
                    ..Default::default()
                },
            ),
            (
                "new_config",
                model::instance::config::network::InstanceNetworkConfigUpdate {
                    new_config: instance.config.network.clone(),
                    ..Default::default()
                },
            ),
        ];
        for (location, update) in cases {
            let empty_network = model::instance::config::network::InstanceNetworkConfig::default();
            sqlx::query("UPDATE instances SET network_config = $2, update_network_config_request = $3 WHERE id = $1")
                .bind(instance_ids[0])
                .bind(sqlx::types::Json(empty_network))
                .bind(sqlx::types::Json(update))
                .execute(&mut *txn)
                .await
                .unwrap();
            let retained = db::vpc_peering::find_by_ids(&mut txn, vec![id])
                .await
                .unwrap()
                .pop()
                .unwrap();
            let hosts = db::vpc_peering::find_receivers(&mut txn, &retained)
                .await
                .unwrap();
            assert_eq!(hosts.len(), 1, "{location} remains a receiver");
            assert_eq!(hosts[0].instance.as_ref().unwrap().id, instance_ids[0]);
        }
        txn.rollback().await.unwrap();
    })
    .await;
}
