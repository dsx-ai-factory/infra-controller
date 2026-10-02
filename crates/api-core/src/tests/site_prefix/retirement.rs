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

use super::readiness::{controller, stored_outcome};
use super::*;

#[crate::sqlx_test]
async fn retirement_waits_for_exact_children_and_preserves_other_roots(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    for tenant in ["tenant-a", "tenant-b"] {
        create_fixture_tenant(&env, tenant).await.unwrap();
    }
    let root = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.66.0.0/16", "tenant-a"),
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let other = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.66.0.0/16", "tenant-b"),
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let operator = persist_configured_site_prefix(&env, "203.0.113.0/24").await;
    let mut txn = env.pool.begin().await.unwrap();
    db::site_prefix::reconcile_configured(&mut txn, &[])
        .await
        .unwrap();
    let child_ids = [VpcPrefixId::new(), VpcPrefixId::new(), VpcPrefixId::new()];
    for (child_id, parent, tenant, prefix) in [
        (child_ids[0], root.id, "tenant-a", "10.66.0.0/24"),
        (child_ids[1], root.id, "tenant-a", "10.66.1.0/24"),
        (child_ids[2], other.id, "tenant-b", "10.66.0.0/24"),
    ] {
        let vpc_id = VpcId::new();
        sqlx::query(
            "INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, $2, $3, $4)",
        )
        .bind(vpc_id)
        .bind("retirement VPC")
        .bind(tenant)
        .bind(ConfigVersion::initial())
        .execute(&mut *txn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id, overlap_vpc_id) \
             VALUES ($1, $2, $3, $4, $5, $4)",
        )
        .bind(child_id)
        .bind(prefix.parse::<IpNetwork>().unwrap())
        .bind("retained child")
        .bind(vpc_id)
        .bind(parent)
        .execute(&mut *txn)
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();
    env.api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(root.id),
            tenant_organization_id: "tenant-a".to_string(),
        }))
        .await
        .unwrap();

    for soft_deleted in [false, true] {
        if soft_deleted {
            sqlx::query("UPDATE network_vpc_prefixes SET deleted = now() WHERE id = ANY($1)")
                .bind(&child_ids[..2])
                .execute(&env.pool)
                .await
                .unwrap();
        }
        // A fresh controller must resume the durable wait after a restart.
        controller(&env).run_single_iteration_ext(false).await;
        let retained = db::site_prefix::find_by_ids(&env.pool, &[root.id])
            .await
            .unwrap();
        assert_eq!(
            retained[0].status.lifecycle_state,
            SitePrefixLifecycleState::Deleting
        );
        let PersistentStateHandlerOutcome::Wait { reason, .. } =
            stored_outcome(&env, root.id).await
        else {
            panic!("retirement must persist the child wait");
        };
        assert!(reason.contains("2"));
        assert!(
            db::site_prefix::find_tenant_prefixes(&env.pool)
                .await
                .unwrap()
                .contains(&root.config.prefix)
        );
    }
    let mut txn = env.pool.begin().await.unwrap();
    db::vpc_prefix::final_delete(child_ids[0], &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    controller(&env).run_single_iteration_ext(false).await;
    let PersistentStateHandlerOutcome::Wait { reason, .. } = stored_outcome(&env, root.id).await
    else {
        panic!("the final retained child must still block retirement");
    };
    assert!(reason.contains("1"));

    let mut txn = env.pool.begin().await.unwrap();
    db::vpc_prefix::final_delete(child_ids[1], &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    controller(&env).run_single_iteration_ext(false).await;
    controller(&env).run_single_iteration_ext(false).await;
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[root.id])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db::site_prefix::find_by_ids(&env.pool, &[other.id])
            .await
            .unwrap(),
        vec![other.clone()]
    );
    assert_eq!(
        db::site_prefix::find_tenant_prefixes(&env.pool)
            .await
            .unwrap(),
        vec![other.config.prefix]
    );

    // Operator removal must remain reversible, with the same identity.
    let retained = db::site_prefix::find_by_ids(&env.pool, &[operator.id])
        .await
        .unwrap();
    assert_eq!(
        retained[0].status.lifecycle_state,
        SitePrefixLifecycleState::Deleting
    );
    let mut txn = env.pool.begin().await.unwrap();
    db::site_prefix::reconcile_configured(&mut txn, &[operator.config.prefix])
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let restored = db::site_prefix::find_by_ids(&env.pool, &[operator.id])
        .await
        .unwrap();
    assert_eq!(
        restored[0].status.lifecycle_state,
        SitePrefixLifecycleState::Ready
    );
}

