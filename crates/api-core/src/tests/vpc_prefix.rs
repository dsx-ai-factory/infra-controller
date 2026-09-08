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
use std::time::Duration;

use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use model::metadata::Metadata as ModelMetadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};
use model::site_prefix::{
    NewTenantManagedSitePrefix, RetireTenantManagedSitePrefix, SitePrefixAuthority,
    SitePrefixLifecycleState, SitePrefixRoutingScope,
};
use model::vpc_prefix::{NewVpcPrefix, VpcPrefixConfig};
use rpc::Metadata;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    PrefixMatchType, VpcDeletionRequest, VpcPrefixCreationRequest, VpcPrefixDeletionRequest,
    VpcPrefixSearchQuery,
};
use sqlx::{PgPool, PgTransaction};
use tonic::Request;

use crate::cfg::file::{FnnConfig, FnnRoutingProfileConfig, VpcIsolationBehaviorType};
use crate::network_segment::allocate::PrefixAllocator;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config_with_vpc_prefix,
};
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{
    self, TEST_SITE_PREFIXES, TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_vpc_fixture_id,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::VpcCreationRequest;

const REFERENCED_VPC_PREFIX: &str = "192.0.4.0/24";
const UNREFERENCED_VPC_PREFIX: &str = "192.1.4.0/27";

/// Returns tenant config matching the shared VPC fixture so lifecycle tests
/// exercise prefix behavior rather than fail ownership validation.
fn fixture_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
        ..default_tenant_config()
    }
}

/// Seeds a tenant-managed SitePrefix at the requested lifecycle boundary.
async fn seed_tenant_managed_site_prefix(
    env: &TestEnv,
    tenant_organization_id: &str,
    prefix: &str,
    lifecycle_state: SitePrefixLifecycleState,
) -> SitePrefixId {
    let site_prefix_id = SitePrefixId::new();
    sqlx::query(
        r#"
            INSERT INTO site_prefixes (
                id,
                prefix,
                authority,
                tenant_organization_id,
                routing_scope,
                lifecycle_state,
                name,
                version
            )
            VALUES ($1, $2::cidr, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(site_prefix_id)
    .bind(prefix)
    .bind(SitePrefixAuthority::TenantManaged)
    .bind(tenant_organization_id)
    .bind(SitePrefixRoutingScope::DatacenterOnly)
    .bind(lifecycle_state)
    .bind(format!("tenant SitePrefix {prefix}"))
    .bind(ConfigVersion::initial())
    .execute(&env.pool)
    .await
    .expect("tenant-managed SitePrefix fixture should be persisted");
    site_prefix_id
}

/// Test-specific function that creates an FNN VPC for an existing tenant.
async fn create_fnn_vpc_for_tenant(
    env: &TestEnv,
    tenant: &str,
    name: &str,
    routing_profile_type: Option<&str>,
) -> VpcId {
    let mut request = VpcCreationRequest::builder(tenant.to_owned())
        .metadata(Metadata {
            name: name.to_owned(),
            ..Default::default()
        })
        .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32);
    if let Some(routing_profile_type) = routing_profile_type {
        request = request.routing_profile_type(routing_profile_type.to_string());
    }

    env.api
        .create_vpc(request.tonic_request())
        .await
        .expect("FNN VPC fixture should be created")
        .into_inner()
        .id
        .expect("created FNN VPC should include an ID")
}

/// Builds an exact-parent VPC-prefix request for admission and locking tests.
fn site_prefix_child_request(
    id: VpcPrefixId,
    vpc_id: VpcId,
    site_prefix_id: Option<SitePrefixId>,
    prefix: &str,
) -> VpcPrefixCreationRequest {
    VpcPrefixCreationRequest {
        id: Some(id),
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: prefix.to_owned(),
        }),
        metadata: Some(Metadata {
            name: format!("VPC prefix {prefix}"),
            ..Default::default()
        }),
        site_prefix_id,
    }
}

/// Test-specific function that configures overlap checks with the requested
/// site gate.
fn tenant_prefix_overlap_overrides(site_gate_enabled: bool) -> TestEnvOverrides {
    let mut config = crate::test_support::default_config::get();
    config.tenant_prefix_overlap_enabled = site_gate_enabled;
    config.vpc_isolation_behavior = VpcIsolationBehaviorType::MutualIsolation;

    let mut overrides = TestEnvOverrides::with_config(config).with_fnn_config(Some(FnnConfig {
        admin_vpc: None,
        common_internal_route_target: None,
        additional_route_target_imports: vec![],
        routing_profiles: HashMap::from([(
            "OVERLAP".to_string(),
            FnnRoutingProfileConfig {
                tenant_prefix_overlap_eligible: true,
                internal: Some(true),
                ..Default::default()
            },
        )]),
        use_vpc_vrf_loopback: false,
    }));
    overrides.create_network_segments = Some(false);
    overrides.site_prefixes = Some(vec!["10.0.0.0/8".parse().unwrap()]);
    overrides
}

/// Test-specific function that creates a tenant allowed to request the overlap profile.
async fn create_overlap_tenant(env: &TestEnv, organization_id: &str) -> Result<(), tonic::Status> {
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: organization_id.to_string(),
            routing_profile_type: Some("OVERLAP".to_string()),
            metadata: Some(Metadata {
                name: organization_id.to_string(),
                ..Default::default()
            }),
        }))
        .await?;
    Ok(())
}

/// Test-specific function that persists a VpcPrefix while the returned transaction holds the lock.
async fn hold_vpc_prefix_create<'a>(
    env: &'a TestEnv,
    id: VpcPrefixId,
    vpc_id: VpcId,
    site_prefix_id: SitePrefixId,
    prefix: &str,
) -> Result<(PgTransaction<'a>, i32), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *txn)
        .await?;
    let vpc = db::vpc::find_by_with_lock(
        txn.as_mut(),
        db::ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?
    .pop()
    .expect("VPC should exist");
    db::vpc_prefix::persist(
        NewVpcPrefix {
            id,
            site_prefix_id: Some(site_prefix_id),
            vpc_id,
            config: VpcPrefixConfig {
                prefix: prefix.parse()?,
            },
            metadata: ModelMetadata {
                name: format!("held VPC prefix {prefix}"),
                ..Default::default()
            },
        },
        vpc.version,
        &mut txn,
    )
    .await?;
    Ok((txn, blocker_pid))
}

/// Test-specific function that persists an attached segment while the returned
/// transaction holds the lock.
async fn hold_attached_segment_create<'a>(
    env: &'a TestEnv,
    vpc_id: VpcId,
    prefix: &str,
) -> Result<(PgTransaction<'a>, i32), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *txn)
        .await?;
    db::network_segment::persist(
        NewNetworkSegment {
            id: NetworkSegmentId::new(),
            name: format!("held segment {prefix}"),
            subdomain_id: None,
            vpc_id: Some(vpc_id),
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: prefix.parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Tenant,
            can_stretch: None,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    Ok((txn, blocker_pid))
}

/// Test-specific function that builds an attached segment with one direct prefix.
fn attached_segment_request(
    id: NetworkSegmentId,
    vpc_id: VpcId,
    prefix: &str,
    gateway: &str,
    segment_type: rpc::forge::NetworkSegmentType,
) -> rpc::forge::NetworkSegmentCreationRequest {
    rpc::forge::NetworkSegmentCreationRequest {
        id: Some(id),
        name: format!("attached segment {prefix}"),
        subdomain_id: None,
        vpc_id: Some(vpc_id),
        mtu: Some(1500),
        prefixes: vec![rpc::forge::NetworkPrefix {
            id: None,
            prefix: prefix.to_string(),
            gateway: Some(gateway.to_string()),
            reserve_first: 1,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }],
        segment_type: segment_type as i32,
        infer_slaac_eui64_addresses: false,
    }
}

/// Test-specific function that builds an unattached HostInband segment with one direct prefix.
fn unattached_host_inband_segment_request(
    id: NetworkSegmentId,
    prefix: &str,
    gateway: &str,
) -> rpc::forge::NetworkSegmentCreationRequest {
    rpc::forge::NetworkSegmentCreationRequest {
        id: Some(id),
        name: format!("unattached segment {prefix}"),
        subdomain_id: None,
        vpc_id: None,
        mtu: Some(1500),
        prefixes: vec![rpc::forge::NetworkPrefix {
            id: None,
            prefix: prefix.to_string(),
            gateway: Some(gateway.to_string()),
            reserve_first: 1,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }],
        segment_type: rpc::forge::NetworkSegmentType::HostInband as i32,
        infer_slaac_eui64_addresses: false,
    }
}

/// Test-specific function that checks an eligible pair reaches the existing database exclusion.
#[crate::sqlx_test]
async fn eligible_exact_overlap_reaches_legacy_database_exclusion(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, tenant_prefix_overlap_overrides(true)).await;
    let tenant_a = "overlap-eligible-a";
    let tenant_b = "overlap-eligible-b";
    create_overlap_tenant(&env, tenant_a).await?;
    create_overlap_tenant(&env, tenant_b).await?;
    let vpc_a = create_fnn_vpc_for_tenant(&env, tenant_a, "overlap VPC A", Some("OVERLAP")).await;
    let vpc_b = create_fnn_vpc_for_tenant(&env, tenant_b, "overlap VPC B", Some("OVERLAP")).await;
    let root_a = seed_tenant_managed_site_prefix(
        &env,
        tenant_a,
        "10.100.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let root_b = seed_tenant_managed_site_prefix(
        &env,
        tenant_b,
        "10.100.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    env.api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            vpc_a,
            Some(root_a),
            "10.100.1.0/24",
        )))
        .await?;
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            vpc_b,
            Some(root_b),
            "10.100.1.0/24",
        )))
        .await
        .expect_err("the legacy database exclusion should still block exact reuse");

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error
            .message()
            .contains("overlaps an existing or deleting VPC prefix"),
        "the pair check should accept the pair before persistence: {error}"
    );
    Ok(())
}

