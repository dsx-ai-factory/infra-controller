// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_resource_pool_api::testing::LocalProvider;
use carbide_resource_pool_api::v1;
use chrono::{DateTime, Utc};
use db::ConditionalWrite;
use db::resource_pool::{binding, remote};
use db::work_lock_manager::{self, KeepaliveConfig, WorkLockManagerHandle};
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationFailure, AllocationFailureCode, AllocationOwner,
    AllocationResult, AllocationSelection, CompletionAuthority, ProviderRouteTargetMode,
    ProviderRouteTargetPolicy, RemoteAllocationRequest, RemotePoolIdentity,
};
use model::resource_pool::{
    OwnerType, ResourcePoolBackendKind, ResourcePoolSelection, ResourcePoolValueDomain,
};
use model::vpc::routing_profile::RouteTargetConfig;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::Code;
use uuid::Uuid;

use super::{RemoteResourcePool, RemoteResourcePoolError, wire, work_key};
use crate::grpc::test_server::{
    Fault, FaultProvider, Gate, MalformedResponse, TestServer, bounded, pool, provider,
};
use crate::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError};
use crate::value::ResourcePoolEligibility;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Harness {
    database: PgPool,
    work_locks: WorkLockManagerHandle,
    tasks: JoinSet<()>,
    server: TestServer,
    client: GrpcResourcePoolBackend,
    remote: RemoteResourcePool,
    fault: FaultProvider,
    request: RemoteAllocationRequest,
    authority: CompletionAuthority,
    eligibility: ResourcePoolEligibility,
    cancel: CancellationToken,
}