#[crate::sqlx_test]
async fn retirement_retries_after_interrupted_parent_lock(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    let root = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.67.0.0/16", "tenant-a"),
        SitePrefixLifecycleState::Deleting,
    )
    .await;
    let mut blocker = env.pool.begin().await.unwrap();
    db::site_prefix::find_by_id_for_update(&mut blocker, root.id)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let mut retirement = controller(&env);
    let iteration = tokio::spawn(async move {
        retirement.run_single_iteration_ext(false).await;
    });
    let blocked_pid = wait_for_blocked_query(&env.pool, blocker_pid, "FROM site_prefixes").await;
    let routing_lock_available: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('tenant_prefix_overlap:checks', 0))",
    )
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    assert!(
        !routing_lock_available,
        "retirement must lock routing before its parent"
    );
    let canceled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
        .bind(blocked_pid)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    assert!(canceled);
    blocker.commit().await.unwrap();
    iteration.await.unwrap();

    let PersistentStateHandlerOutcome::Error { err, .. } = stored_outcome(&env, root.id).await
    else {
        panic!("the interrupted attempt must persist its error");
    };
    assert!(err.contains("canceling statement"));
    assert_eq!(
        db::site_prefix::find_by_ids(&env.pool, &[root.id])
            .await
            .unwrap(),
        vec![root.clone()]
    );
    assert_eq!(
        db::site_prefix::find_tenant_prefixes(&env.pool)
            .await
            .unwrap(),
        vec![root.config.prefix]
    );

    controller(&env).run_single_iteration_ext(false).await;
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[root.id])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db::site_prefix::find_tenant_prefixes(&env.pool)
            .await
            .unwrap()
            .is_empty()
    );
}

#[crate::sqlx_test]
async fn child_creation_racing_physical_retirement_observes_missing_parent(pool: sqlx::PgPool) {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder("tenant-a".to_string())
                .metadata(RpcMetadata {
                    name: "retirement race VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let root = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.68.0.0/16", "tenant-a"),
        SitePrefixLifecycleState::Deleting,
    )
    .await;
    let mut blocker = env.pool.begin().await.unwrap();
    db::site_prefix::find_by_id_for_update(&mut blocker, root.id)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let mut retirement = controller(&env);
    let iteration = tokio::spawn(async move {
        retirement.run_single_iteration_ext(false).await;
    });
    let retirement_pid = wait_for_blocked_query(&env.pool, blocker_pid, "FROM site_prefixes").await;

    let child_id = VpcPrefixId::new();
    let request = rpc::forge::VpcPrefixCreationRequest {
        id: Some(child_id),
        vpc_id: Some(vpc_id),
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: "10.68.0.0/24".to_string(),
        }),
        metadata: Some(rpc_metadata("racing child")),
        site_prefix_id: Some(root.id),
        ..Default::default()
    };
    let api = env.api.clone();
    let create = tokio::spawn(async move { api.create_vpc_prefix(Request::new(request)).await });
    wait_for_blocked_query(&env.pool, retirement_pid, "tenant_prefix_overlap:checks").await;
    blocker.commit().await.unwrap();
    iteration.await.unwrap();
    assert_eq!(create.await.unwrap().unwrap_err().code(), Code::NotFound);
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[root.id])
            .await
            .unwrap()
            .is_empty()
    );
    let child_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM network_vpc_prefixes WHERE id = $1")
            .bind(child_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(child_count, 0);
}