/// Test-specific function that checks VpcPrefix create rereads after a concurrent commit.
#[crate::sqlx_test]
async fn vpc_prefix_create_rechecks_after_concurrent_vpc_prefix_commit(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, tenant_prefix_overlap_overrides(true)).await;
    let tenant_a = "serialized-vpc-prefix-a";
    let tenant_b = "serialized-vpc-prefix-b";
    create_overlap_tenant(&env, tenant_a).await?;
    create_overlap_tenant(&env, tenant_b).await?;
    let vpc_a =
        create_fnn_vpc_for_tenant(&env, tenant_a, "serialized VPC A", Some("OVERLAP")).await;
    let vpc_b =
        create_fnn_vpc_for_tenant(&env, tenant_b, "serialized VPC B", Some("OVERLAP")).await;
    let root_a = seed_tenant_managed_site_prefix(
        &env,
        tenant_a,
        "10.101.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let root_b = seed_tenant_managed_site_prefix(
        &env,
        tenant_b,
        "10.101.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    let first_id = VpcPrefixId::new();
    let (first_create, blocker_pid) =
        hold_vpc_prefix_create(&env, first_id, vpc_a, root_a, "10.101.1.0/24").await?;
    let second_id = VpcPrefixId::new();
    let second_create = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            second_id,
            vpc_b,
            Some(root_b),
            "10.101.1.0/25",
        )));
    let release_first = async {
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
        first_create.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let (second_result, release_result) = tokio::join!(second_create, release_first);
    release_result?;
    let error = second_result.expect_err("the nested overlap should be rejected after reread");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("not eligible for reuse"));
    assert!(!error.message().contains("10.101.1.0/24"));
    assert_eq!(stored_vpc_prefix_count(&env, first_id).await, 1);
    assert_eq!(stored_vpc_prefix_count(&env, second_id).await, 0);
    Ok(())
}

/// Test-specific function that checks VpcPrefix and NetworkSegment writes in both commit orders.
#[crate::sqlx_test]
async fn vpc_prefix_and_attached_segment_recheck_both_commit_orders(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, tenant_prefix_overlap_overrides(false)).await;
    let tenant_a = "serialized-cross-table-a";
    let tenant_b = "serialized-cross-table-b";
    create_overlap_tenant(&env, tenant_a).await?;
    create_overlap_tenant(&env, tenant_b).await?;
    let vpc_a =
        create_fnn_vpc_for_tenant(&env, tenant_a, "cross-table VPC A", Some("OVERLAP")).await;
    let vpc_b =
        create_fnn_vpc_for_tenant(&env, tenant_b, "cross-table VPC B", Some("OVERLAP")).await;
    let root_a = seed_tenant_managed_site_prefix(
        &env,
        tenant_a,
        "10.0.0.0/8",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let root_b = seed_tenant_managed_site_prefix(
        &env,
        tenant_b,
        "10.0.0.0/8",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    // A committed VPC prefix must be visible when an attached segment resumes.
    let (prefix_create, prefix_blocker_pid) =
        hold_vpc_prefix_create(&env, VpcPrefixId::new(), vpc_a, root_a, "10.102.1.0/24").await?;
    let rejected_segment_id = NetworkSegmentId::new();
    let segment_create = env
        .api
        .create_network_segment(Request::new(attached_segment_request(
            rejected_segment_id,
            vpc_b,
            "10.102.1.0/24",
            "10.102.1.1",
            rpc::forge::NetworkSegmentType::Tenant,
        )));
    let release_prefix = async {
        wait_for_blocked_query(
            &env.pool,
            prefix_blocker_pid,
            "tenant_prefix_overlap:checks",
        )
        .await;
        prefix_create.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (segment_result, release_result) = tokio::join!(segment_create, release_prefix);
    release_result?;
    let error = segment_result.expect_err("the attached segment overlap should be rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("not eligible for reuse"));
    let segment_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM network_segments WHERE id = $1")
            .bind(rejected_segment_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(segment_count, 0);

    // A committed attached segment must be visible when a VPC prefix resumes.
    let (segment_create, segment_blocker_pid) =
        hold_attached_segment_create(&env, vpc_a, "10.103.1.0/24").await?;
    let rejected_prefix_id = VpcPrefixId::new();
    let prefix_create = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            rejected_prefix_id,
            vpc_b,
            Some(root_b),
            "10.103.1.0/24",
        )));
    let release_segment = async {
        wait_for_blocked_query(
            &env.pool,
            segment_blocker_pid,
            "tenant_prefix_overlap:checks",
        )
        .await;
        segment_create.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (prefix_result, release_result) = tokio::join!(prefix_create, release_segment);
    release_result?;
    let error = prefix_result.expect_err("the VPC prefix overlap should be rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("not eligible for reuse"));
    assert_eq!(stored_vpc_prefix_count(&env, rejected_prefix_id).await, 0);

    Ok(())
}

/// Test-specific function that checks unattached create skips the lock and attach rereads.
#[crate::sqlx_test]
async fn unattached_segment_bypasses_overlap_lock_and_attach_rechecks(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, tenant_prefix_overlap_overrides(false)).await;
    let tenant = "serialized-segment-attach";
    create_overlap_tenant(&env, tenant).await?;
    let prefix_vpc = create_fnn_vpc_for_tenant(&env, tenant, "prefix VPC", Some("OVERLAP")).await;
    let (attach_vpc, _) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "segment attach VPC".to_string(),
        Some(tenant.to_string()),
    )
    .await;
    let root = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.104.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let (prefix_create, blocker_pid) =
        hold_vpc_prefix_create(&env, VpcPrefixId::new(), prefix_vpc, root, "10.104.1.0/24").await?;

    let segment_id = NetworkSegmentId::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        env.api
            .create_network_segment(Request::new(unattached_host_inband_segment_request(
                segment_id,
                "10.104.1.0/24",
                "10.104.1.1",
            ))),
    )
    .await??;

    let attach = env.api.attach_network_segment_to_vpc(Request::new(
        rpc::forge::AttachNetworkSegmentToVpcRequest {
            network_segment_id: Some(segment_id),
            vpc_id: Some(attach_vpc),
            allow_replace: false,
        },
    ));
    let release_prefix = async {
        wait_for_blocked_query(&env.pool, blocker_pid, "tenant_prefix_overlap:checks").await;
        prefix_create.commit().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (attach_result, release_result) = tokio::join!(attach, release_prefix);
    release_result?;
    let error = attach_result.expect_err("the overlapping attachment should be rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("not eligible for reuse"));

    let attached_vpc: Option<VpcId> =
        sqlx::query_scalar("SELECT vpc_id FROM network_segments WHERE id = $1")
            .bind(segment_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(attached_vpc, None);
    Ok(())
}

/// Test-specific function that checks soft-deleted Admin prefixes still block
/// `VpcPrefix` creation.
#[crate::sqlx_test]
async fn vpc_prefix_create_rejects_soft_deleted_admin_segment_prefix(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(pool, tenant_prefix_overlap_overrides(false)).await;
    let tenant = "soft-deleted-admin-segment";
    create_overlap_tenant(&env, tenant).await?;
    let vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "soft-delete VPC", Some("OVERLAP")).await;
    let root_id = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.105.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    let segment_id = NetworkSegmentId::new();
    env.api
        .create_network_segment(Request::new(attached_segment_request(
            segment_id,
            vpc_id,
            "10.105.1.0/24",
            "10.105.1.1",
            rpc::forge::NetworkSegmentType::Admin,
        )))
        .await?;
    env.api
        .delete_network_segment(Request::new(rpc::forge::NetworkSegmentDeletionRequest {
            id: Some(segment_id),
        }))
        .await?;

    let segment_is_deleted: bool =
        sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id = $1")
            .bind(segment_id)
            .fetch_one(&env.pool)
            .await?;
    assert!(segment_is_deleted);

    let prefix_id = VpcPrefixId::new();
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            prefix_id,
            vpc_id,
            Some(root_id),
            "10.105.1.0/24",
        )))
        .await
        .expect_err("the soft-deleted Admin prefix should remain reserved");

    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("not eligible for reuse"));
    assert_eq!(stored_vpc_prefix_count(&env, prefix_id).await, 0);
    Ok(())
}

