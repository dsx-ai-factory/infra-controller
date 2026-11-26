// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Segment allocation and controller cleanup against a real local provider.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_instrument::testing::capture_logs_async;
use carbide_network_segment_controller::context::NetworkSegmentStateHandlerServices;
use carbide_network_segment_controller::handler::NetworkSegmentStateHandler;
use carbide_network_segment_controller::io::NetworkSegmentStateControllerIO;
use carbide_resource_pool::PoolDefinitionSource;
use carbide_uuid::network::NetworkSegmentId;
use db::TransactionVending as _;
use db::resource_pool::remote;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::network_segment::{
    NetworkDefinition, NetworkDefinitionSegmentType, NetworkSegmentControllerState,
    NetworkSegmentDeletionState,
};
use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolAllocation};
use model::resource_pool::common::{VLANID, VNI};
use model::resource_pool::remote::AllocationResult;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use state_controller::controller::StateController;
use tonic::{Code, Request};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

use crate::logging::stream::{LogStream, LogStreamLayer};
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};
use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};
use crate::tests::common::rpc_builder::VpcCreationRequest;
use crate::tests::wait_for_blocked_queries;

mod timing;

const SEGMENT_POOLS: [RemotePool; 2] = [
    RemotePool {
        name: VLANID,
        start: 101,
        auto_assign: true,
    },
    RemotePool {
        name: VNI,
        start: 20_001,
        auto_assign: true,
    },
];

async fn segment_fixture(
    database: PgPool,
    pools: &[RemotePool],
) -> eyre::Result<RemotePoolFixture> {
    let mut fixture =
        RemotePoolFixture::start_with_values(database, pools, &[101, 102, 20_001, 20_002], None)
            .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    Ok(fixture)
}

fn creation_request(id: NetworkSegmentId, name: &str) -> forge::NetworkSegmentCreationRequest {
    forge::NetworkSegmentCreationRequest {
        id: Some(id),
        name: name.into(),
        mtu: Some(1500),
        prefixes: vec![forge::NetworkPrefix {
            prefix: "198.51.100.0/24".into(),
            gateway: Some("198.51.100.1".into()),
            reserve_first: 1,
            ..Default::default()
        }],
        segment_type: forge::NetworkSegmentType::Admin as i32,
        ..Default::default()
    }
}

async fn receipt(
    fixture: &RemotePoolFixture,
    id: NetworkSegmentId,
    slot: &str,
) -> eyre::Result<ResourcePoolAllocation> {
    Ok(remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::NetworkSegment,
        &id.to_string(),
        slot,
    )
    .await?
    .expect("segment allocation request"))
}

async fn segment_rows(fixture: &RemotePoolFixture, id: NetworkSegmentId) -> eyre::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM network_segments WHERE id = $1")
            .bind(id)
            .fetch_one(&fixture.env.pool)
            .await?,
    )
}

fn segment_controller(
    fixture: &RemotePoolFixture,
) -> eyre::Result<StateController<NetworkSegmentStateControllerIO>> {
    Ok(StateController::builder()
        .database(
            fixture.env.pool.clone(),
            fixture.env.api.work_lock_manager_handle.clone(),
            &fixture.env.api.instrumentation,
        )
        .meter("carbide_machines", fixture.env.test_meter.meter())
        .processor_id("remote-segment-test".into())
        .services(
            NetworkSegmentStateHandlerServices {
                db_pool: fixture.env.pool.clone(),
                resource_pool_registry: Arc::new(fixture.env.api.resource_pool_registry.clone()),
            }
            .into(),
        )
        .state_handler(Arc::new(NetworkSegmentStateHandler::new(
            chrono::Duration::zero(),
            fixture.env.common_pools.ethernet.pool_vlan_id.clone(),
            fixture.env.common_pools.ethernet.pool_vni.clone(),
        )))
        .build_for_manual_iterations(fixture.cancel.clone())?)
}

async fn seed_late_address(
    fixture: &RemotePoolFixture,
    segment_id: NetworkSegmentId,
) -> eyre::Result<uuid::Uuid> {
    let machine_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
        .bind(machine_id)
        .execute(&fixture.env.pool)
        .await?;
    let instance_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO instances (id, machine_id) VALUES (gen_random_uuid(), $1) RETURNING id",
    )
    .bind(machine_id)
    .fetch_one(&fixture.env.pool)
    .await?;
    let vpc_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO vpcs (name, version) VALUES ('segment-drain-vpc', 'v1') RETURNING id",
    )
    .fetch_one(&fixture.env.pool)
    .await?;
    sqlx::query("INSERT INTO instance_addresses (instance_id, address, prefix, segment_id, vpc_id) VALUES ($1, '198.51.100.10'::inet, '198.51.100.0/24'::cidr, $2, $3)")
        .bind(instance_id).bind(segment_id).bind(vpc_id).execute(&fixture.env.pool).await?;
    Ok(instance_id)
}

async fn integrated_vni_state(
    fixture: &RemotePoolFixture,
    vni: i32,
) -> eyre::Result<ResourcePoolEntryState> {
    let state: sqlx::types::Json<ResourcePoolEntryState> =
        sqlx::query_scalar("SELECT state FROM resource_pool WHERE name = $1 AND value = $2")
            .bind(VNI)
            .bind(vni.to_string())
            .fetch_one(&fixture.env.pool)
            .await?;
    Ok(state.0)
}

fn configured_networks() -> eyre::Result<HashMap<String, NetworkDefinition>> {
    [
        ("a-completed", "198.51.100.0/24", "198.51.100.1"),
        ("z-retry", "203.0.113.0/24", "203.0.113.1"),
    ]
    .into_iter()
    .map(|(name, prefix, gateway)| {
        Ok((
            name.to_owned(),
            NetworkDefinition {
                segment_type: NetworkDefinitionSegmentType::Admin,
                prefix: prefix.parse()?,
                prefix_v6: None,
                gateway: Some(gateway.parse()?),
                dhcpv6_link_address: None,
                mtu: 1500,
                reserve_first: 1,
                allocation_strategy: Default::default(),
                infer_slaac_eui64_addresses: false,
                vpc_name: None,
            },
        ))
    })
    .collect()
}

