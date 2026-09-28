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

use std::collections::{HashMap, HashSet};

use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use model::metadata::Metadata;
use model::site_prefix::{
    NewTenantManagedSitePrefix, SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState,
};
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    Label, ManagedHostNetworkConfigRequest, Metadata as RpcMetadata, NetworkSegmentDeletionRequest,
    NetworkSegmentsByIdsRequest, PrefixMatchType, SitePrefixAuthority as RpcSitePrefixAuthority,
    SitePrefixCreationRequest, SitePrefixDeletionRequest,
    SitePrefixLifecycleState as RpcSitePrefixLifecycleState,
    SitePrefixRoutingScope as RpcSitePrefixRoutingScope, SitePrefixSearchFilter,
    SitePrefixStateHistoriesRequest, SitePrefixUpdateRequest, SitePrefixesByIdsRequest,
    VersionRequest,
};
use tonic::{Code, Request};

use crate::cfg::file::{AdminFnnConfig, VpcIsolationBehaviorType};
use crate::handlers::tenant_prefix_overlap::validate_retained_state;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
};
use crate::tests::common::network_segment::NetworkSegmentHelper;
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::VpcCreationRequest;

mod readiness;

fn tenant_managed_site_prefix(
    prefix: &str,
    tenant_organization_id: &str,
) -> NewTenantManagedSitePrefix {
    NewTenantManagedSitePrefix {
        id: SitePrefixId::new(),
        prefix: prefix.parse().unwrap(),
        tenant_organization_id: tenant_organization_id.parse().unwrap(),
        metadata: Metadata {
            name: format!("{tenant_organization_id} prefix"),
            description: "tenant-managed test prefix".to_string(),
            labels: HashMap::from([("owner".to_string(), tenant_organization_id.to_string())]),
        },
    }
}

