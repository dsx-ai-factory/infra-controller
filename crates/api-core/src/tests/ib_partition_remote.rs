// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! InfiniBand PKey preparation and controller cleanup against a remote pool.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_ib_fabric::ib::{IBFabricManagerType, create_ib_fabric_manager};
use carbide_ib_partition_controller::context::IBPartitionStateHandlerServices;
use carbide_ib_partition_controller::handler::IBPartitionStateHandler;
use carbide_ib_partition_controller::io::IBPartitionStateControllerIO;
use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
use carbide_resource_pool::PoolDefinitionSource;
use carbide_resource_pool_api::v1;
use carbide_uuid::infiniband::IBPartitionId;
use db::resource_pool::remote;
use db::{ObjectColumnFilter, TransactionVending as _};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::ib_partition::{IBPartitionControllerState, PKEY_ALLOCATION_SLOT};
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::remote::{AllocationFailureCode, AllocationResult, AllocationSelection};
use model::resource_pool::{OwnerType, Range};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use state_controller::controller::StateController;
use tonic::{Code, Request, Response, Status};

use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};

const PKEY_POOL: &str = "ib_fabrics.default.pkey";
const TENANT: &str = "remote-ib-tenant";

/// `pkey_fixture` moves the default fabric's PKeys to the provider and limits
/// each tenant to one partition, so rejection needs only one existing row.
async fn pkey_fixture(database: PgPool) -> eyre::Result<RemotePoolFixture> {
    let mut fixture = RemotePoolFixture::start_with_values(
        database,
        &[RemotePool {
            name: PKEY_POOL,
            start: 2000,
            auto_assign: true,
        }],
        &[2000, 2001],
        None,
    )
    .await?;
    initialize_ib_fixture(&mut fixture).await?;
    Ok(fixture)
}

async fn initialize_ib_fixture(fixture: &mut RemotePoolFixture) -> eyre::Result<()> {
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let mut fabric_config = fixture.env.api.ib_fabric_manager.get_config();
    fabric_config.manager_type = IBFabricManagerType::Mock;
    fabric_config.max_partition_per_tenant = 1;
    let fabric_manager = Arc::new(create_ib_fabric_manager(
        fixture.env.api.credential_manager.clone(),
        fabric_config,
    )?);
    fixture.env.api = Arc::new(
        TestApiBuilder::new(
            fixture.env.pool.clone(),
            fixture.env.common_pools.clone(),
            fixture.env.api.work_lock_manager_handle.clone(),
        )
        .with_resource_pool_registry(fixture.env.api.resource_pool_registry.clone())
        .with_runtime_config(fixture.env.config.clone())
        .with_ib_fabric_manager(fabric_manager)
        .build(),
    );
    Ok(())
}

async fn create_partition(
    fixture: &RemotePoolFixture,
    id: IBPartitionId,
    name: &str,
    requested_pkey: Option<&str>,
) -> Result<forge::IbPartition, Status> {
    fixture
        .env
        .api
        .create_ib_partition(Request::new(forge::IbPartitionCreationRequest {
            id: Some(id),
            config: Some(forge::IbPartitionConfig {
                name: name.into(),
                tenant_organization_id: TENANT.into(),
                pkey: requested_pkey.map(str::to_owned),
            }),
            metadata: Some(forge::Metadata {
                name: name.into(),
                ..Default::default()
            }),
        }))
        .await
        .map(Response::into_inner)
}

async fn receipt(
    fixture: &RemotePoolFixture,
    id: IBPartitionId,
) -> eyre::Result<ResourcePoolAllocation> {
    Ok(remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::IBPartition,
        &id.to_string(),
        PKEY_ALLOCATION_SLOT,
    )
    .await?
    .expect("IB partition PKey request"))
}

async fn partition_rows(fixture: &RemotePoolFixture, id: IBPartitionId) -> eyre::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM ib_partitions WHERE id = $1")
            .bind(id)
            .fetch_one(&fixture.env.pool)
            .await?,
    )
}

