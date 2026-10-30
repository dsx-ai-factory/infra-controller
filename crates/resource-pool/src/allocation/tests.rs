// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::{Duration, Instant};

use carbide_resource_pool_api::testing::LocalProvider;
use carbide_resource_pool_api::v1;
use db::ConditionalWrite;
use db::resource_pool::{
    ResourcePoolAllocationNotOwned, binding, registration as definitions, remote as storage,
};
use db::work_lock_manager::{self, KeepaliveConfig, WorkLockManagerHandle};
use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolAllocation};
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::remote::{
    AllocationAssignment, AllocationResult, AllocationSelection, CompletionAuthority,
    RemoteAllocationParameters,
};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain, ValueType,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::grpc::GrpcResourcePoolBackend;
use crate::grpc::test_server::{Fault, FaultProvider, Gate, TestServer, bounded, pool, provider};
use crate::remote::{RemoteResourcePool, RemoteResourcePoolError};
use crate::value::{ResourcePoolEligibility, ValueConversionError};
use crate::{
    PoolDefinitionSource, ResourcePoolBackendError, ResourcePoolRegistry, ResourcePoolWithBackend,
    registration,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `RemotePoolFixture` exercises shared operations with saved registrations and
/// a real provider connection, without inserting consumer entities.
struct RemotePoolFixture {
    database: PgPool,
    descriptor: ResourcePool<i32>,
    registry: ResourcePoolRegistry,
    remote: RemoteResourcePool,
    provider: FaultProvider,
    work_locks: WorkLockManagerHandle,
    tasks: JoinSet<()>,
    server: TestServer,
}

impl RemotePoolFixture {
    /// Create a registered remote pool with one automatic integer value.
    async fn automatic(database: PgPool, value: i64) -> TestResult<Self> {
        let local = LocalProvider::new(pool(), v1::ValueDomain::Integer, vec![value.into()], None)?;
        Self::new(
            database,
            local,
            &ResourcePoolEligibility {
                domain: ResourcePoolValueDomain::Integer,
                entries: vec![entry(value, v1::AssignmentMode::Automatic)],
            },
            Fault::None,
        )
        .await
    }

    /// Register the supplied values through the same registry and definition
    /// APIs that allocation preparation reads.
    async fn new(
        database: PgPool,
        local: LocalProvider,
        eligibility: &ResourcePoolEligibility,
        fault: Fault,
    ) -> TestResult<Self> {
        let provider = FaultProvider::new(local, fault);
        let server = TestServer::start(provider.clone()).await;
        let client = GrpcResourcePoolBackend::new(server.endpoint.clone(), None)?;
        let mut tasks = JoinSet::new();
        let work_locks = work_lock_manager::start(
            &mut tasks,
            database.clone(),
            KeepaliveConfig {
                // These short tests exercise allocation, not periodic renewal.
                interval: Duration::from_secs(3600),
                timeout: Duration::from_secs(60),
            },
        )
        .await?;
        let remote = RemoteResourcePool::new(
            database.clone(),
            work_locks.clone(),
            pool().authority_id,
            client,
        );
        let descriptor = ResourcePool::new(pool().logical_pool, ValueType::Integer);
        let selection = ResourcePoolSelection {
            pool_name: descriptor.name().to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("test-provider".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: remote.authority_id().to_owned(),
            remote_pool: pool().pool_id,
        };
        let mut txn = database.begin().await?;
        let registry = ResourcePoolRegistry::resolve_with_remotes(
            PoolDefinitionSource::SeedConfiguration,
            &mut txn,
            &HashMap::from([(selection.pool_name.clone(), selection)]),
            &HashMap::from([(remote.authority_id().to_owned(), remote.clone())]),
        )
        .await?;
        // Registration opens its own transactions and contacts the provider,
        // so its binding must already be committed.
        txn.commit().await?;
        registration::define(
            &database,
            remote.provider(),
            &registry
                .get(descriptor.name())
                .expect("resolved pool")
                .binding,
            None,
            eligibility,
        )
        .await?;
        Ok(Self {
            database,
            descriptor,
            registry,
            remote,
            provider,
            work_locks,
            tasks,
            server,
        })
    }

    /// Rebuild the coordinator and registry using their saved bindings, keeping
    /// the provider's reservations and database records intact.
    async fn restart(&mut self) -> TestResult {
        self.remote = RemoteResourcePool::new(
            self.database.clone(),
            self.work_locks.clone(),
            self.remote.authority_id().to_owned(),
            self.remote.provider().clone(),
        );
        let mut txn = self.database.begin().await?;
        self.registry = ResourcePoolRegistry::resolve_with_remotes(
            PoolDefinitionSource::ListenOnly,
            &mut txn,
            &HashMap::new(),
            &HashMap::from([(self.remote.authority_id().to_owned(), self.remote.clone())]),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Reload the record so lifecycle assertions observe committed state.
    async fn saved(&self) -> TestResult<ResourcePoolAllocation> {
        Ok(storage::latest_for_owner_slot(
            &self.database,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
        )
        .await?
        .expect("saved allocation request"))
    }

    /// Join all owned tasks before the database fixture is torn down.
    async fn stop(self) {
        drop(self.registry);
        drop(self.remote);
        drop(self.work_locks);
        bounded(self.tasks.join_all()).await;
        self.server.stop().await;
    }
}

/// Keep the test's allowed value and assignment mode visible at each call site.
fn entry(value: i64, mode: v1::AssignmentMode) -> v1::Eligibility {
    v1::Eligibility {
        assignment_mode: mode as i32,
        entries: Some(v1::eligibility::Entries::Value(value.into())),
    }
}

/// Read the saved provider inputs independently of the returned allocation.
fn saved_request(record: &ResourcePoolAllocation) -> &RemoteAllocationParameters {
    let AllocationSource::Remote { request, .. } = &record.source else {
        panic!("expected a remote allocation record: {record:?}");
    };
    request
}

/// Read the saved assignment so tests can compare attachment and cleanup identity.
fn assignment(record: &ResourcePoolAllocation) -> &AllocationAssignment {
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &record.source
    else {
        panic!("expected a saved provider assignment: {record:?}");
    };
    assignment
}

/// Shared allocation and release obey the entity transaction. Rolling it back
/// must leave the provider's reservation available for retry, not free it early.
#[carbide_macros::sqlx_test]
async fn shared_remote_allocation_attaches_and_releases_in_the_entity_transaction(
    database: PgPool,
) -> TestResult {
    let fixture = RemotePoolFixture::automatic(database, 1).await?;
    let cancel = CancellationToken::new();
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("selected remote pool");
    txn.commit().await?;
    let allocation = selected
        .prepare_allocation(
            &fixture.database,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            None,
            &cancel,
        )
        .await?;
    let pending = fixture.saved().await?;
    assert_eq!(pending.state, AllocationState::Pending);
    assert_eq!(
        saved_request(&pending).selection,
        AllocationSelection::Automatic
    );
    assert_eq!(allocation.value(), Some(&1));
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);

    // A failed entity transaction can retry the same assignment without another
    // provider allocation. Cancellation also leaves a committed attachment alone.
    let mut txn = fixture.database.begin().await?;
    assert_eq!(allocation.allocate(&mut txn).await?, 1);
    txn.rollback().await?;
    assert_eq!(fixture.saved().await?, pending);
    let mut txn = fixture.database.begin().await?;
    assert_eq!(allocation.allocate(&mut txn).await?, 1);
    txn.commit().await?;
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);
    allocation.cancel(&fixture.database).await?;
    let attached = fixture.saved().await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_eq!(attached.source, pending.source);
    let mut txn = fixture.database.begin().await?;
    let Err(error) = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await
    else {
        panic!("attached allocations require release");
    };
    assert!(error.is_not_current());
    assert!(matches!(
        error,
        ResourcePoolBackendError::Remote(RemoteResourcePoolError::NotCurrent)
    ));
    txn.commit().await?;
    assert_eq!(fixture.saved().await?, attached);
    assert_eq!(fixture.provider.cancel_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.provider.release_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        selected
            .prepare_allocation(
                &fixture.database,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                &cancel,
            )
            .await,
        Err(ResourcePoolBackendError::Database(
            db::DatabaseError::FailedPrecondition(_)
        ))
    ));
    assert_eq!(fixture.saved().await?, attached);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);

    // Release records intent in the caller's transaction. Only reconciliation
    // after commit may contact the provider and free the reservation.
    for commit in [
        // An abandoned entity change must also roll back release intent.
        false, // A completed entity change allows recovery to release the value.
        true,
    ] {
        let mut txn = fixture.database.begin().await?;
        let owned = fixture
            .registry
            .find_owned(
                &fixture.descriptor,
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
            )
            .await?
            .expect("attached allocation");
        // Both handles were found under the same lock; the second release
        // must report that the first already ended ownership.
        let second = fixture
            .registry
            .find_owned(
                &fixture.descriptor,
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
            )
            .await?
            .expect("second handle to the attached allocation");
        assert_eq!(*owned.value(), 1);
        assert_eq!(
            owned.release(&mut txn).await?,
            ConditionalWrite::Applied(())
        );
        assert_eq!(
            second.release(&mut txn).await?,
            ConditionalWrite::NotApplied(ResourcePoolAllocationNotOwned)
        );
        if commit {
            txn.commit().await?;
            let releasing = fixture.saved().await?;
            assert_eq!(releasing.state, AllocationState::ReleaseRequested);
            assert_eq!(releasing.source, attached.source);
        } else {
            txn.rollback().await?;
            assert_eq!(fixture.saved().await?, attached);
        }
        assert_eq!(fixture.provider.release_calls.load(Ordering::SeqCst), 0);
        assert!(
            fixture
                .provider
                .provider
                .is_provisioned(&assignment(&attached).allocation_id)
        );
    }
    let releasing = fixture.saved().await?;
    let mut txn = fixture.database.begin().await?;
    assert!(matches!(
        selected
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                CompletionAuthority::Controller {
                    desire_id: Uuid::new_v4()
                },
            )
            .await,
        Err(ResourcePoolBackendError::CleanupPending)
    ));
    txn.commit().await?;
    assert_eq!(fixture.saved().await?, releasing);
    let mut txn = fixture.database.begin().await?;
    let cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    assert!(!cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    assert_eq!(fixture.provider.release_calls.load(Ordering::SeqCst), 0);
    cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    let released = fixture.saved().await?;
    assert_eq!(released.state, AllocationState::Released);
    assert_eq!(released.source, attached.source);
    assert_eq!(fixture.provider.provider.provisioned_count(), 0);
    assert_eq!(fixture.provider.release_calls.load(Ordering::SeqCst), 1);
    fixture.stop().await;
    Ok(())
}