async fn persist_tenant_site_prefix(
    env: &TestEnv,
    value: NewTenantManagedSitePrefix,
    lifecycle_state: SitePrefixLifecycleState,
) -> SitePrefix {
    let mut txn = env.pool.begin().await.unwrap();
    let site_prefix = db::site_prefix::create_tenant_managed(
        value,
        env.config.max_site_prefixes_per_tenant,
        &mut txn,
    )
    .await
    .unwrap()
    .site_prefix;
    if lifecycle_state != SitePrefixLifecycleState::Provisioning {
        sqlx::query("UPDATE site_prefixes SET lifecycle_state = $1 WHERE id = $2")
            .bind(lifecycle_state)
            .bind(site_prefix.id)
            .execute(&mut *txn)
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
    db::site_prefix::find_by_ids(&env.pool, &[site_prefix.id])
        .await
        .unwrap()
        .pop()
        .unwrap()
}

async fn persist_configured_site_prefix(env: &TestEnv, prefix: &str) -> SitePrefix {
    // `reconcile_configured` treats this one prefix as the complete configured
    // set, so this helper must be called at most once in a test.
    let prefix = prefix.parse().unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    db::site_prefix::reconcile_configured(&mut txn, &[prefix])
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let id = db::site_prefix::find_ids(
        &env.pool,
        model::site_prefix::SitePrefixSearchFilter {
            authority: Some(SitePrefixAuthority::OperatorManaged),
            prefix_match: Some(model::site_prefix::PrefixMatch::Exact(prefix)),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .pop()
    .unwrap();
    db::site_prefix::find_by_ids(&env.pool, &[id])
        .await
        .unwrap()
        .pop()
        .unwrap()
}

/// Reads operator null routes through the public Version RPC so
/// retention tests verify the operator-visible contract rather than DB state.
async fn runtime_config_null_routes(env: &TestEnv) -> Vec<String> {
    env.api
        .version(Request::new(VersionRequest {
            display_config: true,
        }))
        .await
        .expect("Version must succeed")
        .into_inner()
        .runtime_config
        .expect("display_config must return runtime configuration")
        .site_fabric_null_routes
        .expect("new Core must report the operator null-route set")
        .items
}

fn filter_ids(ids: &[SitePrefixId]) -> HashSet<SitePrefixId> {
    ids.iter().copied().collect()
}

fn rpc_metadata(name: &str) -> RpcMetadata {
    RpcMetadata {
        name: name.to_string(),
        description: "tenant-managed SitePrefix".to_string(),
        labels: vec![Label {
            key: "env".to_string(),
            value: Some("test".to_string()),
        }],
    }
}

fn creation_request(
    id: SitePrefixId,
    tenant_organization_id: &str,
    prefix: &str,
) -> SitePrefixCreationRequest {
    SitePrefixCreationRequest {
        id: Some(id),
        tenant_organization_id: tenant_organization_id.to_string(),
        prefix: prefix.to_string(),
        metadata: Some(rpc_metadata("tenant prefix")),
    }
}

#[crate::sqlx_test]
async fn empty_site_prefix_inventory_and_missing_get_are_valid(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let ids = env
        .api
        .find_site_prefix_ids(Request::new(SitePrefixSearchFilter::default()))
        .await
        .unwrap()
        .into_inner();
    assert!(ids.site_prefix_ids.is_empty());

    let site_prefixes = env
        .api
        .find_site_prefixes_by_ids(Request::new(SitePrefixesByIdsRequest {
            site_prefix_ids: vec![SitePrefixId::new()],
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(site_prefixes.site_prefixes.is_empty());

    let error = env
        .api
        .find_site_prefixes_by_ids(Request::new(SitePrefixesByIdsRequest {
            site_prefix_ids: vec![],
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(error.message(), "at least one ID must be provided");
}

#[crate::sqlx_test]
async fn dpu_isolation_includes_retained_tenant_roots_without_reactivating_operator_roots(
    pool: sqlx::PgPool,
) {
    let mut config = get_config();
    config.max_site_prefix_isolation_rules = 1;
    config.site_fabric_prefixes = vec![
        "10.217.0.9/16".parse().unwrap(),
        "fd00::/48".parse().unwrap(),
    ];
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(config.site_fabric_prefixes.clone()),
            ..TestEnvOverrides::with_config(config).with_fnn_config(None)
        },
    )
    .await;
    assert!(!env.config.tenant_prefix_overlap_enabled);

    // These retained rows model a site that lowered its rule limit. Protection
    // must still include every lifecycle state rather than fail DPU rendering.
    for (tenant, prefix, state) in [
        (
            "tenant-a",
            "10.42.0.0/25",
            SitePrefixLifecycleState::Provisioning,
        ),
        (
            "tenant-b",
            "10.42.0.128/25",
            SitePrefixLifecycleState::Ready,
        ),
        ("tenant-c", "172.16.0.0/24", SitePrefixLifecycleState::Error),
        (
            "tenant-d",
            "192.168.0.0/24",
            SitePrefixLifecycleState::Deleting,
        ),
    ] {
        create_fixture_tenant(&env, tenant).await.unwrap();
        persist_tenant_site_prefix(&env, tenant_managed_site_prefix(prefix, tenant), state).await;
    }
    let retired_operator = persist_configured_site_prefix(&env, "198.51.100.0/24").await;
    let mut txn = env.pool.begin().await.unwrap();
    db::site_prefix::reconcile_configured(&mut txn, &[])
        .await
        .unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        db::site_prefix::find_by_ids(&env.pool, &[retired_operator.id])
            .await
            .unwrap()[0]
            .status
            .lifecycle_state,
        SitePrefixLifecycleState::Deleting,
    );

    let host = create_managed_host(&env).await;
    let response = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(host.dpu_ids[0]),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let expected = [
        "10.42.0.0/24",
        "10.217.0.0/16",
        "172.16.0.0/24",
        "192.168.0.0/24",
        "fd00::/48",
    ];
    assert_eq!(response.site_fabric_prefixes, expected);
    assert_eq!(
        response.deprecated_deny_prefixes,
        [
            "10.42.0.0/24",
            "10.217.0.0/16",
            "172.16.0.0/24",
            "192.168.0.0/24",
        ]
    );

    // Attach FNN on the same host so both wire contracts protect every
    // retained tenant state, including roots unrelated to this Instance.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .metadata(Metadata::new_with_default_name())
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let segment_id =
        NetworkSegmentHelper::new_with_tenant_prefix("10.217.1.0/24", "10.217.1.1", vpc_id)
            .create_with_api(&env.api)
            .await
            .unwrap()
            .id
            .unwrap();
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    host.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    let mut expected_null_routes = expected.map(str::to_string).to_vec();
    expected_null_routes.sort();
    assert_eq!(
        response.site_fabric_null_routes.unwrap().items,
        expected_null_routes
    );
    assert_eq!(
        runtime_config_null_routes(&env).await,
        ["10.217.0.0/16", "fd00::/48"]
    );
}

#[crate::sqlx_test]
async fn site_prefix_isolation_admission_counts_compacted_site_rules(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.max_site_prefix_isolation_rules = 3;
    config.site_fabric_prefixes = vec!["10.217.0.0/16".parse().unwrap()];
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(config.site_fabric_prefixes.clone()),
            ..TestEnvOverrides::with_config(config)
        },
    )
    .await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();

    let mut created_ids = Vec::new();
    for (tenant, prefix) in [
        ("tenant-a", "10.42.0.0/25"),
        ("tenant-b", "10.42.0.128/25"),
        ("tenant-a", "172.16.0.0/24"),
    ] {
        let id = SitePrefixId::new();
        let created = env
            .api
            .create_site_prefix(Request::new(creation_request(id, tenant, prefix)))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            created.status.unwrap().lifecycle_state,
            RpcSitePrefixLifecycleState::Provisioning as i32
        );
        created_ids.push(id);
    }
    env.api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(created_ids[2]),
            tenant_organization_id: "tenant-a".to_string(),
        }))
        .await
        .unwrap();

    let rejected_id = SitePrefixId::new();
    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            rejected_id,
            "tenant-b",
            "192.168.0.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    assert_eq!(
        error.message(),
        "SitePrefix isolation rule limit reached: rules in use 3, after creation 4, limit 3"
    );
    assert!(
        error
            .metadata()
            .get("nico-error-mitigation")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("max_site_prefix_isolation_rules")
    );
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[rejected_id])
            .await
            .unwrap()
            .is_empty()
    );
    let history = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![rejected_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(history.histories.is_empty());
}