impl Harness {
    async fn new(database: PgPool, local: LocalProvider, fault: Fault) -> Self {
        let fault = FaultProvider::new(local, fault);
        let server = TestServer::start(fault.clone()).await;
        let client = GrpcResourcePoolBackend::new(server.endpoint.clone(), None).unwrap();
        let mut tasks = JoinSet::new();
        // Avoid repeated heartbeat work during these short lifecycle tests.
        let work_locks = work_lock_manager::start(
            &mut tasks,
            database.clone(),
            KeepaliveConfig {
                interval: Duration::from_secs(3600),
                timeout: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
        let mut txn = database.begin().await.unwrap();
        let binding = binding::bind_or_verify(
            &mut txn,
            &ResourcePoolSelection {
                pool_name: "vpc-vni".into(),
                value_domain: ResourcePoolValueDomain::Integer,
                backend_name: Some("fabric".into()),
                backend_kind: ResourcePoolBackendKind::Grpc,
                authority_id: pool().authority_id,
                remote_pool: pool().pool_id,
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        let request = RemoteAllocationRequest {
            request_id: Uuid::new_v4(),
            binding_id: binding.binding_id,
            pool: RemotePoolIdentity {
                authority_id: binding.authority_id.clone(),
                pool_id: pool().pool_id,
                logical_pool: binding.pool_name,
            },
            owner: AllocationOwner {
                owner_type: OwnerType::Vpc,
                owner_id: Uuid::new_v4().to_string(),
                slot: "vni".into(),
                generation: 0,
            },
            eligibility_revision: "initial".into(),
            selection: AllocationSelection::Automatic,
        };
        let eligibility = ResourcePoolEligibility {
            domain: ResourcePoolValueDomain::Integer,
            entries: vec![
                v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Automatic as i32,
                    entries: Some(v1::eligibility::Entries::Range(v1::ValueRange {
                        start: Some(1.into()),
                        end: Some(5.into()),
                    })),
                },
                v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Manual as i32,
                    entries: Some(v1::eligibility::Entries::Value(5.into())),
                },
            ],
        };
        client
            .define(v1::DefineRequest {
                pool: Some(v1::PoolBinding {
                    authority_id: request.pool.authority_id.clone(),
                    pool_id: request.pool.pool_id.clone(),
                    binding_id: request.binding_id.to_string(),
                    logical_pool: request.pool.logical_pool.clone(),
                }),
                eligibility_revision: request.eligibility_revision.clone(),
                previous_revision: None,
                value_domain: v1::ValueDomain::Integer as i32,
                eligibility: eligibility.entries.clone(),
            })
            .await
            .unwrap();
        let remote = RemoteResourcePool::new(
            database.clone(),
            work_locks.clone(),
            binding.authority_id,
            client.clone(),
        );
        Self {
            database,
            work_locks,
            tasks,
            server,
            client,
            remote,
            fault,
            request,
            authority: CompletionAuthority::Controller {
                desire_id: Uuid::new_v4(),
            },
            eligibility,
            cancel: CancellationToken::new(),
        }
    }

    // Recreate the coordinator while sharing its database, provider, client,
    // and work-lock manager.
    fn restarted(&self) -> RemoteResourcePool {
        RemoteResourcePool::new(
            self.database.clone(),
            self.work_locks.clone(),
            self.request.pool.authority_id.clone(),
            self.client.clone(),
        )
    }

    async fn saved(&self) -> ResourcePoolAllocation {
        remote::load(&self.database, self.request.request_id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn allocate(&self) -> AllocationAssignment {
        bounded(self.remote.allocate(
            &self.request,
            &self.authority,
            &self.eligibility,
            &self.cancel,
        ))
        .await
        .unwrap()
    }

    // Persist intent and allocate at the provider, leaving Core's result unset.
    async fn provision(&self) -> v1::Allocation {
        assert_eq!(self.request.selection, AllocationSelection::Automatic);
        let record = begin_request(&self.database, &self.request, &self.authority)
            .await
            .unwrap();
        self.client
            .allocate(v1::AllocateRequest {
                operation: Some(wire::operation(&record).unwrap()),
                requested_value: None,
            })
            .await
            .unwrap()
            .allocation
            .unwrap()
    }

    async fn save_assignment(&self, allocated: v1::Allocation) -> AllocationAssignment {
        let assignment = wire::assignment(&self.saved().await, allocated).unwrap();
        let mut txn = self.database.begin().await.unwrap();
        assert_eq!(
            remote::record_result(
                &mut txn,
                self.request.request_id,
                &AllocationResult::Allocated(assignment.clone()),
            )
            .await
            .unwrap(),
            ConditionalWrite::Applied(()),
        );
        txn.commit().await.unwrap();
        assignment
    }

    async fn attach(&self, assignment: &AllocationAssignment) {
        let mut txn = self.database.begin().await.unwrap();
        assert_eq!(
            remote::attach(
                &mut txn,
                self.request.request_id,
                &self.authority,
                assignment
            )
            .await
            .unwrap(),
            ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();
    }

    async fn request_release(&self, assignment: &AllocationAssignment) {
        let mut txn = self.database.begin().await.unwrap();
        assert_eq!(
            remote::request_release(&mut txn, self.request.request_id, &assignment.allocation_id)
                .await
                .unwrap(),
            ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();
    }

    async fn stop(self) {
        let Self {
            remote,
            work_locks,
            tasks,
            server,
            ..
        } = self;
        drop(remote);
        drop(work_locks);
        bounded(tasks.join_all()).await;
        server.stop().await;
    }
}

async fn begin_request(
    database: &PgPool,
    request: &RemoteAllocationRequest,
    authority: &CompletionAuthority,
) -> Result<ResourcePoolAllocation, Box<dyn std::error::Error>> {
    let mut txn = database.begin().await?;
    let record = remote::begin(&mut txn, request, authority).await?;
    txn.commit().await?;
    Ok(record)
}

fn assignment(record: &ResourcePoolAllocation) -> &AllocationAssignment {
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(value)),
        ..
    } = &record.source
    else {
        panic!("expected a saved assignment: {record:?}");
    };
    value
}

#[carbide_macros::sqlx_test]
async fn lost_reply_and_restart_recover_one_assignment(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let mut harness = Harness::new(
        database,
        provider(),
        Fault::WithholdAllocation(gate.clone()),
    )
    .await;
    // A lower automatic value remains free, so recovery must preserve `Exact`.
    harness.request.selection = AllocationSelection::Exact(ResourcePoolValue::Integer(4));
    let mut pending = Box::pin(harness.remote.allocate_exact(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation passed its reply gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    let committed = harness.fault.committed.lock().unwrap().clone().unwrap();
    assert_eq!(harness.saved().await.state, AllocationState::Pending);
    // The caller dies after provider commitment, bypassing normal abandonment.
    drop(pending);
    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::Pending);
    assert_eq!(assignment(&saved).allocation_id, committed.allocation_id);
    let recovered = bounded(restarted.allocate_exact(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ))
    .await?;
    assert_eq!(&recovered, assignment(&saved));
    assert_eq!(recovered.value, ResourcePoolValue::Integer(4));
    assert_eq!(
        recovered.route_target_policy,
        Some(ProviderRouteTargetPolicy {
            mode: ProviderRouteTargetMode::Replace,
            imports: Some(vec![]),
            exports: Some(vec![
                RouteTargetConfig {
                    asn: 64512,
                    vni: Some(1),
                    vni_self: false,
                },
                RouteTargetConfig {
                    asn: 64512,
                    vni: Some(1),
                    vni_self: false,
                },
            ]),
            peer_identity: vec![RouteTargetConfig {
                asn: 64512,
                vni: Some(1),
                vni_self: false,
            }],
        }),
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    // Recovery saved the result, but it did not attach on the caller's behalf.
    harness.attach(&recovered).await;
    assert_eq!(harness.saved().await.state, AllocationState::Attached);
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    assert_eq!(harness.saved().await.state, AllocationState::Attached);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&recovered.allocation_id)
    );
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 0);
    gate.release.notify_waiters();
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn lost_release_reply_is_recovered_without_releasing_a_reused_value(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, provider(), Fault::ReleaseReplyUnavailableOnce).await;
    let deadline: DateTime<Utc> =
        sqlx::query_scalar("SELECT clock_timestamp() + interval '5 minutes'")
            .fetch_one(&harness.database)
            .await?;
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: deadline,
    };
    // Leave the first automatic value free so an accidental `Allocate` call
    // cannot satisfy this exact request.
    harness.request.selection = AllocationSelection::Exact(ResourcePoolValue::Integer(4));
    let allocated = bounded(harness.remote.allocate_exact(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ))
    .await?;
    assert_eq!(allocated.value, ResourcePoolValue::Integer(4));
    assert_eq!(allocated.assignment_mode, AssignmentMode::Automatic);
    let saved = harness.saved().await;
    let AllocationSource::Remote { request, .. } = saved.source else {
        panic!("expected the saved exact request");
    };
    assert_eq!(request.selection, harness.request.selection);
    harness.attach(&allocated).await;
    let recover_calls = harness.fault.recover_calls.load(Ordering::SeqCst);
    assert!(matches!(
        harness
            .remote
            .allocate_exact(
                &harness.request,
                &harness.authority,
                &harness.eligibility,
                &harness.cancel,
            )
            .await,
        Err(RemoteResourcePoolError::NotCurrent),
    ));
    assert_eq!(harness.saved().await.state, AllocationState::Attached);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(
        harness.fault.recover_calls.load(Ordering::SeqCst),
        recover_calls
    );
    harness.request_release(&allocated).await;
    let error = bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await
    .expect_err("release committed before its reply was lost");
    assert_eq!(error.provider_code(), Some(Code::Unavailable));
    assert_eq!(
        harness.saved().await.state,
        AllocationState::ReleaseRequested
    );
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    let restarted = harness.restarted();
    let mut replacement = harness.request.clone();
    replacement.request_id = Uuid::new_v4();
    replacement.owner.generation += 1;
    replacement.selection = AllocationSelection::Exact(allocated.value.clone());
    let next = bounded(restarted.allocate_exact(
        &replacement,
        &CompletionAuthority::Controller {
            desire_id: Uuid::new_v4(),
        },
        &harness.eligibility,
        &harness.cancel,
    ))
    .await?;
    assert_eq!(next.value, allocated.value);
    assert_eq!(next.assignment_mode, AssignmentMode::Automatic);
    assert_ne!(next.allocation_id, allocated.allocation_id);
    assert_eq!(
        harness.saved().await.state,
        AllocationState::ReleaseRequested
    );
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    assert_eq!(harness.saved().await.state, AllocationState::Released);
    assert!(harness.fault.provider.is_provisioned(&next.allocation_id));
    assert_eq!(harness.fault.release_calls.load(Ordering::SeqCst), 1);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn lost_cancel_reply_is_confirmed_after_restart(database: PgPool) -> TestResult {
    // The provider can finish cleanup while Core still has only its saved
    // cancellation request. Restart must confirm that same operation.
    let harness = Harness::new(database, provider(), Fault::CancelReplyUnavailableOnce).await;
    let allocated = harness.save_assignment(harness.provision().await).await;
    let mut txn = harness.database.begin().await?;
    assert_eq!(
        remote::request_cancel(&mut txn, harness.request.request_id).await?,
        ConditionalWrite::Applied(())
    );
    txn.commit().await?;
    let before = harness.saved().await;

    let error = bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await
    .expect_err("provider cleanup completed before its Cancel reply was lost");
    assert_eq!(error.provider_code(), Some(Code::Unavailable));
    assert_eq!(harness.saved().await, before);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );

    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::Cancelled);
    assert_eq!(saved.allocation_id, before.allocation_id);
    assert_eq!(saved.source, before.source);
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn pending_cleanup_survives_restart_and_keeps_the_reservation(
    database: PgPool,
) -> TestResult {
    let complete_before_recover = Arc::new(AtomicBool::new(false));
    let mut harness = Harness::new(
        database,
        provider().with_pending_cancellation(),
        Fault::CancelUnavailableOnce {
            complete_before_recover: complete_before_recover.clone(),
        },
    )
    .await;
    // Seed an attempt whose foreground caller has already expired. The provider
    // accepted it earlier; there is no live caller authorized to attach it now.
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: sqlx::query_scalar("SELECT clock_timestamp() - interval '1 second'")
            .fetch_one(&harness.database)
            .await?,
    };
    let saved = begin_request(&harness.database, &harness.request, &harness.authority).await?;
    let allocated = harness
        .client
        .allocate(v1::AllocateRequest {
            operation: Some(wire::operation(&saved)?),
            requested_value: None,
        })
        .await?
        .allocation
        .unwrap();
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    harness.save_assignment(allocated.clone()).await;
    let saved = harness.saved().await;
    let error = bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await
    .expect_err("the first cancellation failed before cleanup");
    assert_eq!(error.provider_code(), Some(Code::Unavailable));
    assert_eq!(
        harness.saved().await.state,
        AllocationState::CancelRequested
    );
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    assert_eq!(
        harness.saved().await.state,
        AllocationState::CancelRequested
    );
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 1);
    let mut competing = wire::operation(&saved)?;
    competing.request_id = Uuid::new_v4().to_string();
    let error = harness
        .client
        .allocate_exact(v1::AllocateExactRequest {
            operation: Some(competing),
            value: allocated.value.clone(),
        })
        .await
        .expect_err("pending cancellation retains the value");
    assert!(
        matches!(error, GrpcResourcePoolError::Rpc(status) if status.code() == Code::FailedPrecondition)
    );
    drop(restarted);
    let restarted = harness.restarted();
    // `Cancel` still returns `Pending`. The provider finishes cleanup before
    // the following `Recover`, and this iteration must save that confirmation.
    complete_before_recover.store(true, Ordering::SeqCst);
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    let cancelled = harness.saved().await;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.allocation_id, saved.allocation_id);
    assert_eq!(cancelled.source, saved.source);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 3);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 2);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn transient_calls_recover_the_original_assignment_without_duplicate_allocation(
    database: PgPool,
) -> TestResult {
    struct Case {
        name: &'static str,
        fault: Fault,
        selection: AllocationSelection,
        value: ResourcePoolValue,
        mode: AssignmentMode,
    }
    // Keep one provider for this authority so saved handles remain unique.
    let local = provider();
    for case in [
        Case {
            name: "recovery retries before a requested allocation",
            fault: Fault::RecoverUnavailableOnce,
            selection: AllocationSelection::Requested(ResourcePoolValue::Integer(5)),
            value: ResourcePoolValue::Integer(5),
            mode: AssignmentMode::Manual,
        },
        Case {
            name: "unavailable reply recovers the committed allocation",
            fault: Fault::AllocationUnavailable,
            selection: AllocationSelection::Automatic,
            value: ResourcePoolValue::Integer(1),
            mode: AssignmentMode::Automatic,
        },
        Case {
            name: "internal reply error recovers the committed allocation",
            fault: Fault::AllocationInternal,
            selection: AllocationSelection::Automatic,
            value: ResourcePoolValue::Integer(2),
            mode: AssignmentMode::Automatic,
        },
    ] {
        let mut harness = Harness::new(database.clone(), local.clone(), case.fault).await;
        harness.request.selection = case.selection;
        let allocated = harness.allocate().await;
        assert_eq!(allocated.value, case.value, "{}", case.name);
        assert_eq!(allocated.assignment_mode, case.mode, "{}", case.name);
        if case.mode == AssignmentMode::Manual {
            assert_eq!(harness.allocate().await, allocated, "{}", case.name);
        }
        assert_eq!(
            harness.fault.recover_calls.load(Ordering::SeqCst),
            2,
            "{}",
            case.name
        );
        assert_eq!(
            harness.fault.allocation_calls.load(Ordering::SeqCst),
            1,
            "{}",
            case.name
        );
        assert_eq!(
            assignment(&harness.saved().await),
            &allocated,
            "{}",
            case.name
        );
        assert_eq!(
            allocated.allocation_id,
            harness
                .fault
                .committed
                .lock()
                .unwrap()
                .as_ref()
                .expect("provider committed an allocation")
                .allocation_id,
            "{}",
            case.name
        );
        assert!(
            harness
                .fault
                .provider
                .is_provisioned(&allocated.allocation_id),
            "{}",
            case.name
        );
        harness.stop().await;
    }
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn cancelled_dispatch_does_not_call_the_provider(database: PgPool) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::None).await;
    let saved = begin_request(&harness.database, &harness.request, &harness.authority).await?;
    harness.cancel.cancel();
    // Exercise the dispatch checkpoint itself, after an enclosing wait could
    // already have polled the foreground future.
    let result = bounded(harness.remote.dispatch(&saved, &harness.cancel)).await;
    assert!(matches!(result, Err(RemoteResourcePoolError::Cancelled)));
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn cancellation_before_delayed_allocation_prevents_late_provisioning(
    database: PgPool,
) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(database, provider(), Fault::DelayAllocation(gate.clone())).await;
    let mut pending = Box::pin(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation passed its dispatch gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    harness.cancel.cancel();
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::Cancelled)
    ));
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::CancelRequested);
    let running = CancellationToken::new();
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &running),
    )
    .await?;
    assert_eq!(harness.saved().await.state, AllocationState::Cancelled);
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 1);
    gate.release.notify_waiters();
    bounded(gate.allocation_finished.notified()).await;
    assert_eq!(harness.fault.provider.provisioned_count(), 0);
    // The provider also rejects a delayed replay if transport cancellation
    // happened to discard the original server future before its gate opened.
    let error = harness
        .client
        .allocate(v1::AllocateRequest {
            operation: Some(wire::operation(&saved)?),
            requested_value: None,
        })
        .await
        .expect_err("terminal cancellation rejects the original allocation over gRPC");
    assert!(
        matches!(error, GrpcResourcePoolError::Rpc(status) if status.code() == Code::FailedPrecondition)
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 2);
    assert_eq!(harness.fault.provider.provisioned_count(), 0);
    // A new operation can still provision through the same provider. This
    // makes the absence above meaningful even when the old RPC was dropped.
    let mut replacement = harness.request.clone();
    replacement.request_id = Uuid::new_v4();
    replacement.owner.generation += 1;
    let allocated = bounded(harness.remote.allocate(
        &replacement,
        &CompletionAuthority::Controller {
            desire_id: Uuid::new_v4(),
        },
        &harness.eligibility,
        &running,
    ))
    .await?;
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.provider.provisioned_count(), 1);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn worker_takeover_prevents_saving_an_intent_with_the_previous_guard(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::None).await;
    let work_key = work_key(harness.request.request_id);
    let guard = harness
        .work_locks
        .try_acquire_lock(work_key.clone())
        .await?;
    let mut takeover = harness.database.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *takeover)
        .await?;
    let new_worker_id: Uuid = sqlx::query_scalar(
        "UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1 RETURNING worker_id",
    )
    .bind(&work_key)
    .fetch_one(&mut *takeover)
    .await?;

    // Hold the replacement uncommitted until the old worker reaches its fence.
    // Once it resumes, it must see the new worker before inserting any intent.
    let mut pending = Box::pin(harness.remote.save_intent(
        &guard,
        &harness.request,
        &harness.authority,
    ));
    tokio::select! {
        result = &mut pending => panic!("intent write did not wait for takeover: {result:?}"),
        _ = bounded(db::test_support::postgres::wait_for_blocked_query(
            &harness.database, blocker_pid, "FROM work_locks",
        )) => {}
    }
    takeover.commit().await?;
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::Database(
            db::DatabaseError::FailedPrecondition(_)
        ))
    ));
    assert!(
        remote::load(&harness.database, harness.request.request_id)
            .await?
            .is_none()
    );
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        guard.release().await,
        Err(work_lock_manager::ReleaseLockError::Database(
            db::DatabaseError::FailedPrecondition(_)
        ))
    ));
    let current_worker_id: Uuid =
        sqlx::query_scalar("SELECT worker_id FROM work_locks WHERE work_key = $1")
            .bind(&work_key)
            .fetch_one(&harness.database)
            .await?;
    assert_eq!(current_worker_id, new_worker_id);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn worker_takeover_fences_a_reply_from_the_previous_worker(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(
        database,
        provider(),
        Fault::WithholdAllocation(gate.clone()),
    )
    .await;
    let mut pending = Box::pin(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation passed its reply gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    // A recovery pass skips the live caller's lease without touching its work.
    let before_recovery = harness.saved().await;
    let recover_calls = harness.fault.recover_calls.load(Ordering::SeqCst);
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    assert_eq!(harness.saved().await, before_recovery);
    assert_eq!(
        harness.fault.recover_calls.load(Ordering::SeqCst),
        recover_calls
    );
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 0);

    // Replace the old worker identity as well as expiring its lease. Its first
    // heartbeat can no longer renew the row before the replacement acquires it.
    sqlx::query(
        "UPDATE work_locks
        SET worker_id = gen_random_uuid(),
            last_keepalive = clock_timestamp() - interval '2 minutes'
        WHERE work_key = $1",
    )
    .bind(work_key(harness.request.request_id))
    .execute(&harness.database)
    .await?;
    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    let recovered = harness.saved().await;
    harness.attach(assignment(&recovered)).await;
    let attached = harness.saved().await;
    gate.release.notify_waiters();
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::Database(
            db::DatabaseError::FailedPrecondition(_)
        ))
    ));
    assert_eq!(harness.saved().await, attached);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&assignment(&recovered).allocation_id)
    );
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.release_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn allocation_domains_must_match_the_binding_before_saving_intent(
    database: PgPool,
) -> TestResult {
    struct Case {
        eligibility_domain: ResourcePoolValueDomain,
        selection: AllocationSelection,
        expected_error: &'static str,
    }
    let mut harness = Harness::new(database, provider(), Fault::None).await;
    for case in [
        Case {
            eligibility_domain: ResourcePoolValueDomain::IpAddress,
            selection: AllocationSelection::Automatic,
            expected_error: "request authority or eligibility domain differs from the binding",
        },
        Case {
            eligibility_domain: ResourcePoolValueDomain::Integer,
            selection: AllocationSelection::Requested(ResourcePoolValue::IpAddress(
                "192.0.2.1".parse().unwrap(),
            )),
            expected_error: "selection differs from the bound value domain",
        },
    ] {
        harness.eligibility.domain = case.eligibility_domain;
        harness.request.selection = case.selection;
        let error = bounded(harness.remote.allocate(
            &harness.request,
            &harness.authority,
            &harness.eligibility,
            &harness.cancel,
        ))
        .await
        .expect_err(case.expected_error);
        assert!(
            matches!(error, RemoteResourcePoolError::InvalidRecord(message) if message == case.expected_error),
            "{}",
            case.expected_error
        );
        assert!(
            remote::load(&harness.database, harness.request.request_id)
                .await?
                .is_none(),
            "{}",
            case.expected_error
        );
        assert_eq!(
            harness.fault.recover_calls.load(Ordering::SeqCst),
            0,
            "{}",
            case.expected_error
        );
        assert_eq!(
            harness.fault.allocation_calls.load(Ordering::SeqCst),
            0,
            "{}",
            case.expected_error
        );
    }
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn capacity_is_shared_by_clones_and_rejects_before_saving_intent(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::None).await;
    let permit = harness
        .remote
        .operations
        .try_acquire_many(super::MAX_OPERATIONS as u32)
        .unwrap();
    let other = harness.remote.clone();
    assert!(matches!(
        other
            .allocate(
                &harness.request,
                &harness.authority,
                &harness.eligibility,
                &harness.cancel
            )
            .await,
        Err(RemoteResourcePoolError::Busy)
    ));
    assert!(
        remote::load(&harness.database, harness.request.request_id)
            .await?
            .is_none()
    );
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        other
            .reconcile(harness.request.request_id, &harness.cancel)
            .await,
        Err(RemoteResourcePoolError::Busy)
    ));
    drop(permit);
    // A competing caller must acquire the request lease before saving intent.
    let held = harness
        .work_locks
        .try_acquire_lock(work_key(harness.request.request_id))
        .await?;
    assert!(matches!(
        bounded(other.allocate(
            &harness.request,
            &harness.authority,
            &harness.eligibility,
            &harness.cancel,
        ))
        .await,
        Err(RemoteResourcePoolError::Acquire(
            work_lock_manager::AcquireLockError::WorkAlreadyLocked(_)
        ))
    ));
    assert!(
        remote::load(&harness.database, harness.request.request_id)
            .await?
            .is_none()
    );
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    held.release().await?;
    harness.allocate().await;
    drop(other);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn recovery_advances_past_unchanged_records_and_joins_on_shutdown(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, provider(), Fault::ReleaseUnavailableOnce).await;
    let cancelled_id = harness.request.request_id;
    let cancelled_assignment = harness.provision().await;
    let mut txn = harness.database.begin().await?;
    assert_eq!(
        remote::request_cancel(&mut txn, cancelled_id).await?,
        ConditionalWrite::Applied(()),
    );
    txn.commit().await?;

    harness.request.request_id = Uuid::new_v4();
    harness.request.owner.owner_id = Uuid::new_v4().to_string();
    let released_id = harness.request.request_id;
    let released_assignment = harness.save_assignment(harness.provision().await).await;
    harness.attach(&released_assignment).await;
    harness.request_release(&released_assignment).await;
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&cancelled_assignment.allocation_id)
    );
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&released_assignment.allocation_id)
    );
    let allocation_calls = harness.fault.allocation_calls.load(Ordering::SeqCst);
    let expired_at = sqlx::query_scalar("SELECT clock_timestamp() - interval '1 second'")
        .fetch_one(&harness.database)
        .await?;
    let mut last_request = harness.request.clone();
    // Sixteen unobserved controller requests stay pending. A later foreground
    // request must still be found and cancelled on the following scan page.
    for id in 1..=17 {
        let request = RemoteAllocationRequest {
            request_id: Uuid::from_u128(id),
            owner: AllocationOwner {
                owner_id: Uuid::new_v4().to_string(),
                ..harness.request.owner.clone()
            },
            ..harness.request.clone()
        };
        let authority = if id == 17 {
            CompletionAuthority::Foreground {
                attempt_id: Uuid::new_v4(),
                attach_before: expired_at,
            }
        } else {
            harness.authority.clone()
        };
        begin_request(&harness.database, &request, &authority).await?;
        last_request = request;
    }
    let mut tasks = JoinSet::new();
    let shutdown = CancellationToken::new();
    harness.restarted().start(&mut tasks, shutdown.clone())?;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if remote::load(&harness.database, last_request.request_id)
                .await
                .unwrap()
                .unwrap()
                .state
                == AllocationState::Cancelled
                && remote::load(&harness.database, cancelled_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
                    == AllocationState::Cancelled
                && remote::load(&harness.database, released_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state
                    == AllocationState::Released
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    shutdown.cancel();
    bounded(tasks.join_all()).await;
    assert_eq!(
        remote::load(&harness.database, Uuid::from_u128(1))
            .await?
            .unwrap()
            .state,
        AllocationState::Pending
    );
    assert_eq!(
        harness.fault.allocation_calls.load(Ordering::SeqCst),
        allocation_calls
    );
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&cancelled_assignment.allocation_id)
    );
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&released_assignment.allocation_id)
    );
    assert_eq!(harness.fault.release_calls.load(Ordering::SeqCst), 2);
    harness.stop().await;
    Ok(())
}

