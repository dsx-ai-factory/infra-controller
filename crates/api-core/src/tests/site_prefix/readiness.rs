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

use std::sync::Arc;

use chrono::{DateTime, Utc};
use model::controller_outcome::PersistentStateHandlerOutcome;
use state_controller::controller::StateController;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::site_prefix_controller::SitePrefixReadiness;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::{
    TestManagedHost, create_managed_host_multi_dpu, network_configured_with_health,
};

fn controller(env: &TestEnv) -> StateController<SitePrefixReadiness> {
    StateController::builder()
        .database(env.pool.clone(), env.api.work_lock_manager_handle.clone())
        .processor_id("site-prefix-readiness-test".to_string())
        .services(Arc::new(env.pool.clone()))
        .state_handler(Arc::new(SitePrefixReadiness {
            vpc_isolation_behavior: env.api.runtime_config.vpc_isolation_behavior,
        }))
        .build_for_manual_iterations(CancellationToken::new())
        .unwrap()
}

fn allocation_request(
    host: &TestManagedHost,
    segment_id: carbide_uuid::network::NetworkSegmentId,
) -> rpc::InstanceAllocationRequest {
    rpc::InstanceAllocationRequest {
        machine_id: Some(host.id),
        config: Some(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn requested_at(env: &TestEnv, id: SitePrefixId) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT isolation_requested_at FROM site_prefixes WHERE id = $1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn stored_prefix(env: &TestEnv, id: SitePrefixId) -> SitePrefix {
    db::site_prefix::find_by_ids(&env.pool, &[id])
        .await
        .unwrap()
        .pop()
        .unwrap()
}

async fn stored_outcome(env: &TestEnv, id: SitePrefixId) -> PersistentStateHandlerOutcome {
    sqlx::query_scalar::<_, sqlx::types::Json<PersistentStateHandlerOutcome>>(
        "SELECT controller_state_outcome FROM site_prefixes WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
    .0
}

#[crate::sqlx_test]
async fn creation_waits_for_every_host_and_dpu_without_repeating_fanout(pool: sqlx::PgPool) {
    // Keep this test's future out of the SQLx runner's nested stack frames.
    Box::pin(async move {
        let env = create_test_env(pool).await;
        create_fixture_tenant(&env, "prefix-owner").await.unwrap();
        let host = create_managed_host_multi_dpu(&env, 2).await;
        let second_host = create_managed_host(&env).await;
        let segment_id = env.create_vpc_and_tenant_segment().await;
        let instance = env
            .api
            .allocate_instance(Request::new(allocation_request(&host, segment_id)))
            .await
            .unwrap()
            .into_inner();
        let second_instance = env
            .api
            .allocate_instance(Request::new(allocation_request(&second_host, segment_id)))
            .await
            .unwrap()
            .into_inner();
        let mut txn = env.pool.begin().await.unwrap();
        let hosts = [&host, &second_host];
        let before = [
            host.snapshot(&mut txn).await,
            second_host.snapshot(&mut txn).await,
        ];
        assert!(before.iter().all(|snapshot| snapshot.use_admin_network()));
        txn.commit().await.unwrap();

        let request = creation_request(SitePrefixId::new(), "prefix-owner", "10.66.0.0/24");
        let id = request.id.unwrap();
        let response = env
            .api
            .create_site_prefix(Request::new(request.clone()))
            .await
            .unwrap();
        assert_eq!(
            response.into_inner().status.unwrap().lifecycle_state,
            RpcSitePrefixLifecycleState::Provisioning as i32
        );
        let first_requested_at = requested_at(&env, id)
            .await
            .expect("creation requests protection");
        let mut txn = env.pool.begin().await.unwrap();
        let after = [
            host.snapshot(&mut txn).await,
            second_host.snapshot(&mut txn).await,
        ];
        let mut target_versions = after
            .each_ref()
            .map(|snapshot| snapshot.host_snapshot.network_config.version);
        for (after, before) in after.iter().zip(&before) {
            let target_version = after.host_snapshot.network_config.version;
            assert_ne!(target_version, before.host_snapshot.network_config.version);
            assert!(
                after
                    .dpu_snapshots
                    .iter()
                    .all(|dpu| dpu.network_config.version == target_version)
            );
            assert!(!after.managed_host_network_config_version_synced());
        }
        txn.commit().await.unwrap();

        network_configured_with_health(&env, &host.dpu_ids[0], None).await;
        // A soft-deleted Instance may still be forwarding. Both hosts must remain
        // included until their DPUs acknowledge the requested configuration.
        let mut txn = env.pool.begin().await.unwrap();
        for instance in [instance, second_instance] {
            db::instance::mark_as_deleted(instance.id.unwrap(), &mut txn)
                .await
                .unwrap();
        }
        txn.commit().await.unwrap();
        controller(&env).run_single_iteration_ext(false).await;
        assert_eq!(
            stored_prefix(&env, id).await.status.lifecycle_state,
            SitePrefixLifecycleState::Provisioning
        );
        assert!(matches!(
            stored_outcome(&env, id).await,
            PersistentStateHandlerOutcome::Wait { .. }
        ));

        // An API retry and a fresh controller both resume the persisted wait.
        env.api
            .create_site_prefix(Request::new(request.clone()))
            .await
            .unwrap();
        sqlx::query("UPDATE site_prefixes SET controller_state_outcome = NULL WHERE id = $1")
            .bind(id)
            .execute(&env.pool)
            .await
            .unwrap();
        let mut writer = env.pool.begin().await.unwrap();
        db::tenant_prefix_overlap::lock_checks(&mut writer)
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            controller(&env).run_single_iteration_ext(false),
        )
        .await
        .expect("an unsynced host must not wait for the routing writer");
        writer.commit().await.unwrap();
        assert_eq!(requested_at(&env, id).await, Some(first_requested_at));
        assert!(matches!(
            stored_outcome(&env, id).await,
            PersistentStateHandlerOutcome::Wait { .. }
        ));
        let mut txn = env.pool.begin().await.unwrap();
        for (host, target_version) in hosts.iter().zip(target_versions) {
            let resumed = host.snapshot(&mut txn).await;
            assert_eq!(resumed.host_snapshot.network_config.version, target_version);
            assert!(
                resumed
                    .dpu_snapshots
                    .iter()
                    .all(|dpu| dpu.network_config.version == target_version)
            );
        }
        txn.commit().await.unwrap();
        second_host.network_configured(&env).await;
        controller(&env).run_single_iteration_ext(false).await;
        assert_eq!(
            stored_prefix(&env, id).await.status.lifecycle_state,
            SitePrefixLifecycleState::Provisioning
        );
        let PersistentStateHandlerOutcome::Wait { reason, .. } = stored_outcome(&env, id).await
        else {
            panic!("the first host's second DPU still needs to acknowledge the prefix");
        };
        assert!(reason.contains(&host.id.to_string()));
        assert!(reason.contains(&target_versions[0].to_string()));

        network_configured_with_health(&env, &host.dpu_ids[1], None).await;
        // Both hosts now appear current. Change a host after the unlocked scan,
        // while the authoritative check waits for the routing lock.
        let mut blocker = env.pool.begin().await.unwrap();
        db::tenant_prefix_overlap::lock_checks(&mut blocker)
            .await
            .unwrap();
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let mut readiness = controller(&env);
        let iteration = tokio::spawn(async move {
            readiness.run_single_iteration_ext(false).await;
        });
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
        let acknowledged = second_host.snapshot(&mut blocker).await;
        assert!(acknowledged.managed_host_network_config_version_synced());
        assert_eq!(
            db::machine::try_update_network_config(
                &mut blocker,
                &second_host.id,
                acknowledged.host_snapshot.network_config.version,
                &acknowledged.host_snapshot.network_config.value,
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        );
        target_versions[1] = second_host
            .snapshot(&mut blocker)
            .await
            .host_snapshot
            .network_config
            .version;
        assert_ne!(
            target_versions[1],
            acknowledged.host_snapshot.network_config.version
        );
        sqlx::query("UPDATE site_prefixes SET controller_state_outcome = NULL WHERE id = $1")
            .bind(id)
            .execute(&mut *blocker)
            .await
            .unwrap();
        blocker.commit().await.unwrap();
        iteration.await.unwrap();
        assert_eq!(
            stored_prefix(&env, id).await.status.lifecycle_state,
            SitePrefixLifecycleState::Provisioning
        );
        let PersistentStateHandlerOutcome::Wait { reason, .. } = stored_outcome(&env, id).await
        else {
            panic!("the locked check must observe the newer host version");
        };
        assert!(reason.contains(&second_host.id.to_string()));
        assert!(reason.contains(&target_versions[1].to_string()));
        second_host.network_configured(&env).await;
        controller(&env).run_single_iteration_ext(false).await;
        assert_eq!(
            stored_prefix(&env, id).await.status.lifecycle_state,
            SitePrefixLifecycleState::Ready
        );
        assert!(matches!(
            stored_outcome(&env, id).await,
            PersistentStateHandlerOutcome::Transition { .. }
        ));

        // Create retries must neither restart protection nor rewrite lifecycle history.
        for lifecycle in [
            SitePrefixLifecycleState::Ready,
            SitePrefixLifecycleState::Deleting,
        ] {
            if lifecycle == SitePrefixLifecycleState::Deleting {
                env.api
                    .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
                        id: Some(id),
                        tenant_organization_id: "prefix-owner".to_string(),
                    }))
                    .await
                    .unwrap();
            }
            let before = stored_prefix(&env, id).await;
            let history_request = SitePrefixStateHistoriesRequest {
                site_prefix_ids: vec![id],
            };
            let history = env
                .api
                .find_site_prefix_state_histories(Request::new(history_request.clone()))
                .await
                .unwrap()
                .into_inner();
            let retry = env
                .api
                .create_site_prefix(Request::new(request.clone()))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(retry.version, before.version.to_string());
            assert_eq!(
                stored_prefix(&env, id).await.status.lifecycle_state,
                lifecycle
            );
            assert_eq!(requested_at(&env, id).await, Some(first_requested_at));
            assert_eq!(
                env.api
                    .find_site_prefix_state_histories(Request::new(history_request))
                    .await
                    .unwrap()
                    .into_inner(),
                history
            );
            let mut txn = env.pool.begin().await.unwrap();
            for (host, target_version) in hosts.iter().zip(target_versions) {
                let snapshot = host.snapshot(&mut txn).await;
                assert_eq!(
                    snapshot.host_snapshot.network_config.version,
                    target_version
                );
                assert!(
                    snapshot
                        .dpu_snapshots
                        .iter()
                        .all(|dpu| dpu.network_config.version == target_version)
                );
            }
            txn.commit().await.unwrap();
        }
    })
    .await;
}

#[crate::sqlx_test]
async fn open_readiness_skips_fanout_and_stale_dpu_acknowledgements(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.vpc_isolation_behavior = VpcIsolationBehaviorType::Open;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    create_fixture_tenant(&env, "prefix-owner").await.unwrap();
    let host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    env.api
        .allocate_instance(Request::new(allocation_request(&host, segment_id)))
        .await
        .unwrap();
    host.network_configured(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    let acknowledged = host.snapshot(&mut txn).await;
    assert!(acknowledged.managed_host_network_config_version_synced());
    assert_eq!(
        db::machine::try_update_network_config(
            &mut txn,
            &host.id,
            acknowledged.host_snapshot.network_config.version,
            &acknowledged.host_snapshot.network_config.value,
        )
        .await
        .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    let before = host.snapshot(&mut txn).await;
    assert!(before.needs_site_prefix_isolation());
    assert!(!before.managed_host_network_config_version_synced());
    assert_ne!(
        before.dpu_snapshots[0]
            .network_status_observation
            .as_ref()
            .unwrap()
            .network_config_version
            .unwrap(),
        before.host_snapshot.network_config.version
    );
    txn.commit().await.unwrap();

    let id = SitePrefixId::new();
    let response = env
        .api
        .create_site_prefix(Request::new(creation_request(
            id,
            "prefix-owner",
            "10.70.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        response.status.unwrap().lifecycle_state,
        RpcSitePrefixLifecycleState::Provisioning as i32
    );
    assert!(requested_at(&env, id).await.is_some());

    // A retained root without a request also exercises controller recovery.
    let recovered = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.71.0.0/24", "prefix-owner"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    assert!(requested_at(&env, recovered.id).await.is_none());
    controller(&env).run_single_iteration_ext(false).await;
    for id in [id, recovered.id] {
        assert_eq!(
            stored_prefix(&env, id).await.status.lifecycle_state,
            SitePrefixLifecycleState::Ready
        );
        assert!(requested_at(&env, id).await.is_some());
        assert!(matches!(
            stored_outcome(&env, id).await,
            PersistentStateHandlerOutcome::Transition { .. }
        ));
    }
    let mut txn = env.pool.begin().await.unwrap();
    let after = host.snapshot(&mut txn).await;
    assert!(!after.managed_host_network_config_version_synced());
    for (name, actual, expected) in [
        (
            "host",
            after.host_snapshot.network_config.version,
            before.host_snapshot.network_config.version,
        ),
        (
            "DPU",
            after.dpu_snapshots[0].network_config.version,
            before.dpu_snapshots[0].network_config.version,
        ),
    ] {
        assert_eq!(actual, expected, "{name}");
    }
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn fanout_failure_rolls_back_and_recovery_retries(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "prefix-owner").await.unwrap();
    let host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    env.api
        .allocate_instance(Request::new(allocation_request(&host, segment_id)))
        .await
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    let before = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();

    // The host write succeeds, then the DPU group write fails. Both must roll
    // back, including the newly created root or a recovery request marker.
    let constraint = format!(
        "ALTER TABLE machines ADD CONSTRAINT reject_test_dpu_refresh CHECK \
         (id <> '{}' OR network_config_version = '{}')",
        host.dpu_ids[0], before.dpu_snapshots[0].network_config.version,
    );
    // The DDL substitutes only typed machine ID and configuration version values.
    sqlx::query(sqlx::AssertSqlSafe(constraint))
        .execute(&env.pool)
        .await
        .unwrap();
    let request = creation_request(SitePrefixId::new(), "prefix-owner", "10.67.0.0/24");
    let id = request.id.unwrap();
    assert_eq!(
        env.api
            .create_site_prefix(Request::new(request))
            .await
            .unwrap_err()
            .code(),
        Code::Internal
    );
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[id])
            .await
            .unwrap()
            .is_empty()
    );

    let legacy = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.67.0.0/24", "prefix-owner"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    controller(&env).run_single_iteration_ext(false).await;
    assert_eq!(requested_at(&env, legacy.id).await, None);
    assert!(matches!(
        stored_outcome(&env, legacy.id).await,
        PersistentStateHandlerOutcome::Error { .. }
    ));
    let mut txn = env.pool.begin().await.unwrap();
    let failed = host.snapshot(&mut txn).await;
    for (name, actual, expected) in [
        (
            "host",
            &failed.host_snapshot.network_config,
            &before.host_snapshot.network_config,
        ),
        (
            "DPU",
            &failed.dpu_snapshots[0].network_config,
            &before.dpu_snapshots[0].network_config,
        ),
    ] {
        assert_eq!(
            (actual.version, &actual.value),
            (expected.version, &expected.value),
            "{name}",
        );
    }
    txn.commit().await.unwrap();

    sqlx::query("ALTER TABLE machines DROP CONSTRAINT reject_test_dpu_refresh")
        .execute(&env.pool)
        .await
        .unwrap();
    let mut reader = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_config(&mut reader)
        .await
        .unwrap();
    let reader_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reader)
        .await
        .unwrap();
    let mut readiness = controller(&env);
    let iteration = tokio::spawn(async move {
        readiness.run_single_iteration_ext(false).await;
    });
    wait_for_blocked_query(&env.pool, reader_pid, "tenant_prefix_overlap:checks").await;
    assert_eq!(requested_at(&env, legacy.id).await, None);
    reader.commit().await.unwrap();
    iteration.await.unwrap();
    let request_time = requested_at(&env, legacy.id)
        .await
        .expect("retry requests protection");
    assert_eq!(
        stored_prefix(&env, legacy.id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Provisioning
    );
    let mut txn = env.pool.begin().await.unwrap();
    assert_ne!(
        host.snapshot(&mut txn)
            .await
            .host_snapshot
            .network_config
            .version,
        before.host_snapshot.network_config.version
    );
    txn.commit().await.unwrap();
    host.network_configured(&env).await;
    controller(&env).run_single_iteration_ext(false).await;
    assert_eq!(requested_at(&env, legacy.id).await, Some(request_time));
    assert_eq!(
        stored_prefix(&env, legacy.id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Ready
    );
}

#[crate::sqlx_test]
async fn retirement_wins_before_readiness_promotion(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    let id = SitePrefixId::new();
    env.api
        .create_site_prefix(Request::new(creation_request(
            id,
            "tenant-a",
            "10.68.0.0/24",
        )))
        .await
        .unwrap();
    let mut blocker = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut blocker)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let mut readiness = controller(&env);
    let iteration = tokio::spawn(async move {
        readiness.run_single_iteration_ext(false).await;
    });
    wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
    env.api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(id),
            tenant_organization_id: "tenant-a".to_string(),
        }))
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    iteration.await.unwrap();
    assert_eq!(
        stored_prefix(&env, id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Deleting
    );
    assert!(matches!(
        stored_outcome(&env, id).await,
        PersistentStateHandlerOutcome::DoNothing { .. }
    ));
    let history = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![id],
        }))
        .await
        .unwrap()
        .into_inner();
    let records = &history.histories[&id.to_string()].records;
    assert_eq!(records.len(), 2);
    assert!(records[0].state.contains("provisioning"));
    assert!(records[1].state.contains("deleting"));
}