#[derive(serde::Deserialize)]
struct LifecycleStateJson {
    state: String,
}

/// Builds and submits the common structured create request used by lifecycle tests.
async fn create_vpc_prefix(env: &TestEnv, vpc_id: VpcId, prefix: &str) -> rpc::forge::VpcPrefix {
    // Send a normal create request so handler validation and response conversion are covered.
    env.api
        .create_vpc_prefix(Request::new(VpcPrefixCreationRequest {
            id: None,
            prefix: String::new(),
            vpc_id: Some(vpc_id),
            site_prefix_id: None,
            config: Some(rpc::forge::VpcPrefixConfig {
                prefix: prefix.into(),
            }),
            metadata: Some(rpc::forge::Metadata {
                name: format!("VPC prefix {prefix}"),
                description: String::from("lifecycle test prefix"),
                labels: vec![rpc::forge::Label {
                    key: "test".into(),
                    value: Some("vpc-prefix-lifecycle".into()),
                }],
            }),
        }))
        .await
        .expect("Could not create VPC prefix")
        .into_inner()
}

/// Returns one VPC prefix from the default get API, if it is visible.
async fn find_vpc_prefix(env: &TestEnv, id: VpcPrefixId) -> Option<rpc::forge::VpcPrefix> {
    // Use the default API path because soft-deleted prefixes should be omitted from normal reads.
    let mut prefixes = env
        .api
        .get_vpc_prefixes(Request::new(rpc::forge::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![id],
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
        }))
        .await
        .expect("Could not find VPC prefixes")
        .into_inner()
        .vpc_prefixes;
    assert!(
        prefixes.len() <= 1,
        "a single VPC prefix ID should return at most one object"
    );
    prefixes.pop()
}

/// Asserts lifecycle fields and family-aware VPC-prefix utilization counters.
fn assert_status_with_lifecycle_and_linknet_counters(
    prefix: &rpc::forge::VpcPrefix,
    expected_state: &str,
    expected_total_linknets: u64,
    expected_available_linknets: u64,
) {
    let status = prefix
        .status
        .as_ref()
        .expect("VPC prefix status should be populated");

    // Verify lifecycle status is present and points at the expected controller state.
    let lifecycle_status = status
        .lifecycle
        .as_ref()
        .expect("VPC prefix status should include lifecycle");
    let lifecycle_state: LifecycleStateJson =
        serde_json::from_str(&lifecycle_status.state).expect("lifecycle state should be JSON");
    assert_eq!(lifecycle_state.state, expected_state);
    assert!(
        !lifecycle_status.version.is_empty(),
        "lifecycle version should be populated"
    );
    assert!(
        lifecycle_status.sla.is_some(),
        "lifecycle SLA should be populated"
    );

    // Verify the existing utilization counters are still present alongside lifecycle.
    assert_eq!(status.total_linknet_segments, expected_total_linknets);
    assert_eq!(
        status.available_linknet_segments,
        expected_available_linknets
    );
}