async fn abandon_configured_vlan(
    fixture: &RemotePoolFixture,
    networks: &HashMap<String, NetworkDefinition>,
) -> eyre::Result<uuid::Uuid> {
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    {
        let abandoned =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, networks);
        tokio::pin!(abandoned);
        tokio::select! {
            result = &mut abandoned => panic!("startup ended before its saved grant: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
    }
    tokio::time::timeout(Duration::from_secs(5), pause.dropped.cancelled()).await?;
    Ok(sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1 AND state = 'pending'",
    ).bind(VLANID).fetch_one(&fixture.env.pool).await?)
}

/// Native startup preserves one transaction for all configured segments,
/// including their inventory ownership, snapshots and reverse zones.
#[crate::sqlx_test]
async fn configured_native_startup_rolls_back_the_complete_batch(
    database: PgPool,
) -> eyre::Result<()> {
    let env =
        create_test_env_with_overrides(database, TestEnvOverrides::no_network_segments()).await;
    let networks = configured_networks()?;
    let snapshot = "SELECT jsonb_build_object(
        'segments', (SELECT jsonb_agg(s ORDER BY id) FROM network_segments s),
        'prefixes', (SELECT jsonb_agg(p ORDER BY id) FROM network_prefixes p),
        'definitions', (SELECT jsonb_agg(n ORDER BY name) FROM network_def n),
        'domains', (SELECT jsonb_agg(d ORDER BY id) FROM domains d),
        'allocations', (SELECT jsonb_agg(a ORDER BY allocation_id) FROM resource_pool_allocations a),
        'inventory', (SELECT jsonb_agg(r ORDER BY name, value) FROM resource_pool r))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot).fetch_one(&env.pool).await?;
    let zone =
        db::dns::cidr_to_reverse_zone(networks["z-retry"].prefix).expect("aligned fixture prefix");
    let constraint = format!(
        "ALTER TABLE domains ADD CONSTRAINT test_native_batch_dns_failure CHECK (name <> '{zone}')"
    );
    sqlx::query(sqlx::AssertSqlSafe(constraint.as_str()))
        .execute(&env.pool)
        .await?;
    let error = crate::db_init::create_initial_networks(&env.api, &env.pool, &networks)
        .await
        .expect_err("late DNS failure must reject the whole native batch");
    assert!(
        format!("{error:?}").contains("test_native_batch_dns_failure"),
        "{error:?}"
    );
    let after: serde_json::Value = sqlx::query_scalar(snapshot).fetch_one(&env.pool).await?;
    assert_eq!(after, before);
    Ok(())
}

/// A conflicting Underlay must not commit ahead of the static-assignment anchor
/// and leave restart unable to repair the reserved prefix.
#[crate::sqlx_test]
async fn configured_underlay_conflict_does_not_poison_provider_startup(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS[..1]).await?;
    let mut networks = configured_networks()?;
    let mut underlay = networks.remove("a-completed").expect("first definition");
    underlay.segment_type = NetworkDefinitionSegmentType::Underlay;
    underlay.prefix = "100::/64".parse()?;
    underlay.gateway = None;
    networks.insert("a-underlay".into(), underlay);
    crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks)
        .await
        .expect_err("configured prefix overlaps the reserved static assignment");
    assert!(!db::network_segment::segment_exists(&fixture.env.pool, "a-underlay").await?);
    assert_eq!(
        db::network_segment::stored_def(&fixture.env.pool, "a-underlay").await?,
        None
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 0);

    networks
        .get_mut("a-underlay")
        .expect("Underlay definition")
        .prefix = "2001:db8:100::/64".parse()?;
    crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks).await?;
    let anchor = db::network_segment::static_assignments(&fixture.env.pool).await?;
    assert!(
        anchor
            .prefixes
            .iter()
            .any(|prefix| prefix.prefix.to_string() == "100::/128")
    );
    let underlay = db::network_segment::find_by_name(&fixture.env.pool, "a-underlay").await?;
    assert_eq!(underlay.prefixes[0].prefix, networks["a-underlay"].prefix);
    assert_eq!(underlay.status.vlan_id, None);
    assert_eq!(underlay.status.vni, None);
    assert_eq!(
        db::network_segment::all_stored_defs(&fixture.env.pool).await?,
        networks
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    fixture.stop().await
}

