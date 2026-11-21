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

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::api_fixtures::tenant::create_fixture_tenant;
use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};
use crate::tests::common::rpc_builder::VpcCreationRequest;

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
    assert!(matches!(
        mismatched.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { .. })
    ));
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