async fn assert_unused_pkey_recovered(
    fixture: &RemotePoolFixture,
    id: IBPartitionId,
) -> eyre::Result<()> {
    assert_eq!(partition_rows(fixture, id).await?, 0);
    let cancelling = receipt(fixture, id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &cancelling.source
    else {
        panic!("failed creation must retain its assignment: {cancelling:?}");
    };
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    fixture
        .remote(PKEY_POOL)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let cancelled = receipt(fixture, id).await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, cancelling.source);
    assert!(
        !fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    Ok(())
}

async fn assert_remote_exhausted(
    fixture: &RemotePoolFixture,
    scenario: &str,
    requested: &str,
) -> eyre::Result<()> {
    let id = IBPartitionId::new();
    let provisioned = fixture.provider.local.provisioned_count();
    let error = create_partition(fixture, id, scenario, Some(requested))
        .await
        .expect_err(scenario);
    assert_eq!(error.code(), Code::ResourceExhausted, "{scenario}: {error}");
    assert_eq!(partition_rows(fixture, id).await?, 0);
    let rejected = receipt(fixture, id).await?;
    assert_eq!(rejected.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Failed(failure)),
        ..
    } = &rejected.source
    else {
        panic!("{scenario}: provider must retain its rejected request");
    };
    assert_eq!(failure.code, AllocationFailureCode::ResourceExhausted);
    fixture
        .remote(PKEY_POOL)
        .reconcile(rejected.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        receipt(fixture, id).await?.state,
        AllocationState::Cancelled
    );
    assert_eq!(fixture.provider.local.provisioned_count(), provisioned);
    Ok(())
}