async fn check_configured_provider_progress(
    database: PgPool,
    external_pool: &'static str,
    slot: &'static str,
    first_value: i64,
) -> eyre::Result<()> {
    let mut fixture = segment_fixture(
        database,
        &[RemotePool {
            name: external_pool,
            start: first_value,
            auto_assign: true,
        }],
    )
    .await?;
    let mut networks = configured_networks()?;
    create_fixture_tenant(&fixture.env, FIXTURE_TENANT_ORG_ID).await?;
    let vpc_name = "configured-progress-fnn";
    let vpc = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(forge::Metadata {
                    name: vpc_name.into(),
                    ..Default::default()
                })
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    networks
        .get_mut("a-completed")
        .expect("first configured segment")
        .vpc_name = Some(vpc_name.into());
    let expected_svi = Some("198.51.100.2".parse()?);
    let failed_zone =
        db::dns::cidr_to_reverse_zone(networks["z-retry"].prefix).expect("aligned fixture prefix");
    let constraint = format!(
        "ALTER TABLE domains ADD CONSTRAINT test_provider_segment_dns_failure CHECK (name <> '{failed_zone}')"
    );
    sqlx::query(sqlx::AssertSqlSafe(constraint.as_str()))
        .execute(&fixture.env.pool)
        .await?;
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let (first, attached, native_id, native, zone, domains) = {
        let creation =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks);
        tokio::pin!(creation);
        tokio::select! {
            result = &mut creation => panic!("startup ended before its first provider pause: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        pause.pause_next.store(true, Ordering::SeqCst);
        pause.resume.notify_one();
        tokio::select! {
            result = &mut creation => panic!("startup ended before its second provider pause: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        // The next segment is waiting on its provider. The prior segment's full
        // transaction must already be visible to independent database readers.
        let first = db::network_segment::find_by_name(&fixture.env.pool, "a-completed").await?;
        assert_eq!(first.config.vpc_id, vpc.id);
        assert_eq!(first.prefixes.len(), 1);
        assert_eq!(first.prefixes[0].prefix, networks["a-completed"].prefix);
        assert_eq!(first.prefixes[0].svi_ip, expected_svi);
        assert!(!db::network_segment::segment_exists(&fixture.env.pool, "z-retry").await?);
        assert_eq!(
            db::network_segment::stored_def(&fixture.env.pool, "a-completed").await?,
            Some(networks["a-completed"].clone())
        );
        assert_eq!(
            db::network_segment::stored_def(&fixture.env.pool, "z-retry").await?,
            None
        );
        let attached = receipt(&fixture, first.id, slot).await?;
        assert_eq!(attached.state, AllocationState::Attached);
        let native_pool = if external_pool == VLANID { VNI } else { VLANID };
        let native_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1 AND owner_type = $2 AND owner_id = $3 AND integrated_resource_id IS NOT NULL",
    ).bind(native_pool).bind(OwnerType::NetworkSegment.to_string()).bind("a-completed").fetch_one(&fixture.env.pool).await?;
        let native = db::resource_pool::allocation::load(&fixture.env.pool, native_id)
            .await?
            .expect("first segment's native allocation");
        assert_eq!(native.state, AllocationState::Attached);
        let zone = db::dns::cidr_to_reverse_zone(first.prefixes[0].prefix)
            .expect("aligned fixture prefix");
        let domains =
            db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &zone).await?;
        assert_eq!(domains.len(), 1);
        assert_eq!(fixture.provider.local.provisioned_count(), 2);
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
        pause.resume.notify_one();
        let error = creation
            .await
            .expect_err("the later segment's DNS write fails");
        assert!(
            format!("{error:?}").contains("test_provider_segment_dns_failure"),
            "{error:?}"
        );
        (first, attached, native_id, native, zone, domains)
    };
    assert_eq!(segment_rows(&fixture, first.id).await?, 1);
    let retained = db::network_segment::find_by_name(&fixture.env.pool, "a-completed").await?;
    assert_eq!(retained.prefixes[0].svi_ip, expected_svi);
    assert_eq!(receipt(&fixture, first.id, slot).await?, attached);
    assert!(!db::network_segment::segment_exists(&fixture.env.pool, "z-retry").await?);
    assert_eq!(
        db::network_segment::stored_def(&fixture.env.pool, "z-retry").await?,
        None
    );
    assert!(
        db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &failed_zone)
            .await?
            .is_empty()
    );
    let native_pool = if external_pool == VLANID { VNI } else { VLANID };
    let native_for_retry =
        "SELECT count(*) FROM resource_pool WHERE name = $1 AND state->>'owner' = 'z-retry'";
    let native_leaks: i64 = sqlx::query_scalar(native_for_retry)
        .bind(native_pool)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(
        native_leaks, 0,
        "the failed segment's native sibling must roll back"
    );

    let cancellations: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1 AND state = 'cancel_requested'",
    ).bind(external_pool).fetch_all(&fixture.env.pool).await?;
    assert_eq!(cancellations.len(), 1);
    fixture
        .remote(external_pool)
        .reconcile(cancellations[0], &fixture.cancel)
        .await?;
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    sqlx::query("ALTER TABLE domains DROP CONSTRAINT test_provider_segment_dns_failure")
        .execute(&fixture.env.pool)
        .await?;
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks).await?;
    let restarted = db::network_segment::find_by_name(&fixture.env.pool, "a-completed").await?;
    assert_eq!(restarted.id, first.id);
    assert_eq!(restarted.prefixes[0].id, first.prefixes[0].id);
    assert_eq!(restarted.prefixes[0].svi_ip, expected_svi);
    assert_eq!(restarted.status.vlan_id, first.status.vlan_id);
    assert_eq!(restarted.status.vni, first.status.vni);
    assert_eq!(receipt(&fixture, first.id, slot).await?, attached);
    assert_eq!(
        db::resource_pool::allocation::load(&fixture.env.pool, native_id).await?,
        Some(native)
    );
    let restarted_domains =
        db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &zone).await?;
    assert_eq!(restarted_domains.len(), 1);
    assert_eq!(restarted_domains[0].id, domains[0].id);
    assert_eq!(
        db::network_segment::all_stored_defs(&fixture.env.pool).await?,
        networks
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 2);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 3);
    let native_retried: i64 = sqlx::query_scalar(native_for_retry)
        .bind(native_pool)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(
        native_retried, 1,
        "the successful retry owns one native allocation"
    );
    fixture.stop().await
}

#[crate::sqlx_test]
async fn configured_startup_with_external_vlan_retains_completed_segments(
    database: PgPool,
) -> eyre::Result<()> {
    check_configured_provider_progress(database, VLANID, "vlan_id", 101).await
}

#[crate::sqlx_test]
async fn configured_startup_with_external_vni_retains_completed_segments(
    database: PgPool,
) -> eyre::Result<()> {
    check_configured_provider_progress(database, VNI, "vni", 20_001).await
}