/// Foreground cancellation retains the original error even if its cleanup write
/// fails after the provider committed an allocation and withheld the reply.
#[carbide_macros::sqlx_test]
async fn interrupted_foreground_preparation_preserves_the_original_error(
    database: PgPool,
) -> TestResult {
    let eligibility = ResourcePoolEligibility {
        domain: ResourcePoolValueDomain::Integer,
        entries: vec![entry(1, v1::AssignmentMode::Automatic)],
    };
    let local = LocalProvider::new(pool(), v1::ValueDomain::Integer, vec![1_i64.into()], None)?;
    // The provider reserves the value before the gate withholds its reply.
    let gate = Arc::new(Gate::default());
    let fixture = RemotePoolFixture::new(
        database,
        local,
        &eligibility,
        Fault::WithholdAllocation(gate.clone()),
    )
    .await?;
    for cancellation_saved in [true, false] {
        let allocation_calls = fixture.provider.allocation_calls.load(Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let mut txn = fixture.database.begin().await?;
        let selected = fixture
            .registry
            .select(&mut txn, &fixture.descriptor)
            .await?
            .expect("selected remote pool");
        txn.commit().await?;
        let mut preparing = Box::pin(selected.prepare_allocation(
            &fixture.database,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            None,
            &cancel,
        ));
        tokio::select! {
            result = &mut preparing => panic!("preparation passed its reply gate: {:?}", result.err()),
            () = bounded(gate.entered.notified()) => {}
        }
        let pending = fixture.saved().await?;
        assert_eq!(pending.state, AllocationState::Pending);
        assert!(matches!(
            saved_request(&pending).completion_authority,
            CompletionAuthority::Foreground { .. }
        ));
        let committed = fixture
            .provider
            .committed
            .lock()
            .expect("committed allocation lock")
            .clone()
            .expect("provider committed before withholding its reply");
        assert_eq!(
            committed.operation.expect("provider operation").request_id,
            pending.allocation_id.to_string()
        );

        if !cancellation_saved {
            // Reject the real cleanup write after the provider has already reserved
            // its value. The foreground wrapper must preserve the original error.
            bounded(
                sqlx::query(
                    "ALTER TABLE resource_pool_allocations
                     ADD CONSTRAINT reject_test_cancellation CHECK (state <> 'cancel_requested')",
                )
                .execute(&fixture.database),
            )
            .await?;
        }
        cancel.cancel();
        assert!(matches!(
            bounded(preparing).await,
            Err(ResourcePoolBackendError::Remote(
                RemoteResourcePoolError::Cancelled
            ))
        ));
        let cancelling = fixture.saved().await?;
        assert_eq!(
            cancelling.state,
            if cancellation_saved {
                AllocationState::CancelRequested
            } else {
                AllocationState::Pending
            }
        );
        assert_eq!(cancelling.allocation_id, pending.allocation_id);
        assert_eq!(cancelling.source, pending.source);
        assert!(
            fixture
                .provider
                .provider
                .is_provisioned(&committed.allocation_id)
        );
        if !cancellation_saved {
            bounded(
                sqlx::query(
                    "ALTER TABLE resource_pool_allocations
                     DROP CONSTRAINT reject_test_cancellation",
                )
                .execute(&fixture.database),
            )
            .await?;
        }
        let running = CancellationToken::new();
        let mut txn = fixture.database.begin().await?;
        let cleanup = fixture
            .registry
            .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
            .await?;
        txn.commit().await?;
        bounded(cleanup.wait(&fixture.registry, &fixture.database, &running)).await?;
        let cancelled = fixture.saved().await?;
        assert_eq!(cancelled.state, AllocationState::Cancelled);
        assert_eq!(cancelled.allocation_id, pending.allocation_id);
        assert_eq!(saved_request(&cancelled), saved_request(&pending));
        assert_eq!(fixture.provider.provider.provisioned_count(), 0);
        assert_eq!(
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            allocation_calls + 1
        );
        gate.release.notify_waiters();
        bounded(gate.dropped.notified()).await;
    }
    fixture.stop().await;
    Ok(())
}

/// A caller can inspect provider policy before saving its VPC. If that entity
/// transaction rolls back, cancellation prevents attachment while cleanup runs.
#[carbide_macros::sqlx_test]
async fn rolled_back_allocation_cancels_and_resumes_pending_cleanup(
    database: PgPool,
) -> TestResult {
    let eligibility = ResourcePoolEligibility {
        domain: ResourcePoolValueDomain::Integer,
        entries: vec![entry(1, v1::AssignmentMode::Automatic)],
    };
    let fixture = RemotePoolFixture::new(
        database,
        provider().with_pending_cancellation(),
        &eligibility,
        Fault::None,
    )
    .await?;
    let cancel = CancellationToken::new();
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("selected remote pool");
    txn.commit().await?;
    let allocation = selected
        .prepare_allocation(
            &fixture.database,
            OwnerType::Vpc,
            "vpc-1",
            "vni",
            None,
            &cancel,
        )
        .await?;
    let pending = storage::latest_for_owner_slot(&fixture.database, OwnerType::Vpc, "vpc-1", "vni")
        .await?
        .expect("saved VPC allocation");
    let policy = allocation.route_target_policy().expect("provider policy");
    assert!(!policy.peer_identity.is_empty());
    assert_eq!(
        Some(policy),
        assignment(&pending).route_target_policy.as_ref()
    );
    let mut txn = fixture.database.begin().await?;
    assert_eq!(allocation.allocate(&mut txn).await?, 1);
    txn.rollback().await?;
    assert_eq!(
        storage::load(&fixture.database, pending.allocation_id)
            .await?
            .as_ref(),
        Some(&pending)
    );

    // A late entity retry must not attach after cleanup intent was saved.
    allocation.cancel(&fixture.database).await?;
    let cancelling = storage::load(&fixture.database, pending.allocation_id)
        .await?
        .expect("saved cancellation");
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    assert_eq!(cancelling.allocation_id, pending.allocation_id);
    assert_eq!(cancelling.source, pending.source);
    let mut txn = fixture.database.begin().await?;
    let error = allocation
        .allocate(&mut txn)
        .await
        .expect_err("cancelled allocation cannot attach");
    assert!(error.is_not_current());
    assert!(matches!(
        error,
        ResourcePoolBackendError::Remote(RemoteResourcePoolError::NotCurrent)
    ));
    txn.commit().await?;
    assert_eq!(
        storage::load(&fixture.database, pending.allocation_id)
            .await?
            .as_ref(),
        Some(&cancelling)
    );
    assert_eq!(fixture.provider.cancel_calls.load(Ordering::SeqCst), 0);

    // The provider keeps the reservation until its asynchronous cleanup ends.
    // Stopping our wait must preserve that saved cancellation for the next try.
    let mut txn = fixture.database.begin().await?;
    let cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Vpc, "vpc-1", "vni")
        .await?;
    txn.commit().await?;

    // Cancellation also bounds the initial wait for a database connection.
    let single_connection = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(fixture.database.connect_options().as_ref().clone())
        .await?;
    let held = single_connection.acquire().await?;
    let calls_before_wait = [
        fixture.provider.allocation_calls.load(Ordering::SeqCst),
        fixture.provider.recover_calls.load(Ordering::SeqCst),
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        fixture.provider.release_calls.load(Ordering::SeqCst),
    ];
    let interrupted = CancellationToken::new();
    let mut waiting = Box::pin(cleanup.wait(&fixture.registry, &single_connection, &interrupted));
    assert!(
        poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    interrupted.cancel();
    assert!(matches!(
        bounded(waiting).await,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Cancelled
        ))
    ));
    assert_eq!(
        storage::load(&fixture.database, pending.allocation_id)
            .await?
            .as_ref(),
        Some(&cancelling)
    );
    assert_eq!(
        [
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            fixture.provider.recover_calls.load(Ordering::SeqCst),
            fixture.provider.cancel_calls.load(Ordering::SeqCst),
            fixture.provider.release_calls.load(Ordering::SeqCst),
        ],
        calls_before_wait
    );
    assert!(
        fixture
            .provider
            .provider
            .is_provisioned(&assignment(&pending).allocation_id)
    );
    drop(held);
    single_connection.close().await;

    let mut cleaning = Box::pin(cleanup.wait(&fixture.registry, &fixture.database, &cancel));
    tokio::select! {
        result = &mut cleaning => {
            panic!("cleanup finished before provider confirmation: {:?}", result.err());
        }
        // A second call proves the helper waited through its five-second poll
        // interval after the provider left cleanup pending.
        result = tokio::time::timeout(Duration::from_secs(10), async {
            while fixture.provider.cancel_calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }) => result?,
    }
    assert_eq!(fixture.provider.cancel_calls.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .provider
            .provider
            .is_provisioned(&assignment(&pending).allocation_id)
    );
    cancel.cancel();
    assert!(matches!(
        bounded(cleaning).await,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Cancelled
        ))
    ));
    assert_eq!(
        storage::load(&fixture.database, pending.allocation_id)
            .await?
            .as_ref(),
        Some(&cancelling)
    );

    // The deadline is another interrupted wait, not proof of deprovisioning.
    let running = CancellationToken::new();
    let started = Instant::now();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(40),
            cleanup.wait(&fixture.registry, &fixture.database, &running),
        )
        .await?,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Incomplete
        ))
    ));
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert_eq!(
        storage::load(&fixture.database, pending.allocation_id).await?,
        Some(cancelling)
    );
    assert!(
        fixture
            .provider
            .provider
            .is_provisioned(&assignment(&pending).allocation_id)
    );
    let cancel_calls = fixture.provider.cancel_calls.load(Ordering::SeqCst);
    let mut cleaning = Box::pin(cleanup.wait(&fixture.registry, &fixture.database, &running));
    tokio::select! {
        result = &mut cleaning => {
            panic!("retry finished before provider confirmation: {:?}", result.err());
        }
        () = bounded(async {
            while fixture.provider.cancel_calls.load(Ordering::SeqCst) == cancel_calls {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }) => {}
    }
    let operation = fixture
        .provider
        .committed
        .lock()
        .expect("committed allocation lock")
        .as_ref()
        .and_then(|allocation| allocation.operation.clone())
        .expect("provider operation");
    // Simulate actual asynchronous deprovisioning before allowing completion.
    fixture
        .provider
        .provider
        .complete_cleanup(&v1::OperationIdentity {
            pool: operation.pool,
            request_id: operation.request_id,
        })?;
    tokio::time::timeout(Duration::from_secs(10), cleaning).await??;
    let cancelled = storage::load(&fixture.database, pending.allocation_id)
        .await?
        .expect("confirmed cancellation");
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.allocation_id, pending.allocation_id);
    assert_eq!(cancelled.source, pending.source);
    let mut txn = fixture.database.begin().await?;
    assert!(cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    assert_eq!(fixture.provider.provider.provisioned_count(), 0);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);
    fixture.stop().await;
    Ok(())
}