/// Returns the number of backing VPC-prefix rows for an ID, including soft-deleted rows.
async fn stored_vpc_prefix_count(env: &TestEnv, id: VpcPrefixId) -> i64 {
    // Query the backing table directly because public get intentionally hides deleted prefixes.
    sqlx::query_scalar("SELECT COUNT(*) FROM network_vpc_prefixes WHERE id = $1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .expect("Could not count stored VPC prefixes")
}

/// Returns whether a backing VPC-prefix row is marked deleted.
async fn stored_vpc_prefix_is_deleted(env: &TestEnv, id: VpcPrefixId) -> bool {
    // Inspect the soft-delete marker that the public get API omits.
    sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_vpc_prefixes WHERE id = $1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .expect("Could not read VPC prefix deleted marker")
}

/// Returns the stored top-level controller state for a VPC prefix row.
async fn stored_vpc_prefix_controller_state(env: &TestEnv, id: VpcPrefixId) -> Option<String> {
    // Inspect controller-owned JSON because public get hides deleted rows.
    sqlx::query_scalar("SELECT controller_state->>'state' FROM network_vpc_prefixes WHERE id = $1")
        .bind(id)
        .fetch_optional(&env.pool)
        .await
        .expect("Could not read stored VPC prefix controller state")
        .flatten()
}

/// Returns the number of network_prefix rows still referencing a VPC prefix.
async fn network_prefix_ref_count(env: &TestEnv, id: VpcPrefixId) -> i64 {
    // Count durable prefix references, which are the controller's deletion gate.
    sqlx::query_scalar("SELECT COUNT(*) FROM network_prefixes WHERE vpc_prefix_id = $1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .expect("Could not count network prefixes for VPC prefix")
}

/// Drives the manual VPC-prefix controller until the prefix is ready.
async fn drive_vpc_prefix_to_ready(env: &TestEnv, id: VpcPrefixId) -> rpc::forge::VpcPrefix {
    // Run the controller once to process the initial Provisioning state.
    env.run_vpc_prefix_controller_iteration().await;

    // Verify the transition using the public API.
    let prefix = find_vpc_prefix(env, id)
        .await
        .expect("VPC prefix should be visible through the public get API");
    let status = prefix
        .status
        .as_ref()
        .expect("VPC prefix status should be populated");
    let lifecycle_status = status
        .lifecycle
        .as_ref()
        .expect("VPC prefix status should include lifecycle");
    let lifecycle_state: LifecycleStateJson =
        serde_json::from_str(&lifecycle_status.state).expect("lifecycle state should be JSON");
    assert_eq!(lifecycle_state.state, "ready");
    prefix
}

#[crate::sqlx_test]
async fn test_create_and_delete_vpc_prefix_deprecated_fields(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let ip_prefix = UNREFERENCED_VPC_PREFIX;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create with the deprecated flat prefix field to preserve compatibility coverage.
    let new_vpc_prefix = VpcPrefixCreationRequest {
        id: None,
        prefix: ip_prefix.into(),
        metadata: Some(Metadata {
            name: "Test VPC prefix".to_string(),
            ..Default::default()
        }),
        vpc_id: Some(vpc_id),
        ..Default::default()
    };
    let request = Request::new(new_vpc_prefix);
    let response = env.api.create_vpc_prefix(request).await;
    let vpc_prefix = response.expect("Could not create VPC prefix").into_inner();

    // Assert the create response still mirrors the requested deprecated field.
    assert_eq!(
        vpc_prefix.prefix.as_str(),
        ip_prefix,
        "The prefix after resource creation was different from what we requested"
    );

    let id = vpc_prefix
        .id
        .expect("The id field on the new VPC prefix is missing (this should be impossible)");

    // Verify persistence through the public get API after checking the create response.
    let persisted = find_vpc_prefix(&env, id)
        .await
        .expect("VPC prefix should be visible through the public get API");
    assert_eq!(persisted.prefix.as_str(), ip_prefix);

    // Delete marks the prefix deleted; it should no longer be public but should still exist.
    let delete_by_id = VpcPrefixDeletionRequest { id: Some(id) };
    let request = Request::new(delete_by_id);
    let response = env.api.delete_vpc_prefix(request).await;
    let _deletion_result = response.expect("Could not delete VPC prefix").into_inner();

    // Verify public reads hide the soft-deleted prefix.
    assert!(find_vpc_prefix(&env, id).await.is_none());

    // Verify controller-backed deletion has not hard-deleted the row yet.
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(stored_vpc_prefix_is_deleted(&env, id).await);
    assert_eq!(network_prefix_ref_count(&env, id).await, 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_create_and_delete_vpc_prefix(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let ip_prefix = UNREFERENCED_VPC_PREFIX;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create with the structured config field used by current clients.
    let new_vpc_prefix = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let response = env.api.create_vpc_prefix(request).await;
    let vpc_prefix = response.expect("Could not create VPC prefix").into_inner();

    // Assert the create response mirrors the requested prefix.
    assert_eq!(
        vpc_prefix.prefix.as_str(),
        ip_prefix,
        "The prefix after resource creation was different from what we requested"
    );

    let id = vpc_prefix
        .id
        .expect("The id field on the new VPC prefix is missing (this should be impossible)");

    // Verify persistence through the public get API after checking the create response.
    let persisted = find_vpc_prefix(&env, id)
        .await
        .expect("VPC prefix should be visible through the public get API");
    assert_eq!(persisted.prefix.as_str(), ip_prefix);

    // Delete marks the prefix deleted; it should no longer be public but should still exist.
    let delete_by_id = VpcPrefixDeletionRequest { id: Some(id) };
    let request = Request::new(delete_by_id);
    let response = env.api.delete_vpc_prefix(request).await;
    let _deletion_result = response.expect("Could not delete VPC prefix").into_inner();

    // Verify public reads hide the soft-deleted prefix.
    assert!(find_vpc_prefix(&env, id).await.is_none());

    // Verify controller-backed deletion has not hard-deleted the row yet.
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(stored_vpc_prefix_is_deleted(&env, id).await);
    assert_eq!(network_prefix_ref_count(&env, id).await, 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_create_returns_initial_lifecycle_state(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create a prefix that has no existing network-prefix references.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");

    // Verify the create response reports the initial controller state and counters.
    assert_status_with_lifecycle_and_linknet_counters(&created, "provisioning", 16, 16);

    // Verify the same lifecycle state was persisted through the public get API.
    let persisted = find_vpc_prefix(&env, id)
        .await
        .expect("VPC prefix should be visible through the public get API");
    assert_status_with_lifecycle_and_linknet_counters(&persisted, "provisioning", 16, 16);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_controller_transitions_provisioning_to_ready(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create the prefix and capture the initial lifecycle version.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    let created_status = created
        .status
        .as_ref()
        .expect("VPC prefix status should be populated");
    let initial_version = created_status
        .lifecycle
        .as_ref()
        .expect("VPC prefix status should include lifecycle")
        .version
        .clone();

    // Drive the controller and verify the persisted state transitions to Ready.
    let ready = drive_vpc_prefix_to_ready(&env, id).await;
    let ready_status = ready
        .status
        .as_ref()
        .expect("VPC prefix status should be populated");
    let ready_lifecycle = ready_status
        .lifecycle
        .as_ref()
        .expect("VPC prefix status should include lifecycle");
    assert_ne!(
        ready_lifecycle.version, initial_version,
        "controller transition should increment the lifecycle version"
    );
    assert!(
        ready_lifecycle.state_reason.is_some(),
        "controller outcome should be exposed as lifecycle state_reason"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_delete_soft_deletes_with_network_prefix_refs(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create a VPC prefix covering the tenant segment fixture so a network_prefix row references it.
    let created = create_vpc_prefix(&env, vpc_id, REFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    drive_vpc_prefix_to_ready(&env, id).await;
    assert!(network_prefix_ref_count(&env, id).await > 0);

    // Request deletion while durable network-prefix references still exist.
    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect("delete should soft-delete referenced VPC prefixes");

    // Verify public active-only reads hide the soft-deleted prefix.
    assert!(find_vpc_prefix(&env, id).await.is_none());
    let active_search_ids = env
        .api
        .search_vpc_prefixes(Request::new(VpcPrefixSearchQuery {
            vpc_id: Some(vpc_id),
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
            ..Default::default()
        }))
        .await
        .expect("active-only search should succeed")
        .into_inner()
        .vpc_prefix_ids;
    assert!(
        !active_search_ids.contains(&id),
        "active-only VPC-prefix search should hide soft-deleted prefixes"
    );

    // Verify the backing row remains soft-deleted while references still exist.
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(stored_vpc_prefix_is_deleted(&env, id).await);
    assert!(network_prefix_ref_count(&env, id).await > 0);

    // Drive the controller and verify the FK-backed references keep the row alive.
    env.run_vpc_prefix_controller_iteration().await;
    env.run_vpc_prefix_controller_iteration().await;
    assert_eq!(
        stored_vpc_prefix_controller_state(&env, id)
            .await
            .as_deref(),
        Some("deleting")
    );
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(network_prefix_ref_count(&env, id).await > 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_deleted_provisioning_vpc_prefix_enters_deleting_on_first_controller_pass(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Delete the prefix before the controller has transitioned it out of Provisioning.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect("delete should mark the VPC prefix deleted");

    assert!(find_vpc_prefix(&env, id).await.is_none());
    let include_deleted_prefixes = env
        .api
        .get_vpc_prefixes(Request::new(rpc::forge::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![id],
            deleted: rpc::forge::DeletedFilter::Include as i32,
        }))
        .await
        .expect("Could not find VPC prefix including deleted")
        .into_inner()
        .vpc_prefixes;
    assert_eq!(
        include_deleted_prefixes.len(),
        1,
        "include-deleted get should return the soft-deleted VPC prefix"
    );

    // The first controller pass should enter Deleting directly rather than recording Ready.
    env.run_vpc_prefix_controller_iteration().await;
    assert_eq!(
        stored_vpc_prefix_controller_state(&env, id)
            .await
            .as_deref(),
        Some("deleting")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM vpc_prefix_state_history \
             WHERE object_id = $1 AND state->>'state' = 'ready'",
        )
        .bind(id.to_string())
        .fetch_one(&env.pool)
        .await
        .expect("Could not count ready history records"),
        0
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_deleted_vpc_prefix_cannot_allocate_new_instance_interface(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create and ready a prefix before marking it deleted.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    drive_vpc_prefix_to_ready(&env, id).await;
    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect("delete should mark the VPC prefix deleted");

    // Attempt to allocate a new instance interface from the deleted prefix.
    let managed_host = create_managed_host(&env).await;
    let allocation = env
        .api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            instance_id: None,
            machine_id: Some(managed_host.host().id.into()),
            instance_type_id: None,
            config: Some(rpc::forge::InstanceConfig {
                tenant: Some(fixture_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config_with_vpc_prefix(id)),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: None,
            allow_unhealthy_machine: false,
        }))
        .await;

    // Verify allocation rejects deleted prefixes instead of generating a new segment.
    assert!(
        allocation.is_err(),
        "deleted VPC prefixes must not be usable for new interface allocation"
    );
    assert_eq!(network_prefix_ref_count(&env, id).await, 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_final_delete_after_generated_segment_cleanup(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            network_segments_drain_period: Some(chrono::Duration::seconds(0)),
            vpc_prefixes_drain_period: Some(chrono::Duration::seconds(0)),
            ..Default::default()
        },
    )
    .await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create a ready prefix that instance allocation will use to generate a segment.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    drive_vpc_prefix_to_ready(&env, id).await;

    // Allocate an instance so the generated segment creates network-prefix references.
    let managed_host = create_managed_host(&env).await;
    let (instance, rpc_instance) = managed_host
        .instance_builer(&env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(single_interface_network_config_with_vpc_prefix(id))
        .build_and_return()
        .await;
    let generated_segment_id = rpc_instance.config().network().interfaces[0]
        .network_segment_id
        .expect("VPC-prefix allocation should assign a generated segment");
    assert!(network_prefix_ref_count(&env, id).await > 0);

    // Delete while the generated segment still references this VPC prefix.
    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect("delete should soft-delete referenced VPC prefixes");
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(stored_vpc_prefix_is_deleted(&env, id).await);
    assert!(network_prefix_ref_count(&env, id).await > 0);

    // Verify the controller will not hard-delete while FK-backed references remain.
    env.run_vpc_prefix_controller_iteration().await;
    env.run_vpc_prefix_controller_iteration().await;
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(network_prefix_ref_count(&env, id).await > 0);

    // Release the instance so generated segment cleanup removes the prefix references.
    instance.delete().await;
    assert_eq!(network_prefix_ref_count(&env, id).await, 0);

    // Run VPC-prefix cleanup to advance through DBDelete after dependencies drain.
    env.run_vpc_prefix_controller_iteration().await;
    env.run_vpc_prefix_controller_iteration().await;
    env.run_vpc_prefix_controller_iteration().await;

    // Verify the prefix is finally hard-deleted after generated segment cleanup.
    assert!(find_vpc_prefix(&env, id).await.is_none());
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM network_segments WHERE id = $1")
            .bind(generated_segment_id)
            .fetch_one(&env.pool)
            .await
            .expect("Could not count generated network segment rows"),
        0
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_repeat_vpc_prefix_delete_returns_not_found_while_soft_deleted(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create a prefix and submit the first delete request.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect("first delete should mark the VPC prefix deleted");

    // Submit the same delete again while the row is still soft-deleted.
    let err = env
        .api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest { id: Some(id) }))
        .await
        .expect_err("repeat delete should return NotFound while soft-deleted");

    // Verify repeat delete follows the active-only public API behavior.
    assert_eq!(err.code(), tonic::Code::NotFound);
    assert!(find_vpc_prefix(&env, id).await.is_none());
    assert_eq!(stored_vpc_prefix_count(&env, id).await, 1);
    assert!(stored_vpc_prefix_is_deleted(&env, id).await);

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_delete_rejects_existing_vpc_prefix(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Any VPC-prefix row, active or soft-deleted, keeps the parent VPC from being deleted.
    let created = create_vpc_prefix(&env, vpc_id, UNREFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");

    let err = env
        .api
        .delete_vpc(Request::new(VpcDeletionRequest { id: Some(vpc_id) }))
        .await
        .expect_err("VPC delete should be rejected while a VPC prefix exists");

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        find_vpc_prefix(&env, id).await.is_some(),
        "rejected parent VPC delete must not remove the VPC prefix"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_rpc_status_includes_lifecycle_and_counters(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    // Create and ready a prefix that adopts an existing tenant segment reference.
    let created = create_vpc_prefix(&env, vpc_id, REFERENCED_VPC_PREFIX).await;
    let id = created.id.expect("created VPC prefix should include an id");
    let ready = drive_vpc_prefix_to_ready(&env, id).await;

    // The adopted /24 tenant segment occupies every /31 in this /24 VPC prefix.
    assert_status_with_lifecycle_and_linknet_counters(&ready, "ready", 128, 0);
    let status = ready
        .status
        .as_ref()
        .expect("VPC prefix status should be populated");
    assert_eq!(u64::from(status.total_31_segments), 128);
    assert_eq!(u64::from(status.available_31_segments), 0);
    let lifecycle_status = status
        .lifecycle
        .as_ref()
        .expect("VPC prefix status should include lifecycle");
    assert!(
        lifecycle_status.state_reason.is_some(),
        "RPC lifecycle status should expose controller reason"
    );

    Ok(())
}

/// Verifies that large IPv6 VPC prefixes report saturated linknet counters.
///
/// A /48 contains 2^79 /127 linknets, which does not fit in the u64 RPC/status
/// fields. The DB layer should cap these display counters at u64::MAX and
/// persist that value through the public API instead of overflowing or
/// attempting to enumerate the linknets.
#[crate::sqlx_test]
async fn test_ipv6_vpc_prefix_linknet_counters_cap_large_prefix(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Extend the test fabric with IPv6 so the VPC-prefix validator accepts the /48.
    let mut site_prefixes = TEST_SITE_PREFIXES.clone();
    site_prefixes.push("2001:db8::/32".parse().unwrap());
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(site_prefixes),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    let (_, vpc) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat-large-v6".to_string(),
        Some("2829bbe3-c169-4cd9-8b2a-19a8b1618a93".to_string()),
    )
    .await;
    let vpc_id = vpc.id.expect("flat VPC should include an id");

    // Create a large IPv6 prefix whose /127 linknet count exceeds u64.
    let created = create_vpc_prefix(&env, vpc_id, "2001:db8:1000::/48").await;
    let id = created.id.expect("created VPC prefix should include an id");
    assert_status_with_lifecycle_and_linknet_counters(&created, "provisioning", u64::MAX, u64::MAX);

    // Re-read through the public API to verify the capped counters persisted.
    let persisted = find_vpc_prefix(&env, id)
        .await
        .expect("VPC prefix should be visible through the public get API");
    assert_status_with_lifecycle_and_linknet_counters(
        &persisted,
        "provisioning",
        u64::MAX,
        u64::MAX,
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_overlapping_vpc_prefixes(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    let ip_prefix = "192.0.2.128/25";
    let overlapping_ip_prefix = "192.0.2.192/26";

    let new_vpc_prefix = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Test VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let response = env.api.create_vpc_prefix(request).await;
    assert!(response.is_ok());

    let overlapping_vpc_prefix = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: overlapping_ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "Overlapping VPC prefix".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(overlapping_vpc_prefix);
    let response = env.api.create_vpc_prefix(request).await;
    assert!(response.is_err());

    Ok(())
}

#[crate::sqlx_test]
async fn test_reject_create_with_invalid_metadata(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    let ip_prefix = "192.0.2.0/24";

    let new_vpc_prefix = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: ip_prefix.into(),
        }),
        metadata: Some(rpc::forge::Metadata {
            name: "".into(),
            description: String::from("some description"),
            labels: vec![rpc::forge::Label {
                key: "example_key".into(),
                value: Some("example_value".into()),
            }],
        }),
    };
    let request = Request::new(new_vpc_prefix);
    let response = env.api.create_vpc_prefix(request).await;
    let error = response
        .expect_err("expected create create vpc prefix to fail")
        .to_string();
    assert!(
        error.contains("invalid value"),
        "error message should contain 'invalid value', but is {error}"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_invalid_vpc_prefixes(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    for (prefix, description) in [
        (
            "198.51.100.0/24",
            "This VPC prefix is not within the site prefixes",
        ),
        (
            "2001:db8::/64",
            "This IPv6 VPC prefix is also not within the site prefixes",
        ),
        (
            "192.0.2.255/25",
            "This VPC prefix is not specified in canonical form (bits after the prefix are set to 1)",
        ),
    ] {
        let bad_vpc_prefix = VpcPrefixCreationRequest {
            id: None,
            prefix: String::new(),
            vpc_id: Some(vpc_id),
            site_prefix_id: None,

            config: Some(rpc::forge::VpcPrefixConfig {
                prefix: prefix.into(),
            }),
            metadata: Some(rpc::forge::Metadata {
                name: description.into(),
                ..Default::default()
            }),
        };
        let request = Request::new(bad_vpc_prefix);
        let response = env.api.create_vpc_prefix(request).await;

        assert!(
            response.is_err(),
            "A prefix ({prefix}) with description \"{description}\" was accepted when it should have been rejected"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_vpc_prefix_search(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc_id = get_vpc_fixture_id(&env).await;

    let p1 = "192.0.2.0/25";
    let p2 = "192.0.2.128/25";
    let create_p1 = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig { prefix: p1.into() }),
        metadata: Some(rpc::forge::Metadata {
            name: "VPC prefix p1".into(),
            ..Default::default()
        }),
    };
    let create_p2 = VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: Some(vpc_id),
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig { prefix: p2.into() }),
        metadata: Some(rpc::forge::Metadata {
            name: "VPC prefix p2".into(),
            ..Default::default()
        }),
    };
    let p1_request = Request::new(create_p1);
    let p2_request = Request::new(create_p2);
    let p1_response = env.api.create_vpc_prefix(p1_request).await;
    let p2_response = env.api.create_vpc_prefix(p2_request).await;
    let p1 = p1_response
        .expect("Couldn't create a VPC prefix ({p1})")
        .into_inner();
    let p2 = p2_response
        .expect("Couldn't create a VPC prefix ({p2})")
        .into_inner();

    // Search for each prefix by exact prefix match.
    for (prefix, vpc_prefix_id) in [(p1.prefix.as_str(), p1.id), (p2.prefix.as_str(), p2.id)] {
        dbg!(&vpc_prefix_id);
        dbg!(vpc_id);
        let prefix_query = VpcPrefixSearchQuery {
            vpc_id: Some(vpc_id),
            tenant_prefix_id: None,
            name: None,
            prefix_match: Some(prefix.into()),
            prefix_match_type: Some(PrefixMatchType::PrefixExact as i32),
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
            site_prefix_id: None,
        };
        let search_request = Request::new(prefix_query);
        let search_response = env.api.search_vpc_prefixes(search_request).await;
        let vpc_prefix_id_list = search_response
            .expect("Couldn't execute VPC prefix search")
            .into_inner();
        // Each search should return a single ID matching the ID we already
        // know.
        let expected = vpc_prefix_id_list.vpc_prefix_ids.as_slice();
        let found = vpc_prefix_id.as_slice();
        assert_eq!(
            expected, found,
            "When searching for an exact prefix {prefix}, we expected a single \
            VPC prefix ID ({expected:?}) but found {found:?}",
        );
    }

    // Search for each prefix by an address it contains.
    for (prefix, vpc_prefix_id) in [
        // A bare address should be treated the same as an explicit /32.
        ("192.0.2.85", p1.id),
        ("192.0.2.170/32", p2.id),
    ] {
        dbg!(&vpc_prefix_id);
        dbg!(vpc_id);
        let prefix_query = VpcPrefixSearchQuery {
            vpc_id: Some(vpc_id),
            tenant_prefix_id: None,
            name: None,
            prefix_match: Some(prefix.into()),
            prefix_match_type: Some(PrefixMatchType::PrefixContains as i32),
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
            site_prefix_id: None,
        };
        let search_request = Request::new(prefix_query);
        let search_response = env.api.search_vpc_prefixes(search_request).await;
        let vpc_prefix_id_list = search_response
            .expect("Couldn't execute VPC prefix search")
            .into_inner();
        // Each search should return a single ID matching the ID we already
        // know.
        let expected = vpc_prefix_id_list.vpc_prefix_ids.as_slice();
        let found = vpc_prefix_id.as_slice();
        assert_eq!(
            expected, found,
            "When searching for a contained prefix {prefix}, we expected a \
            single VPC prefix ID ({expected:?}) but found {found:?}",
        );
    }

    // Search for both prefixes by searching for a network prefix containing
    // both of them.
    let prefix = "192.0.2.0/24";
    let prefix_query = VpcPrefixSearchQuery {
        vpc_id: Some(vpc_id),
        tenant_prefix_id: None,
        name: None,
        prefix_match: Some(prefix.into()),
        prefix_match_type: Some(PrefixMatchType::PrefixContainedBy as i32),
        deleted: rpc::forge::DeletedFilter::Exclude as i32,
        site_prefix_id: None,
    };
    let search_request = Request::new(prefix_query);
    let search_response = env.api.search_vpc_prefixes(search_request).await;
    let vpc_prefix_id_list = search_response
        .expect("Couldn't execute VPC prefix search")
        .into_inner();
    let returned_vpc_prefix_ids = vpc_prefix_id_list.vpc_prefix_ids;
    for expected_vpc_prefix in [&p1, &p2] {
        let expected_id = expected_vpc_prefix.id.unwrap();
        let expected_prefix = expected_vpc_prefix.prefix.as_str();
        assert!(
            returned_vpc_prefix_ids.contains(&expected_id),
            "We expected to find the VPC prefix id {expected_id} for prefix {expected_prefix} in the search results ({returned_vpc_prefix_ids:?}), but it was absent"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn exact_site_prefix_attachment_enforces_lineage_and_round_trips(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let tenant = "site-prefix-tenant-a";
    let other_tenant = "site-prefix-tenant-b";
    create_fixture_tenant(&env, tenant).await?;
    create_fixture_tenant(&env, other_tenant).await?;
    let fnn_vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "tenant-root FNN VPC", None).await;

    let ready_parent = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.40.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let vpc_prefix_id = VpcPrefixId::new();
    let created = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            vpc_prefix_id,
            fnn_vpc_id,
            Some(ready_parent),
            "10.40.1.0/24",
        )))
        .await?
        .into_inner();
    assert_eq!(created.site_prefix_id, Some(ready_parent));

    let persisted = find_vpc_prefix(&env, vpc_prefix_id)
        .await
        .expect("exact-parent VPC prefix should be readable");
    assert_eq!(persisted.site_prefix_id, Some(ready_parent));
    let ids = env
        .api
        .search_vpc_prefixes(Request::new(VpcPrefixSearchQuery {
            site_prefix_id: Some(ready_parent),
            ..Default::default()
        }))
        .await?
        .into_inner()
        .vpc_prefix_ids;
    assert_eq!(ids, vec![vpc_prefix_id]);

    let wrong_tenant_parent = seed_tenant_managed_site_prefix(
        &env,
        other_tenant,
        "10.41.0.0/16",
        SitePrefixLifecycleState::Deleting,
    )
    .await;
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            fnn_vpc_id,
            Some(wrong_tenant_parent),
            "10.41.1.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);

    let _omitted_tenant_parent = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.47.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let omitted_tenant_child_id = VpcPrefixId::new();
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            omitted_tenant_child_id,
            fnn_vpc_id,
            None,
            "10.47.1.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        stored_vpc_prefix_count(&env, omitted_tenant_child_id).await,
        0
    );

    for (state, parent_prefix, child_prefix) in [
        (
            SitePrefixLifecycleState::Provisioning,
            "10.42.0.0/16",
            "10.42.1.0/24",
        ),
        (
            SitePrefixLifecycleState::Deleting,
            "10.43.0.0/16",
            "10.43.1.0/24",
        ),
        (
            SitePrefixLifecycleState::Error,
            "10.44.0.0/16",
            "10.44.1.0/24",
        ),
    ] {
        let parent = seed_tenant_managed_site_prefix(&env, tenant, parent_prefix, state).await;
        let error = env
            .api
            .create_vpc_prefix(Request::new(site_prefix_child_request(
                VpcPrefixId::new(),
                fnn_vpc_id,
                Some(parent),
                child_prefix,
            )))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{state:?}");
    }

    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            fnn_vpc_id,
            Some(ready_parent),
            "10.46.1.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);

    let non_fnn_parent = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.45.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let (non_fnn_vpc_id, _) = api_fixtures::vpc::create_vpc(
        &env,
        "tenant-root ETV VPC".to_string(),
        Some(tenant.to_string()),
        None,
    )
    .await;
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            non_fnn_vpc_id,
            Some(non_fnn_parent),
            "10.45.1.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let single_linknet_parent = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.49.0.0/31",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let single_linknet_vpc_prefix_id = VpcPrefixId::new();
    let single_linknet: IpNetwork = "10.49.0.0/31".parse()?;
    let created = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            single_linknet_vpc_prefix_id,
            fnn_vpc_id,
            Some(single_linknet_parent),
            "10.49.0.0/31",
        )))
        .await?
        .into_inner();
    assert_eq!(created.site_prefix_id, Some(single_linknet_parent));

    let allocator = PrefixAllocator::new(single_linknet_vpc_prefix_id, single_linknet, None, 31)?;
    let mut allocation = env.pool.begin().await?;
    let selected = allocator.next_free_prefix(&mut allocation).await?;
    assert_eq!(selected, single_linknet);
    allocator
        .allocate_network_segment_for_prefix(&mut allocation, fnn_vpc_id, selected)
        .await?;
    assert!(matches!(
        allocator.next_free_prefix(&mut allocation).await,
        Err(crate::CarbideError::ResourceExhausted(_))
    ));
    allocation.commit().await?;

    // An old client can omit the ID for a uniquely containing operator root.
    // That legacy operator root takes precedence even when the tenant owns an
    // overlapping root, because tenant-managed address space is explicit.
    let _overlapping_tenant_root = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.50.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let operator_root: IpNetwork = "10.50.0.0/16".parse()?;
    let mut txn = env.pool.begin().await?;
    db::site_prefix::reconcile_configured(&mut txn, &[operator_root]).await?;
    let operator_root_id: SitePrefixId =
        sqlx::query_scalar("SELECT id FROM site_prefixes WHERE prefix = $1 AND authority = $2")
            .bind(operator_root)
            .bind(SitePrefixAuthority::OperatorManaged)
            .fetch_one(&mut *txn)
            .await?;
    txn.commit().await?;
    let legacy = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            fnn_vpc_id,
            None,
            "10.50.1.0/24",
        )))
        .await?
        .into_inner();
    assert_eq!(legacy.site_prefix_id, Some(operator_root_id));

    Ok(())
}

#[crate::sqlx_test]
async fn omitted_site_prefix_id_checks_only_the_vpc_tenant(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    let tenant = "legacy-prefix-tenant-a";
    let other_tenant = "legacy-prefix-tenant-b";
    create_fixture_tenant(&env, tenant).await?;
    create_fixture_tenant(&env, other_tenant).await?;
    let vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "legacy tenant-scoped VPC", None).await;

    seed_tenant_managed_site_prefix(
        &env,
        other_tenant,
        "10.80.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    // Another tenant may own the same private address space without changing
    // this VPC's legacy feature-off behavior.
    let legacy = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            vpc_id,
            None,
            "10.80.1.0/24",
        )))
        .await?
        .into_inner();
    assert_eq!(legacy.site_prefix_id, None);

    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            VpcId::new(),
            None,
            "10.80.2.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);

    seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.80.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let error = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            vpc_id,
            None,
            "10.80.2.0/24",
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    Ok(())
}

#[crate::sqlx_test]
async fn omitted_site_prefix_id_serializes_with_tenant_root_creation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    let tenant = "legacy-prefix-creation-race";
    create_fixture_tenant(&env, tenant).await?;
    let vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "legacy creation race VPC", None).await;

    let mut site_prefix_create = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *site_prefix_create)
        .await?;
    db::site_prefix::create_tenant_managed(
        NewTenantManagedSitePrefix {
            id: SitePrefixId::new(),
            tenant_organization_id: tenant.parse()?,
            prefix: "10.81.0.0/16".parse()?,
            metadata: ModelMetadata {
                name: "concurrent tenant root".to_string(),
                ..Default::default()
            },
        },
        env.config.max_site_prefixes_per_tenant,
        &mut site_prefix_create,
    )
    .await?;

    let vpc_prefix_id = VpcPrefixId::new();
    let request = site_prefix_child_request(vpc_prefix_id, vpc_id, None, "10.81.1.0/24");
    let api = env.api.clone();
    let create = tokio::spawn(async move { api.create_vpc_prefix(Request::new(request)).await });
    wait_for_blocked_query(&env.pool, blocker_pid, "site_prefixes:tenant:").await;

    site_prefix_create.commit().await?;
    let error = create.await?.unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(stored_vpc_prefix_count(&env, vpc_prefix_id).await, 0);

    Ok(())
}