#[crate::sqlx_test]
async fn remote_ib_partition_failed_completion_recovers_unused_pkey(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = pkey_fixture(database).await?;
    let api_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(fixture.env.pool.connect_options().as_ref().clone())
        .await?;
    let api_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&api_pool)
        .await?;
    Arc::get_mut(&mut fixture.env.api)
        .expect("unshared rebuilt API")
        .database_connection = api_pool;
    let invalid_id = IBPartitionId::new();
    let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
    let error = create_partition(&fixture, invalid_id, "", None)
        .await
        .expect_err("invalid metadata must precede provider allocation");
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(error.message().contains("Invalid metadata for IBPartition"));
    assert_eq!(
        fixture.provider.allocations.load(Ordering::SeqCst),
        allocations
    );
    assert_eq!(partition_rows(&fixture, invalid_id).await?, 0);
    assert!(
        remote::latest_for_owner_slot(
            &fixture.env.pool,
            OwnerType::IBPartition,
            &invalid_id.to_string(),
            PKEY_ALLOCATION_SLOT
        )
        .await?
        .is_none()
    );
    assert_remote_exhausted(&fixture, "automatic-only pool", "07d0").await?;
    #[derive(Clone, Copy)]
    enum CompletionFailure {
        Commit,
        LatePartitionLimit,
    }
    for failure in [
        CompletionFailure::Commit,
        CompletionFailure::LatePartitionLimit,
    ] {
        let (scenario, expected_code, expected_message) = match failure {
            CompletionFailure::Commit => {
                ("commit failure", Code::Internal, "internal database error")
            }
            CompletionFailure::LatePartitionLimit => (
                "partition limit",
                Code::InvalidArgument,
                "maximum limit of infiniband partitions had been reached",
            ),
        };
        if matches!(failure, CompletionFailure::Commit) {
            // The INSERT and receipt attachment succeed. The missing referenced UUID
            // rejects the transaction only when Core tries to commit it.
            for query in [
                "CREATE TABLE test_allowed_partition_ids (id uuid PRIMARY KEY)",
                "ALTER TABLE ib_partitions ADD CONSTRAINT reject_remote_partition_commit
                 FOREIGN KEY (id) REFERENCES test_allowed_partition_ids (id)
                 DEFERRABLE INITIALLY DEFERRED NOT VALID",
            ] {
                sqlx::query(query).execute(&fixture.env.pool).await?;
            }
        }
        let id = IBPartitionId::new();
        let pause = fixture.provider.allocate_pause.clone();
        pause.pause_next.store(
            matches!(failure, CompletionFailure::LatePartitionLimit),
            Ordering::SeqCst,
        );
        let creation = capture_logs_async(create_partition(&fixture, id, "failed-partition", None));
        tokio::pin!(creation);
        let existing = if matches!(failure, CompletionFailure::LatePartitionLimit) {
            tokio::select! {
                result = &mut creation => panic!("creation passed the paused provider: {result:?}"),
                reached = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => reached?,
            }
            let in_transaction: bool = sqlx::query_scalar(
                "SELECT xact_start IS NOT NULL FROM pg_stat_activity WHERE pid = $1",
            )
            .bind(api_pid)
            .fetch_one(&fixture.env.pool)
            .await?;
            assert!(
                !in_transaction,
                "provider wait must not hold the partition transaction"
            );
            let sibling_id = IBPartitionId::new();
            let sibling = tokio::time::timeout(
                Duration::from_secs(5),
                create_partition(&fixture, sibling_id, "existing partition", None),
            )
            .await??;
            let sibling_receipt = receipt(&fixture, sibling_id).await?;
            pause.resume.notify_one();
            Some((sibling_id, sibling, sibling_receipt))
        } else {
            None
        };
        let (result, logs) = tokio::time::timeout(Duration::from_secs(5), &mut creation).await?;
        let error = result.expect_err(scenario);
        assert_eq!(error.code(), expected_code, "{scenario}: {error}");
        assert_eq!(error.message(), expected_message, "{scenario}");
        if matches!(failure, CompletionFailure::Commit) {
            assert!(
                logs.iter()
                    .any(|log| log.message == "database error conversion"
                        && log
                            .field("error")
                            .is_some_and(|error| error.contains("reject_remote_partition_commit"))),
                "commit failure must identify the injected deferred constraint: {logs:?}"
            );
        }
        assert_eq!(partition_rows(&fixture, id).await?, 0, "{scenario}");
        assert_unused_pkey_recovered(&fixture, id).await?;
        if let Some((existing_id, sibling, existing)) = existing {
            assert_eq!(receipt(&fixture, existing_id).await?, existing);
            let found = fixture
                .env
                .api
                .find_ib_partitions_by_ids(Request::new(forge::IbPartitionsByIdsRequest {
                    ib_partition_ids: vec![existing_id],
                    include_history: false,
                }))
                .await?
                .into_inner();
            assert_eq!(found.ib_partitions, vec![sibling]);
            let AllocationSource::Remote {
                result: Some(AllocationResult::Allocated(assignment)),
                ..
            } = &existing.source
            else {
                panic!("sibling must keep its assigned PKey");
            };
            assert!(
                fixture
                    .provider
                    .local
                    .is_provisioned(&assignment.allocation_id)
            );
        } else {
            for query in [
                "ALTER TABLE ib_partitions DROP CONSTRAINT reject_remote_partition_commit",
                "DROP TABLE test_allowed_partition_ids",
            ] {
                sqlx::query(query).execute(&fixture.env.pool).await?;
            }
        }
    }
    fixture.stop().await
}