/// A controller retry keeps its request after cancellation while waiting for a
/// work lease, and keeps its original definition after restart.
/// Confirmed cleanup permits a new generation and invalidates the old cleanup check.
#[carbide_macros::sqlx_test]
async fn controller_replay_keeps_its_request_and_definition_until_cleanup(
    database: PgPool,
) -> TestResult {
    // Manual value 2 remains replayable after automatic value 3 is introduced.
    let mut eligibility = ResourcePoolEligibility {
        domain: ResourcePoolValueDomain::Integer,
        entries: vec![entry(2, v1::AssignmentMode::Manual)],
    };
    let local = LocalProvider::new(
        pool(),
        v1::ValueDomain::Integer,
        vec![2_i64.into(), 3_i64.into()],
        None,
    )?;
    let mut fixture = RemotePoolFixture::new(database, local, &eligibility, Fault::None).await?;
    let cancel = CancellationToken::new();
    let authority = CompletionAuthority::Controller {
        desire_id: Uuid::new_v4(),
    };
    // No request means nothing needs cleanup, but that answer expires as soon
    // as another caller prepares work for this owner and slot.
    let mut txn = fixture.database.begin().await?;
    let empty_cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    txn.commit().await?;
    empty_cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    let mut txn = fixture.database.begin().await?;
    assert!(empty_cleanup.is_complete(&mut txn).await?);
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("selected remote pool");
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            Some(2),
            authority.clone(),
        )
        .await?;
    txn.commit().await?;
    let pending = fixture.saved().await?;
    let mut txn = fixture.database.begin().await?;
    assert!(!empty_cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    let held = fixture
        .work_locks
        .try_acquire_lock(format!("resource-pool:{}", pending.allocation_id))
        .await?;
    let interrupted = CancellationToken::new();
    let mut resolving = Box::pin(prepared.resolve(&fixture.database, &interrupted));
    assert!(
        tokio::time::timeout(super::LEASE_RETRY_INTERVAL * 2, &mut resolving)
            .await
            .is_err(),
        "resolution must wait while another worker holds the lease",
    );
    interrupted.cancel();
    assert!(matches!(
        bounded(resolving).await,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Cancelled
        ))
    ));
    assert_eq!(pending.state, AllocationState::Pending);
    assert_eq!(fixture.saved().await?, pending);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 0);

    // Repeated WorkAlreadyLocked responses must also obey the enclosing budget.
    // Expiry leaves the same request available for a later resolution attempt.
    let mut txn = fixture.database.begin().await?;
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            Some(2),
            authority.clone(),
        )
        .await?;
    txn.commit().await?;
    let started = Instant::now();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(40),
            Box::pin(prepared.resolve(&fixture.database, &cancel))
        )
        .await?,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Incomplete
        ))
    ));
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert_eq!(fixture.saved().await?, pending);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 0);

    // Only the wait was cancelled. The same controller desire can replay its
    // committed request and finish once the other worker releases the lease.
    let mut txn = fixture.database.begin().await?;
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            Some(2),
            authority.clone(),
        )
        .await?;
    txn.commit().await?;
    let mut resolving = Box::pin(prepared.resolve(&fixture.database, &cancel));
    assert!(
        tokio::time::timeout(super::LEASE_RETRY_INTERVAL * 2, &mut resolving)
            .await
            .is_err(),
        "replayed resolution must also wait for the lease",
    );
    assert_eq!(fixture.saved().await?, pending);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 0);
    held.release().await?;
    let allocation = bounded(resolving).await?;
    assert_eq!(allocation.value(), Some(&2));
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);
    drop(allocation);
    let original = fixture.saved().await?;
    assert_eq!(original.state, AllocationState::Pending);
    assert_eq!(original.allocation_id, pending.allocation_id);
    assert_eq!(saved_request(&original), saved_request(&pending));
    assert_eq!(saved_request(&original).eligibility_revision, "1");

    // New allocations may use revision 2, but a retry must still recover the
    // request the provider accepted under revision 1.
    eligibility
        .entries
        .push(entry(3, v1::AssignmentMode::Automatic));
    let accepted = registration::define(
        &fixture.database,
        fixture.remote.provider(),
        &fixture
            .registry
            .get(fixture.descriptor.name())
            .expect("resolved pool")
            .binding,
        Some(1),
        &eligibility,
    )
    .await?;
    assert_eq!(accepted.eligibility_revision, "2");
    fixture.restart().await?;
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("selected remote pool after restart");
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            Some(2),
            authority.clone(),
        )
        .await?;
    txn.commit().await?;
    let replayed = Box::pin(prepared.resolve(&fixture.database, &cancel)).await?;
    assert_eq!(replayed.value(), Some(&2));
    let replayed_record = fixture.saved().await?;
    assert_eq!(replayed_record.allocation_id, original.allocation_id);
    assert_eq!(replayed_record.source, original.source);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);

    // Changing the requested value must leave the original attempt available
    // for cleanup. Commit the rejected preparation to expose accidental writes.
    let mut txn = fixture.database.begin().await?;
    assert!(matches!(
        selected
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                Some(3),
                authority.clone(),
            )
            .await,
        Err(ResourcePoolBackendError::RequestChanged)
    ));
    txn.commit().await?;
    assert_eq!(fixture.saved().await?, replayed_record);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);

    // Capture has no provider effect and rolls back with the consumer's write.
    let cancels = fixture.provider.cancel_calls.load(Ordering::SeqCst);
    let calls_before_capture = [
        fixture.provider.allocation_calls.load(Ordering::SeqCst),
        fixture.provider.recover_calls.load(Ordering::SeqCst),
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        fixture.provider.release_calls.load(Ordering::SeqCst),
    ];
    let mut txn = fixture.database.begin().await?;
    let rolled_back = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    assert!(!rolled_back.is_complete(&mut txn).await?);
    assert_eq!(
        storage::load(&mut *txn, original.allocation_id)
            .await?
            .expect("staged cancellation")
            .state,
        AllocationState::CancelRequested
    );
    assert_eq!(fixture.saved().await?, replayed_record);
    assert_eq!(
        [
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            fixture.provider.recover_calls.load(Ordering::SeqCst),
            fixture.provider.cancel_calls.load(Ordering::SeqCst),
            fixture.provider.release_calls.load(Ordering::SeqCst),
        ],
        calls_before_capture
    );
    txn.rollback().await?;
    assert_eq!(fixture.saved().await?, replayed_record);
    assert!(matches!(
        rolled_back
            .wait(&fixture.registry, &fixture.database, &cancel)
            .await,
        Err(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::NotCurrent
        ))
    ));
    assert_eq!(fixture.saved().await?, replayed_record);
    assert_eq!(
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        cancels
    );

    let mut txn = fixture.database.begin().await?;
    let cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    txn.commit().await?;
    let cancelling = fixture.saved().await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    assert!(matches!(
        cleanup.wait(&ResourcePoolRegistry::default(), &fixture.database, &cancel).await,
        Err(ResourcePoolBackendError::Database(db::DatabaseError::FailedPrecondition(message)))
            if message.contains("needs its original backend")
    ));
    assert_eq!(fixture.saved().await?, cancelling);
    assert_eq!(
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        cancels
    );
    assert!(
        fixture
            .provider
            .provider
            .is_provisioned(&assignment(&original).allocation_id)
    );
    cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    let cancelled = fixture.saved().await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, original.source);
    assert_eq!(fixture.provider.provider.provisioned_count(), 0);
    let mut txn = fixture.database.begin().await?;
    assert!(cleanup.is_complete(&mut txn).await?);

    // The previous cleanup result cannot authorize forgetting a replacement
    // attempt, even though it uses the same owner, slot, and controller desire.
    let replacement_prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            Some(2),
            authority,
        )
        .await?;
    assert!(!cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    let replacement = fixture.saved().await?;
    assert_ne!(replacement.allocation_id, original.allocation_id);
    assert_eq!(replacement.state, AllocationState::Pending);
    assert_eq!(
        saved_request(&replacement).generation,
        saved_request(&original).generation + 1
    );
    assert_eq!(saved_request(&replacement).eligibility_revision, "2");
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 1);

    // A delayed caller waits for the captured attempt, even after a valid new
    // generation starts. Neither an old terminal token nor an absence token
    // may select and cancel that replacement.
    let cancels = fixture.provider.cancel_calls.load(Ordering::SeqCst);
    cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    empty_cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    assert_eq!(fixture.saved().await?, replacement);
    assert_eq!(
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        cancels
    );
    let mut txn = fixture.database.begin().await?;
    assert!(!cleanup.is_complete(&mut txn).await?);
    assert!(!empty_cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;

    // The untouched replacement can still resolve and attach. Roll back the
    // entity transaction so its cleanup token can exercise the terminal checks.
    let replacement_allocation =
        Box::pin(replacement_prepared.resolve(&fixture.database, &cancel)).await?;
    let resolved_replacement = fixture.saved().await?;
    assert_eq!(
        resolved_replacement.allocation_id,
        replacement.allocation_id
    );
    assert_eq!(resolved_replacement.state, AllocationState::Pending);
    let mut txn = fixture.database.begin().await?;
    assert_eq!(replacement_allocation.allocate(&mut txn).await?, 2);
    assert_eq!(
        storage::load(&mut *txn, replacement.allocation_id)
            .await?
            .expect("replacement attached in the entity transaction")
            .state,
        AllocationState::Attached
    );
    txn.rollback().await?;
    assert_eq!(fixture.saved().await?, resolved_replacement);
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 2);

    // A terminal replacement still needs its own cleanup check. Accepting any
    // terminal generation would let an old caller forget newer creation IDs.
    let mut txn = fixture.database.begin().await?;
    let replacement_cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    txn.commit().await?;
    replacement_cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    let cancelled_replacement = fixture.saved().await?;
    assert_eq!(cancelled_replacement.state, AllocationState::Cancelled);
    assert_eq!(
        cancelled_replacement.allocation_id,
        replacement.allocation_id
    );
    assert_eq!(
        saved_request(&cancelled_replacement),
        saved_request(&replacement)
    );
    let mut txn = fixture.database.begin().await?;
    assert!(!cleanup.is_complete(&mut txn).await?);
    assert!(replacement_cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    // A later cleanup request can return the existing terminal result without
    // asking the provider to cancel it again.
    let cancels = fixture.provider.cancel_calls.load(Ordering::SeqCst);
    let mut txn = fixture.database.begin().await?;
    let terminal_cleanup = fixture
        .registry
        .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
        .await?;
    txn.commit().await?;
    terminal_cleanup
        .wait(&fixture.registry, &fixture.database, &cancel)
        .await?;
    let mut txn = fixture.database.begin().await?;
    assert!(terminal_cleanup.is_complete(&mut txn).await?);
    txn.commit().await?;
    assert_eq!(
        fixture.provider.cancel_calls.load(Ordering::SeqCst),
        cancels
    );
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 2);
    fixture.stop().await;
    Ok(())
}