#[crate::sqlx_test]
async fn idle_hosts_skip_fanout_and_later_assignment_receives_the_prefix(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "prefix-owner").await.unwrap();
    let host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mut txn = env.pool.begin().await.unwrap();
    let before = host.snapshot(&mut txn).await;
    txn.commit().await.unwrap();
    let id = SitePrefixId::new();
    env.api
        .create_site_prefix(Request::new(creation_request(
            id,
            "prefix-owner",
            "10.69.0.0/24",
        )))
        .await
        .unwrap();
    controller(&env).run_single_iteration_ext(false).await;
    assert_eq!(
        stored_prefix(&env, id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Ready
    );
    let mut txn = env.pool.begin().await.unwrap();
    let after = host.snapshot(&mut txn).await;
    assert_eq!(
        after.host_snapshot.network_config.version,
        before.host_snapshot.network_config.version
    );
    assert_eq!(
        after.dpu_snapshots[0].network_config.version,
        before.dpu_snapshots[0].network_config.version
    );
    txn.commit().await.unwrap();
    env.api
        .allocate_instance(Request::new(allocation_request(&host, segment_id)))
        .await
        .unwrap();
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu_ids[0]),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        response
            .site_fabric_prefixes
            .contains(&"10.69.0.0/24".to_string())
    );
}