#[crate::sqlx_test]
async fn omitted_site_prefix_id_serializes_with_first_operator_root_reconciliation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(Vec::new()),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    let tenant = "legacy-operator-creation-race";
    create_fixture_tenant(&env, tenant).await?;
    let vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "legacy operator race VPC", None).await;

    let operator_root: IpNetwork = "198.19.0.0/16".parse()?;
    let mut reconciliation = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reconciliation)
        .await?;
    db::site_prefix::reconcile_configured(&mut reconciliation, &[operator_root]).await?;
    let operator_root_id: SitePrefixId =
        sqlx::query_scalar("SELECT id FROM site_prefixes WHERE prefix = $1")
            .bind(operator_root)
            .fetch_one(&mut *reconciliation)
            .await?;

    let vpc_prefix_id = VpcPrefixId::new();
    let request = site_prefix_child_request(vpc_prefix_id, vpc_id, None, "198.19.1.0/24");
    let api = env.api.clone();
    let create = tokio::spawn(async move { api.create_vpc_prefix(Request::new(request)).await });
    wait_for_blocked_query(&env.pool, blocker_pid, "pg_advisory_xact_lock_shared").await;

    reconciliation.commit().await?;
    let created = create.await??.into_inner();
    assert_eq!(created.site_prefix_id, Some(operator_root_id));
    assert_eq!(stored_vpc_prefix_count(&env, vpc_prefix_id).await, 1);

    Ok(())
}