/// A provider value can fit its declared domain but exceed the consumer's Rust
/// type. Resolution requests cleanup and preserves that error if persistence fails.
#[carbide_macros::sqlx_test]
async fn unrepresentable_remote_value_preserves_the_original_error(database: PgPool) -> TestResult {
    // Eligible i64 provider data exceeds the consumer's i32 representation.
    let value = 2_147_483_648_i64;
    let fixture = RemotePoolFixture::automatic(database, value).await?;
    for cancellation_saved in [true, false] {
        let allocation_calls = fixture.provider.allocation_calls.load(Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let mut txn = fixture.database.begin().await?;
        let selected = fixture
            .registry
            .select(&mut txn, &fixture.descriptor)
            .await?
            .expect("selected remote pool");
        let prepared = selected
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                CompletionAuthority::Controller {
                    desire_id: Uuid::new_v4(),
                },
            )
            .await?;
        txn.commit().await?;
        let pending = fixture.saved().await?;
        if !cancellation_saved {
            bounded(
                sqlx::query(
                    "ALTER TABLE resource_pool_allocations
                     ADD CONSTRAINT reject_test_cancellation CHECK (state <> 'cancel_requested')",
                )
                .execute(&fixture.database),
            )
            .await?;
        }
        let Err(error) = prepared.resolve(&fixture.database, &cancel).await else {
            panic!("provider value must not fit i32");
        };
        assert!(matches!(
            error,
            ResourcePoolBackendError::Value(ValueConversionError::Consumer {
                rust_type: "i32",
                ..
            })
        ));
        let cancelling = fixture.saved().await?;
        assert_eq!(
            cancelling.state,
            if cancellation_saved {
                AllocationState::CancelRequested
            } else {
                AllocationState::Pending
            }
        );
        assert_eq!(cancelling.allocation_id, pending.allocation_id);
        assert_eq!(saved_request(&cancelling), saved_request(&pending));

        if cancellation_saved {
            // A retry must wait for cleanup, preserving the request that still owns
            // the provider reservation even when this transaction commits.
            let mut txn = fixture.database.begin().await?;
            assert!(matches!(
                selected
                    .prepare_request(
                        &mut txn,
                        OwnerType::Machine,
                        "machine-1",
                        "test-allocation",
                        None,
                        saved_request(&pending).completion_authority.clone(),
                    )
                    .await,
                Err(ResourcePoolBackendError::CleanupPending)
            ));
            txn.commit().await?;
            assert_eq!(fixture.saved().await?, cancelling);
        }
        assert_eq!(
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            allocation_calls + 1
        );
        assert!(
            fixture
                .provider
                .provider
                .is_provisioned(&assignment(&cancelling).allocation_id)
        );

        // Controller work has no expiry: restore persistence and explicitly clean
        // the same unusable assignment rather than abandoning its saved identity.
        if !cancellation_saved {
            bounded(
                sqlx::query(
                    "ALTER TABLE resource_pool_allocations
                     DROP CONSTRAINT reject_test_cancellation",
                )
                .execute(&fixture.database),
            )
            .await?;
        }
        let mut txn = fixture.database.begin().await?;
        let cleanup = fixture
            .registry
            .request_cancellation(&mut txn, OwnerType::Machine, "machine-1", "test-allocation")
            .await?;
        txn.commit().await?;
        cleanup
            .wait(&fixture.registry, &fixture.database, &cancel)
            .await?;
        let cancelled = fixture.saved().await?;
        assert_eq!(cancelled.state, AllocationState::Cancelled);
        assert_eq!(cancelled.source, cancelling.source);
        assert_eq!(fixture.provider.provider.provisioned_count(), 0);
        assert_eq!(
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            allocation_calls + 1
        );
    }
    fixture.stop().await;
    Ok(())
}

