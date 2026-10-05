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

/// Reloads the deletion marker so retries are checked against durable state.
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

/// Requires the controller to have persisted a wait, not merely kept the row.
pub(super) async fn stored_wait(env: &TestEnv, id: VpcPeeringId) -> String {
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
        let first = create_managed_host(&env).await;
        let second = create_managed_host_multi_dpu(&env, 2).await;
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
        assert!(!before.tenant_interfaces[0].vpc_peer_prefixes.is_empty());
        let second_before = db::machine::get_network_config(&env.pool, &second.id.into())
            .await
            .unwrap()
            .version;

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
        assert_ne!(second_target, second_before);
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

        // Follow receiver order so checking only the first host cannot pass.
        // The two-DPU host must also wait for its second receipt in either order.
        let mut receivers = [(&first, first_target), (&second, second_target)];
        receivers.sort_unstable_by_key(|(host, _)| host.id);
        for (host, target) in receivers {
            let reason = stored_wait(&env, id).await;
            assert!(reason.contains(&host.id.to_string()));
            assert!(reason.contains(&target.to_string()));
            for (index, dpu_id) in host.dpu_ids.iter().enumerate() {
                network_configured_with_health(&env, dpu_id, None).await;
                deletion_controller(&env)
                    .run_single_iteration_ext(false)
                    .await;
                if index + 1 < host.dpu_ids.len() {
                    assert!(stored_wait(&env, id).await.contains(&host.id.to_string()));
                }
            }
        }
        assert!(
            get_vpc_peerings(&env, vpc)
                .await
                .unwrap()
                .into_inner()
                .vpc_peerings
                .is_empty()
        );
        let error = env
            .api
            .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest { id: Some(id) }))
            .await
            .expect_err("a completed deletion no longer has a peering to resume");
        assert_eq!(error.code(), tonic::Code::NotFound);
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
    let before = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu),
        }))
        .await?
        .into_inner();
    assert_eq!(before.tenant_interfaces[0].vpc_peer_prefixes.len(), 2);

    // Queue both deletions behind the routing lock. The second request must
    // wait behind the first, then use its committed host version.
    let mut blocker = env.pool.begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut blocker).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let first_api = env.api.clone();
    let first_id = peering_ids[0];
    let first = tokio::spawn(async move {
        first_api
            .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest {
                id: Some(first_id),
            }))
            .await
    });
    let first_pid =
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
    let second_api = env.api.clone();
    let second_id = peering_ids[1];
    let second = tokio::spawn(async move {
        second_api
            .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest {
                id: Some(second_id),
            }))
            .await
    });
    wait_for_blocked_query(&env.pool, first_pid, "tenant_prefix_overlap:checks").await;
    blocker.commit().await?;
    first.await??;
    second.await??;
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

/// A receiver can change after the first receipt check while completion waits
/// for a routing writer. Only the second check sees the newer target version.
#[crate::sqlx_test]
async fn deletion_rechecks_receivers_after_acquiring_routing_lock(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (vpc, _, _, _, dpu) = create_vpc_peering(
        &env,
        VpcVirtualizationType::EthernetVirtualizer,
        VpcVirtualizationType::EthernetVirtualizer,
    )
    .await?;
    let id = get_vpc_peerings(&env, vpc).await?.into_inner().vpc_peerings[0]
        .id
        .expect("created peering has an ID");
    env.api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest { id: Some(id) }))
        .await?;
    network_configured_with_health(&env, &dpu, None).await;

    let mut writer = env.pool.begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut writer).await?;
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let mut controller = deletion_controller(&env);
    let completion = controller.run_single_iteration_ext(false);
    let change_receiver = async {
        wait_for_blocked_query(&env.pool, writer_pid, "tenant_prefix_overlap:checks").await;
        let config = db::machine::get_network_config(&mut *writer, &dpu.into()).await?;
        assert!(matches!(
            db::machine::try_update_network_config(
                &mut writer,
                &dpu.into(),
                config.version,
                &config.value,
            )
            .await?,
            db::ConditionalWrite::Applied(())
        ));
        let new_version = db::machine::get_network_config(&mut *writer, &dpu.into())
            .await?
            .version;
        writer.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(new_version)
    };
    let ((), changed) = tokio::join!(completion, change_receiver);
    assert!(stored_wait(&env, id).await.contains(&changed?.to_string()));
    network_configured_with_health(&env, &dpu, None).await;
    controller.run_single_iteration_ext(false).await;
    assert!(
        get_vpc_peerings(&env, vpc)
            .await?
            .into_inner()
            .vpc_peerings
            .is_empty()
    );
    Ok(())
}

