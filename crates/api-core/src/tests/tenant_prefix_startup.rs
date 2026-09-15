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

use carbide_test_support::{Case, Outcome};
use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use model::metadata::Metadata;
use model::site_prefix::{NewTenantManagedSitePrefix, SitePrefixLifecycleState};
use model::vpc_prefix::{NewVpcPrefix, VpcPrefixConfig};
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;

use crate::handlers::tenant_prefix_overlap::validate_retained_state;
use crate::tests::common::api_fixtures::network_segment::{
    create_network_segment, create_tenant_network_segment,
};
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides, get_vpc_fixture_id,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::VpcCreationRequest;

async fn create_other_vpc(env: &TestEnv) -> rpc::forge::Vpc {
    create_fixture_tenant(env, "startup-tenant").await.unwrap();
    env.api
        .create_vpc(
            VpcCreationRequest::builder("startup-tenant")
                .metadata(Metadata {
                    name: "startup-other-vpc".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
}

async fn retain_prefix(
    env: &TestEnv,
    txn: &mut sqlx::PgConnection,
    vpc: &rpc::forge::Vpc,
    prefix: &str,
) -> VpcPrefixId {
    let root = db::site_prefix::create_tenant_managed(
        NewTenantManagedSitePrefix {
            id: SitePrefixId::new(),
            prefix: prefix.parse().unwrap(),
            tenant_organization_id: "startup-tenant".parse().unwrap(),
            metadata: Metadata {
                name: "retained startup root".to_string(),
                ..Default::default()
            },
        },
        env.config.max_site_prefixes_per_tenant,
        txn,
    )
    .await
    .unwrap()
    .site_prefix;
    sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
        .bind(SitePrefixLifecycleState::Deleting)
        .bind(root.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    let id = VpcPrefixId::new();
    // Separate table constraints permit this retained-data fixture. Keep both
    // exclusions installed so it cannot enable ordinary duplicate persistence.
    db::vpc_prefix::persist(
        NewVpcPrefix {
            id,
            site_prefix_id: Some(root.id),
            vpc_id: vpc.id.unwrap(),
            config: VpcPrefixConfig {
                prefix: prefix.parse().unwrap(),
            },
            metadata: Metadata {
                name: "retained startup prefix".to_string(),
                ..Default::default()
            },
        },
        vpc.version.parse().unwrap(),
        txn,
    )
    .await
    .unwrap();
    id
}

#[crate::sqlx_test]
async fn retained_startup_audit_waits_for_prefix_commit(pool: PgPool) {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            ..Default::default()
        },
    )
    .await;
    env.create_vpc_and_tenant_segment().await;
    let tenant_vpc_id = get_vpc_fixture_id(&env).await;
    create_tenant_network_segment(
        &env.api,
        Some(tenant_vpc_id),
        "10.118.1.1/24".parse().unwrap(),
        "startup direct prefix",
        false,
    )
    .await;
    validate_retained_state(&env.api).await.unwrap();
    let other_vpc = create_other_vpc(&env).await;

    let mut writer = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut writer)
        .await
        .unwrap();
    let writer_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let prefix_id = retain_prefix(&env, &mut writer, &other_vpc, "10.118.1.0/24").await;
    sqlx::query("UPDATE network_vpc_prefixes SET deleted = NOW() WHERE id = $1")
        .bind(prefix_id)
        .execute(&mut *writer)
        .await
        .unwrap();

    let commit = async {
        wait_for_blocked_query(&env.pool, writer_pid, "tenant_prefix_overlap:checks").await;
        writer.commit().await.unwrap();
    };
    let (result, ()) = tokio::join!(validate_retained_state(&env.api), commit);
    let error: tonic::Status = result.unwrap_err().into();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    let mut expected = vec![tenant_vpc_id, other_vpc.id.unwrap()];
    expected.sort_unstable();
    let mut txn = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut txn)
        .await
        .unwrap();
    assert_eq!(
        db::tenant_prefix_overlap::find_duplicate_vpc_ids(&mut *txn, false)
            .await
            .unwrap(),
        expected
    );
    for case in [
        Case {
            scenario: "direct segment copy",
            input: vec![tenant_vpc_id],
            expect: Outcome::Yields(true),
        },
        Case {
            scenario: "deleting tenant-managed copy",
            input: vec![other_vpc.id.unwrap()],
            expect: Outcome::Yields(true),
        },
    ] {
        let txn = &mut *txn;
        case.check_async(|source_ids| async move {
            db::tenant_prefix_overlap::vpcs_use_duplicate_space(txn, &source_ids)
                .await
                .map_err(drop)
        })
        .await;
    }
    assert!(
        !db::tenant_prefix_overlap::has_direct_prefix_overlap(&mut *txn, &[])
            .await
            .unwrap()
    );
    assert!(
        !db::tenant_prefix_overlap::has_direct_prefix_overlap(&mut *txn, &[VpcId::new()])
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn admin_startup_overlap_rejection_rolls_back_attachment(pool: PgPool) {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    create_network_segment(
        &env.api,
        "startup admin prefix",
        "10.118.2.0/24",
        "10.118.2.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        false,
    )
    .await;
    let other_vpc = create_other_vpc(&env).await;
    let mut txn = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut txn)
        .await
        .unwrap();
    retain_prefix(&env, &mut txn, &other_vpc, "10.118.2.0/24").await;
    txn.commit().await.unwrap();

    // The Admin segment has no VPC until reconciliation attaches it. Its
    // prefix must be checked before that new routing relationship commits.
    validate_retained_state(&env.api).await.unwrap();
    let error: tonic::Status = crate::db_init::create_admin_vpc(&env.api, Some(10_000))
        .await
        .unwrap_err()
        .into();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        db::vpc::find_by_name(&env.pool, "admin")
            .await
            .unwrap()
            .is_empty()
    );
    let mut txn = env.pool.begin().await.unwrap();
    let segments = db::network_segment::admin(&mut txn).await.unwrap();
    assert!(!segments.is_empty());
    assert!(
        segments
            .iter()
            .all(|segment| segment.config.vpc_id.is_none())
    );
    txn.commit().await.unwrap();
}