/// Preparation must reject unusable admission evidence before saving a request.
#[carbide_macros::sqlx_test]
async fn preparation_rejects_unaccepted_corrupt_and_incompatible_definitions(
    database: PgPool,
) -> TestResult {
    struct Case {
        label: &'static str,
        registration: ResourcePoolRegistration,
        value_type: ValueType,
        check_error: fn(&ResourcePoolBackendError) -> bool,
    }
    let fixture = RemotePoolFixture::automatic(database, 1).await?;
    let resolved = fixture
        .registry
        .get(fixture.descriptor.name())
        .expect("resolved pool");
    let original = definitions::latest(&fixture.database, resolved.binding.binding_id)
        .await?
        .expect("accepted definition");
    for case in [
        Case {
            label: "pending definition",
            registration: ResourcePoolRegistration {
                accepted: false,
                ..original.clone()
            },
            value_type: ValueType::Integer,
            check_error: |error| {
                matches!(
                    error,
                    ResourcePoolBackendError::Database(db::DatabaseError::FailedPrecondition(message))
                        if message.contains("has no accepted definition")
                )
            },
        },
        Case {
            label: "corrupt definition",
            registration: ResourcePoolRegistration {
                // A truncated length-delimited field must fail decoding, not look absent.
                request: vec![0x0a],
                ..original.clone()
            },
            value_type: ValueType::Integer,
            check_error: |error| {
                matches!(
                    error,
                    ResourcePoolBackendError::Registration(
                        registration::RegistrationError::Decode(_)
                    )
                )
            },
        },
        Case {
            label: "descriptor domain",
            registration: original,
            value_type: ValueType::Ipv4,
            check_error: |error| {
                matches!(
                    error,
                    ResourcePoolBackendError::Remote(RemoteResourcePoolError::InvalidRecord(
                        "selected backend or value domain differs from the saved binding"
                    ))
                )
            },
        },
    ] {
        let expected = &case.registration;
        sqlx::query(
            "UPDATE resource_pool_registration
             SET request = $1, accepted = $2 WHERE binding_id = $3",
        )
        .bind(&expected.request)
        .bind(expected.accepted)
        .bind(expected.binding_id)
        .execute(&fixture.database)
        .await?;
        let descriptor =
            ResourcePool::<String>::new(fixture.descriptor.name().to_owned(), case.value_type);
        let selected = ResourcePoolWithBackend {
            pool: &descriptor,
            backend: &resolved.backend,
        };
        let mut txn = fixture.database.begin().await?;
        let error = selected
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                CompletionAuthority::Controller {
                    desire_id: Uuid::new_v4(),
                },
            )
            .await
            .err()
            .expect(case.label);
        assert!((case.check_error)(&error), "{}: {error:?}", case.label);
        txn.commit().await?;
        assert_eq!(
            definitions::latest(&fixture.database, expected.binding_id).await?,
            Some(expected.clone()),
            "{}",
            case.label
        );
        assert_eq!(
            binding::load(&fixture.database, fixture.descriptor.name()).await?,
            Some(resolved.binding.clone()),
            "{}",
            case.label
        );
        assert_eq!(
            storage::latest_for_owner_slot(
                &fixture.database,
                OwnerType::Machine,
                "machine-1",
                "test-allocation"
            )
            .await?,
            None,
            "{}",
            case.label
        );
        assert_eq!(
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            0,
            "{}",
            case.label
        );
        assert_eq!(
            fixture.provider.recover_calls.load(Ordering::SeqCst),
            0,
            "{}",
            case.label
        );
    }
    fixture.stop().await;
    Ok(())
}