#[crate::sqlx_test]
async fn remote_ib_partition_rename_preserves_release_handle_across_retry(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = pkey_fixture(database).await?;
    let id = IBPartitionId::new();
    let created = create_partition(&fixture, id, "original partition", None).await?;
    let attached = receipt(&fixture, id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_eq!(attached.pool_name, PKEY_POOL);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("partition must retain its assigned PKey: {attached:?}");
    };

    // RemotePoolFixture rebuilt the API; construct the controller with that registry.
    let mut controller = StateController::builder()
        .io(Arc::new(IBPartitionStateControllerIO::new(
            fixture.env.api.instrumentation.clone(),
        )))
        .database(
            fixture.env.pool.clone(),
            fixture.env.api.work_lock_manager_handle.clone(),
            &fixture.env.api.instrumentation,
        )
        .meter("ib_partitions", fixture.env.test_meter.meter())
        .processor_id("remote-ib-test".into())
        .services(
            IBPartitionStateHandlerServices {
                instrumentation: fixture.env.api.instrumentation.clone(),
                db_pool: fixture.env.pool.clone(),
                ib_fabric_manager: fixture.env.api.ib_fabric_manager.clone(),
                ib_pools: fixture.env.common_pools.infiniband.clone(),
                resource_pool_registry: Arc::new(fixture.env.api.resource_pool_registry.clone()),
            }
            .into(),
        )
        .state_handler(Arc::new(IBPartitionStateHandler::default()))
        .build_for_manual_iterations(fixture.cancel.clone())?;
    controller.run_single_iteration_ext(false).await;
    let renamed = fixture
        .env
        .api
        .update_ib_partition(Request::new(forge::IbPartitionUpdateRequest {
            id: Some(id),
            config: created.config,
            if_version_match: Some(created.config_version),
            metadata: Some(forge::Metadata {
                name: "renamed partition".into(),
                ..Default::default()
            }),
        }))
        .await?
        .into_inner();
    assert_eq!(
        renamed.metadata.expect("renamed metadata").name,
        "renamed partition"
    );
    fixture
        .env
        .api
        .delete_ib_partition(Request::new(forge::IbPartitionDeletionRequest {
            id: Some(id),
        }))
        .await?;
    controller.run_single_iteration_ext(false).await;
    let persisted = db::ib_partition::find_by(
        &fixture.env.pool,
        ObjectColumnFilter::One(db::ib_partition::IdColumn, &id),
        &fixture.env.api.instrumentation,
    )
    .await?
    .remove(0);
    assert_eq!(
        persisted.controller_state.value,
        IBPartitionControllerState::Deleting
    );
    assert_eq!(receipt(&fixture, id).await?, attached);
    let pkey_pool = &fixture.env.common_pools.infiniband.pkey_pools["default"];
    let ResourcePoolValue::Integer(value) = &assignment.value else {
        panic!("PKey assignment must contain an integer");
    };
    let native_pkey = u16::try_from(*value)?;
    let mut native = fixture.env.pool.txn_begin().await?;
    db::resource_pool::populate(pkey_pool, &mut native, vec![native_pkey], true).await?;
    let native_value = db::resource_pool::allocate(
        pkey_pool,
        &mut native,
        OwnerType::Machine,
        "unrelated native owner",
        None,
    )
    .await?;
    assert_eq!(native_value, native_pkey);
    native.commit().await?;
    let native_before: (serde_json::Value, serde_json::Value) = sqlx::query_as(
        "SELECT to_jsonb(r), to_jsonb(a) FROM resource_pool r
         JOIN resource_pool_allocations a ON a.integrated_resource_id = r.id
         WHERE r.name = $1 AND r.value = $2",
    )
    .bind(PKEY_POOL)
    .bind(native_pkey.to_string())
    .fetch_one(&fixture.env.pool)
    .await?;

    // A failed release-intent write must undo the partition deletion.
    sqlx::query("ALTER TABLE resource_pool_allocations ADD CONSTRAINT reject_ib_release CHECK (state <> 'release_requested')")
        .execute(&fixture.env.pool).await?;
    let recoveries = fixture.provider.recoveries.load(Ordering::SeqCst);
    controller.run_single_iteration_ext(false).await;
    let failed = db::ib_partition::find_by(
        &fixture.env.pool,
        ObjectColumnFilter::One(db::ib_partition::IdColumn, &id),
        &fixture.env.api.instrumentation,
    )
    .await?
    .remove(0);
    assert_eq!(
        failed.controller_state.value,
        IBPartitionControllerState::Deleting
    );
    let Some(PersistentStateHandlerOutcome::Error { err, .. }) = failed.controller_state_outcome
    else {
        panic!("release-intent failure must persist a controller error");
    };
    assert!(err.contains("reject_ib_release"), "{err}");
    assert_eq!(receipt(&fixture, id).await?, attached);
    assert_eq!(
        fixture.provider.recoveries.load(Ordering::SeqCst),
        recoveries
    );
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);
    let metrics = MetricsCapture::for_registry(fixture.env.test_meter.registry());
    assert_eq!(
        metrics.gauge_value(
            "ib_partitions_with_state_handling_errors_per_state",
            &[
                ("state", "deleting"),
                ("substate", ""),
                ("error", "db_error"),
                ("fresh", "true"),
            ]
        ),
        1.0
    );
    sqlx::query("ALTER TABLE resource_pool_allocations DROP CONSTRAINT reject_ib_release")
        .execute(&fixture.env.pool)
        .await?;

    // Native inventory with this value cannot authorize or obstruct remote release.
    let mut native_guard = fixture.env.pool.txn_begin().await?;
    sqlx::query("SELECT state FROM resource_pool WHERE name = $1 AND value = $2 FOR UPDATE")
        .bind(PKEY_POOL)
        .bind(native_pkey.to_string())
        .fetch_one(native_guard.as_mut())
        .await?;
    tokio::time::timeout(
        Duration::from_secs(5),
        controller.run_single_iteration_ext(false),
    )
    .await?;
    native_guard.commit().await?;
    let native_after: (serde_json::Value, serde_json::Value) = sqlx::query_as(
        "SELECT to_jsonb(r), to_jsonb(a) FROM resource_pool r
         JOIN resource_pool_allocations a ON a.integrated_resource_id = r.id
         WHERE r.name = $1 AND r.value = $2",
    )
    .bind(PKEY_POOL)
    .bind(native_pkey.to_string())
    .fetch_one(&fixture.env.pool)
    .await?;
    assert_eq!(native_after, native_before);
    assert_eq!(
        fixture.provider.recoveries.load(Ordering::SeqCst),
        recoveries
    );
    assert_eq!(partition_rows(&fixture, id).await?, 0);
    let releasing = receipt(&fixture, id).await?;
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 0);

    fixture
        .provider
        .release_unavailable
        .store(true, Ordering::SeqCst);
    let error = fixture
        .remote(PKEY_POOL)
        .reconcile(attached.allocation_id, &fixture.cancel)
        .await
        .expect_err("injected transient release failure");
    let carbide_resource_pool::remote::RemoteResourcePoolError::Provider(
        carbide_resource_pool::grpc::GrpcResourcePoolError::Rpc(status),
    ) = error
    else {
        panic!("expected the injected provider RPC failure: {error:?}");
    };
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(receipt(&fixture, id).await?, releasing);
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 1);

    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture
        .provider
        .release_unavailable
        .store(false, Ordering::SeqCst);
    fixture
        .remote(PKEY_POOL)
        .reconcile(attached.allocation_id, &fixture.cancel)
        .await?;
    let released = receipt(&fixture, id).await?;
    assert_eq!(released.state, AllocationState::Released);
    assert_eq!(released.source, attached.source);
    assert!(
        !fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), 2);
    fixture.stop().await
}