/// A stale last receiver rolls back earlier group updates and the deletion
/// marker, while preserving the unrelated update that caused the conflict.
#[crate::sqlx_test]
async fn deletion_receiver_conflict_rolls_back_all_requested_updates(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (vpc, _, segment, peer, _, peer_segment) = env
        .create_vpc_and_peer_vpc_with_tenant_segments(
            VpcVirtualizationType::EthernetVirtualizer,
            VpcVirtualizationType::EthernetVirtualizer,
        )
        .await;
    let mut hosts = [
        create_managed_host(&env).await,
        create_managed_host(&env).await,
    ];
    for (host, segment) in hosts.iter().zip([segment, peer_segment]) {
        host.instance_builer(&env)
            .network(single_interface_network_config(segment))
            .build()
            .await;
    }
    // `find_receivers` sorts by host ID. Block the last host so the first
    // group's update has already been attempted when the conflict occurs.
    hosts.sort_unstable_by_key(|host| host.id);
    let first_id = hosts[0].id.into();
    let last_id = hosts[1].id.into();
    let first_before = db::machine::get_network_config(&env.pool, &first_id).await?;
    let last_before = db::machine::get_network_config(&env.pool, &last_id).await?;
    let id = env
        .api
        .create_vpc_peering(Request::new(VpcPeeringCreationRequest {
            id: None,
            vpc_id: vpc,
            peer_vpc_id: peer,
        }))
        .await?
        .into_inner()
        .id
        .expect("created peering has an ID");
    let mut updater = env.pool.begin().await?;
    let updater_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *updater)
        .await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(last_id)
        .fetch_one(&mut *updater)
        .await?;

    let deletion = env
        .api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest { id: Some(id) }));
    let change_receiver = async {
        wait_for_blocked_query(
            &env.pool,
            updater_pid,
            "UPDATE machines SET network_config_version",
        )
        .await;
        assert!(matches!(
            db::machine::try_update_network_config(
                &mut updater,
                &last_id,
                last_before.version,
                &last_before.value,
            )
            .await?,
            db::ConditionalWrite::Applied(())
        ));
        updater.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (result, changed) = tokio::join!(deletion, change_receiver);
    changed?;
    let error = result.expect_err("the last receiver changed after selection");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains(&last_id.to_string()));
    assert!(
        error
            .message()
            .contains("retry the peering deletion request")
    );
    assert!(stored_peering(&env, id).await.deletion_version.is_none());
    let first_after = db::machine::get_network_config(&env.pool, &first_id).await?;
    assert_eq!(first_after.version, first_before.version);
    assert_eq!(first_after.value, first_before.value);
    assert_eq!(
        db::machine::get_network_config(&env.pool, &hosts[0].dpu().id.into())
            .await?
            .version,
        first_before.version,
    );
    assert_ne!(
        db::machine::get_network_config(&env.pool, &last_id)
            .await?
            .version,
        last_before.version
    );

    env.api
        .delete_vpc_peering(Request::new(VpcPeeringDeletionRequest { id: Some(id) }))
        .await?;
    assert!(stored_peering(&env, id).await.deletion_version.is_some());
    Ok(())
}

/// Receiver selection must follow retained updates, not only the current
/// interface list, even without an address reservation keeping it discoverable.
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
        let unrelated_vpc = env
            .api
            .create_vpc(
                VpcCreationRequest::builder("")
                    .metadata(Metadata {
                        name: "unrelated receiver".to_string(),
                        ..Default::default()
                    })
                    .tonic_request(),
            )
            .await
            .unwrap()
            .into_inner();
        let unrelated_segment = create_tenant_network_segment(
            &env.api,
            unrelated_vpc.id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2],
            "unrelated receiver",
            true,
        )
        .await;
        env.run_network_segment_controller_iteration().await;
        let unrelated_host = create_managed_host(&env).await;
        unrelated_host
            .instance_builer(&env)
            .network(single_interface_network_config(unrelated_segment))
            .build()
            .await;
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
        let deleted_addresses = sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1")
            .bind(instance_ids[0])
            .execute(&mut *txn)
            .await
            .unwrap();
        assert_eq!(deleted_addresses.rows_affected(), 1);
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