/// Replay preserves the original desire, pool selection, and accepted identity.
#[carbide_macros::sqlx_test]
async fn preparation_rejects_changed_or_invalid_replay_evidence(database: PgPool) -> TestResult {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        OtherDesire,
        OtherPool,
        ProviderIdentity,
        Revision,
    }
    let fixture = RemotePoolFixture::automatic(database, 1).await?;
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("remote pool");
    let authority = CompletionAuthority::Controller {
        desire_id: Uuid::new_v4(),
    };
    selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            None,
            authority.clone(),
        )
        .await?;
    let other_descriptor = ResourcePool::<i32>::new("other-vni".to_owned(), ValueType::Integer);
    let other_binding = binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: other_descriptor.name().to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("test-provider".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: fixture.remote.authority_id().to_owned(),
            remote_pool: pool().pool_id,
        },
    )
    .await?;
    txn.commit().await?;
    registration::define(
        &fixture.database,
        fixture.remote.provider(),
        &other_binding,
        None,
        &ResourcePoolEligibility {
            domain: ResourcePoolValueDomain::Integer,
            entries: vec![entry(1, v1::AssignmentMode::Automatic)],
        },
    )
    .await?;
    let original = fixture.saved().await?;
    let other = ResourcePoolWithBackend {
        pool: &other_descriptor,
        backend: selected.backend,
    };
    for case in [
        Case::OtherDesire,
        Case::OtherPool,
        Case::ProviderIdentity,
        Case::Revision,
    ] {
        let mut request = saved_request(&original).clone();
        match case {
            // The lower-level store accepts provider pool IDs and revisions without
            // checking them against the saved registration.
            Case::ProviderIdentity => request.pool_id = "restored-provider-pool".to_owned(),
            Case::Revision => request.eligibility_revision = "initial".to_owned(),
            Case::OtherDesire | Case::OtherPool => {}
        }
        sqlx::query(
            "UPDATE resource_pool_allocations SET remote_request = $1 WHERE allocation_id = $2",
        )
        .bind(Json(&request))
        .bind(original.allocation_id)
        .execute(&fixture.database)
        .await?;
        let before = fixture.saved().await?;
        let mut txn = fixture.database.begin().await?;
        let target = if matches!(case, Case::OtherPool) {
            &other
        } else {
            &selected
        };
        let error = target
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                if matches!(case, Case::OtherDesire) {
                    CompletionAuthority::Controller {
                        desire_id: Uuid::new_v4(),
                    }
                } else {
                    authority.clone()
                },
            )
            .await
            .err()
            .expect("replay evidence must agree");
        match case {
            Case::OtherDesire => assert!(matches!(
                error,
                ResourcePoolBackendError::Database(db::DatabaseError::FailedPrecondition(message))
                    if message == "machine `machine-1` already has an allocation or unfinished request for slot `test-allocation`"
            )),
            Case::OtherPool => assert!(matches!(error, ResourcePoolBackendError::RequestChanged)),
            Case::ProviderIdentity => assert!(matches!(
                error,
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::InvalidRecord(
                    "saved request differs from its accepted pool identity"
                ))
            )),
            Case::Revision => assert!(matches!(
                error,
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::InvalidRecord(
                    "saved request has an invalid eligibility revision"
                ))
            )),
        }
        txn.commit().await?;
        assert_eq!(fixture.saved().await?, before, "{case:?}");
        let requests: Vec<Uuid> = sqlx::query_scalar(
            "SELECT allocation_id FROM resource_pool_allocations
             WHERE owner_type = 'machine' AND owner_id = 'machine-1'",
        )
        .fetch_all(&fixture.database)
        .await?;
        assert_eq!(requests, vec![original.allocation_id], "{case:?}");
        assert_eq!(
            fixture.provider.allocation_calls.load(Ordering::SeqCst),
            0,
            "{case:?}"
        );
        assert_eq!(
            fixture.provider.recover_calls.load(Ordering::SeqCst),
            0,
            "{case:?}"
        );
    }
    fixture.stop().await;
    Ok(())
}