/// A pass must return to earlier requests before following newly inserted IDs.
/// Otherwise a busy request can wait forever while later pages keep filling up.
#[carbide_macros::sqlx_test]
async fn recovery_revisits_earlier_requests_while_new_requests_arrive(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, provider(), Fault::None).await;
    harness.request.request_id = Uuid::from_u128(1);
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: sqlx::query_scalar("SELECT clock_timestamp() - interval '1 second'")
            .fetch_one(&harness.database)
            .await?,
    };
    let allocated = harness.provision().await;

    // Keep two full pages in Pending. Leave the expired first request unchanged
    // to represent work skipped during the first pass.
    for id in 2..=32 {
        let request = RemoteAllocationRequest {
            request_id: Uuid::from_u128(id),
            owner: AllocationOwner {
                owner_id: Uuid::new_v4().to_string(),
                ..harness.request.owner.clone()
            },
            ..harness.request.clone()
        };
        begin_request(
            &harness.database,
            &request,
            &CompletionAuthority::Controller {
                desire_id: Uuid::new_v4(),
            },
        )
        .await?;
    }
    let mut cursor = None;
    let first_page = harness
        .remote
        .next_page(AllocationState::Pending, &mut cursor)
        .await?;
    assert_eq!(
        first_page
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );

    // Add another full page after the pass has captured its boundary. These
    // requests must wait for the next pass instead of extending this one.
    for id in 33..=48 {
        let request = RemoteAllocationRequest {
            request_id: Uuid::from_u128(id),
            owner: AllocationOwner {
                owner_id: Uuid::new_v4().to_string(),
                ..harness.request.owner.clone()
            },
            ..harness.request.clone()
        };
        begin_request(&harness.database, &request, &harness.authority).await?;
    }
    let second_page = harness
        .remote
        .next_page(AllocationState::Pending, &mut cursor)
        .await?;
    assert_eq!(
        second_page
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        (17..=32).collect::<Vec<_>>()
    );
    let next_pass = harness
        .remote
        .next_page(AllocationState::Pending, &mut cursor)
        .await?;
    assert_eq!(
        next_pass
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );

    // The next pass revisits the expired request, whose original operation can
    // still be cancelled without allocating another value.
    assert_eq!(harness.saved().await.state, AllocationState::Pending);
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    assert_eq!(harness.saved().await.state, AllocationState::Cancelled);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