#[crate::sqlx_test]
async fn vpc_prefix_create_rechecks_parent_after_concurrent_retirement(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let tenant = "site-prefix-retirement-race";
    create_fixture_tenant(&env, tenant).await?;
    let vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "retirement race VPC", None).await;
    let site_prefix_id = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.60.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;

    let mut retirement = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *retirement)
        .await?;
    let current = db::site_prefix::find_by_id_for_update(&mut retirement, site_prefix_id)
        .await?
        .expect("SitePrefix should exist");
    db::site_prefix::retire_tenant_managed(
        &RetireTenantManagedSitePrefix {
            id: site_prefix_id,
            tenant_organization_id: tenant.parse()?,
        },
        &current,
        &mut retirement,
    )
    .await?;

    let vpc_prefix_id = VpcPrefixId::new();
    let request =
        site_prefix_child_request(vpc_prefix_id, vpc_id, Some(site_prefix_id), "10.60.1.0/24");
    let api = env.api.clone();
    let create = tokio::spawn(async move { api.create_vpc_prefix(Request::new(request)).await });
    wait_for_blocked_query(&env.pool, blocker_pid, "site_prefixes").await;

    retirement.commit().await?;
    let error = create.await?.unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(stored_vpc_prefix_count(&env, vpc_prefix_id).await, 0);
    let lifecycle_state: SitePrefixLifecycleState =
        sqlx::query_scalar("SELECT lifecycle_state FROM site_prefixes WHERE id = $1")
            .bind(site_prefix_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(lifecycle_state, SitePrefixLifecycleState::Deleting);

    Ok(())
}