/// A failed typed ownership read is an error and cannot withdraw Attached ownership.
#[carbide_macros::sqlx_test]
async fn owned_lookup_preserves_attachment_when_identity_or_value_is_invalid(
    database: PgPool,
) -> TestResult {
    #[derive(Clone, Copy, Debug)]
    enum Case {
        Authority,
        Domain,
        Narrowing,
    }
    let fixture = RemotePoolFixture::automatic(database, 2_147_483_648).await?;
    let descriptor =
        ResourcePool::<i64>::new(fixture.descriptor.name().to_owned(), ValueType::Integer);
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &descriptor)
        .await?
        .expect("i64 consumer");
    txn.commit().await?;
    let allocation = selected
        .prepare_allocation(
            &fixture.database,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            None,
            &CancellationToken::new(),
        )
        .await?;
    let mut txn = fixture.database.begin().await?;
    assert_eq!(allocation.allocate(&mut txn).await?, 2_147_483_648);
    txn.commit().await?;
    let attached = fixture.saved().await?;
    assert_eq!(attached.state, AllocationState::Attached);
    for case in [Case::Authority, Case::Domain, Case::Narrowing] {
        let mut request = saved_request(&attached).clone();
        if matches!(case, Case::Authority) {
            request.authority_id = "restored-authority".to_owned();
        }
        sqlx::query(
            "UPDATE resource_pool_allocations SET remote_request = $1 WHERE allocation_id = $2",
        )
        .bind(Json(&request))
        .bind(attached.allocation_id)
        .execute(&fixture.database)
        .await?;
        let before = fixture.saved().await?;
        let mut txn = fixture.database.begin().await?;
        let error = if matches!(case, Case::Domain) {
            let descriptor = ResourcePool::<std::net::Ipv4Addr>::new(
                fixture.descriptor.name().to_owned(),
                ValueType::Ipv4,
            );
            fixture
                .registry
                .find_owned(
                    &descriptor,
                    &mut txn,
                    OwnerType::Machine,
                    "machine-1",
                    "test-allocation",
                )
                .await
                .err()
        } else {
            fixture
                .registry
                .find_owned(
                    &fixture.descriptor,
                    &mut txn,
                    OwnerType::Machine,
                    "machine-1",
                    "test-allocation",
                )
                .await
                .err()
        }
        .expect("incompatible ownership lookup must fail rather than return absence");
        match case {
            Case::Authority => assert!(matches!(
                error,
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::InvalidRecord(
                    "attached allocation belongs to a different provider authority"
                ))
            )),
            Case::Domain => assert!(matches!(
                error,
                ResourcePoolBackendError::Value(ValueConversionError::Wire(_))
            )),
            Case::Narrowing => assert!(matches!(
                error,
                ResourcePoolBackendError::Value(ValueConversionError::Consumer {
                    rust_type: "i32",
                    ..
                })
            )),
        }
        txn.commit().await?;
        assert_eq!(fixture.saved().await?, before, "{case:?}");
        assert!(
            fixture
                .provider
                .provider
                .is_provisioned(&assignment(&attached).allocation_id),
            "{case:?}"
        );
        assert_eq!(
            fixture.provider.cancel_calls.load(Ordering::SeqCst),
            0,
            "{case:?}"
        );
        assert_eq!(
            fixture.provider.release_calls.load(Ordering::SeqCst),
            0,
            "{case:?}"
        );
    }
    fixture.stop().await;
    Ok(())
}