/// Overlapping startups share one configured-name lease, so capacity for one
/// segment is enough and the second caller observes the first caller's commit.
#[crate::sqlx_test]
async fn configured_startups_share_one_provider_reservation(database: PgPool) -> eyre::Result<()> {
    let mut fixture =
        RemotePoolFixture::start_with_values(database, &SEGMENT_POOLS[..1], &[101], None).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let mut networks = configured_networks()?;
    networks.remove("z-retry");
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    {
        let first =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks);
        tokio::pin!(first);
        tokio::select! {
            result = &mut first => panic!("first startup ended before provider pause: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        // Hold the lease row briefly so PostgreSQL can show the second caller
        // attempting acquisition while the first still owns the provider grant.
        let mut blocker = fixture.env.pool.txn_begin().await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let held_key: String = sqlx::query_scalar(
            "SELECT work_key FROM work_locks WHERE work_key = 'initial-network-segment:a-completed' FOR UPDATE",
        ).fetch_one(&mut *blocker).await?;
        assert_eq!(held_key, "initial-network-segment:a-completed");
        let second =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks);
        tokio::pin!(second);
        tokio::select! {
            result = &mut first => panic!("first startup escaped its provider pause: {result:?}"),
            result = &mut second => panic!("second startup did not wait for the configured name: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), wait_for_blocked_queries(&fixture.env.pool, blocker_pid, "INSERT INTO work_locks", 1)) => { result?; },
        }
        assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        assert!(!db::network_segment::segment_exists(&fixture.env.pool, "a-completed").await?);
        blocker.commit().await?;
        pause.resume.notify_one();
        let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(first, second)
        })
        .await?;
        first?;
        second?;
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM network_segments WHERE name = 'a-completed'")
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(count, 1);
    let saved = db::network_segment::find_by_name(&fixture.env.pool, "a-completed").await?;
    assert_eq!(
        receipt(&fixture, saved.id, "vlan_id").await?.state,
        AllocationState::Attached
    );
    assert_eq!(
        db::network_segment::all_stored_defs(&fixture.env.pool).await?,
        networks
    );
    let zone =
        db::dns::cidr_to_reverse_zone(saved.prefixes[0].prefix).expect("aligned fixture prefix");
    assert_eq!(
        db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &zone)
            .await?
            .len(),
        1
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// An API writer does not take the configured-name lease. If it wins during
/// provider preparation, the seeder must cancel only its unused reservation.
#[crate::sqlx_test]
async fn configured_startup_cancels_its_grant_after_api_writer_wins(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS[..1]).await?;
    let mut networks = configured_networks()?;
    networks.remove("z-retry");
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let winner_id = NetworkSegmentId::new();
    let (losing_id, winner_vlan) = {
        let startup =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks);
        tokio::pin!(startup);
        tokio::select! {
            result = &mut startup => panic!("seeder ended before its grant: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        let losing_owner: String = sqlx::query_scalar(
            "SELECT owner_id FROM resource_pool_allocations WHERE pool_name = $1 AND state = 'pending'",
        )
        .bind(VLANID)
        .fetch_one(&fixture.env.pool)
        .await?;
        let losing_id: NetworkSegmentId = losing_owner.parse()?;

        // The public create can commit while startup waits for its provider.
        let api_creation = fixture
            .env
            .api
            .create_network_segment(Request::new(creation_request(winner_id, "a-completed")));
        let winner = tokio::time::timeout(Duration::from_secs(5), api_creation)
            .await??
            .into_inner();
        assert_eq!(winner.id, Some(winner_id));
        let winner_vlan = receipt(&fixture, winner_id, "vlan_id").await?;
        assert_eq!(winner_vlan.state, AllocationState::Attached);
        pause.resume.notify_one();
        tokio::time::timeout(Duration::from_secs(5), startup).await??;
        (losing_id, winner_vlan)
    };

    let losing_vlan = receipt(&fixture, losing_id, "vlan_id").await?;
    assert_eq!(losing_vlan.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(grant)),
        ..
    } = &losing_vlan.source
    else {
        panic!("expected the losing seeder's provider grant: {losing_vlan:?}");
    };
    assert!(fixture.provider.local.is_provisioned(&grant.allocation_id));
    fixture
        .remote(VLANID)
        .reconcile(losing_vlan.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        receipt(&fixture, losing_id, "vlan_id").await?.state,
        AllocationState::Cancelled
    );
    assert!(!fixture.provider.local.is_provisioned(&grant.allocation_id));

    // Recovery must leave the winning segment and its reservation intact.
    let configured_id: NetworkSegmentId =
        sqlx::query_scalar("SELECT segment_id FROM network_def WHERE name = 'a-completed'")
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(configured_id, winner_id);
    assert_eq!(segment_rows(&fixture, losing_id).await?, 0);
    assert_eq!(receipt(&fixture, winner_id, "vlan_id").await?, winner_vlan);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// A stopped caller can leave the last provider value in an unattached request.
/// Startup must stay alive while shared recovery expires and cancels that grant.
#[crate::sqlx_test]
async fn configured_startup_waits_for_abandoned_foreground_capacity(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture =
        RemotePoolFixture::start_with_values(database, &SEGMENT_POOLS[..1], &[101], None).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let mut networks = configured_networks()?;
    networks.remove("z-retry");
    let abandoned_id = abandon_configured_vlan(&fixture, &networks).await?;
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    assert!(!db::network_segment::segment_exists(&fixture.env.pool, "a-completed").await?);
    // Only the persisted attachment deadline moves. The next startup and shared
    // recovery still run against real provider calls and PostgreSQL state.
    sqlx::query(
        "UPDATE resource_pool_allocations SET remote_request = jsonb_set(
            remote_request, '{completion_authority,attach_before}',
            to_jsonb(clock_timestamp() - interval '1 second')) WHERE allocation_id = $1",
    )
    .bind(abandoned_id)
    .execute(&fixture.env.pool)
    .await?;
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    {
        let stream = LogStream::new(256, 64 * 1024);
        let mut lines = stream.subscribe();
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream));
        let startup =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks)
                .with_subscriber(subscriber);
        tokio::pin!(startup);
        let waiting_for_cleanup = async {
            loop {
                let line = lines.recv().await?;
                if line.message
                    == "Waiting for earlier network allocation cleanup before retrying startup"
                {
                    return Ok::<_, eyre::Report>(());
                }
            }
        };
        tokio::select! {
            biased;
            result = &mut startup => panic!("startup exited before abandoned capacity could recover: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), waiting_for_cleanup) => result??,
        }
        assert!(fixture.provider.allocations.load(Ordering::SeqCst) >= 2);
        let failed_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT allocation_id FROM resource_pool_allocations
             WHERE pool_name = $1 AND allocation_id <> $2
               AND state = 'cancel_requested' AND result->>'status' = 'failed'",
        )
        .bind(VLANID)
        .bind(abandoned_id)
        .fetch_one(&fixture.env.pool)
        .await?;
        let recovery = async {
            fixture
                .remote(VLANID)
                .reconcile(abandoned_id, &fixture.cancel)
                .await?;
            fixture
                .remote(VLANID)
                .reconcile(failed_id, &fixture.cancel)
                .await?;
            Ok::<_, eyre::Report>(())
        };
        let (recovered, created) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(recovery, startup)
        })
        .await?;
        recovered?;
        created?;
    }
    let abandoned = remote::load(&fixture.env.pool, abandoned_id)
        .await?
        .expect("abandoned request remains identifiable");
    assert_eq!(abandoned.state, AllocationState::Cancelled);
    let saved = db::network_segment::find_by_name(&fixture.env.pool, "a-completed").await?;
    assert_ne!(saved.id.to_string(), abandoned.owner_id);
    let attached = receipt(&fixture, saved.id, "vlan_id").await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, abandoned_id);
    assert_eq!(saved.status.vlan_id, Some(101));
    assert_eq!(
        db::network_segment::all_stored_defs(&fixture.env.pool).await?,
        networks
    );
    assert!(fixture.provider.allocations.load(Ordering::SeqCst) >= 3);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// Cleaning an unused VLAN cannot restore capacity in a full VNI pool. Startup
