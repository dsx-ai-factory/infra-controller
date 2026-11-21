// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Shared segment preparation time covers both provider work and registry reads.

use model::network_segment::{NewNetworkSegment, VLAN_ALLOCATION_SLOT, VNI_ALLOCATION_SLOT};
use model::resource_pool::{ResourcePool, ValueType};

use super::*;
use crate::CarbideError;
use crate::handlers::network_segment::SegmentAllocations;
use crate::test_support::builder::TestApiBuilder;

fn segment() -> eyre::Result<NewNetworkSegment> {
    Ok(NewNetworkSegment::try_from(creation_request(
        NetworkSegmentId::new(),
        "segment-preparation-deadline",
    ))?)
}

fn assert_preparation_deadline(error: CarbideError) {
    assert!(
        matches!(
            error,
            CarbideError::ResourcePoolDeadlineExceeded {
                operation: "network segment allocation preparation",
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        tonic::Status::from(error).code(),
        tonic::Code::DeadlineExceeded
    );
}

/// Both provider grants are known before the final transaction waits. Its local
/// deadline must return and preserve cleanup while PostgreSQL is still blocked.
#[crate::sqlx_test]
async fn segment_final_attachment_deadline_cancels_both_grants(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS).await?;
    let id = NetworkSegmentId::new();
    let api = fixture.env.api.clone();
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let creation = api.create_network_segment(Request::new(creation_request(
        id,
        "final-attachment-deadline",
    )));
    tokio::pin!(creation);
    tokio::select! {
        result = &mut creation => panic!("creation passed its paused VLAN: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    pause.pause_next.store(true, Ordering::SeqCst);
    pause.resume.notify_one();
    tokio::select! {
        result = &mut creation => panic!("creation passed its paused VNI: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    let mut blocker = fixture.env.pool.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut blocker).await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    pause.resume.notify_one();
    tokio::select! {
        result = &mut creation => panic!("final attachment bypassed its SQL blocker: {result:?}"),
        _ = wait_for_blocked_queries(&fixture.env.pool, blocker_pid, "tenant_prefix_overlap:checks", 1) => {},
    }
    assert_eq!(fixture.provider.local.provisioned_count(), 2);
    // Tokio's completion deadline moves; PostgreSQL's attachment authority and
    // statement clock do not. No segment write or commit has begun at this wait.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(5), creation)
        .await?
        .expect_err("final SQL must share the bounded completion phase");
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert!(
        error
            .message()
            .contains("network segment creation completion"),
        "{error}"
    );
    assert_eq!(segment_rows(&fixture, id).await?, 0);
    for slot in [VLAN_ALLOCATION_SLOT, VNI_ALLOCATION_SLOT] {
        let cancelled = receipt(&fixture, id, slot).await?;
        assert_eq!(cancelled.state, AllocationState::CancelRequested);
        assert!(matches!(
            cancelled.source,
            AllocationSource::Remote {
                result: Some(AllocationResult::Allocated(_)),
                ..
            }
        ));
    }
    blocker.rollback().await?;
    for (pool, slot) in [(VLANID, VLAN_ALLOCATION_SLOT), (VNI, VNI_ALLOCATION_SLOT)] {
        let cancelled = receipt(&fixture, id, slot).await?;
        fixture
            .remote(pool)
            .reconcile(cancelled.allocation_id, &fixture.cancel)
            .await?;
    }
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// Cleanup inspection is secondary to the shortage. A stalled probe must stop
/// within its SQL phase budget, preserve that shortage, and release the name.
#[crate::sqlx_test]
async fn configured_cleanup_probe_deadline_preserves_the_shortage(
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
    let api = fixture.env.api.clone();
    // The ordinary reply pause follows a successful grant. Gate before the
    // provider checks capacity so the exhausted Allocate can be held as well.
    let allocate_barrier = Arc::new(tokio::sync::Barrier::new(2));
    *fixture
        .provider
        .allocate_barrier
        .lock()
        .expect("Allocate barrier") = Some(allocate_barrier.clone());
    let startup =
        crate::db_init::create_initial_networks(&api, &api.database_connection, &networks);
    tokio::pin!(startup);
    let allocation_entered = async {
        while fixture.provider.allocations.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        result = &mut startup => panic!("startup ended before the exhaustion reply: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), allocation_entered) => result?,
    }
    // Gate the winner recheck first. This lets the provider failure and its
    // cancellation commit before locking the table that the cleanup probe reads.
    let mut recheck_blocker = fixture.env.pool.txn_begin().await?;
    sqlx::query("LOCK TABLE network_segments IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *recheck_blocker)
        .await?;
    let recheck_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *recheck_blocker)
        .await?;
    allocate_barrier.wait().await;
    *fixture
        .provider
        .allocate_barrier
        .lock()
        .expect("Allocate barrier") = None;
    tokio::select! {
        result = &mut startup => panic!("startup bypassed the winner recheck: {result:?}"),
        _ = wait_for_blocked_queries(&fixture.env.pool, recheck_pid, "FROM network_segments WHERE name", 1) => {},
    }
    let mut probe_blocker = fixture.env.pool.txn_begin().await?;
    sqlx::query("LOCK TABLE resource_pool_allocations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *probe_blocker)
        .await?;
    let probe_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *probe_blocker)
        .await?;
    recheck_blocker.rollback().await?;
    tokio::select! {
        result = &mut startup => panic!("startup bypassed the blocked cleanup probe: {result:?}"),
        _ = wait_for_blocked_queries(&fixture.env.pool, probe_pid, "IS DISTINCT FROM 'failed'", 1) => {},
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    // The Tokio deadline has elapsed, but PostgreSQL's five-second statement
    // clock has not. Keep its blocker held while checking caller completion.
    let error = tokio::time::timeout(Duration::from_secs(2), startup)
        .await?
        .expect_err("blocked cleanup inspection must preserve the original shortage");
    assert!(error.is_pool_exhausted(), "{error:?}");
    assert_eq!(tonic::Status::from(error).code(), Code::ResourceExhausted);
    let name_lock = api
        .work_lock_manager_handle
        .try_acquire_lock("initial-network-segment:a-completed".to_owned())
        .await?;
    name_lock.release().await?;
    probe_blocker.rollback().await?;
    let original = remote::load(&fixture.env.pool, abandoned_id)
        .await?
        .expect("abandoned grant survives");
    assert_eq!(original.state, AllocationState::Pending);
    let cancellations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_pool_allocations WHERE pool_name = $1 AND state = 'cancel_requested'",
    ).bind(VLANID).fetch_one(&fixture.env.pool).await?;
    assert_eq!(cancellations, 1);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    assert!(!db::network_segment::segment_exists(&fixture.env.pool, "a-completed").await?);
    fixture.stop().await
}

/// The provider has issued both grants, but Core has not received the VNI reply.
/// Expiration must let the framework save that ambiguous request's cancellation.
#[crate::sqlx_test]
async fn segment_pair_deadline_cancels_an_ambiguous_vni(database: PgPool) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS).await?;
    let ns = segment()?;
    let pause = fixture.provider.allocate_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let api = fixture.env.api.clone();
    let preparation =
        SegmentAllocations::prepare_with_test_timeout(&api, &ns, Duration::from_secs(12));
    tokio::pin!(preparation);
    tokio::select! {
        result = &mut preparation => panic!("preparation passed the paused VLAN: {:?}", result.err()),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    pause.pause_next.store(true, Ordering::SeqCst);
    pause.resume.notify_one();
    tokio::select! {
        result = &mut preparation => panic!("preparation passed the paused VNI: {:?}", result.err()),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    assert_eq!(fixture.provider.local.provisioned_count(), 2);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    let issued_vni = receipt(&fixture, ns.id, VNI_ALLOCATION_SLOT).await?;
    assert!(matches!(
        &issued_vni.source,
        AllocationSource::Remote { result: None, .. }
    ));
    tokio::time::pause();
    // Neither individual provider call reaches its ten-second transport limit.
    // Only the shared preparation budget has now expired.
    tokio::time::advance(Duration::from_secs(7)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(5), &mut preparation)
        .await?
        .err()
        .expect("the pair must share its remaining time");
    assert_preparation_deadline(error);
    pause.resume.notify_one();

    for (pool, slot) in [(VLANID, VLAN_ALLOCATION_SLOT), (VNI, VNI_ALLOCATION_SLOT)] {
        let cancelled = receipt(&fixture, ns.id, slot).await?;
        assert_eq!(cancelled.state, AllocationState::CancelRequested);
        if slot == VNI_ALLOCATION_SLOT {
            assert_eq!(cancelled.allocation_id, issued_vni.allocation_id);
            assert_eq!(cancelled.source, issued_vni.source);
        } else {
            assert!(matches!(
                &cancelled.source,
                AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(_)),
                    ..
                }
            ));
        }
        fixture
            .remote(pool)
            .reconcile(cancelled.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            receipt(&fixture, ns.id, slot).await?.state,
            AllocationState::Cancelled
        );
    }
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// An uncached Native sibling needs a registry query after the VLAN is ready.
/// The production budget accepts a forty-second pair and cancels a blocked read
/// when the full preparation budget is exhausted, without resetting it per slot.
#[crate::sqlx_test]
async fn segment_pair_budget_includes_registry_lookup(database: PgPool) -> eyre::Result<()> {
    let fixture = segment_fixture(database, &SEGMENT_POOLS[..1]).await?;
    let late_pool = ResourcePool::<i32>::new("late-segment-vni".into(), ValueType::Integer);
    let mut txn = fixture.env.pool.txn_begin().await?;
    db::resource_pool::populate(&late_pool, &mut txn, vec![20_001], true).await?;
    txn.commit().await?;
    assert!(
        fixture
            .env
            .api
            .resource_pool_registry
            .get(late_pool.name())
            .is_none()
    );
    let mut common = db::resource_pool::create_common_pools(
        &mut fixture.env.api.db_reader(),
        Default::default(),
    )
    .await?;
    Arc::get_mut(&mut common)
        .expect("new pool handles are unshared")
        .ethernet
        .pool_vni = Arc::new(late_pool);
    let api = TestApiBuilder::new(
        fixture.env.pool.clone(),
        common,
        fixture.env.api.work_lock_manager_handle.clone(),
    )
    .with_resource_pool_registry(fixture.env.api.resource_pool_registry.clone())
    .with_runtime_config(fixture.env.config.clone())
    .build();

    for (scenario, elapsed, expired) in [
        (
            "healthy preparation may exceed one provider budget",
            Duration::from_secs(40),
            false,
        ),
        (
            "registry wait consumes the pair budget",
            Duration::from_secs(56),
            true,
        ),
    ] {
        let ns = segment()?;
        let pause = fixture.provider.allocate_pause.clone();
        pause.pause_next.store(true, Ordering::SeqCst);
        let preparation = async {
            SegmentAllocations::prepare(&api, &ns)
                .await
                .map_err(CarbideError::from)
        };
        tokio::pin!(preparation);
        tokio::select! {
            result = &mut preparation => panic!("{scenario}: preparation passed paused VLAN: {:?}", result.err()),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        let mut blocker = fixture.env.pool.txn_begin().await?;
        sqlx::query("LOCK TABLE resource_pool_binding IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        pause.resume.notify_one();
        tokio::select! {
            result = &mut preparation => panic!("{scenario}: lookup passed its real database blocker: {:?}", result.err()),
            _ = wait_for_blocked_queries(&fixture.env.pool, blocker_pid, "resource_pool_binding", 1) => {},
        }
        tokio::time::pause();
        tokio::time::advance(elapsed).await;
        tokio::time::resume();
        if expired {
            // Keep PostgreSQL blocked: cancellation must not await the lookup's completion.
            let error = tokio::time::timeout(Duration::from_secs(5), &mut preparation)
                .await?
                .err()
                .expect("registry lookup must observe preparation cancellation");
            assert_preparation_deadline(error);
            blocker.rollback().await?;
        } else {
            blocker.rollback().await?;
            let allocations =
                tokio::time::timeout(Duration::from_secs(5), &mut preparation).await??;
            allocations.cancel(&api).await;
        }
        let cancelled = receipt(&fixture, ns.id, VLAN_ALLOCATION_SLOT).await?;
        assert_eq!(
            cancelled.state,
            AllocationState::CancelRequested,
            "{scenario}"
        );
        assert!(
            remote::latest_for_owner_slot(
                &fixture.env.pool,
                OwnerType::NetworkSegment,
                &ns.id.to_string(),
                VNI_ALLOCATION_SLOT
            )
            .await?
            .is_none(),
            "{scenario}"
        );
        fixture
            .remote(VLANID)
            .reconcile(cancelled.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(fixture.provider.local.provisioned_count(), 0, "{scenario}");
    }
    fixture.stop().await
}