#[crate::sqlx_test]
async fn tenant_managed_lineage_blocks_virtualization_transition_and_race(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let tenant = "site-prefix-virtualization-guard";
    create_fixture_tenant(&env, tenant).await?;

    let existing_vpc_id =
        create_fnn_vpc_for_tenant(&env, tenant, "existing lineage VPC", None).await;
    let existing_parent_id = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.90.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let existing_vpc_prefix_id = env
        .api
        .create_vpc_prefix(Request::new(site_prefix_child_request(
            VpcPrefixId::new(),
            existing_vpc_id,
            Some(existing_parent_id),
            "10.90.1.0/24",
        )))
        .await?
        .into_inner()
        .id
        .expect("created VPC prefix should include an ID");

    let error = env
        .api
        .update_vpc_virtualization(Request::new(rpc::forge::VpcUpdateVirtualizationRequest {
            id: Some(existing_vpc_id),
            if_version_match: None,
            network_virtualization_type: Some(rpc::forge::VpcVirtualizationType::Flat as i32),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    env.api
        .delete_vpc_prefix(Request::new(VpcPrefixDeletionRequest {
            id: Some(existing_vpc_prefix_id),
        }))
        .await?;
    assert!(stored_vpc_prefix_is_deleted(&env, existing_vpc_prefix_id).await);

    // Soft-deleted lineage still owns address space until the controller
    // physically removes it, so it must continue to keep the VPC on FNN.
    let error = env
        .api
        .update_vpc_virtualization(Request::new(rpc::forge::VpcUpdateVirtualizationRequest {
            id: Some(existing_vpc_id),
            if_version_match: None,
            network_virtualization_type: Some(rpc::forge::VpcVirtualizationType::Flat as i32),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    let racing_vpc_id = create_fnn_vpc_for_tenant(&env, tenant, "racing lineage VPC", None).await;
    let racing_parent_id = seed_tenant_managed_site_prefix(
        &env,
        tenant,
        "10.91.0.0/16",
        SitePrefixLifecycleState::Ready,
    )
    .await;
    let mut child_create = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *child_create)
        .await?;
    let locked_vpc = db::vpc::find_by_with_lock(
        child_create.as_mut(),
        db::ObjectColumnFilter::One(db::vpc::IdColumn, &racing_vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?
    .pop()
    .expect("racing VPC should exist");
    let racing_vpc_prefix_id = VpcPrefixId::new();
    db::vpc_prefix::persist(
        NewVpcPrefix {
            id: racing_vpc_prefix_id,
            site_prefix_id: Some(racing_parent_id),
            vpc_id: racing_vpc_id,
            config: VpcPrefixConfig {
                prefix: "10.91.1.0/24".parse()?,
            },
            metadata: ModelMetadata {
                name: "racing tenant-managed lineage".to_string(),
                ..Default::default()
            },
        },
        locked_vpc.version,
        &mut child_create,
    )
    .await?;

    let api = env.api.clone();
    let update = tokio::spawn(async move {
        api.update_vpc_virtualization(Request::new(rpc::forge::VpcUpdateVirtualizationRequest {
            id: Some(racing_vpc_id),
            if_version_match: None,
            network_virtualization_type: Some(rpc::forge::VpcVirtualizationType::Flat as i32),
        }))
        .await
    });
    wait_for_blocked_query(&env.pool, blocker_pid, "vpcs").await;
    child_create.commit().await?;

    let error = update.await?.unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    let vpc = db::vpc::find_by(
        &env.pool,
        db::ObjectColumnFilter::One(db::vpc::IdColumn, &racing_vpc_id),
    )
    .await?
    .pop()
    .expect("racing VPC should remain visible");
    assert_eq!(
        vpc.config.network_virtualization_type,
        carbide_network::virtualization::VpcVirtualizationType::Fnn
    );
    assert_eq!(stored_vpc_prefix_count(&env, racing_vpc_prefix_id).await, 1);

    Ok(())
}

#[crate::sqlx_test]
async fn flat_vpc_accepts_ipv4_prefix(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    // Flat VPCs should accept IPv4 prefixes just like every other VPC type.
    let env = create_test_env(pool).await;
    let (_, vpc) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;

    let request = Request::new(VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: vpc.id,
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: "192.0.2.0/25".to_string(),
        }),
        metadata: Some(Metadata {
            name: "flat-v4".to_string(),
            ..Default::default()
        }),
    });

    let created = env
        .api
        .create_vpc_prefix(request)
        .await
        .expect("Flat VPC should accept IPv4 prefix")
        .into_inner();

    // Acceptance alone doesn't say the prefix landed on the right VPC, or landed at all.
    assert_eq!(created.vpc_id, vpc.id);
    assert_eq!(
        created.config.as_ref().expect("prefix config").prefix,
        "192.0.2.0/25"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn flat_vpc_accepts_ipv6_prefix(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    // Flat VPCs are allowed IPv6 prefixes alongside FNN -- ETV is the only
    // type that rejects IPv6 prefixes. Extend the site fabric prefixes with an
    // IPv6 range since the default test fabric is IPv4-only.
    let mut site_prefixes = TEST_SITE_PREFIXES.clone();
    site_prefixes.push("2001:db8::/32".parse().unwrap());
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(site_prefixes),
            create_network_segments: Some(false),
            ..Default::default()
        },
    )
    .await;
    let (_, vpc) = api_fixtures::vpc::create_flat_vpc(
        &env,
        "flat".to_string(),
        Some(FIXTURE_TENANT_ORG_ID.to_string()),
    )
    .await;

    let request = Request::new(VpcPrefixCreationRequest {
        id: None,
        prefix: String::new(),
        vpc_id: vpc.id,
        site_prefix_id: None,
        config: Some(rpc::forge::VpcPrefixConfig {
            prefix: "2001:db8::/64".to_string(),
        }),
        metadata: Some(Metadata {
            name: "flat-v6".to_string(),
            ..Default::default()
        }),
    });

    env.api
        .create_vpc_prefix(request)
        .await
        .expect("Flat VPC should accept IPv6 prefix");

    Ok(())
}