/// must return that shortage without repeatedly reserving sibling VLANs.
#[crate::sqlx_test]
async fn configured_startup_ignores_cleanup_in_another_pool(database: PgPool) -> eyre::Result<()> {
    let mut fixture =
        RemotePoolFixture::start_with_values(database, &SEGMENT_POOLS, &[101, 102, 20_001], None)
            .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let holder_id = NetworkSegmentId::new();
    let holder = {
        let selected_vni = fixture
            .env
            .api
            .resource_pool_registry
            .select(
                &mut fixture.env.api.db_reader(),
                &fixture.env.common_pools.ethernet.pool_vni,
            )
            .await?
            .expect("selected VNI pool");
        let held_vni = selected_vni
            .prepare_allocation(
                &fixture.env.pool,
                OwnerType::NetworkSegment,
                &holder_id.to_string(),
                "vni",
                None,
                &fixture.cancel,
            )
            .await?;
        let mut txn = fixture.env.pool.txn_begin().await?;
        held_vni.allocate(&mut txn).await?;
        txn.commit().await?;
        receipt(&fixture, holder_id, "vni").await?
    };
    {
        let unused_id = NetworkSegmentId::new();
        let selected_vlan = fixture
            .env
            .api
            .resource_pool_registry
            .select(
                &mut fixture.env.api.db_reader(),
                &fixture.env.common_pools.ethernet.pool_vlan_id,
            )
            .await?
            .expect("selected VLAN pool");
        let unused_vlan = selected_vlan
            .prepare_allocation(
                &fixture.env.pool,
                OwnerType::NetworkSegment,
                &unused_id.to_string(),
                "vlan_id",
                None,
                &fixture.cancel,
            )
            .await?;
        unused_vlan.cancel(&fixture.env.pool).await?;
    }

    let mut networks = configured_networks()?;
    networks.remove("z-retry");
    let error = {
        let stream = LogStream::new(256, 64 * 1024);
        let mut lines = stream.subscribe();
        let subscriber = tracing_subscriber::registry().with(LogStreamLayer::new(stream));
        let startup =
            crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks)
                .with_subscriber(subscriber);
        tokio::pin!(startup);
        let unrelated_retry = async {
            loop {
                let line = lines.recv().await?;
                if line.message
                    == "Waiting for earlier network allocation cleanup before retrying startup"
                {
                    return Ok::<_, eyre::Report>(());
                }
            }
        };
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(5), &mut startup) => result?.expect_err("VNI pool has no recoverable capacity"),
            result = unrelated_retry => { result?; panic!("VLAN cleanup must not cause VNI retries"); },
        }
    };
    assert!(error.is_pool_exhausted(), "{error:?}");
    assert_eq!(tonic::Status::from(error).code(), Code::ResourceExhausted);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 4);
    assert!(!db::network_segment::segment_exists(&fixture.env.pool, "a-completed").await?);
    let cleanup: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT allocation_id, pool_name FROM resource_pool_allocations WHERE state = 'cancel_requested'",
    ).fetch_all(&fixture.env.pool).await?;
    for (id, pool) in cleanup {
        fixture.remote(&pool).reconcile(id, &fixture.cancel).await?;
    }
    assert_eq!(receipt(&fixture, holder_id, "vni").await?, holder);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// Admission failures take precedence over provider failures, including replay
/// of an ID whose remote owner slots are already attached.
#[crate::sqlx_test]
async fn remote_segment_preflight_rejects_overlap_before_provider_work(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS).await?;
    let original_id = NetworkSegmentId::new();
    fixture
        .env
        .api
        .create_network_segment(Request::new(creation_request(
            original_id,
            "existing-segment",
        )))
        .await?;
    let vlan = receipt(&fixture, original_id, "vlan_id").await?;
    let vni = receipt(&fixture, original_id, "vni").await?;
    let before = (
        fixture.provider.allocations.load(Ordering::SeqCst),
        fixture.provider.recoveries.load(Ordering::SeqCst),
    );
    fixture
        .provider
        .allocation_unavailable
        .store(true, Ordering::SeqCst);
    for (scenario, id) in [
        ("attached ID replay", original_id),
        ("new ID with existing prefix", NetworkSegmentId::new()),
    ] {
        let error = fixture
            .env
            .api
            .create_network_segment(Request::new(creation_request(id, "overlapping-segment")))
            .await
            .expect_err("preflight must reject overlap before contacting the provider");
        assert_eq!(error.code(), Code::InvalidArgument, "{scenario}: {error}");
        assert_eq!(
            error.message(),
            "prefix overlaps with an existing one",
            "{scenario}"
        );
        assert_eq!(
            (
                fixture.provider.allocations.load(Ordering::SeqCst),
                fixture.provider.recoveries.load(Ordering::SeqCst),
            ),
            before,
            "{scenario}"
        );
        assert_eq!(receipt(&fixture, original_id, "vlan_id").await?, vlan);
        assert_eq!(receipt(&fixture, original_id, "vni").await?, vni);
    }
    fixture.stop().await
}