#[crate::sqlx_test]
async fn lowered_isolation_limit_preserves_retry_but_rejects_a_compacting_new_root(
    pool: sqlx::PgPool,
) {
    let mut config = get_config();
    config.max_site_prefix_isolation_rules = 1;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            site_prefixes: Some(vec![]),
            ..Default::default()
        },
    )
    .await;
    for tenant in ["tenant-a", "tenant-b", "tenant-c"] {
        create_fixture_tenant(&env, tenant).await.unwrap();
    }
    let existing = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.1.0.0/24", "tenant-a"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.2.0.0/24", "tenant-b"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    let retry = env
        .api
        .create_site_prefix(Request::new(creation_request(
            existing.id,
            "tenant-a",
            "10.1.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.id, Some(existing.id));
    assert_eq!(retry.metadata.unwrap().name, existing.metadata.name);

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-c",
            "10.0.0.0/8",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    assert_eq!(
        error.message(),
        "SitePrefix isolation rule limit reached: rules in use 2, after creation 1, limit 1"
    );
}

#[crate::sqlx_test]
async fn tenant_site_prefix_admission_rejects_configured_denied_space(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.deny_prefixes = vec!["10.2.0.0/16".parse().unwrap(), "fd00::/8".parse().unwrap()];
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            site_prefixes: Some(vec![]),
            ..Default::default()
        },
    )
    .await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();
    let existing = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("10.2.0.0/24", "tenant-a"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    let retry = env
        .api
        .create_site_prefix(Request::new(creation_request(
            existing.id,
            "tenant-a",
            "10.2.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.id, Some(existing.id));

    for prefix in ["10.2.1.0/24", "10.0.0.0/8"] {
        let error = env
            .api
            .create_site_prefix(Request::new(creation_request(
                SitePrefixId::new(),
                "tenant-b",
                prefix,
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument);
        assert!(error.message().contains(&format!(
            "tenant SitePrefix {prefix} overlaps configured deny prefix 10.2.0.0/16"
        )));
    }
    let created = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-b",
            "192.168.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(created.config.unwrap().prefix, "192.168.0.0/24");
}

#[crate::sqlx_test]
async fn concurrent_tenant_roots_share_the_site_isolation_limit(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.max_site_prefix_isolation_rules = 1;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            site_prefixes: Some(vec![]),
            ..Default::default()
        },
    )
    .await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();

    let mut blocker = env.pool.begin().await.unwrap();
    db::tenant_prefix_overlap::lock_checks(&mut blocker)
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let first_id = SitePrefixId::new();
    let second_id = SitePrefixId::new();
    let first_api = env.api.clone();
    let first = tokio::spawn(async move {
        first_api
            .create_site_prefix(Request::new(creation_request(
                first_id,
                "tenant-a",
                "10.1.0.0/24",
            )))
            .await
    });
    let first_pid =
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
    let second_api = env.api.clone();
    let second = tokio::spawn(async move {
        second_api
            .create_site_prefix(Request::new(creation_request(
                second_id,
                "tenant-b",
                "10.2.0.0/24",
            )))
            .await
    });
    wait_for_blocked_query(&env.pool, first_pid, "tenant_prefix_overlap:checks").await;
    blocker.commit().await.unwrap();
    let results = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let error = results.into_iter().find_map(Result::err).unwrap();
    assert_eq!(error.code(), Code::ResourceExhausted);
    assert_eq!(
        error.message(),
        "SitePrefix isolation rule limit reached: rules in use 1, after creation 2, limit 1"
    );
    assert_eq!(
        db::site_prefix::find_by_ids(&env.pool, &[first_id, second_id])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[crate::sqlx_test]
async fn explicit_null_routes_reject_uncovered_creation_but_preserve_retries(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.site_fabric_null_routes = Some(vec!["10.0.0.0/8".parse().unwrap()]);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();
    assert!(!env.config.tenant_prefix_overlap_enabled);

    // An older configuration admitted this root. Retrying its immutable
    // identity does not add uncovered space or rewrite its history.
    let existing = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("192.168.0.0/24", "tenant-a"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    let history_request = SitePrefixStateHistoriesRequest {
        site_prefix_ids: vec![existing.id],
    };
    let history_before = env
        .api
        .find_site_prefix_state_histories(Request::new(history_request.clone()))
        .await
        .unwrap()
        .into_inner();
    let retry = env
        .api
        .create_site_prefix(Request::new(creation_request(
            existing.id,
            "tenant-a",
            "192.168.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.id, Some(existing.id));
    assert_eq!(retry.version, existing.version.to_string());
    assert_eq!(retry.metadata.unwrap().name, existing.metadata.name);
    assert_eq!(
        env.api
            .find_site_prefix_state_histories(Request::new(history_request))
            .await
            .unwrap()
            .into_inner(),
        history_before
    );

    let rejected_id = SitePrefixId::new();
    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            rejected_id,
            "tenant-b",
            "192.168.0.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "tenant SitePrefix 192.168.0.0/24 is not covered by configured site_fabric_null_routes"
    );
    assert!(
        db::site_prefix::find_by_ids(&env.pool, &[rejected_id])
            .await
            .unwrap()
            .is_empty()
    );
    let history = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![rejected_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(history.histories.is_empty());
}

#[crate::sqlx_test]
async fn open_isolation_does_not_require_tenant_null_routes_or_rule_budget(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.vpc_isolation_behavior = VpcIsolationBehaviorType::Open;
    config.site_fabric_null_routes = Some(vec![]);
    config.max_site_prefix_isolation_rules = 0;
    config.deny_prefixes = vec!["10.2.0.0/16".parse().unwrap()];
    let mut overrides = TestEnvOverrides::with_config(config).with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        vpc_vni: Some(10000),
        routing_profile: Default::default(),
    });
    let env = create_test_env_with_overrides(pool, overrides).await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    let host = create_managed_host(&env).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();

    let root = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "192.168.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(root.config.unwrap().prefix, "192.168.0.0/24");
    validate_retained_state(&env.api).await.unwrap();
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.use_admin_network);
    assert_eq!(
        response.network_virtualization_type,
        Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
    );
    assert_eq!(
        response.vpc_isolation_behavior,
        rpc::forge::VpcIsolationBehaviorType::VpcIsolationOpen as i32
    );
    assert!(response.site_fabric_null_routes.unwrap().items.is_empty());

    // `open` removes mutual isolation, not the configured site-wide deny list.
    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "10.2.0.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(error.message().contains("overlaps configured deny prefix"));
}

#[crate::sqlx_test]
async fn uncovered_retained_tenant_root_blocks_fnn_admin_and_startup_but_not_version(
    pool: sqlx::PgPool,
) {
    let mut config = get_config();
    config.site_fabric_null_routes = Some(vec![]);
    let mut overrides = TestEnvOverrides::with_config(config).with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        vpc_vni: Some(10000),
        routing_profile: Default::default(),
    });
    let env = create_test_env_with_overrides(pool, overrides).await;
    assert!(!env.config.tenant_prefix_overlap_enabled);

    // Test setup does not run the startup hooks that attach the Admin FNN VPC.
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    let host = create_managed_host(&env).await;
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.use_admin_network);
    assert_eq!(
        response.network_virtualization_type,
        Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
    );

    // Another replica can admit this root after the host reaches Admin FNN.
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("192.168.0.0/24", "tenant-a"),
        SitePrefixLifecycleState::Deleting,
    )
    .await;
    let mut txn = env.pool.begin().await.unwrap();
    assert!(
        db::tenant_prefix_overlap::find_duplicate_vpc_ids(&mut *txn, false)
            .await
            .unwrap()
            .is_empty()
    );
    txn.commit().await.unwrap();

    // Version must still show the explicit override so operators can inspect it.
    assert!(runtime_config_null_routes(&env).await.is_empty());
    let fnn_error = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await
        .unwrap_err();
    let startup_error: tonic::Status = validate_retained_state(&env.api).await.unwrap_err().into();
    for (operation, error) in [("Admin FNN", fnn_error), ("startup", startup_error)] {
        assert_eq!(error.code(), Code::FailedPrecondition, "{operation}");
        assert_eq!(
            error.message(),
            "tenant SitePrefix 192.168.0.0/24 is not covered by configured site_fabric_null_routes",
            "{operation}"
        );
    }
}

#[crate::sqlx_test]
async fn site_prefix_filters_and_readback_return_complete_inventory(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();

    let configured = persist_configured_site_prefix(&env, "10.0.0.0/8").await;
    let tenant_a = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("192.168.0.0/16", "tenant-a"),
        SitePrefixLifecycleState::Provisioning,
    )
    .await;
    let tenant_b = persist_tenant_site_prefix(
        &env,
        tenant_managed_site_prefix("172.16.0.0/12", "tenant-b"),
        SitePrefixLifecycleState::Error,
    )
    .await;

    let all_ids = filter_ids(&[configured.id, tenant_a.id, tenant_b.id]);
    let cases = [
        ("all", SitePrefixSearchFilter::default(), all_ids.clone()),
        (
            "tenant owner",
            SitePrefixSearchFilter {
                tenant_organization_id: Some("tenant-a".to_string()),
                ..Default::default()
            },
            filter_ids(&[tenant_a.id]),
        ),
        (
            "operator-managed authority",
            SitePrefixSearchFilter {
                authority: Some(RpcSitePrefixAuthority::OperatorManaged as i32),
                ..Default::default()
            },
            filter_ids(&[configured.id]),
        ),
        (
            "exact prefix",
            SitePrefixSearchFilter {
                prefix_match: Some("10.0.0.0/8".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixExact as i32),
                ..Default::default()
            },
            filter_ids(&[configured.id]),
        ),
        (
            "routing scope",
            SitePrefixSearchFilter {
                routing_scope: Some(RpcSitePrefixRoutingScope::DatacenterOnly as i32),
                ..Default::default()
            },
            all_ids.clone(),
        ),
        (
            "error lifecycle",
            SitePrefixSearchFilter {
                lifecycle_state: Some(RpcSitePrefixLifecycleState::Error as i32),
                ..Default::default()
            },
            filter_ids(&[tenant_b.id]),
        ),
        (
            "stored prefix contains query",
            SitePrefixSearchFilter {
                prefix_match: Some("192.168.1.0/24".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixContains as i32),
                ..Default::default()
            },
            filter_ids(&[tenant_a.id]),
        ),
        (
            "stored prefix is contained by query",
            SitePrefixSearchFilter {
                prefix_match: Some("172.16.0.0/11".to_string()),
                prefix_match_type: Some(PrefixMatchType::PrefixContainedBy as i32),
                ..Default::default()
            },
            filter_ids(&[tenant_b.id]),
        ),
    ];

    for (scenario, filter, expected) in cases {
        let result = env
            .api
            .find_site_prefix_ids(Request::new(filter))
            .await
            .unwrap_or_else(|error| panic!("{scenario}: {error}"))
            .into_inner();
        assert_eq!(filter_ids(&result.site_prefix_ids), expected, "{scenario}");
    }

    let missing_id = SitePrefixId::new();
    let response = env
        .api
        .find_site_prefixes_by_ids(Request::new(SitePrefixesByIdsRequest {
            site_prefix_ids: vec![configured.id, tenant_a.id, tenant_b.id, missing_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.site_prefixes.len(), 3);

    let configured_rpc = response
        .site_prefixes
        .iter()
        .find(|site_prefix| site_prefix.id == Some(configured.id))
        .expect("configured prefix should be returned");
    assert_eq!(
        configured_rpc.config.as_ref().unwrap().prefix,
        configured.config.prefix.to_string()
    );
    assert_eq!(
        configured_rpc.status.as_ref().unwrap().authority,
        RpcSitePrefixAuthority::OperatorManaged as i32
    );
    assert_eq!(
        configured_rpc.status.as_ref().unwrap().lifecycle_state,
        RpcSitePrefixLifecycleState::Ready as i32
    );
    assert!(configured_rpc.status.as_ref().unwrap().quota.is_none());
    assert_eq!(
        configured_rpc.metadata.as_ref().unwrap().description,
        configured.metadata.description
    );
    assert!(!configured_rpc.version.is_empty());
    assert!(configured_rpc.created_at.is_some());
    assert!(configured_rpc.updated_at.is_some());

    let tenant_rpc = response
        .site_prefixes
        .iter()
        .find(|site_prefix| site_prefix.id == Some(tenant_a.id))
        .expect("tenant prefix should be returned");
    assert_eq!(
        tenant_rpc
            .config
            .as_ref()
            .unwrap()
            .tenant_organization_id
            .as_deref(),
        Some("tenant-a")
    );
    assert_eq!(
        tenant_rpc.config.as_ref().unwrap().routing_scope,
        RpcSitePrefixRoutingScope::DatacenterOnly as i32
    );
    let quota = tenant_rpc.status.as_ref().unwrap().quota.as_ref().unwrap();
    assert_eq!(
        (quota.used, quota.limit),
        (1, env.config.max_site_prefixes_per_tenant)
    );
    assert_eq!(tenant_rpc.metadata.as_ref().unwrap().labels[0].key, "owner");
}

#[crate::sqlx_test]
async fn site_prefix_get_enforces_max_find_by_ids(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let site_prefix_ids = (0..=env.config.max_find_by_ids)
        .map(|_| SitePrefixId::new())
        .collect();

    let error = env
        .api
        .find_site_prefixes_by_ids(Request::new(SitePrefixesByIdsRequest { site_prefix_ids }))
        .await
        .expect_err("over-limit SitePrefix lookup should fail");
    assert_eq!(error.code(), Code::InvalidArgument);
}

#[crate::sqlx_test]
async fn site_prefix_create_derives_policy_and_enforces_tenant_admission(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();

    let site_prefix_id = SitePrefixId::new();
    let request = creation_request(site_prefix_id, "tenant-a", "10.0.0.0/24");
    let created = env
        .api
        .create_site_prefix(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(created.id, Some(site_prefix_id));
    let config = created.config.as_ref().unwrap();
    assert_eq!(config.prefix, "10.0.0.0/24");
    assert_eq!(config.tenant_organization_id.as_deref(), Some("tenant-a"));
    assert_eq!(
        config.routing_scope,
        RpcSitePrefixRoutingScope::DatacenterOnly as i32
    );
    let status = created.status.as_ref().unwrap();
    assert_eq!(
        status.authority,
        RpcSitePrefixAuthority::TenantManaged as i32
    );
    assert_eq!(
        status.lifecycle_state,
        RpcSitePrefixLifecycleState::Provisioning as i32
    );
    assert_eq!(status.quota.as_ref().unwrap().used, 1);
    assert_eq!(
        status.quota.as_ref().unwrap().limit,
        env.config.max_site_prefixes_per_tenant
    );

    // Immutable create identity, rather than mutable metadata, makes a retry
    // idempotent. The current persisted representation is returned and the
    // retry does not create another history entry.
    let mut retry_request = request;
    retry_request.metadata = Some(rpc_metadata("ignored retry metadata"));
    let retry = env
        .api
        .create_site_prefix(Request::new(retry_request))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry, created);

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            site_prefix_id,
            "tenant-a",
            "10.0.1.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "10.0.0.128/25",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(error.message().contains("overlaps"));

    // Overlap and quota are scoped to one tenant. Another tenant may own the
    // same private CIDR in this Core site.
    let tenant_b = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-b",
            "10.0.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        tenant_b
            .config
            .as_ref()
            .unwrap()
            .tenant_organization_id
            .as_deref(),
        Some("tenant-b")
    );
    assert_eq!(
        tenant_b
            .status
            .as_ref()
            .unwrap()
            .quota
            .as_ref()
            .unwrap()
            .used,
        1
    );

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "missing-tenant",
            "192.168.0.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::NotFound);

    let histories = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![site_prefix_id],
        }))
        .await
        .unwrap()
        .into_inner();
    let records = &histories.histories[&site_prefix_id.to_string()].records;
    assert_eq!(records.len(), 1);
    assert!(records[0].state.contains("provisioning"));
}

#[crate::sqlx_test]
async fn site_prefix_quota_retains_deleting_rows(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.max_site_prefixes_per_tenant = 2;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();

    let first_id = SitePrefixId::new();
    let first = env
        .api
        .create_site_prefix(Request::new(creation_request(
            first_id,
            "tenant-a",
            "10.0.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        first.status.as_ref().unwrap().quota.as_ref().unwrap().used,
        1
    );

    let second = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "10.0.1.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    let quota = second.status.as_ref().unwrap().quota.as_ref().unwrap();
    assert_eq!((quota.used, quota.limit), (2, 2));

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "10.0.2.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    assert_eq!(
        error.message(),
        "tenant SitePrefix quota reached: 2 of 2 retained SitePrefixes are in use"
    );
    assert_eq!(
        error
            .metadata()
            .get("nico-error-mitigation")
            .unwrap()
            .to_str()
            .unwrap(),
        "Review the tenant's retained SitePrefixes; complete removal of an unneeded prefix or \
         increase max_site_prefixes_per_tenant if additional roots are intended."
    );

    let deleting = env
        .api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(first_id),
            tenant_organization_id: "tenant-a".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .site_prefix
        .unwrap();
    assert_eq!(
        deleting.status.as_ref().unwrap().lifecycle_state,
        RpcSitePrefixLifecycleState::Deleting as i32
    );
    assert_eq!(
        deleting
            .status
            .as_ref()
            .unwrap()
            .quota
            .as_ref()
            .unwrap()
            .used,
        2
    );

    let error = env
        .api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-a",
            "10.0.2.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
}

#[crate::sqlx_test]
async fn site_prefix_update_retirement_and_history_enforce_ownership(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    create_fixture_tenant(&env, "tenant-a").await.unwrap();
    create_fixture_tenant(&env, "tenant-b").await.unwrap();

    let site_prefix_id = SitePrefixId::new();
    let created = env
        .api
        .create_site_prefix(Request::new(creation_request(
            site_prefix_id,
            "tenant-a",
            "192.168.0.0/24",
        )))
        .await
        .unwrap()
        .into_inner();
    let configured = persist_configured_site_prefix(&env, "203.0.113.0/24").await;

    let updated = env
        .api
        .update_site_prefix(Request::new(SitePrefixUpdateRequest {
            id: Some(site_prefix_id),
            tenant_organization_id: "tenant-a".to_string(),
            metadata: Some(rpc_metadata("updated prefix")),
            if_version_match: Some(created.version.clone()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.metadata.as_ref().unwrap().name, "updated prefix");
    assert_eq!(updated.config, created.config);
    assert_eq!(updated.status, created.status);
    assert_ne!(updated.version, created.version);

    let error = env
        .api
        .update_site_prefix(Request::new(SitePrefixUpdateRequest {
            id: Some(site_prefix_id),
            tenant_organization_id: "tenant-a".to_string(),
            metadata: Some(rpc_metadata("stale update")),
            if_version_match: Some(created.version.clone()),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);

    let error = env
        .api
        .update_site_prefix(Request::new(SitePrefixUpdateRequest {
            id: Some(site_prefix_id),
            tenant_organization_id: "tenant-b".to_string(),
            metadata: Some(rpc_metadata("wrong owner")),
            if_version_match: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::PermissionDenied);

    let error = env
        .api
        .update_site_prefix(Request::new(SitePrefixUpdateRequest {
            id: Some(configured.id),
            tenant_organization_id: "tenant-a".to_string(),
            metadata: Some(rpc_metadata("configured root")),
            if_version_match: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "operator-managed SitePrefixes cannot be changed through the tenant API"
    );

    let error = env
        .api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(site_prefix_id),
            tenant_organization_id: "tenant-b".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::PermissionDenied);

    let error = env
        .api
        .delete_site_prefix(Request::new(SitePrefixDeletionRequest {
            id: Some(configured.id),
            tenant_organization_id: "tenant-a".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "operator-managed SitePrefixes cannot be changed through the tenant API"
    );

    let retire_request = SitePrefixDeletionRequest {
        id: Some(site_prefix_id),
        tenant_organization_id: "tenant-a".to_string(),
    };
    let deleting = env
        .api
        .delete_site_prefix(Request::new(retire_request.clone()))
        .await
        .unwrap()
        .into_inner()
        .site_prefix
        .unwrap();
    assert_eq!(
        deleting.status.as_ref().unwrap().lifecycle_state,
        RpcSitePrefixLifecycleState::Deleting as i32
    );
    assert_ne!(deleting.version, updated.version);
    assert_eq!(
        deleting
            .status
            .as_ref()
            .unwrap()
            .quota
            .as_ref()
            .unwrap()
            .used,
        1
    );

    let retry = env
        .api
        .delete_site_prefix(Request::new(retire_request))
        .await
        .unwrap()
        .into_inner()
        .site_prefix
        .unwrap();
    assert_eq!(retry, deleting);

    let error = env
        .api
        .update_site_prefix(Request::new(SitePrefixUpdateRequest {
            id: Some(site_prefix_id),
            tenant_organization_id: "tenant-a".to_string(),
            metadata: Some(rpc_metadata("too late")),
            if_version_match: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);

    let histories = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![site_prefix_id],
        }))
        .await
        .unwrap()
        .into_inner();
    let records = &histories.histories[&site_prefix_id.to_string()].records;
    assert_eq!(records.len(), 2);
    assert!(records[0].state.contains("provisioning"));
    assert!(records[1].state.contains("deleting"));
    assert_eq!(records[0].version, created.version);
    assert_eq!(records[1].version, deleting.version);

    let error = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: vec![],
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);

    let error = env
        .api
        .find_site_prefix_state_histories(Request::new(SitePrefixStateHistoriesRequest {
            site_prefix_ids: (0..=env.config.max_find_by_ids)
                .map(|_| SitePrefixId::new())
                .collect(),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
}

/// Verifies the Version RPC retains a retired operator root through child soft
/// deletion, so operator-visible isolation lasts until the address space drains.
#[crate::sqlx_test]
async fn runtime_config_retains_removed_operator_root_until_child_hard_delete(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Current configuration omits the old root; persistence must supply it.
    let configured_root: IpNetwork = "10.0.0.0/8".parse()?;
    let retiring_root: IpNetwork = "172.16.0.0/12".parse()?;
    let mut config = get_config();
    config.site_fabric_prefixes = vec![configured_root];
    config.site_fabric_null_routes = None;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    // Reconstruct a retired root with exact legacy child lineage. Direct SQL
    // models the predecessor state after new admission for that root has closed.
    let mut txn = env.pool.begin().await?;
    db::site_prefix::reconcile_configured(&mut txn, &[configured_root, retiring_root]).await?;
    db::site_prefix::reconcile_configured(&mut txn, &[configured_root]).await?;
    let retiring_root_id: SitePrefixId =
        sqlx::query_scalar("SELECT id FROM site_prefixes WHERE prefix = $1")
            .bind(retiring_root)
            .fetch_one(&mut *txn)
            .await?;
    let vpc_id = VpcId::new();
    sqlx::query("INSERT INTO vpcs (id, name, organization_id, version) VALUES ($1, $2, $3, $4)")
        .bind(vpc_id)
        .bind("runtime-config-retained-root")
        .bind("tenant-a")
        .bind(ConfigVersion::initial())
        .execute(&mut *txn)
        .await?;
    let child_id = VpcPrefixId::new();
    sqlx::query(
        "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id, site_prefix_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(child_id)
    .bind("172.16.1.0/24".parse::<IpNetwork>()?)
    .bind("retained child")
    .bind(vpc_id)
    .bind(retiring_root_id)
    .execute(&mut *txn)
    .await?;
    // A draining child still owns address space even after its soft deletion.
    sqlx::query("UPDATE network_vpc_prefixes SET deleted = now() WHERE id = $1")
        .bind(child_id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    // Re-read through Version so the assertion proves committed query wiring.
    let mut null_routes = runtime_config_null_routes(&env).await;
    null_routes.sort();
    assert_eq!(
        null_routes,
        vec![configured_root.to_string(), retiring_root.to_string()],
        "soft-deleted children must retain the retiring root"
    );

    // Only physical removal of the last child permits the route to disappear.
    sqlx::query("DELETE FROM network_vpc_prefixes WHERE id = $1")
        .bind(child_id)
        .execute(&env.pool)
        .await?;
    assert_eq!(
        runtime_config_null_routes(&env).await,
        vec![configured_root.to_string()],
        "the retiring root may be withdrawn after its last child is hard-deleted"
    );

    Ok(())
}

/// Proves a retired root remains protected while a public direct VPC segment
/// drains, because its NetworkPrefix has no VpcPrefix lineage to retain it.
#[crate::sqlx_test]
async fn runtime_config_retains_removed_operator_root_until_direct_segment_hard_delete(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Keep admission on the predecessor roots while runtime config models
    // retirement, so the public segment creation path remains representative.
    let configured_root: IpNetwork = "10.0.0.0/8".parse()?;
    let retiring_root: IpNetwork = "172.16.0.0/12".parse()?;
    let mut config = get_config();
    config.site_fabric_prefixes = vec![configured_root];
    config.site_fabric_null_routes = None;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            // Admission represents the pre-retirement configuration, while
            // runtime configuration represents the restarted target Core.
            site_prefixes: Some(vec![configured_root, retiring_root]),
            network_segments_drain_period: Some(chrono::Duration::zero()),
            ..TestEnvOverrides::with_config(config).with_fnn_config(None)
        },
    )
    .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;

    // Reconstruct the operator root before creating the tenant resource
    // through the same public path used before its configuration retirement.
    let mut txn = env.pool.begin().await?;
    db::site_prefix::reconcile_configured(&mut txn, &[configured_root, retiring_root]).await?;
    txn.commit().await?;
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .metadata(RpcMetadata {
                    name: "direct-prefix retained-root VPC".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner()
        .id
        .expect("created VPC must have an ID");
    let segment =
        NetworkSegmentHelper::new_with_tenant_prefix("172.16.1.0/24", "172.16.1.1", vpc_id)
            .create_with_api(&env.api)
            .await?;
    let segment_id = segment.id.expect("created segment must have an ID");
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    // Reload through the public API to prove the direct segment and its prefix
    // reached persistence before testing the retention contract.
    let persisted_segments = env
        .api
        .find_network_segments_by_ids(Request::new(NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_id],
            include_history: false,
            include_num_free_ips: false,
        }))
        .await?
        .into_inner()
        .network_segments;
    assert_eq!(persisted_segments.len(), 1);
    assert_eq!(persisted_segments[0].id, Some(segment_id));

    // Retire the root as startup reconciliation does after its removal from
    // configuration, then verify the direct prefix retains its blackhole.
    let mut txn = env.pool.begin().await?;
    db::site_prefix::reconcile_configured(&mut txn, &[configured_root]).await?;
    txn.commit().await?;
    assert_eq!(
        runtime_config_null_routes(&env).await,
        vec![configured_root.to_string(), retiring_root.to_string()]
    );

    // Soft deletion must not withdraw isolation while the segment's direct
    // NetworkPrefix remains present during controller draining.
    env.api
        .delete_network_segment(Request::new(NetworkSegmentDeletionRequest {
            id: Some(segment_id),
        }))
        .await?;
    assert_eq!(
        runtime_config_null_routes(&env).await,
        vec![configured_root.to_string(), retiring_root.to_string()]
    );

    // Drive the zero-duration drain through physical deletion, which removes
    // the final direct prefix and permits the retired route to disappear.
    for _ in 0..3 {
        env.run_network_segment_controller_iteration().await;
    }
    assert!(
        env.api
            .find_network_segments_by_ids(Request::new(NetworkSegmentsByIdsRequest {
                network_segments_ids: vec![segment_id],
                include_history: false,
                include_num_free_ips: false,
            }))
            .await?
            .into_inner()
            .network_segments
            .is_empty()
    );
    assert_eq!(
        runtime_config_null_routes(&env).await,
        vec![configured_root.to_string()]
    );

    Ok(())
}

/// Proves an FNN DPU response remains fail-closed for predecessor data whose
/// VpcPrefix lineage is ambiguous after the final configured root is removed.
#[crate::sqlx_test]
async fn dpu_response_retains_containing_roots_for_unassigned_predecessor_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Separate the predecessor owner from the serving tenant so retained roots
    // must be discovered site-wide even with no roots in current configuration.
    let broad_root: IpNetwork = "10.0.0.0/8".parse()?;
    let specific_root: IpNetwork = "10.1.0.0/16".parse()?;
    let mut config = get_config();
    config.site_fabric_prefixes.clear();
    config.site_fabric_null_routes = None;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(vec![]),
            ..TestEnvOverrides::with_config(config).with_fnn_config(None)
        },
    )
    .await;
    let predecessor_tenant = "predecessor-lineage-tenant";
    create_fixture_tenant(&env, predecessor_tenant).await?;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    let predecessor_vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(predecessor_tenant)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .metadata(RpcMetadata {
                    name: "predecessor-lineage VPC".to_string(),
                    ..Default::default()
                })
                .tonic_request(),
        )
        .await?
        .into_inner()
        .id
        .expect("predecessor VPC must have an ID");
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .metadata(RpcMetadata {
                    name: "serving tenant VPC".to_string(),
                    ..Default::default()
                })
                .rpc(),
        )
        .await;

    // Reconstruct v2.1-style nested roots and a child left unassigned by the
    // lineage migration, then remove the final target configuration root.
    let mut txn = env.pool.begin().await?;
    db::site_prefix::reconcile_configured(&mut txn, &[broad_root, specific_root]).await?;
    db::site_prefix::reconcile_configured(&mut txn, &[]).await?;
    sqlx::query(
        "INSERT INTO network_vpc_prefixes (id, prefix, name, vpc_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(VpcPrefixId::new())
    .bind("10.1.2.0/24".parse::<IpNetwork>()?)
    .bind("ambiguous predecessor child")
    .bind(predecessor_vpc_id)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    // Exercise the public DPU response rather than only the retained-root
    // query so the complete fail-closed rendering path is protected.
    let host = create_managed_host(&env).await;
    host.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await?
        .into_inner();
    let mut rendered_roots = response
        .site_fabric_null_routes
        .expect("new Core must send a presence-bearing FNN route list")
        .items;
    rendered_roots.sort();
    let expected = vec![broad_root.to_string()];

    // Core discovers both possible operator parents, then combines their exact
    // union for FNN. Legacy fields exclude retiring operator roots; this fixture
    // has no configured or tenant roots, so those fields remain empty.
    assert_eq!(rendered_roots, expected);
    assert!(response.site_fabric_prefixes.is_empty());
    assert!(response.deprecated_deny_prefixes.is_empty());

    Ok(())
}

/// Proves the public FNN DPU response uses the explicit runtime override,
/// rather than the independently seeded legacy Ethernet data.
#[crate::sqlx_test]
async fn dpu_response_uses_explicit_fnn_null_route_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let inherited_root: IpNetwork = "198.51.100.0/24".parse()?;
    let null_route: IpNetwork = "10.0.0.0/8".parse()?;
    let stronger_null_route: IpNetwork = "10.2.0.0/24".parse()?;

    // Keep every possible source distinct so the response identifies whether
    // Core used runtime configuration or independently seeded legacy data.
    let mut config = get_config();
    config.site_fabric_prefixes = vec![inherited_root];
    config.site_fabric_null_routes = Some(vec![null_route, stronger_null_route]);
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_fnn_config(None),
    )
    .await;

    create_fixture_tenant(&env, "tenant-null-route").await?;
    env.api
        .create_site_prefix(Request::new(creation_request(
            SitePrefixId::new(),
            "tenant-null-route",
            "10.2.0.0/16",
        )))
        .await?;

    // Attach an FNN instance so the public DPU handler takes its FNN-specific
    // null-route path rather than the ETV site-prefix path.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn)
                .metadata(RpcMetadata {
                    name: "explicit null-route VPC".to_string(),
                    ..Default::default()
                })
                .rpc(),
        )
        .await;
    let host = create_managed_host(&env).await;
    host.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Exercise the wire response because Core must keep legacy site prefixes
    // separate from the authoritative null routes selected for new agents.
    let response = env
        .api
        .get_managed_host_network_config(Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu().id),
        }))
        .await?
        .into_inner();
    let expected = vec![null_route.to_string(), stronger_null_route.to_string()];

    // Legacy agents still receive the tenant root. Its /16 must not replace
    // the explicit /8 and /24 routes: the narrower blackhole remains stronger
    // than an authorized /8 import on updated agents.
    assert_eq!(
        response.site_fabric_prefixes,
        vec!["10.2.0.0/16".to_string(), inherited_root.to_string()]
    );
    assert_eq!(
        response.deprecated_deny_prefixes,
        vec!["10.2.0.0/16".to_string(), inherited_root.to_string()]
    );
    assert_eq!(
        response
            .site_fabric_null_routes
            .expect("new Core must send a presence-bearing FNN route list")
            .items,
        expected
    );
    assert_eq!(runtime_config_null_routes(&env).await, expected);

    Ok(())
}