/// One provider call that uses its whole recovery budget must not stop later
/// cleanup or prevent the scanner from joining when shutdown is requested.
#[carbide_macros::sqlx_test]
async fn recovery_continues_after_a_record_uses_its_entire_budget(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let stalled_request_id = Uuid::from_u128(1);
    let mut harness = Harness::new(
        database,
        provider(),
        Fault::StallRecoveryForRequest {
            request_id: stalled_request_id.to_string(),
            gate: gate.clone(),
        },
    )
    .await;
    harness.request.request_id = stalled_request_id;
    let stalled = harness.save_assignment(harness.provision().await).await;
    harness.attach(&stalled).await;
    harness.request_release(&stalled).await;
    let stalled_record = harness.saved().await;

    harness.request.request_id = Uuid::from_u128(2);
    harness.request.owner.owner_id = Uuid::new_v4().to_string();
    let later = harness.save_assignment(harness.provision().await).await;
    harness.attach(&later).await;
    harness.request_release(&later).await;

    // Keep the provider deadline longer than the coordinator's budget. This
    // request stays blocked across passes, while the later request can finish.
    let slow_client = harness
        .client
        .clone()
        .with_timeouts(Duration::from_secs(60), Duration::from_secs(60));
    let remote = RemoteResourcePool::new(
        harness.database.clone(),
        harness.work_locks.clone(),
        harness.request.pool.authority_id.clone(),
        slow_client,
    );
    let mut tasks = JoinSet::new();
    let shutdown = CancellationToken::new();
    remote.start(&mut tasks, shutdown.clone())?;
    bounded(gate.entered.notified()).await;
    // Advance past the coordinator's deadline while the longer provider
    // deadline remains pending. Resume real time for the remaining database work.
    tokio::time::pause();
    tokio::time::advance(super::RECONCILE_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(10), async {
        while harness.saved().await.state != AllocationState::Released {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;

    let saved_stalled = remote::load(&harness.database, Uuid::from_u128(1))
        .await?
        .expect("stalled release");
    assert_eq!(saved_stalled.state, AllocationState::ReleaseRequested);
    assert_eq!(saved_stalled.source, stalled_record.source);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&stalled.allocation_id)
    );
    assert!(!harness.fault.provider.is_provisioned(&later.allocation_id));
    assert_eq!(harness.fault.release_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 2);
    shutdown.cancel();
    bounded(tasks.join_all()).await;
    gate.release.notify_waiters();
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn invalid_recovery_cancels_the_original_request(database: PgPool) -> TestResult {
    let local = provider();
    for (scenario, fault) in [
        ("saved owner differs", Fault::RecoveredOwnerMismatch),
        ("bound value domain differs", Fault::RecoveredWrongDomain),
        (
            "failure diagnostic contains NUL",
            Fault::RecoveredFailureWithNul,
        ),
        (
            "recovery identity differs",
            Fault::Malformed(MalformedResponse::Recover),
        ),
    ] {
        let harness = Harness::new(database.clone(), local.clone(), fault).await;
        let allocated = harness.provision().await;
        let result = bounded(
            harness
                .remote
                .reconcile(harness.request.request_id, &harness.cancel),
        )
        .await;
        assert!(
            matches!(result, Err(RemoteResourcePoolError::InvalidResponse(_))),
            "{scenario}: {result:?}"
        );
        let saved = harness.saved().await;
        assert_eq!(saved.state, AllocationState::CancelRequested, "{scenario}");
        assert!(
            matches!(saved.source, AllocationSource::Remote { result: None, .. }),
            "{scenario}"
        );
        assert!(
            harness
                .fault
                .provider
                .is_provisioned(&allocated.allocation_id),
            "{scenario}"
        );
        bounded(
            harness
                .remote
                .reconcile(harness.request.request_id, &harness.cancel),
        )
        .await?;
        assert_eq!(
            harness.saved().await.state,
            AllocationState::Cancelled,
            "{scenario}"
        );
        assert!(
            !harness
                .fault
                .provider
                .is_provisioned(&allocated.allocation_id),
            "{scenario}"
        );
        assert_eq!(
            harness.fault.allocation_calls.load(Ordering::SeqCst),
            1,
            "{scenario}"
        );
        harness.stop().await;
    }
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn release_retries_after_a_failure_before_provider_cleanup(database: PgPool) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::ReleaseUnavailableOnce).await;
    let allocated = harness.allocate().await;
    harness.attach(&allocated).await;
    harness.request_release(&allocated).await;
    let error = bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await
    .expect_err("the first release failed before cleanup");
    assert_eq!(error.provider_code(), Some(Code::Unavailable));
    assert_eq!(
        harness.saved().await.state,
        AllocationState::ReleaseRequested
    );
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
    assert_eq!(harness.saved().await.state, AllocationState::Released);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.release_calls.load(Ordering::SeqCst), 2);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn ambiguous_cleanup_preserves_the_saved_assignment_until_confirmed(
    database: PgPool,
) -> TestResult {
    struct Case {
        scenario: &'static str,
        fault: Fault,
        check_error: fn(&RemoteResourcePoolError),
        release_calls: usize,
    }
    let local = provider();
    for case in [
        Case {
            scenario: "unknown recovery is not cleanup",
            fault: Fault::RecoverUnknownOnce,
            check_error: |error| assert!(matches!(error, RemoteResourcePoolError::Incomplete)),
            release_calls: 1,
        },
        Case {
            scenario: "not found is not cleanup",
            fault: Fault::RecoverNotFoundOnce,
            check_error: |error| assert_eq!(error.provider_code(), Some(Code::NotFound)),
            release_calls: 1,
        },
        Case {
            scenario: "false release is not cleanup",
            fault: Fault::ReleaseNotAppliedOnce,
            check_error: |error| assert!(matches!(error, RemoteResourcePoolError::Incomplete)),
            release_calls: 2,
        },
        Case {
            scenario: "changed routing policy cannot replace the saved assignment",
            fault: Fault::RecoveredPolicyMismatchOnce,
            check_error: |error| {
                assert!(matches!(error, RemoteResourcePoolError::InvalidResponse(_)))
            },
            release_calls: 1,
        },
    ] {
        let harness = Harness::new(database.clone(), local.clone(), case.fault).await;
        let allocated = harness.save_assignment(harness.provision().await).await;
        harness.attach(&allocated).await;
        harness.request_release(&allocated).await;
        let before = harness.saved().await;
        let error = bounded(
            harness
                .remote
                .reconcile(harness.request.request_id, &harness.cancel),
        )
        .await
        .expect_err(case.scenario);
        (case.check_error)(&error);
        assert_eq!(harness.saved().await, before, "{}", case.scenario);
        if matches!(error, RemoteResourcePoolError::InvalidResponse(_)) {
            assert_eq!(
                harness.fault.release_calls.load(Ordering::SeqCst),
                0,
                "{}",
                case.scenario
            );
        }
        assert!(
            harness
                .fault
                .provider
                .is_provisioned(&allocated.allocation_id),
            "{}",
            case.scenario
        );

        let restarted = harness.restarted();
        bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
        let saved = harness.saved().await;
        assert_eq!(saved.state, AllocationState::Released, "{}", case.scenario);
        assert_eq!(assignment(&saved), &allocated, "{}", case.scenario);
        assert!(
            !harness
                .fault
                .provider
                .is_provisioned(&allocated.allocation_id),
            "{}",
            case.scenario
        );
        assert_eq!(
            harness.fault.release_calls.load(Ordering::SeqCst),
            case.release_calls,
            "{}",
            case.scenario
        );
        drop(restarted);
        harness.stop().await;
    }
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn unusable_foreground_success_requests_cleanup_without_saving_a_result(
    database: PgPool,
) -> TestResult {
    struct Case {
        scenario: &'static str,
        fault: Fault,
        check_error: fn(&RemoteResourcePoolError),
    }
    let local = provider();
    for case in [
        Case {
            scenario: "the successful response names another request",
            fault: Fault::MismatchedOperation,
            check_error: |error| {
                assert!(matches!(
                    error,
                    RemoteResourcePoolError::Provider(GrpcResourcePoolError::InvalidResponse(_))
                ))
            },
        },
        Case {
            scenario: "the successful response is outside accepted eligibility",
            fault: Fault::IneligibleAllocation,
            check_error: |error| assert!(matches!(error, RemoteResourcePoolError::Value(_))),
        },
    ] {
        let harness = Harness::new(database.clone(), local.clone(), case.fault).await;
        let error = bounded(harness.remote.allocate(
            &harness.request,
            &harness.authority,
            &harness.eligibility,
            &harness.cancel,
        ))
        .await
        .expect_err(case.scenario);
        (case.check_error)(&error);
        let saved = harness.saved().await;
        assert_eq!(
            saved.state,
            AllocationState::CancelRequested,
            "{}",
            case.scenario
        );
        assert!(
            matches!(saved.source, AllocationSource::Remote { result: None, .. }),
            "{}",
            case.scenario
        );
        let committed = harness.fault.committed.lock().unwrap().clone().unwrap();
        assert!(
            harness
                .fault
                .provider
                .is_provisioned(&committed.allocation_id)
        );
        bounded(
            harness
                .remote
                .reconcile(harness.request.request_id, &harness.cancel),
        )
        .await?;
        assert_eq!(
            harness.saved().await.state,
            AllocationState::Cancelled,
            "{}",
            case.scenario
        );
        assert!(
            !harness
                .fault
                .provider
                .is_provisioned(&committed.allocation_id)
        );
        assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
        harness.stop().await;
    }
    Ok(())
}

/// Background recovery saves provider identity before foreground eligibility is
/// available. Replaying it must reject an ineligible value and keep cleanup possible.
#[carbide_macros::sqlx_test]
async fn saved_assignment_outside_eligibility_requests_cleanup_without_dispatch(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::RecoveredIneligibleAllocation).await;
    let allocated = harness.provision().await;
    // The provider reports a valid integer and the original request identity,
    // but its value is outside the caller's accepted ranges.
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    let recovered = harness.saved().await;
    assert_eq!(recovered.state, AllocationState::Pending);
    let saved_assignment = assignment(&recovered).clone();
    assert_eq!(saved_assignment.allocation_id, allocated.allocation_id);
    assert_eq!(saved_assignment.value, ResourcePoolValue::Integer(99));
    let result = bounded(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ))
    .await;
    assert!(matches!(result, Err(RemoteResourcePoolError::Value(_))));
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::CancelRequested);
    assert_eq!(assignment(&saved), &saved_assignment);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 1);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );

    // Rejecting the value must leave the original provider operation usable for
    // cancellation, even though its saved assignment cannot be attached.
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    let cancelled = harness.saved().await;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(assignment(&cancelled), &saved_assignment);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn reused_provider_handle_requests_cleanup_after_result_write_rollback(
    database: PgPool,
) -> TestResult {
    let original = Harness::new(database.clone(), provider(), Fault::None).await;
    let attached = original.allocate().await;
    original.attach(&attached).await;
    let before = original.saved().await;

    // A replacement provider starts its handle sequence over while claiming
    // the same authority. The existing attachment still owns that handle.
    let replacement = Harness::new(database, provider(), Fault::None).await;
    let error = bounded(replacement.remote.allocate(
        &replacement.request,
        &replacement.authority,
        &replacement.eligibility,
        &replacement.cancel,
    ))
    .await
    .expect_err("another saved assignment already owns the provider handle");
    assert!(matches!(
        error,
        RemoteResourcePoolError::Database(error)
            if error.violates_constraint("resource_pool_allocations_handle")
    ));
    let saved = replacement.saved().await;
    assert_eq!(saved.state, AllocationState::CancelRequested);
    assert!(matches!(
        saved.source,
        AllocationSource::Remote { result: None, .. }
    ));
    let committed = replacement.fault.committed.lock().unwrap().clone().unwrap();
    assert_eq!(committed.allocation_id, attached.allocation_id);
    assert!(
        replacement
            .fault
            .provider
            .is_provisioned(&committed.allocation_id)
    );
    bounded(
        replacement
            .remote
            .reconcile(replacement.request.request_id, &replacement.cancel),
    )
    .await?;
    assert_eq!(replacement.saved().await.state, AllocationState::Cancelled);
    assert!(
        !replacement
            .fault
            .provider
            .is_provisioned(&committed.allocation_id)
    );
    assert_eq!(original.saved().await, before);
    assert!(
        original
            .fault
            .provider
            .is_provisioned(&attached.allocation_id)
    );
    assert_eq!(replacement.fault.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.fault.cancel_calls.load(Ordering::SeqCst), 1);
    replacement.stop().await;
    original.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn provider_failures_obey_the_retry_limit_and_preserve_the_status(
    database: PgPool,
) -> TestResult {
    let local = provider();
    for (scenario, code, expected_calls) in [
        (
            "peer cancellation exhausts retries without cancelling the caller",
            Code::Cancelled,
            3,
        ),
        (
            "recovery exhaustion remains a retryable transport status",
            Code::ResourceExhausted,
            3,
        ),
        (
            "authentication failure stops immediately",
            Code::Unauthenticated,
            1,
        ),
    ] {
        let harness =
            Harness::new(database.clone(), local.clone(), Fault::RecoverStatus(code)).await;
        let error = bounded(harness.remote.allocate(
            &harness.request,
            &harness.authority,
            &harness.eligibility,
            &harness.cancel,
        ))
        .await
        .expect_err(scenario);
        assert_eq!(error.provider_code(), Some(code), "{scenario}");
        assert_eq!(
            harness.fault.recover_calls.load(Ordering::SeqCst),
            expected_calls,
            "{scenario}"
        );
        assert_eq!(
            harness.fault.allocation_calls.load(Ordering::SeqCst),
            0,
            "{scenario}"
        );
        let saved = harness.saved().await;
        assert_eq!(saved.state, AllocationState::CancelRequested, "{scenario}");
        assert!(matches!(
            saved.source,
            AllocationSource::Remote { result: None, .. }
        ));
        harness.stop().await;
    }
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn definite_allocation_failure_is_saved_before_terminal_cancellation(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, provider(), Fault::None).await;
    harness.request.selection = AllocationSelection::Requested(ResourcePoolValue::Integer(99));
    let result = bounded(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ))
    .await;
    assert!(matches!(
        result,
        Err(RemoteResourcePoolError::Rejected(
            AllocationFailureCode::FailedPrecondition
        ))
    ));
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Failed(failure)),
        ..
    } = saved.source
    else {
        panic!("the provider's definite failure must be saved");
    };
    assert_eq!(failure.code, AllocationFailureCode::FailedPrecondition);
    assert!(!failure.message.is_empty());
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 2);
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    assert_eq!(harness.saved().await.state, AllocationState::Cancelled);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn foreground_expiry_cleans_up_after_abandonment_cannot_be_saved(
    database: PgPool,
) -> TestResult {
    // The saved intent must still block another generation when the provider
    // has allocated but caller cancellation cannot be recorded in the database.
    let gate = Arc::new(Gate::default());
    let mut harness = Harness::new(
        database,
        provider(),
        Fault::WithholdAllocation(gate.clone()),
    )
    .await;
    let deadline: DateTime<Utc> =
        sqlx::query_scalar("SELECT clock_timestamp() + interval '2 seconds'")
            .fetch_one(&harness.database)
            .await?;
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: deadline,
    };
    let mut pending = Box::pin(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation passed its reply gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    let committed = harness.fault.committed.lock().unwrap().clone().unwrap();
    let before = harness.saved().await;
    assert_eq!(before.state, AllocationState::Pending);
    assert!(matches!(
        &before.source,
        AllocationSource::Remote { result: None, .. }
    ));

    // Keep the row locked until the caller returns. Its cancellation write
    // must fail to persist even though the provider has already allocated.
    let mut blocker = harness.database.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query(
        "SELECT allocation_id FROM resource_pool_allocations WHERE allocation_id = $1 FOR UPDATE",
    )
    .bind(harness.request.request_id)
    .fetch_one(&mut *blocker)
    .await?;
    harness.cancel.cancel();
    tokio::select! {
        result = &mut pending => panic!("abandonment did not wait for the allocation row: {result:?}"),
        _ = bounded(db::test_support::postgres::wait_for_blocked_query(
            &harness.database, blocker_pid, "UPDATE resource_pool_allocations",
        )) => {}
    }
    tokio::time::pause();
    tokio::time::advance(super::CANCELLATION_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    assert!(matches!(
        tokio::time::timeout(
            super::CANCELLATION_TIMEOUT + Duration::from_secs(3),
            pending
        )
        .await?,
        Err(RemoteResourcePoolError::Cancelled)
    ));
    assert_eq!(harness.saved().await, before);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&committed.allocation_id)
    );
    blocker.rollback().await?;
    gate.release.notify_waiters();
    let mut newer = harness.request.clone();
    newer.request_id = Uuid::new_v4();
    newer.owner.generation += 1;
    let error = bounded(begin_request(&harness.database, &newer, &harness.authority))
        .await
        .expect_err("the unresolved allocation still reserves the owner slot");
    assert!(matches!(
        error.downcast_ref::<remote::RemoteAllocationError>(),
        Some(remote::RemoteAllocationError::SlotInUse(_))
    ));

    // The persisted deadline lets a new coordinator clean up without a saved
    // assignment or cancellation request from the interrupted caller.
    sqlx::query(
        "SELECT pg_sleep(GREATEST(
            EXTRACT(EPOCH FROM ($1::timestamptz - clock_timestamp())), 0
        )::double precision)",
    )
    .bind(deadline)
    .execute(&harness.database)
    .await?;
    let restarted = harness.restarted();
    bounded(restarted.reconcile(harness.request.request_id, &CancellationToken::new())).await?;
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::Cancelled);
    assert_eq!(saved.source, before.source);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&committed.allocation_id)
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.cancel_calls.load(Ordering::SeqCst), 1);
    drop(restarted);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn foreground_expiry_while_waiting_for_a_reply_preserves_the_result_for_cleanup(
    database: PgPool,
) -> TestResult {
    let gate = Arc::new(Gate::default());
    let mut harness = Harness::new(
        database,
        provider(),
        Fault::WithholdAllocation(gate.clone()),
    )
    .await;
    let deadline: DateTime<Utc> =
        sqlx::query_scalar("SELECT clock_timestamp() + interval '2 seconds'")
            .fetch_one(&harness.database)
            .await?;
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: deadline,
    };
    let mut pending = Box::pin(harness.remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation passed its reply gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    // The provider has allocated, but the caller's attachment deadline passes
    // before NICo receives the reply. Saving it must not make it attachable.
    sqlx::query(
        "SELECT pg_sleep(GREATEST(
            EXTRACT(EPOCH FROM ($1::timestamptz - clock_timestamp())), 0
        )::double precision)",
    )
    .bind(deadline)
    .execute(&harness.database)
    .await?;
    gate.release.notify_waiters();
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::NotCurrent)
    ));
    let saved = harness.saved().await;
    assert_eq!(saved.state, AllocationState::CancelRequested);
    let allocated = assignment(&saved);
    let committed = harness.fault.committed.lock().unwrap().clone().unwrap();
    assert_eq!(allocated.allocation_id, committed.allocation_id);
    assert!(
        harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    let cancelled = harness.saved().await;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(assignment(&cancelled), allocated);
    assert!(
        !harness
            .fault
            .provider
            .is_provisioned(&allocated.allocation_id)
    );
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 1);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn expired_foreground_replay_requests_cleanup_without_dispatch(
    database: PgPool,
) -> TestResult {
    let mut harness = Harness::new(database, provider(), Fault::None).await;
    harness.authority = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: sqlx::query_scalar("SELECT clock_timestamp() - interval '1 second'")
            .fetch_one(&harness.database)
            .await?,
    };
    begin_request(&harness.database, &harness.request, &harness.authority).await?;
    // Expiry alone does not authorize another backend to change this request.
    let before = harness.saved().await;
    let foreign = RemoteResourcePool::new(
        harness.database.clone(),
        harness.work_locks.clone(),
        "another-authority".into(),
        harness.client.clone(),
    );
    bounded(foreign.reconcile(harness.request.request_id, &harness.cancel)).await?;
    assert_eq!(harness.saved().await, before);
    for calls in [
        &harness.fault.allocation_calls,
        &harness.fault.recover_calls,
        &harness.fault.cancel_calls,
        &harness.fault.release_calls,
    ] {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    drop(foreign);
    assert!(matches!(
        bounded(harness.remote.allocate(
            &harness.request,
            &harness.authority,
            &harness.eligibility,
            &harness.cancel,
        ))
        .await,
        Err(RemoteResourcePoolError::NotCurrent)
    ));
    assert_eq!(
        harness.saved().await.state,
        AllocationState::CancelRequested
    );
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    )
    .await?;
    assert_eq!(harness.saved().await.state, AllocationState::Cancelled);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn saved_failure_resumes_cleanup_without_contacting_the_provider(
    database: PgPool,
) -> TestResult {
    for (scenario, foreground) in [
        ("foreground replay returns the saved rejection", true),
        ("recovery resumes cancellation after restart", false),
    ] {
        let harness = Harness::new(database.clone(), provider(), Fault::None).await;
        begin_request(&harness.database, &harness.request, &harness.authority).await?;
        let result = AllocationResult::Failed(AllocationFailure {
            code: AllocationFailureCode::ResourceExhausted,
            message: "provider capacity exhausted".into(),
        });
        // Simulate a stop after saving the failure but before requesting cleanup.
        let mut txn = harness.database.begin().await?;
        assert_eq!(
            remote::record_result(&mut txn, harness.request.request_id, &result).await?,
            ConditionalWrite::Applied(()),
            "{scenario}",
        );
        txn.commit().await?;
        let before = harness.saved().await;
        assert_eq!(before.state, AllocationState::Pending, "{scenario}");
        if foreground {
            assert!(
                matches!(
                    bounded(harness.remote.allocate(
                        &harness.request,
                        &harness.authority,
                        &harness.eligibility,
                        &harness.cancel,
                    ))
                    .await,
                    Err(RemoteResourcePoolError::Rejected(
                        AllocationFailureCode::ResourceExhausted
                    ))
                ),
                "{scenario}"
            );
        } else {
            let restarted = harness.restarted();
            bounded(restarted.reconcile(harness.request.request_id, &harness.cancel)).await?;
        }
        let saved = harness.saved().await;
        assert_eq!(saved.state, AllocationState::CancelRequested, "{scenario}");
        assert_eq!(saved.source, before.source, "{scenario}");
        for calls in [
            &harness.fault.allocation_calls,
            &harness.fault.recover_calls,
            &harness.fault.cancel_calls,
            &harness.fault.release_calls,
        ] {
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{scenario}");
        }
        harness.stop().await;
    }
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn foreground_budget_stops_waiting_and_records_cleanup(database: PgPool) -> TestResult {
    let gate = Arc::new(Gate::default());
    let harness = Harness::new(database, provider(), Fault::StallRecovery(gate.clone())).await;
    // Make the client deadline longer than the coordinator's complete budget,
    // so this test observes the coordinator stopping the pending call.
    let slow_client = harness
        .client
        .clone()
        .with_timeouts(Duration::from_secs(60), Duration::from_secs(60));
    let remote = RemoteResourcePool::new(
        harness.database.clone(),
        harness.work_locks.clone(),
        harness.request.pool.authority_id.clone(),
        slow_client,
    );
    let mut pending = Box::pin(remote.allocate(
        &harness.request,
        &harness.authority,
        &harness.eligibility,
        &harness.cancel,
    ));
    tokio::select! {
        result = &mut pending => panic!("allocation did not reach the provider gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    tokio::time::pause();
    tokio::time::advance(super::FOREGROUND_TIMEOUT + Duration::from_millis(1)).await;
    tokio::time::resume();
    let result = bounded(pending).await;
    assert!(matches!(result, Err(RemoteResourcePoolError::Incomplete)));
    assert_eq!(
        harness.saved().await.state,
        AllocationState::CancelRequested
    );
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    gate.release.notify_waiters();
    drop(remote);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn shutdown_stops_a_database_wait_without_dispatching(database: PgPool) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::None).await;
    begin_request(&harness.database, &harness.request, &harness.authority).await?;
    let mut blocker = harness.database.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query(
        "SELECT allocation_id FROM resource_pool_allocations WHERE allocation_id = $1 FOR UPDATE",
    )
    .bind(harness.request.request_id)
    .fetch_one(&mut *blocker)
    .await?;
    let mut pending = Box::pin(
        harness
            .remote
            .reconcile(harness.request.request_id, &harness.cancel),
    );
    tokio::select! {
        result = &mut pending => panic!("recovery did not wait for the allocation row: {result:?}"),
        _ = bounded(db::test_support::postgres::wait_for_blocked_query(&harness.database, blocker_pid, "SELECT allocation_id FROM resource_pool_allocations")) => {}
    }
    harness.cancel.cancel();
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::Cancelled)
    ));
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    tokio::time::timeout(Duration::from_secs(10), async {
        let lock = harness
            .work_locks
            .try_acquire_lock("unrelated-after-cancelled-database-wait".into())
            .await?;
        lock.release().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;
    blocker.rollback().await?;
    bounded(
        harness
            .remote
            .reconcile(harness.request.request_id, &CancellationToken::new()),
    )
    .await?;
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn cancelled_read_returns_its_only_connection_while_the_table_stays_locked(
    database: PgPool,
) -> TestResult {
    let harness = Harness::new(database, provider(), Fault::None).await;
    begin_request(&harness.database, &harness.request, &harness.authority).await?;
    let single_connection = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(harness.database.connect_options().as_ref().clone())
        .await?;
    sqlx::query("SET statement_timeout = 0")
        .execute(&single_connection)
        .await?;
    let remote = RemoteResourcePool::new(
        single_connection.clone(),
        harness.work_locks.clone(),
        harness.request.pool.authority_id.clone(),
        harness.client.clone(),
    );
    let mut blocker = harness.database.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("LOCK TABLE resource_pool_allocations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let mut pending = Box::pin(remote.reconcile(harness.request.request_id, &harness.cancel));
    tokio::select! {
        result = &mut pending => panic!("initial read did not wait for the table: {result:?}"),
        _ = bounded(db::test_support::postgres::wait_for_blocked_query(
            &harness.database, blocker_pid, "FROM resource_pool_allocations",
        )) => {}
    }
    harness.cancel.cancel();
    assert!(matches!(
        bounded(pending).await,
        Err(RemoteResourcePoolError::Cancelled)
    ));

    // Dropping the Rust future does not cancel PostgreSQL's query. Its server
    // timeout must free this only connection while the table remains locked.
    let value: i32 = tokio::time::timeout(
        super::DATABASE_WAIT_TIMEOUT + Duration::from_secs(3),
        sqlx::query_scalar("SELECT 1").fetch_one(&single_connection),
    )
    .await??;
    assert_eq!(value, 1);
    assert_eq!(harness.fault.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.fault.allocation_calls.load(Ordering::SeqCst), 0);
    blocker.rollback().await?;
    assert_eq!(harness.saved().await.state, AllocationState::Pending);
    drop(remote);
    single_connection.close().await;
    harness.stop().await;
    Ok(())
}

#[carbide_macros::sqlx_test]
async fn database_wait_preserves_a_shorter_operator_timeout(database: PgPool) -> TestResult {
    let single_connection = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(database.connect_options().as_ref().clone())
        .await?;
    sqlx::query("SET statement_timeout = '100ms'")
        .execute(&single_connection)
        .await?;
    let mut txn = super::database_transaction(&single_connection).await?;
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(txn.as_pgconn())
        .await?;
    assert_eq!(timeout, "100ms");
    let error = sqlx::query("SELECT pg_sleep(1)")
        .execute(txn.as_pgconn())
        .await
        .expect_err("the operator's shorter timeout must interrupt the query");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("57014"),
    );
    txn.rollback().await?;
    let restored: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&single_connection)
        .await?;
    assert_eq!(restored, "100ms");
    single_connection.close().await;
    Ok(())
}