/// A provider can satisfy its generic integer pool while returning a reserved
/// VLAN tag. Cancel that grant before preparing the segment's VNI.
#[crate::sqlx_test]
async fn remote_segment_invalid_vlan_cancels_before_preparing_vni(
    database: PgPool,
) -> eyre::Result<()> {
    let pools = [
        RemotePool {
            name: VLANID,
            start: 4095,
            auto_assign: true,
        },
        RemotePool {
            name: VNI,
            start: 20_001,
            auto_assign: true,
        },
    ];
    let mut fixture =
        RemotePoolFixture::start_with_values(database, &pools, &[4095, 20_001], None).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let id = NetworkSegmentId::new();
    let error = fixture
        .env
        .api
        .create_network_segment(Request::new(creation_request(id, "invalid-provider-vlan")))
        .await
        .expect_err("reserved VLAN tag must not attach");
    assert_eq!(error.code(), Code::Internal);
    assert!(
        error.message().contains("VLAN provider returned `4095`"),
        "{error}"
    );
    assert_eq!(segment_rows(&fixture, id).await?, 0);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    assert_eq!(
        remote::latest_for_owner_slot(
            &fixture.env.pool,
            OwnerType::NetworkSegment,
            &id.to_string(),
            "vni"
        )
        .await?,
        None
    );
    let cancelling = receipt(&fixture, id, "vlan_id").await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &cancelling.source
    else {
        panic!("invalid VLAN must retain its original grant: {cancelling:?}");
    };
    assert_eq!(
        assignment.value,
        model::resource_pool::allocation::ResourcePoolValue::Integer(4095)
    );
    fixture
        .remote(VLANID)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let cancelled = receipt(&fixture, id, "vlan_id").await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, cancelling.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// A successful VLAN reservation cannot disappear when preparing VNI or saving
/// the segment fails. Recovery must clean both attempts without a segment row.
#[crate::sqlx_test]
async fn remote_segment_failed_creation_preserves_both_cleanup_requests(
    database: PgPool,
) -> eyre::Result<()> {
    #[derive(Clone, Copy, Debug)]
    enum Failure {
        VniRejected,
        VniReplyLost,
        ReverseZone,
    }

    let mut fixture = segment_fixture(database, &SEGMENT_POOLS).await?;
    create_fixture_tenant(&fixture.env, FIXTURE_TENANT_ORG_ID).await?;
    let vpc = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let zone =
        db::dns::cidr_to_reverse_zone("198.51.100.0/24".parse()?).expect("aligned test prefix");
    for (failure, provisioned, code) in [
        (Failure::VniRejected, 1, Code::Unavailable),
        (Failure::VniReplyLost, 2, Code::Unavailable),
        (Failure::ReverseZone, 2, Code::Internal),
    ] {
        let id = NetworkSegmentId::new();
        let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
        let pause = fixture.provider.allocate_pause.clone();
        match failure {
            Failure::VniRejected => fixture
                .provider
                .reject_allocate_call
                .store(allocations + 2, Ordering::SeqCst),
            Failure::VniReplyLost => pause.pause_next.store(true, Ordering::SeqCst),
            Failure::ReverseZone => {
                // This zone write follows segment, prefix, SVI and attachment
                // writes, so its failure must roll back the whole creation.
                let constraint = format!(
                    "ALTER TABLE domains ADD CONSTRAINT test_remote_segment_dns_failure CHECK (name <> '{zone}')"
                );
                sqlx::query(sqlx::AssertSqlSafe(constraint.as_str()))
                    .execute(&fixture.env.pool)
                    .await?;
            }
        }

        let mut request = creation_request(id, "failed-remote-segment");
        request.vpc_id = vpc.id;
        let (result, logs) = {
            let creation = fixture
                .env
                .api
                .create_network_segment(Request::new(request.clone()));
            tokio::pin!(creation);
            if matches!(failure, Failure::VniReplyLost) {
                // Rearm the pause during VLAN so the second stop is VNI. Lose
                // that reply and stop Recover from hiding the failed attempt.
                tokio::select! {
                    result = &mut creation => panic!("creation ended before VLAN pause: {result:?}"),
                    result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
                }
                pause.pause_next.store(true, Ordering::SeqCst);
                pause.resume.notify_one();
                tokio::select! {
                    result = &mut creation => panic!("creation ended before VNI pause: {result:?}"),
                    result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
                }
                fixture
                    .provider
                    .lose_allocate_reply
                    .store(true, Ordering::SeqCst);
                fixture
                    .provider
                    .allocation_unavailable
                    .store(true, Ordering::SeqCst);
                pause.resume.notify_one();
            }
            capture_logs_async(creation).await
        };
        let error = result.expect_err("failed creation must not return a usable segment");
        assert_eq!(error.code(), code, "{failure:?}: {error}");
        if matches!(failure, Failure::ReverseZone) {
            // Core logs the database cause and redacts it from the RPC response.
            assert!(logs.iter().any(|log| {
                log.field("error")
                    .is_some_and(|message| message.contains("test_remote_segment_dns_failure"))
            }));
        }
        assert_eq!(segment_rows(&fixture, id).await?, 0, "{failure:?}");
        let found = fixture
            .env
            .api
            .find_network_segments_by_ids(Request::new(forge::NetworkSegmentsByIdsRequest {
                network_segments_ids: vec![id],
                ..Default::default()
            }))
            .await?
            .into_inner();
        assert!(found.network_segments.is_empty(), "{failure:?}");
        let prefix_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM network_prefixes WHERE segment_id = $1")
                .bind(id)
                .fetch_one(&fixture.env.pool)
                .await?;
        assert_eq!(prefix_count, 0, "{failure:?}");
        assert!(
            db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &zone)
                .await?
                .is_empty(),
            "{failure:?}"
        );
        assert_eq!(
            fixture.provider.local.provisioned_count(),
            provisioned,
            "{failure:?}"
        );

        let vlan = receipt(&fixture, id, "vlan_id").await?;
        let vni = receipt(&fixture, id, "vni").await?;
        assert_ne!(vlan.allocation_id, vni.allocation_id);
        for allocation in [&vlan, &vni] {
            assert_eq!(
                allocation.state,
                AllocationState::CancelRequested,
                "{failure:?}"
            );
            assert_eq!(allocation.owner_id, id.to_string());
        }
        assert!(matches!(
            &vlan.source,
            AllocationSource::Remote {
                result: Some(AllocationResult::Allocated(_)),
                ..
            }
        ));
        assert_eq!(
            fixture.provider.allocations.load(Ordering::SeqCst),
            allocations + 2
        );

        fixture
            .provider
            .allocation_unavailable
            .store(false, Ordering::SeqCst);
        fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        for (pool, original) in [(VLANID, vlan), (VNI, vni)] {
            fixture
                .remote(pool)
                .reconcile(original.allocation_id, &fixture.cancel)
                .await?;
            let cleaned = remote::load(&fixture.env.pool, original.allocation_id)
                .await?
                .expect("cleanup receipt survives");
            assert_eq!(cleaned.state, AllocationState::Cancelled, "{failure:?}");
            assert_eq!(cleaned.source, original.source, "{failure:?}");
        }
        assert_eq!(fixture.provider.local.provisioned_count(), 0, "{failure:?}");
        if matches!(failure, Failure::ReverseZone) {
            sqlx::query("ALTER TABLE domains DROP CONSTRAINT test_remote_segment_dns_failure")
                .execute(&fixture.env.pool)
                .await?;
            fixture
                .env
                .api
                .create_network_segment(Request::new(request))
                .await?;
            let saved =
                db::network_segment::find_by_name(&fixture.env.pool, "failed-remote-segment")
                    .await?;
            assert_eq!(saved.id, id);
            assert!(saved.prefixes[0].svi_ip.is_some());
            assert_eq!(
                db::dns::domain::find_reverse_zone_by_normalized_name(&fixture.env.pool, &zone)
                    .await?
                    .len(),
                1
            );
            for slot in ["vlan_id", "vni"] {
                assert_eq!(
                    receipt(&fixture, id, slot).await?.state,
                    AllocationState::Attached
                );
            }
        }
    }
    fixture.stop().await
}