#[crate::sqlx_test]
async fn remote_ib_partition_requested_pkey_uses_manual_eligibility(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with_values(
        database,
        &[RemotePool {
            name: PKEY_POOL,
            start: 2000,
            auto_assign: true,
        }],
        &[2000, 2001],
        None,
    )
    .await?;
    fixture
        .definitions
        .get_mut(PKEY_POOL)
        .expect("PKey definition")
        .ranges = vec![
        Range {
            start: "2000".into(),
            end: "2001".into(),
            auto_assign: true,
        },
        Range {
            start: "2001".into(),
            end: "2002".into(),
            auto_assign: false,
        },
    ];
    initialize_ib_fixture(&mut fixture).await?;
    let id = IBPartitionId::new();
    let created = create_partition(&fixture, id, "manual partition", Some("07D1")).await?;
    assert_eq!(
        created
            .config
            .as_ref()
            .expect("partition config")
            .pkey
            .as_deref(),
        Some("0x7d1")
    );
    assert_eq!(
        created
            .status
            .as_ref()
            .expect("partition status")
            .pkey
            .as_deref(),
        Some("0x7d1")
    );
    let saved = receipt(&fixture, id).await?;
    let AllocationSource::Remote {
        request,
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = saved.source
    else {
        panic!("requested partition must retain its assignment");
    };
    assert_eq!(
        request.selection,
        AllocationSelection::Requested(ResourcePoolValue::Integer(2001))
    );
    assert_eq!(assignment.assignment_mode, AssignmentMode::Manual);
    assert_eq!(assignment.value, ResourcePoolValue::Integer(2001));
    let persisted = db::ib_partition::find_by(
        &fixture.env.pool,
        ObjectColumnFilter::One(db::ib_partition::IdColumn, &id),
        &fixture.env.api.instrumentation,
    )
    .await?
    .remove(0);
    assert_eq!(persisted.config.pkey.map(u16::from), Some(2001));
    assert_eq!(
        persisted
            .status
            .and_then(|status| status.pkey)
            .map(u16::from),
        Some(2001)
    );
    assert_remote_exhausted(&fixture, "exhausted manual range", "07d1").await?;
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );
    fixture.stop().await
}