/// Caller records and prepared requests share one transaction, including when
/// two callers independently select the same next owner-slot generation.
#[carbide_macros::sqlx_test]
async fn competing_preparations_preserve_the_callers_transaction(database: PgPool) -> TestResult {
    async fn create(
        selected: &ResourcePoolWithBackend<'_, i32>,
        database: &PgPool,
        creation_id: Uuid,
    ) -> TestResult<Option<Uuid>> {
        let mut txn = database.begin().await?;
        sqlx::query("INSERT INTO shared_test_creation (id) VALUES ($1)")
            .bind(creation_id)
            .execute(&mut *txn)
            .await?;
        match selected
            .prepare_request(
                &mut txn,
                OwnerType::Machine,
                "machine-1",
                "test-allocation",
                None,
                CompletionAuthority::Controller {
                    desire_id: creation_id,
                },
            )
            .await
        {
            Ok(prepared) => {
                let super::PreparedState::Remote { request, .. } = prepared.state else {
                    panic!("expected a prepared remote request");
                };
                assert_eq!(request.owner.generation, 0);
                sqlx::query("UPDATE shared_test_creation SET request_id = $1 WHERE id = $2")
                    .bind(request.request_id)
                    .bind(creation_id)
                    .execute(&mut *txn)
                    .await?;
                // Commit before returning: the competing INSERT waits for
                // this transaction before it can report its constraint error.
                txn.commit().await?;
                Ok(Some(request.request_id))
            }
            Err(ResourcePoolBackendError::Remote(RemoteResourcePoolError::Storage(error)))
                if matches!(
                    error.as_ref(),
                    storage::RemoteAllocationError::GenerationExists(_)
                        | storage::RemoteAllocationError::SlotInUse(_)
                ) =>
            {
                // The rejected INSERT must roll back only its savepoint.
                // Updating the caller's earlier row proves it survived and
                // that the outer transaction remains writable.
                let updated =
                    sqlx::query("UPDATE shared_test_creation SET request_id = id WHERE id = $1")
                        .bind(creation_id)
                        .execute(&mut *txn)
                        .await?;
                assert_eq!(updated.rows_affected(), 1);
                txn.rollback().await?;
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    let fixture = RemotePoolFixture::automatic(database, 1).await?;
    sqlx::query("CREATE TABLE shared_test_creation (id UUID PRIMARY KEY, request_id UUID)")
        .execute(&fixture.database)
        .await?;
    let mut txn = fixture.database.begin().await?;
    let selected = fixture
        .registry
        .select(&mut txn, &fixture.descriptor)
        .await?
        .expect("selected remote pool");
    txn.commit().await?;

    // A successful preparation also disappears with its caller's rollback.
    let discarded_creation_id = Uuid::new_v4();
    let mut txn = fixture.database.begin().await?;
    sqlx::query("INSERT INTO shared_test_creation (id) VALUES ($1)")
        .bind(discarded_creation_id)
        .execute(&mut *txn)
        .await?;
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::Machine,
            "machine-1",
            "test-allocation",
            None,
            CompletionAuthority::Controller {
                desire_id: discarded_creation_id,
            },
        )
        .await?;
    let super::PreparedState::Remote { request, .. } = prepared.state else {
        panic!("expected a prepared remote request");
    };
    let discarded_request_id = request.request_id;
    sqlx::query("UPDATE shared_test_creation SET request_id = $1 WHERE id = $2")
        .bind(discarded_request_id)
        .bind(discarded_creation_id)
        .execute(&mut *txn)
        .await?;
    txn.rollback().await?;
    assert_eq!(
        storage::load(&fixture.database, discarded_request_id).await?,
        None
    );
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM shared_test_creation WHERE id = $1)",
        )
        .bind(discarded_creation_id)
        .fetch_one(&fixture.database)
        .await?
    );

    let mut blocker = fixture.database.begin().await?;
    sqlx::query("LOCK TABLE resource_pool_allocations IN SHARE MODE")
        .execute(&mut *blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let unblock = async {
        // Both preparations must finish their history reads and reach INSERT
        // before either can commit its independently generated request UUID.
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()
                 AND wait_event_type = 'Lock' AND $1 = ANY(pg_blocking_pids(pid))
                 AND strpos(query, 'INSERT INTO resource_pool_allocations') > 0",
            )
            .bind(blocker_pid)
            .fetch_one(&fixture.database)
            .await?;
            if blocked == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        blocker.commit().await
    };
    let first_creation_id = Uuid::new_v4();
    let second_creation_id = Uuid::new_v4();
    let (first, second, unblocked) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            create(&selected, &fixture.database, first_creation_id),
            create(&selected, &fixture.database, second_creation_id),
            unblock,
        )
    })
    .await?;
    unblocked?;
    let (creation_id, request_id) = match (first?, second?) {
        (Some(request_id), None) => (first_creation_id, request_id),
        (None, Some(request_id)) => (second_creation_id, request_id),
        other => panic!("one preparation must commit and one must conflict: {other:?}"),
    };

    let creations: Vec<(Uuid, Option<Uuid>)> =
        sqlx::query_as("SELECT id, request_id FROM shared_test_creation ORDER BY id")
            .fetch_all(&fixture.database)
            .await?;
    assert_eq!(creations, vec![(creation_id, Some(request_id))]);
    let requests: Vec<Uuid> = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations
         WHERE owner_type = $1 AND owner_id = $2 AND remote_request->>'slot' = $3
         ORDER BY allocation_id",
    )
    .bind(OwnerType::Machine.to_string())
    .bind("machine-1")
    .bind("test-allocation")
    .fetch_all(&fixture.database)
    .await?;
    assert_eq!(requests, vec![request_id]);
    let saved = fixture.saved().await?;
    assert_eq!(saved.allocation_id, request_id);
    assert_eq!(saved.state, AllocationState::Pending);
    assert_eq!(saved_request(&saved).generation, 0);
    assert_eq!(
        saved_request(&saved).completion_authority,
        CompletionAuthority::Controller {
            desire_id: creation_id,
        }
    );
    assert_eq!(fixture.provider.allocation_calls.load(Ordering::SeqCst), 0);
    fixture.stop().await;
    Ok(())
}
