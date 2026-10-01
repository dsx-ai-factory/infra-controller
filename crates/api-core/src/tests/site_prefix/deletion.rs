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

use rpc::forge::{
    VpcPrefixConfig, VpcPrefixCreationRequest, VpcPrefixDeletionRequest, VpcVirtualizationType,
};

use super::*;

#[crate::sqlx_test]
async fn tenant_root_deletion_waits_for_child_cleanup_and_releases_cidr_and_quota(
    pool: sqlx::PgPool,
) {
    let mut config = get_config();
    config.max_site_prefixes_per_tenant = 2;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            vpc_prefixes_drain_period: Some(chrono::Duration::zero()),
            ..TestEnvOverrides::with_config(config).with_fnn_config(None)
        },
    )
    .await;
    let tenant = "prefix-owner";
    create_fixture_tenant(&env, tenant).await.unwrap();
    let root_id = SitePrefixId::new();
    let retained_id = SitePrefixId::new();
    let root_cidr = "10.66.0.0/24";
    let retained_cidr = "10.67.0.0/24";
    for (id, prefix) in [(root_id, root_cidr), (retained_id, retained_cidr)] {
        env.api
            .create_site_prefix(Request::new(creation_request(id, tenant, prefix)))
            .await
            .unwrap();
    }
    controller(&env).run_single_iteration_ext(false).await;
    assert_eq!(
        stored_prefix(&env, root_id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Ready
    );

    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(tenant.to_string())
                .metadata(rpc_metadata("tenant root deletion"))
                .network_virtualization_type(VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let child_id = VpcPrefixId::new();
    let child = env
        .api
        .create_vpc_prefix(Request::new(VpcPrefixCreationRequest {
            id: Some(child_id),
            vpc_id: Some(vpc_id),
            site_prefix_id: Some(root_id),
            config: Some(VpcPrefixConfig {
                prefix: root_cidr.to_string(),
            }),
            metadata: Some(rpc_metadata("child prefix")),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(child.site_prefix_id, Some(root_id));

    let operator_root = persist_configured_site_prefix(&env, "198.51.100.0/24").await;
    let mut txn = env.pool.begin().await.unwrap();
    db::site_prefix::reconcile_configured(&mut txn, &[])
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let deleting = env
        .api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(root_id),
            tenant_organization_id: tenant.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .site_prefix
        .unwrap();
    let status = deleting.status.unwrap();
    assert_eq!(
        status.lifecycle_state,
        RpcSitePrefixLifecycleState::Deleting as i32
    );
    // Waiting for children must not queue behind routing writers or delay
    // subsequent DPU configuration reads by requesting the routing lock.
    let mut routing_guard = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut routing_guard)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        controller(&env).run_single_iteration_ext(false),
    )
    .await
    .expect("a child wait must complete while another transaction holds the routing lock");
    routing_guard.rollback().await.unwrap();
    assert_eq!(
        stored_prefix(&env, root_id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Deleting
    );
    let PersistentStateHandlerOutcome::Wait { reason, .. } = stored_outcome(&env, root_id).await
    else {
        panic!("the root must wait for its child VPC prefix");
    };
    assert_eq!(reason, "waiting for child VPC prefixes to be deleted");

    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest {
            id: Some(child_id),
        }))
        .await
        .unwrap();
    // No generated segments use this child. Its controller still owns the
    // drain and physical deletion; deleting the root must not skip them.
    for _ in 0..3 {
        env.run_vpc_prefix_controller_iteration().await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM network_vpc_prefixes WHERE id = $1")
            .bind(child_id)
            .fetch_one(&env.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        stored_prefix(&env, root_id).await.status.lifecycle_state,
        SitePrefixLifecycleState::Deleting
    );

    // The next controller pass finishes the persisted deletion request.
    controller(&env).run_single_iteration_ext(false).await;
    let inventory = env
        .api
        .find_site_prefix_ids(Request::new(SitePrefixSearchFilter::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        filter_ids(&inventory.site_prefix_ids),
        filter_ids(&[retained_id, operator_root.id])
    );
    assert_eq!(
        db::site_prefix::find_tenant_prefixes(&env.pool)
            .await
            .unwrap(),
        vec![retained_cidr.parse::<IpNetwork>().unwrap()]
    );

    let replacement_id = SitePrefixId::new();
    let replacement = env
        .api
        .create_site_prefix(Request::new(creation_request(
            replacement_id,
            tenant,
            root_cidr,
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(replacement.id, Some(replacement_id));
    assert_eq!(replacement.config.unwrap().prefix, root_cidr);
    let quota = replacement.status.unwrap().quota.unwrap();
    assert_eq!((quota.used, quota.limit), (2, 2));

    // The controller leaves operator roots available for restoration, even
    // when no child prevents their physical deletion.
    assert_eq!(
        stored_prefix(&env, operator_root.id)
            .await
            .status
            .lifecycle_state,
        SitePrefixLifecycleState::Deleting
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT controller_state_outcome IS NULL FROM site_prefixes WHERE id = $1",
        )
        .bind(operator_root.id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
    );
}