#[crate::sqlx_test]
async fn remote_ib_partition_invalid_policy_response_recovers_assignment(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with_values(
        database,
        &[RemotePool {
            name: PKEY_POOL,
            start: 2000,
            auto_assign: true,
        }],
        &[2000],
        Some(v1::RouteTargetPolicy {
            mode: v1::RouteTargetMode::PreserveGenerated as i32,
            ..Default::default()
        }),
    )
    .await?;
    initialize_ib_fixture(&mut fixture).await?;
    let id = IBPartitionId::new();
    let error = create_partition(&fixture, id, "invalid policy response", None)
        .await
        .expect_err("routing policy is a protocol violation for a non-VPC owner");
    assert_eq!(error.code(), Code::Internal);
    let source = std::error::Error::source(&error)
        .expect("in-process error source")
        .downcast_ref::<crate::errors::CarbideErrorWithLocation>()
        .expect("Core error source");
    let crate::CarbideError::RemoteResourcePool(remote) = &source.error else {
        panic!("expected a protocol error source: {source:?}");
    };
    assert!(
        matches!(
            remote.as_ref(),
            carbide_resource_pool::remote::RemoteResourcePoolError::Provider(
                carbide_resource_pool::grpc::GrpcResourcePoolError::InvalidResponse(
                    "route target policy requires a VPC owner"
                )
            )
        ),
        "{remote:?}"
    );
    assert_eq!(partition_rows(&fixture, id).await?, 0);
    let cancelling = receipt(&fixture, id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    assert!(matches!(
        &cancelling.source,
        AllocationSource::Remote { result: None, .. }
    ));
    assert_eq!(fixture.provider.local.provisioned_count(), 1);

    // Cancel returns a terminal status without the invalid allocation payload.
    let recoveries = fixture.provider.recoveries.load(Ordering::SeqCst);
    fixture
        .remote(PKEY_POOL)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let cancelled = receipt(&fixture, id).await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, cancelling.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(
        fixture.provider.recoveries.load(Ordering::SeqCst),
        recoveries
    );
    fixture.stop().await
}

#[crate::sqlx_test]
async fn remote_ib_partition_invalid_pkey_recovers_assignment(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with_values(
        database,
        &[RemotePool {
            name: PKEY_POOL,
            start: 0x8000,
            auto_assign: true,
        }],
        &[0x8000],
        None,
    )
    .await?;
    initialize_ib_fixture(&mut fixture).await?;
    let id = IBPartitionId::new();
    let error = create_partition(&fixture, id, "invalid PKey", None)
        .await
        .expect_err("provider assignment must fit the IB PKey domain");
    assert_eq!(error.code(), Code::Internal);
    assert!(error.message().contains("not a valid pkey"), "{error}");
    assert_unused_pkey_recovered(&fixture, id).await?;
    fixture.stop().await
}