/// Configured startup can combine a remote VLAN and an Integrated VNI, and a
/// restart must retain both assignments without preparing another generation.
/// Final deletion commits both backends' release with the segment removal.
#[crate::sqlx_test]
async fn configured_segment_mixes_backends_and_retains_assignments_on_restart(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = segment_fixture(database, &SEGMENT_POOLS[..1]).await?;
    let networks = HashMap::from([(
        "configured-remote-segment".into(),
        NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix: "198.51.100.0/24".parse()?,
            prefix_v6: None,
            gateway: Some("198.51.100.1".parse()?),
            dhcpv6_link_address: None,
            mtu: 1500,
            reserve_first: 1,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        },
    )]);
    crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks).await?;
    let original =
        db::network_segment::find_by_name(&fixture.env.pool, "configured-remote-segment").await?;
    assert_eq!(original.status.vlan_id, Some(101));
    let vni = original.status.vni.expect("Integrated VNI");
    assert_eq!(original.prefixes.len(), 1);
    let remote_vlan = receipt(&fixture, original.id, "vlan_id").await?;
    assert_eq!(remote_vlan.state, AllocationState::Attached);
    let native_owner = ResourcePoolEntryState::Allocated {
        owner: original.config.name.clone(),
        owner_type: OwnerType::NetworkSegment.to_string(),
    };
    assert_eq!(integrated_vni_state(&fixture, vni).await?, native_owner);
    let native_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1 AND owner_type = $2 AND owner_id = $3 AND integrated_resource_id IS NOT NULL",
    )
    .bind(VNI)
    .bind(OwnerType::NetworkSegment.to_string())
    .bind(&original.config.name)
    .fetch_one(&fixture.env.pool)
    .await?;
    let native_vni = db::resource_pool::allocation::load(&fixture.env.pool, native_id)
        .await?
        .expect("Integrated VNI allocation");
    let snapshot_query = "SELECT segment_id, definition FROM network_def WHERE name = $1";
    let snapshot: (NetworkSegmentId, sqlx::types::Json<NetworkDefinition>) =
        sqlx::query_as(snapshot_query)
            .bind(&original.config.name)
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(snapshot.0, original.id);
    assert_eq!(&snapshot.1.0, &networks[&original.config.name]);

    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    crate::db_init::create_initial_networks(&fixture.env.api, &fixture.env.pool, &networks).await?;
    let restarted =
        db::network_segment::find_by_name(&fixture.env.pool, "configured-remote-segment").await?;
    assert_eq!(restarted.id, original.id);
    assert_eq!(restarted.status.vlan_id, original.status.vlan_id);
    assert_eq!(restarted.status.vni, original.status.vni);
    assert_eq!(
        receipt(&fixture, original.id, "vlan_id").await?,
        remote_vlan
    );
    assert_eq!(integrated_vni_state(&fixture, vni).await?, native_owner);
    assert_eq!(
        db::resource_pool::allocation::load(&fixture.env.pool, native_id).await?,
        Some(native_vni.clone())
    );
    let restarted_snapshot: (NetworkSegmentId, sqlx::types::Json<NetworkDefinition>) =
        sqlx::query_as(snapshot_query)
            .bind(&original.config.name)
            .fetch_one(&fixture.env.pool)
            .await?;
    assert_eq!(restarted_snapshot, snapshot);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);

    let mut controller = segment_controller(&fixture)?;
    fixture
        .env
        .api
        .delete_network_segment(Request::new(forge::NetworkSegmentDeletionRequest {
            id: Some(original.id),
        }))
        .await?;
    controller.run_single_iteration_ext(false).await;
    controller.run_single_iteration_ext(false).await;
    let drained =
        db::network_segment::find_by_name(&fixture.env.pool, &original.config.name).await?;
    assert_eq!(
        drained.status.controller_state.value,
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DBDelete,
        }
    );
    // This late allocation fails final_delete after both release writes. The
    // rollback must restore Integrated inventory and the remote release intent.
    let instance_id = seed_late_address(&fixture, original.id).await?;
    controller.run_single_iteration_ext(false).await;
    let blocked =
        db::network_segment::find_by_name(&fixture.env.pool, &original.config.name).await?;
    let Some(PersistentStateHandlerOutcome::Error { err, .. }) =
        &blocked.status.controller_state_outcome
    else {
        panic!("late allocation must fail final deletion: {blocked:?}");
    };
    assert!(
        err.contains("network segment can't be deleted while allocations still reference it"),
        "{err}"
    );
    assert_eq!(integrated_vni_state(&fixture, vni).await?, native_owner);
    assert_eq!(
        db::resource_pool::allocation::load(&fixture.env.pool, native_id).await?,
        Some(native_vni)
    );
    assert_eq!(
        receipt(&fixture, original.id, "vlan_id").await?,
        remote_vlan
    );
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);

    sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1")
        .bind(instance_id)
        .execute(&fixture.env.pool)
        .await?;
    controller.run_single_iteration_ext(false).await;
    assert_eq!(segment_rows(&fixture, original.id).await?, 0);
    assert_eq!(
        integrated_vni_state(&fixture, vni).await?,
        ResourcePoolEntryState::Free
    );
    assert_eq!(
        db::resource_pool::allocation::load(&fixture.env.pool, native_id).await?,
        None
    );
    assert_eq!(
        db::network_segment::stored_def(&fixture.env.pool, &original.config.name).await?,
        None
    );
    let releasing = receipt(&fixture, original.id, "vlan_id").await?;
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, remote_vlan.source);
    assert_eq!(releasing.allocation_id, remote_vlan.allocation_id);
    fixture
        .remote(VLANID)
        .reconcile(remote_vlan.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        receipt(&fixture, original.id, "vlan_id").await?.state,
        AllocationState::Released
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// The controller keeps attached allocations through IP drain, then commits
/// both release requests with deletion. Provider failure remains retryable after
/// the segment is gone and Core reconstructs its registry.
#[crate::sqlx_test]
async fn remote_segment_controller_drains_then_retries_persisted_release(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = segment_fixture(database, &SEGMENT_POOLS).await?;
    let id = NetworkSegmentId::new();
    let created = fixture
        .env
        .api
        .create_network_segment(Request::new(creation_request(
            id,
            "remote-controller-segment",
        )))
        .await?
        .into_inner();
    assert_eq!(created.id, Some(id));
    let persisted =
        db::network_segment::find_by_name(&fixture.env.pool, "remote-controller-segment").await?;
    assert_eq!(persisted.status.vlan_id, Some(101));
    assert_eq!(persisted.status.vni, Some(20_001));
    let vlan = receipt(&fixture, id, "vlan_id").await?;
    let vni = receipt(&fixture, id, "vni").await?;
    for allocation in [&vlan, &vni] {
        assert_eq!(allocation.state, AllocationState::Attached);
    }
    let mut controller = segment_controller(&fixture)?;
    controller.run_single_iteration_ext(false).await;

    fixture
        .env
        .api
        .delete_network_segment(Request::new(forge::NetworkSegmentDeletionRequest {
            id: Some(id),
        }))
        .await?;
    // The public delete rejects existing allocations. Seed a late address after
    // it accepts deletion so the controller must still enforce its own drain.
    let instance_id = seed_late_address(&fixture, id).await?;
    controller.run_single_iteration_ext(false).await;
    let draining =
        db::network_segment::find_by_name(&fixture.env.pool, "remote-controller-segment").await?;
    let NetworkSegmentControllerState::Deleting {
        deletion_state:
            NetworkSegmentDeletionState::DrainAllocatedIps {
                delete_at: first_deadline,
            },
    } = draining.status.controller_state.value
    else {
        panic!("expected persisted drain state: {draining:?}");
    };
    controller.run_single_iteration_ext(false).await;
    let blocked =
        db::network_segment::find_by_name(&fixture.env.pool, "remote-controller-segment").await?;
    let NetworkSegmentControllerState::Deleting {
        deletion_state:
            NetworkSegmentDeletionState::DrainAllocatedIps {
                delete_at: renewed_deadline,
            },
    } = blocked.status.controller_state.value
    else {
        panic!("allocated IP must retain drain state: {blocked:?}");
    };
    assert!(renewed_deadline > first_deadline);
    assert_ne!(
        blocked.status.controller_state.version,
        draining.status.controller_state.version
    );
    assert_eq!(receipt(&fixture, id, "vlan_id").await?, vlan);
    assert_eq!(receipt(&fixture, id, "vni").await?, vni);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);

    sqlx::query("DELETE FROM instance_addresses WHERE instance_id = $1")
        .bind(instance_id)
        .execute(&fixture.env.pool)
        .await?;
    controller.run_single_iteration_ext(false).await;
    let drained =
        db::network_segment::find_by_name(&fixture.env.pool, "remote-controller-segment").await?;
    assert_eq!(
        drained.status.controller_state.value,
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DBDelete
        }
    );

    // VLAN release follows VNI release. A VLAN that differs from its receipt
    // must roll back the first release intent and preserve the segment.
    sqlx::query("UPDATE network_segments SET vlan_id = 102 WHERE id = $1")
        .bind(id)
        .execute(&fixture.env.pool)
        .await?;
    controller.run_single_iteration_ext(false).await;
    assert_eq!(segment_rows(&fixture, id).await?, 1);
    let mismatched =
        db::network_segment::find_by_name(&fixture.env.pool, "remote-controller-segment").await?;
    assert_eq!(mismatched.status.vlan_id, Some(102));
    assert_eq!(
        mismatched.status.controller_state.value,
        NetworkSegmentControllerState::Deleting {
            deletion_state: NetworkSegmentDeletionState::DBDelete
        }
    );
    let Some(PersistentStateHandlerOutcome::Error { err, .. }) =
        &mismatched.status.controller_state_outcome
    else {
        panic!("mismatched receipt must persist a diagnostic: {mismatched:?}");
    };
    let expected_failure = format!(
        "resource pool `{VLANID}` has no attached vlan_id allocation matching network_segment `{id}` and value `102`"
    );
    assert!(err.contains(&expected_failure), "{err}");
    assert_eq!(receipt(&fixture, id, "vlan_id").await?, vlan);
    assert_eq!(receipt(&fixture, id, "vni").await?, vni);
    sqlx::query("UPDATE network_segments SET vlan_id = 101 WHERE id = $1")
        .bind(id)
        .execute(&fixture.env.pool)
        .await?;
    controller.run_single_iteration_ext(false).await;
    assert_eq!(segment_rows(&fixture, id).await?, 0);
    for (slot, original) in [("vlan_id", &vlan), ("vni", &vni)] {
        let releasing = receipt(&fixture, id, slot).await?;
        assert_eq!(releasing.state, AllocationState::ReleaseRequested);
        assert_eq!(releasing.source, original.source);
        assert_eq!(releasing.allocation_id, original.allocation_id);
    }
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);
    fixture
        .provider
        .release_unavailable
        .store(true, Ordering::SeqCst);
    fixture
        .remote(VLANID)
        .reconcile(vlan.allocation_id, &fixture.cancel)
        .await
        .expect_err("injected transient provider release failure");
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 1);
    assert_eq!(
        receipt(&fixture, id, "vlan_id").await?.state,
        AllocationState::ReleaseRequested
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 2);

    drop(controller);
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture
        .provider
        .release_unavailable
        .store(false, Ordering::SeqCst);
    for (pool, original) in [(VLANID, vlan), (VNI, vni)] {
        fixture
            .remote(pool)
            .reconcile(original.allocation_id, &fixture.cancel)
            .await?;
        let released = remote::load(&fixture.env.pool, original.allocation_id)
            .await?
            .expect("released receipt");
        assert_eq!(released.state, AllocationState::Released);
        assert_eq!(released.source, original.source);
    }
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(segment_rows(&fixture, id).await?, 0);
    fixture.stop().await
}
