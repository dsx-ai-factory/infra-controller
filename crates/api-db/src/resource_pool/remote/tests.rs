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

use std::num::NonZeroU16;
use std::time::Duration;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use chrono::{DateTime, Utc};
use futures::TryFutureExt;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationFailure, AllocationFailureCode, AllocationOwner,
    AllocationResult, AllocationSelection, CleanupConfirmation, CompletionAuthority,
    ProviderRouteTargetMode, ProviderRouteTargetPolicy, RemoteAllocationParameters,
    RemoteAllocationRequest, RemotePoolIdentity,
};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolSelection,
    ResourcePoolValueDomain, ValueType,
};
use model::vpc::routing_profile::RouteTargetConfig;
use sqlx::{Acquire, Connection, PgConnection, PgPool};
use uuid::Uuid;

use super::{
    OperationNotCurrent, RemoteAllocationError, attach, confirm_cleanup, list, load, record_result,
    request_cancel, request_cancel_if_expired, request_release,
};
use crate::ConditionalWrite::{Applied, NotApplied};
use crate::Transaction;
use crate::resource_pool::binding::bind_or_verify;
use crate::resource_pool::{allocate, allocation, populate};
use crate::test_support::postgres::wait_for_blocked_query;

async fn begin(
    pool: &PgPool,
    request: &RemoteAllocationRequest,
    authority: &CompletionAuthority,
) -> Result<ResourcePoolAllocation, RemoteAllocationError> {
    let mut txn = Transaction::begin(pool).await?;
    let saved = super::begin(txn.as_mut(), request, authority).await?;
    txn.commit().await?;
    Ok(saved)
}

fn saved_result(allocation: &ResourcePoolAllocation) -> Option<&AllocationResult> {
    let AllocationSource::Remote { result, .. } = &allocation.source else {
        panic!("expected a remote allocation");
    };
    result.as_ref()
}

fn assert_saved_request(
    allocation: &ResourcePoolAllocation,
    request: &RemoteAllocationRequest,
    completion_authority: &CompletionAuthority,
) {
    assert_eq!(allocation.allocation_id, request.request_id);
    assert_eq!(allocation.pool_name, request.pool.logical_pool);
    assert_eq!(allocation.owner_type, request.owner.owner_type);
    assert_eq!(allocation.owner_id, request.owner.owner_id);
    let AllocationSource::Remote {
        binding_id,
        request: saved,
        ..
    } = &allocation.source
    else {
        panic!("expected a remote allocation");
    };
    assert_eq!(*binding_id, request.binding_id);
    assert_eq!(
        saved.as_ref(),
        &RemoteAllocationParameters {
            authority_id: request.pool.authority_id.clone(),
            pool_id: request.pool.pool_id.clone(),
            slot: request.owner.slot.clone(),
            generation: request.owner.generation,
            eligibility_revision: request.eligibility_revision.clone(),
            selection: request.selection.clone(),
            completion_authority: completion_authority.clone(),
        }
    );
}

async fn wait_for_database_time_after(
    connection: &mut PgConnection,
    timestamp: DateTime<Utc>,
) -> eyre::Result<()> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let advanced: bool = sqlx::query_scalar("SELECT clock_timestamp() > $1")
                .bind(timestamp)
                .fetch_one(&mut *connection)
                .await?;
            if advanced {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

async fn new_request(
    pool: &PgPool,
    logical_pool: &str,
    value_domain: ResourcePoolValueDomain,
) -> eyre::Result<RemoteAllocationRequest> {
    let mut txn = pool.begin().await?;
    let binding = bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: logical_pool.to_owned(),
            value_domain,
            backend_name: Some("fabric".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "fabric-provider".to_owned(),
            remote_pool: "tenant-vnis".to_owned(),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(RemoteAllocationRequest {
        request_id: Uuid::new_v4(),
        binding_id: binding.binding_id,
        pool: RemotePoolIdentity {
            authority_id: binding.authority_id,
            pool_id: "provider-pool-17".to_owned(),
            logical_pool: binding.pool_name,
        },
        owner: AllocationOwner {
            owner_type: OwnerType::Vpc,
            owner_id: Uuid::new_v4().to_string(),
            slot: "vni".to_owned(),
            generation: 0,
        },
        eligibility_revision: "eligible-vnis-1".to_owned(),
        selection: AllocationSelection::Automatic,
    })
}

fn controller_authority() -> CompletionAuthority {
    CompletionAuthority::Controller {
        desire_id: Uuid::new_v4(),
    }
}

fn assignment() -> AllocationAssignment {
    AllocationAssignment {
        allocation_id: Uuid::new_v4().to_string(),
        value: ResourcePoolValue::Integer(73),
        assignment_mode: AssignmentMode::Automatic,
        route_target_policy: None,
    }
}

async fn save_assignment(
    pool: &PgPool,
    request_id: Uuid,
    assignment: &AllocationAssignment,
) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    let result = AllocationResult::Allocated(assignment.clone());
    assert_eq!(
        record_result(&mut txn, request_id, &result).await?,
        Applied(())
    );
    txn.commit().await?;
    Ok(())
}

async fn attach_assignment(
    pool: &PgPool,
    request_id: Uuid,
    authority: &CompletionAuthority,
    assignment: &AllocationAssignment,
) -> eyre::Result<()> {
    save_assignment(pool, request_id, assignment).await?;
    let mut txn = pool.begin().await?;
    assert_eq!(
        attach(&mut txn, request_id, authority, assignment).await?,
        Applied(())
    );
    txn.commit().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn begin_preserves_caller_rollback_and_recovers_from_slot_conflicts(
    pool: PgPool,
) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let mut txn = pool.begin().await?;
    let pending = super::begin(&mut txn, &request, &authority).await?;
    assert_eq!(load(txn.as_mut(), request.request_id).await?, Some(pending));
    assert_eq!(load(&pool, request.request_id).await?, None);
    txn.rollback().await?;
    assert_eq!(load(&pool, request.request_id).await?, None);

    // A rejected second generation must roll back only its own savepoint.
    // The caller can still commit the first request in this transaction.
    let mut txn = pool.begin().await?;
    let saved = super::begin(&mut txn, &request, &authority).await?;
    let mut conflicting = request.clone();
    conflicting.request_id = Uuid::new_v4();
    conflicting.owner.generation += 1;
    assert!(matches!(
        super::begin(&mut txn, &conflicting, &authority).await,
        Err(RemoteAllocationError::SlotInUse(_))
    ));
    assert_eq!(
        load(txn.as_mut(), request.request_id).await?,
        Some(saved.clone())
    );
    txn.commit().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(saved));
    assert_eq!(load(&pool, conflicting.request_id).await?, None);
    Ok(())
}

#[crate::sqlx_test]
async fn saved_request_and_result_survive_entity_rollback(pool: PgPool) -> eyre::Result<()> {
    sqlx::query("CREATE TABLE remote_test_entity (id UUID PRIMARY KEY)")
        .execute(&pool)
        .await?;
    let mut request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    request.owner.generation = u64::MAX;
    let authority = controller_authority();
    let before_begin: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await?;
    let saved = begin(&pool, &request, &authority).await?;
    let after_begin: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await?;
    let allocation_id = request.request_id;
    assert_saved_request(&saved, &request, &authority);
    assert_eq!(saved.state, AllocationState::Pending);
    assert_eq!(saved_result(&saved), None);
    assert!(before_begin <= saved.created_at && saved.created_at <= after_begin);
    assert_eq!(allocation::load(&pool, allocation_id).await?, None);
    let limit = NonZeroU16::new(10).expect("test page size");
    assert!(
        allocation::list(&pool, &request.pool.logical_pool, None, limit)
            .await?
            .is_empty()
    );

    let assignment = AllocationAssignment {
        route_target_policy: Some(ProviderRouteTargetPolicy {
            mode: ProviderRouteTargetMode::Preserve,
            imports: None,
            exports: Some(vec![]),
            peer_identity: vec![
                RouteTargetConfig { asn: 64512, vni: 9 },
                RouteTargetConfig { asn: 64512, vni: 3 },
                RouteTargetConfig { asn: 64512, vni: 9 },
            ],
        }),
        ..assignment()
    };
    let result = AllocationResult::Allocated(assignment.clone());
    let mut rolled_back_result = pool.begin().await?;
    assert_eq!(
        record_result(&mut rolled_back_result, request.request_id, &result).await?,
        Applied(())
    );
    let uncommitted = allocation::load(rolled_back_result.as_mut(), allocation_id)
        .await?
        .expect("uncommitted assignment");
    assert_eq!(saved_result(&uncommitted), Some(&result));
    rolled_back_result.rollback().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(saved.clone()));
    assert_eq!(allocation::load(&pool, allocation_id).await?, None);

    let mut result_txn = pool.begin().await?;
    assert_eq!(
        record_result(&mut result_txn, request.request_id, &result).await?,
        Applied(())
    );
    let reserved = allocation::load(result_txn.as_mut(), allocation_id)
        .await?
        .expect("saved assignment");
    assert_saved_request(&reserved, &request, &authority);
    assert_eq!(saved_result(&reserved), Some(&result));
    assert_eq!(reserved.created_at, saved.created_at);
    assert_eq!(reserved.state, AllocationState::Pending);
    assert_eq!(
        allocation::list(result_txn.as_mut(), &request.pool.logical_pool, None, limit).await?,
        vec![reserved.clone()]
    );
    result_txn.commit().await?;

    let mut replay_txn = pool.begin().await?;
    wait_for_database_time_after(&mut replay_txn, reserved.updated_at).await?;
    assert_eq!(
        record_result(&mut replay_txn, request.request_id, &result).await?,
        Applied(())
    );
    let replayed = load(replay_txn.as_mut(), request.request_id)
        .await?
        .expect("replayed assignment");
    assert_saved_request(&replayed, &request, &authority);
    assert_eq!(replayed.source, reserved.source);
    assert_eq!(replayed.state, reserved.state);
    assert_eq!(replayed.created_at, reserved.created_at);
    assert!(replayed.updated_at > reserved.updated_at);
    let different = AllocationResult::Failed(AllocationFailure {
        code: AllocationFailureCode::ResourceExhausted,
        message: "no available VNIs".to_owned(),
    });
    assert_eq!(
        record_result(&mut replay_txn, request.request_id, &different).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        allocation::load(replay_txn.as_mut(), allocation_id).await?,
        Some(replayed.clone())
    );
    replay_txn.commit().await?;

    let mut entity_txn = pool.begin().await?;
    let entity_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *entity_txn)
        .await?;
    sqlx::query("INSERT INTO remote_test_entity VALUES ($1)")
        .bind(request.request_id)
        .execute(&mut *entity_txn)
        .await?;
    assert_eq!(
        attach(&mut entity_txn, request.request_id, &authority, &assignment).await?,
        Applied(())
    );
    assert_eq!(
        allocation::load(entity_txn.as_mut(), allocation_id)
            .await?
            .expect("attached allocation")
            .state,
        AllocationState::Attached
    );

    // The committed request and result remain visible while entity creation
    // is uncommitted. Rolling creation back must preserve both.
    let mut observer = pool.acquire().await?;
    let observer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *observer)
        .await?;
    assert_ne!(entity_pid, observer_pid);
    assert_eq!(
        load(&mut *observer, request.request_id).await?,
        Some(replayed.clone())
    );
    entity_txn.rollback().await?;
    assert_eq!(
        allocation::load(&mut *observer, allocation_id).await?,
        Some(replayed.clone())
    );
    assert_eq!(
        load(&mut *observer, request.request_id).await?,
        Some(replayed)
    );
    let entity_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM remote_test_entity)")
            .fetch_one(&mut *observer)
            .await?;
    assert!(!entity_exists);
    Ok(())
}

/// Saved requests remain replayable when their JSON includes the removed site
/// field, while every remaining request input must still match.
#[crate::sqlx_test]
async fn request_replay_rejects_changed_inputs(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let saved = begin(&pool, &request, &authority).await?;
    assert_eq!(begin(&pool, &request, &authority).await?, saved);

    // Rows written before site identity was removed must not need a backfill
    // before we can retry them or reject a changed request.
    let updated = sqlx::query(
        "UPDATE resource_pool_allocations
         SET remote_request = remote_request || jsonb_build_object('site_id', $2::text)
         WHERE allocation_id = $1",
    )
    .bind(request.request_id)
    .bind(Uuid::new_v4().to_string())
    .execute(&pool)
    .await?;
    assert_eq!(updated.rows_affected(), 1);
    assert_eq!(begin(&pool, &request, &authority).await?, saved);

    let mut provider = request.clone();
    provider.pool.pool_id = "another-provider-pool".to_owned();
    let mut owner = request.clone();
    owner.owner.generation += 1;
    let mut eligibility = request.clone();
    eligibility.eligibility_revision = "eligible-vnis-2".to_owned();
    let mut selection = request.clone();
    selection.selection = AllocationSelection::Exact(ResourcePoolValue::Integer(73));
    let mut owner_id = request.clone();
    owner_id.owner.owner_id = Uuid::new_v4().to_string();
    let mut owner_type = request.clone();
    owner_type.owner.owner_type = OwnerType::Machine;
    let another_pool = new_request(&pool, "another-pool", ResourcePoolValueDomain::Integer).await?;
    let mut binding = request.clone();
    binding.binding_id = another_pool.binding_id;
    let mut logical_pool = request.clone();
    logical_pool.pool.logical_pool = another_pool.pool.logical_pool;
    check_cases_async(
        [
            ("provider identity", provider, authority.clone()),
            ("owner generation", owner, authority.clone()),
            ("eligibility revision", eligibility, authority.clone()),
            ("allocation method", selection, authority.clone()),
            ("owner identity", owner_id, authority.clone()),
            ("owner type", owner_type, authority.clone()),
            ("pool binding", binding, authority.clone()),
            ("logical pool", logical_pool, authority.clone()),
            (
                "completion authority",
                request.clone(),
                controller_authority(),
            ),
        ]
        .map(|(scenario, request, authority)| Case {
            scenario,
            input: (scenario, request, authority),
            expect: Yields(()),
        }),
        |(scenario, request, authority)| {
            let pool = &pool;
            let saved = &saved;
            async move {
                assert!(
                    matches!(
                        begin(pool, &request, &authority).await,
                        Err(RemoteAllocationError::RequestConflict(id)) if id == request.request_id
                    ),
                    "{scenario}"
                );
                assert_eq!(
                    load(pool, request.request_id).await?,
                    Some(saved.clone()),
                    "{scenario}"
                );
                Ok::<_, eyre::Report>(())
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;

    Ok(())
}

#[crate::sqlx_test]
async fn begin_rejects_mismatched_bindings(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let mut wrong_authority = request.clone();
    wrong_authority.pool.authority_id = "another-authority".to_owned();
    let mut wrong_pool = request.clone();
    wrong_pool.pool.logical_pool = "another-logical-pool".to_owned();
    let mut absent_binding = request.clone();
    absent_binding.binding_id = Uuid::new_v4();
    let mut txn = pool.begin().await?;
    let integrated_binding = bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: "integrated-kind".to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: None,
            backend_kind: ResourcePoolBackendKind::Integrated,
            // Match the authority so only the backend kind rejects this binding.
            authority_id: request.pool.authority_id.clone(),
            remote_pool: "integrated-kind".to_owned(),
        },
    )
    .await?;
    txn.commit().await?;
    let mut integrated = request.clone();
    integrated.binding_id = integrated_binding.binding_id;
    integrated.pool.logical_pool = integrated_binding.pool_name;
    check_cases_async(
        [
            ("different authority", wrong_authority),
            ("different logical pool", wrong_pool),
            ("absent binding", absent_binding),
            ("Integrated backend kind", integrated),
        ]
        .map(|(scenario, mut request)| {
            request.request_id = Uuid::new_v4();
            Case {
                scenario,
                input: (scenario, request),
                expect: Yields(()),
            }
        }),
        |(scenario, request)| {
            let pool = &pool;
            let authority = &authority;
            async move {
                assert!(
                    matches!(
                        begin(pool, &request, authority).await,
                        Err(RemoteAllocationError::BindingMismatch(id)) if id == request.binding_id
                    ),
                    "{scenario}"
                );
                assert_eq!(load(pool, request.request_id).await?, None, "{scenario}");
                Ok::<_, eyre::Report>(())
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;

    Ok(())
}

#[crate::sqlx_test]
async fn committed_failure_is_immutable_and_keeps_its_owner_slot(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let saved = begin(&pool, &request, &authority).await?;
    let rejection = AllocationResult::Failed(AllocationFailure {
        code: AllocationFailureCode::FailedPrecondition,
        message: "eligibility revision is not accepted".to_owned(),
    });
    let mut txn = pool.begin().await?;
    assert_eq!(
        record_result(&mut txn, request.request_id, &rejection).await?,
        Applied(())
    );
    txn.commit().await?;

    let mut newer = request.clone();
    newer.request_id = Uuid::new_v4();
    newer.owner.generation += 1;
    assert!(matches!(
        begin(&pool, &newer, &authority).await,
        Err(RemoteAllocationError::SlotInUse(_))
    ));

    check_cases_async(
        [
            ("same failure", rejection.clone(), Applied(())),
            (
                "different failure",
                AllocationResult::Failed(AllocationFailure {
                    code: AllocationFailureCode::ResourceExhausted,
                    message: "no available VNIs".to_owned(),
                }),
                NotApplied(OperationNotCurrent),
            ),
            (
                "assignment after failure",
                AllocationResult::Allocated(assignment()),
                NotApplied(OperationNotCurrent),
            ),
        ]
        .map(|(scenario, result, expected)| Case {
            scenario,
            input: result,
            expect: Yields(expected),
        }),
        |result| {
            let pool = &pool;
            let request = &request;
            let saved = &saved;
            let rejection = &rejection;
            let authority = &authority;
            async move {
                let mut txn = pool.begin().await?;
                let applied = record_result(&mut txn, request.request_id, &result).await?;
                txn.commit().await?;
                let rejected = load(pool, request.request_id)
                    .await?
                    .expect("saved rejection");
                assert_saved_request(&rejected, request, authority);
                assert_eq!(rejected.created_at, saved.created_at);
                assert_eq!(rejected.state, AllocationState::Pending);
                assert_eq!(saved_result(&rejected), Some(rejection));
                assert_eq!(allocation::load(pool, request.request_id).await?, None);
                assert!(
                    allocation::list(
                        pool,
                        &request.pool.logical_pool,
                        None,
                        NonZeroU16::new(10).expect("test page size")
                    )
                    .await?
                    .is_empty()
                );
                Ok::<_, eyre::Report>(applied)
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn owner_slots_keep_generations_separate_through_cleanup(pool: PgPool) -> eyre::Result<()> {
    let mut original = new_request(&pool, "loopback", ResourcePoolValueDomain::Integer).await?;
    original.owner.owner_type = OwnerType::Machine;
    original.owner.slot = "loopback:vpc-a".to_owned();
    let authority = controller_authority();
    begin(&pool, &original, &authority).await?;
    let mut second_slot = original.clone();
    second_slot.request_id = Uuid::new_v4();
    second_slot.owner.slot = "loopback:vpc-b".to_owned();
    begin(&pool, &second_slot, &authority).await?;

    let mut replacement = new_request(
        &pool,
        "another-loopback-pool",
        ResourcePoolValueDomain::Integer,
    )
    .await?;
    replacement.owner = original.owner.clone();
    replacement.owner.generation += 1;
    assert!(matches!(
        begin(&pool, &replacement, &authority).await,
        Err(RemoteAllocationError::SlotInUse(_))
    ));
    attach_assignment(&pool, original.request_id, &authority, &assignment()).await?;
    begin(&pool, &replacement, &authority).await?;

    let mut txn = pool.begin().await?;
    assert_eq!(
        request_cancel(&mut txn, replacement.request_id).await?,
        Applied(())
    );
    let limit = NonZeroU16::new(10).expect("test page size");
    assert!(
        allocation::list(txn.as_mut(), &replacement.pool.logical_pool, None, limit)
            .await?
            .is_empty()
    );
    let late_assignment = assignment();
    let late_result = AllocationResult::Allocated(late_assignment.clone());
    assert_eq!(
        record_result(&mut txn, replacement.request_id, &late_result).await?,
        Applied(())
    );
    assert_eq!(
        attach(
            &mut txn,
            replacement.request_id,
            &authority,
            &late_assignment
        )
        .await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    let late_allocation_id = replacement.request_id;
    assert_eq!(
        allocation::load(&pool, late_allocation_id)
            .await?
            .expect("late allocation")
            .state,
        AllocationState::CancelRequested
    );
    let waiting = load(&pool, replacement.request_id)
        .await?
        .expect("late assignment");
    assert_eq!(
        allocation::list(&pool, &replacement.pool.logical_pool, None, limit).await?,
        vec![waiting]
    );
    let mut newer = replacement.clone();
    newer.request_id = Uuid::new_v4();
    newer.owner.generation += 1;
    assert!(matches!(
        begin(&pool, &newer, &authority).await,
        Err(RemoteAllocationError::SlotInUse(_))
    ));
    let mut txn = pool.begin().await?;
    assert_eq!(
        confirm_cleanup(
            &mut txn,
            replacement.request_id,
            CleanupConfirmation::Released
        )
        .await?,
        Applied(())
    );
    txn.commit().await?;
    let cleaned_up = load(&pool, replacement.request_id)
        .await?
        .expect("released late assignment");
    assert_eq!(cleaned_up.state, AllocationState::Released);
    assert_eq!(saved_result(&cleaned_up), Some(&late_result));
    assert_eq!(allocation::load(&pool, late_allocation_id).await?, None);
    assert!(
        allocation::list(&pool, &replacement.pool.logical_pool, None, limit)
            .await?
            .is_empty()
    );
    assert_eq!(begin(&pool, &replacement, &authority).await?, cleaned_up);
    let mut conflicting_replay = replacement.clone();
    conflicting_replay.owner.owner_id = Uuid::new_v4().to_string();
    assert!(matches!(
        begin(&pool, &conflicting_replay, &authority).await,
        Err(RemoteAllocationError::RequestConflict(id)) if id == replacement.request_id
    ));
    assert_eq!(load(&pool, replacement.request_id).await?, Some(cleaned_up));

    // Cleanup permits another generation, but no request can reuse the old
    // owner, slot, and generation, even when it selects a different pool binding.
    let mut reused = replacement.clone();
    reused.request_id = Uuid::new_v4();
    reused.binding_id = original.binding_id;
    reused.pool = original.pool.clone();
    assert!(matches!(
        begin(&pool, &reused, &authority).await,
        Err(RemoteAllocationError::GenerationExists(_))
    ));
    assert_eq!(
        begin(&pool, &newer, &authority).await?.state,
        AllocationState::Pending
    );
    assert_eq!(
        load(&pool, second_slot.request_id)
            .await?
            .expect("other VPC slot")
            .state,
        AllocationState::Pending
    );

    // Cancellation can finish before any provider result arrives. A late
    // assignment must not fill that empty result or reopen the request.
    let mut txn = pool.begin().await?;
    assert_eq!(
        request_cancel(&mut txn, second_slot.request_id).await?,
        Applied(())
    );
    assert_eq!(
        confirm_cleanup(
            &mut txn,
            second_slot.request_id,
            CleanupConfirmation::Cancelled
        )
        .await?,
        Applied(())
    );
    assert_eq!(
        record_result(
            &mut txn,
            second_slot.request_id,
            &AllocationResult::Allocated(assignment())
        )
        .await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    let terminal = load(&pool, second_slot.request_id)
        .await?
        .expect("terminal cancellation");
    assert_eq!(terminal.state, AllocationState::Cancelled);
    assert_eq!(saved_result(&terminal), None);
    assert_eq!(allocation::load(&pool, second_slot.request_id).await?, None);
    assert_eq!(begin(&pool, &second_slot, &authority).await?, terminal);
    let attached = load(&pool, original.request_id)
        .await?
        .expect("original assignment");
    assert_eq!(
        allocation::list(&pool, &original.pool.logical_pool, None, limit).await?,
        vec![attached]
    );
    let mut next_slot_generation = second_slot.clone();
    next_slot_generation.request_id = Uuid::new_v4();
    next_slot_generation.owner.generation += 1;
    assert_eq!(
        begin(&pool, &next_slot_generation, &authority).await?.state,
        AllocationState::Pending
    );
    Ok(())
}

#[crate::sqlx_test]
async fn stale_cleanup_preserves_a_later_assignment(pool: PgPool) -> eyre::Result<()> {
    let original = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    begin(&pool, &original, &authority).await?;
    attach_assignment(&pool, original.request_id, &authority, &assignment()).await?;

    // Keep the old worker's saved identity and connection while another
    // connection attaches a new generation using the same numeric value.
    let mut stale_worker = pool.acquire().await?;
    let old = load(&mut *stale_worker, original.request_id)
        .await?
        .expect("old worker snapshot");
    let Some(AllocationResult::Allocated(old_assignment)) = saved_result(&old).cloned() else {
        panic!("the old worker must load its attached allocation");
    };
    let old_request_id = old.allocation_id;
    let old_result = AllocationResult::Allocated(old_assignment.clone());
    let mut newer = original.clone();
    newer.request_id = Uuid::new_v4();
    newer.owner.generation += 1;
    let pending = begin(&pool, &newer, &authority).await?;
    let mut reused_handle = pool.begin().await?;
    let error = record_result(&mut reused_handle, newer.request_id, &old_result)
        .await
        .expect_err("an allocation handle cannot identify two requests");
    let crate::DatabaseError::Sqlx(error) = error else {
        panic!("expected the provider handle constraint: {error}");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .expect("PostgreSQL error")
            .constraint(),
        Some("resource_pool_allocations_handle")
    );
    reused_handle.rollback().await?;
    assert_eq!(load(&pool, newer.request_id).await?, Some(pending));
    assert_eq!(allocation::load(&pool, newer.request_id).await?, None);
    assert_eq!(load(&pool, old_request_id).await?, Some(old));

    // Handles belong to the issuing pool. Another pool may use the same string.
    let mut other_pool = newer.clone();
    other_pool.request_id = Uuid::new_v4();
    other_pool.owner.owner_id = Uuid::new_v4().to_string();
    other_pool.pool.pool_id = "provider-pool-18".to_owned();
    begin(&pool, &other_pool, &authority).await?;
    save_assignment(&pool, other_pool.request_id, &old_assignment).await?;
    let other_allocation = load(&pool, other_pool.request_id)
        .await?
        .expect("other pool allocation");
    assert_eq!(saved_result(&other_allocation), Some(&old_result));

    // Another authority may use the same provider pool and handle strings.
    let mut binding_txn = pool.begin().await?;
    let binding = bind_or_verify(
        &mut binding_txn,
        &ResourcePoolSelection {
            pool_name: "another-authority-vni".to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("another-fabric".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "another-fabric-provider".to_owned(),
            remote_pool: "tenant-vnis".to_owned(),
        },
    )
    .await?;
    binding_txn.commit().await?;
    let mut other_authority = original.clone();
    other_authority.request_id = Uuid::new_v4();
    other_authority.owner.owner_id = Uuid::new_v4().to_string();
    other_authority.binding_id = binding.binding_id;
    other_authority.pool.authority_id = binding.authority_id;
    other_authority.pool.logical_pool = binding.pool_name;
    begin(&pool, &other_authority, &authority).await?;
    save_assignment(&pool, other_authority.request_id, &old_assignment).await?;
    let other_authority_allocation = load(&pool, other_authority.request_id)
        .await?
        .expect("other authority allocation");
    assert_saved_request(&other_authority_allocation, &other_authority, &authority);
    assert_eq!(saved_result(&other_authority_allocation), Some(&old_result));

    let new_assignment = assignment();
    assert_eq!(new_assignment.value, old_assignment.value);
    attach_assignment(&pool, newer.request_id, &authority, &new_assignment).await?;
    let newest = load(&pool, newer.request_id)
        .await?
        .expect("new assignment");
    let new_allocation_id = newer.request_id;
    let new_allocation = allocation::load(&pool, new_allocation_id)
        .await?
        .expect("new allocation");
    let mut txn = stale_worker.begin().await?;
    assert_eq!(
        request_release(&mut txn, old_request_id, &new_assignment.allocation_id).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        request_release(&mut txn, old_request_id, &old_assignment.allocation_id).await?,
        Applied(())
    );
    assert_eq!(
        confirm_cleanup(&mut txn, old_request_id, CleanupConfirmation::Cancelled).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        confirm_cleanup(&mut txn, old_request_id, CleanupConfirmation::Released).await?,
        Applied(())
    );
    let cleaned = load(txn.as_mut(), old_request_id)
        .await?
        .expect("confirmed cleanup");
    wait_for_database_time_after(&mut txn, cleaned.updated_at).await?;
    assert_eq!(
        confirm_cleanup(&mut txn, old_request_id, CleanupConfirmation::Released).await?,
        Applied(())
    );
    let repeated = load(txn.as_mut(), old_request_id)
        .await?
        .expect("repeated cleanup");
    assert!(repeated.updated_at > cleaned.updated_at);
    assert_eq!(repeated.created_at, cleaned.created_at);
    assert_eq!(repeated.source, cleaned.source);
    assert_eq!(
        request_release(&mut txn, old_request_id, &old_assignment.allocation_id).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        confirm_cleanup(&mut txn, newer.request_id, CleanupConfirmation::Released).await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    assert_eq!(load(&pool, newer.request_id).await?, Some(newest));
    assert_eq!(allocation::load(&pool, old_request_id).await?, None);
    assert_eq!(
        allocation::load(&pool, new_allocation_id).await?,
        Some(new_allocation)
    );
    let terminal = load(&pool, old_request_id)
        .await?
        .expect("released attempt");
    assert_eq!(terminal.state, AllocationState::Released);
    assert_eq!(saved_result(&terminal), Some(&old_result));

    // Terminal allocations keep the provider handle reserved, so a later
    // generation cannot claim the old assignment.
    let mut after_cleanup = newer.clone();
    after_cleanup.request_id = Uuid::new_v4();
    after_cleanup.owner.generation += 1;
    let pending_after_cleanup = begin(&pool, &after_cleanup, &authority).await?;
    let mut reused_terminal_handle = pool.begin().await?;
    let error = record_result(
        &mut reused_terminal_handle,
        after_cleanup.request_id,
        &old_result,
    )
    .await
    .expect_err("a terminal allocation still reserves its provider handle");
    let crate::DatabaseError::Sqlx(error) = error else {
        panic!("expected the provider handle constraint: {error}");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .expect("PostgreSQL error")
            .constraint(),
        Some("resource_pool_allocations_handle")
    );
    reused_terminal_handle.rollback().await?;
    assert_eq!(load(&pool, old_request_id).await?, Some(terminal));
    assert_eq!(
        load(&pool, after_cleanup.request_id).await?,
        Some(pending_after_cleanup)
    );
    assert_eq!(
        allocation::load(&pool, after_cleanup.request_id).await?,
        None
    );
    Ok(())
}

#[crate::sqlx_test]
async fn concurrent_begins_preserve_request_identity_and_owner_slots(
    pool: PgPool,
) -> eyre::Result<()> {
    for (scenario, replay) in [
        ("same request replay", true),
        ("different generation", false),
    ] {
        let first = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
        let mut second = first.clone();
        if !replay {
            second.request_id = Uuid::new_v4();
            second.owner.generation += 1;
        }
        let authority = controller_authority();
        let mut blocker = pool.begin().await?;
        sqlx::query("LOCK TABLE resource_pool_allocations IN SHARE MODE")
            .execute(&mut *blocker)
            .await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let unblock = async {
            // Hold both INSERTs at the table lock so they begin on separate
            // connections before either request has committed its record.
            loop {
                let blocked: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()
                     AND wait_event_type = 'Lock' AND $1 = ANY(pg_blocking_pids(pid))
                     AND strpos(query, 'INSERT INTO resource_pool_allocations') > 0",
                )
                .bind(blocker_pid)
                .fetch_one(&pool)
                .await?;
                if blocked == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            blocker.commit().await
        };
        let (first_result, second_result, committed) =
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(
                    begin(&pool, &first, &authority),
                    begin(&pool, &second, &authority),
                    unblock
                )
            })
            .await
            .expect(scenario);
        committed?;
        if replay {
            let first_saved = first_result?;
            assert_eq!(second_result?, first_saved, "{scenario}");
            assert_eq!(load(&pool, first.request_id).await?, Some(first_saved));
        } else {
            let (winner, loser) = match (first_result, second_result) {
                (Ok(saved), Err(RemoteAllocationError::SlotInUse(_))) => (saved, second.request_id),
                (Err(RemoteAllocationError::SlotInUse(_)), Ok(saved)) => (saved, first.request_id),
                other => panic!("{scenario}: one request must claim the slot: {other:?}"),
            };
            assert_eq!(load(&pool, winner.allocation_id).await?, Some(winner));
            assert_eq!(load(&pool, loser).await?, None);
        }
    }
    Ok(())
}

#[crate::sqlx_test]
async fn competing_first_results_preserve_the_committed_result(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    begin(&pool, &request, &controller_authority()).await?;
    let result = AllocationResult::Allocated(assignment());
    let mut first = pool.begin().await?;
    assert_eq!(
        record_result(&mut first, request.request_id, &result).await?,
        Applied(())
    );
    let saved = load(first.as_mut(), request.request_id)
        .await?
        .expect("first result");
    assert_eq!(saved_result(&saved), Some(&result));
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(first.as_mut())
        .await?;

    let different_result = AllocationResult::Failed(AllocationFailure {
        code: AllocationFailureCode::ResourceExhausted,
        message: "no available VNIs".to_owned(),
    });
    let mut competing = pool.begin().await?;
    let save_competing = async {
        let applied = record_result(&mut competing, request.request_id, &different_result).await?;
        competing.commit().await?;
        Ok::<_, eyre::Report>(applied)
    };
    let commit_first = async {
        // The other writer starts while the committed row still has no result.
        wait_for_blocked_query(&pool, first_pid, "resource_pool_allocations").await;
        first.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (competing_write, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(save_competing, commit_first)
    })
    .await??;
    assert_eq!(competing_write, NotApplied(OperationNotCurrent));
    assert_eq!(load(&pool, request.request_id).await?, Some(saved));
    Ok(())
}

#[crate::sqlx_test]
async fn attachment_and_cancellation_commit_only_one_result(pool: PgPool) -> eyre::Result<()> {
    sqlx::query("CREATE TABLE remote_test_entity (id UUID PRIMARY KEY)")
        .execute(&pool)
        .await?;
    for attach_first in [true, false] {
        let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
        let authority = controller_authority();
        begin(&pool, &request, &authority).await?;
        let assignment = assignment();
        save_assignment(&pool, request.request_id, &assignment).await?;
        let mut first = pool.begin().await?;
        let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *first)
            .await?;
        if attach_first {
            sqlx::query("INSERT INTO remote_test_entity VALUES ($1)")
                .bind(request.request_id)
                .execute(&mut *first)
                .await?;
            assert_eq!(
                attach(&mut first, request.request_id, &authority, &assignment).await?,
                Applied(())
            );
        } else {
            assert_eq!(
                request_cancel(&mut first, request.request_id).await?,
                Applied(())
            );
        }
        let mut second = pool.begin().await?;
        let second_write = async {
            let result = if attach_first {
                request_cancel(&mut second, request.request_id).await?
            } else {
                sqlx::query("INSERT INTO remote_test_entity VALUES ($1)")
                    .bind(request.request_id)
                    .execute(&mut *second)
                    .await?;
                attach(&mut second, request.request_id, &authority, &assignment).await?
            };
            // Observe the losing transaction before its rollback can hide a write.
            assert_eq!(
                load(second.as_mut(), request.request_id).await?,
                load(&pool, request.request_id).await?
            );
            second.rollback().await?;
            Ok::<_, eyre::Report>(result)
        };
        let commit_first = async {
            wait_for_blocked_query(&pool, first_pid, "resource_pool_allocations").await;
            first.commit().await?;
            Ok::<_, eyre::Report>(())
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(second_write, commit_first)
        })
        .await??;
        assert_eq!(
            result,
            NotApplied(OperationNotCurrent),
            "attach_first={attach_first}"
        );
        let state = if attach_first {
            AllocationState::Attached
        } else {
            AllocationState::CancelRequested
        };
        assert_eq!(
            load(&pool, request.request_id)
                .await?
                .expect("competing request")
                .state,
            state
        );
        let entity_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM remote_test_entity WHERE id = $1)")
                .bind(request.request_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(entity_exists, attach_first);
    }
    Ok(())
}

#[crate::sqlx_test]
async fn attachment_requires_the_saved_authority_and_deadline(pool: PgPool) -> eyre::Result<()> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await?;
    let foreground_attempt_id = Uuid::new_v4();
    let foreground = CompletionAuthority::Foreground {
        attempt_id: foreground_attempt_id,
        attach_before: now + chrono::Duration::minutes(5),
    };
    let expired = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before: now - chrono::Duration::seconds(1),
    };
    check_cases_async(
        [
            (
                "different foreground attempt",
                foreground.clone(),
                CompletionAuthority::Foreground {
                    attempt_id: Uuid::new_v4(),
                    attach_before: now + chrono::Duration::minutes(5),
                },
                NotApplied(OperationNotCurrent),
            ),
            (
                "expired foreground attempt",
                expired.clone(),
                expired,
                NotApplied(OperationNotCurrent),
            ),
            (
                "caller extends the saved deadline",
                foreground.clone(),
                CompletionAuthority::Foreground {
                    attempt_id: foreground_attempt_id,
                    attach_before: now + chrono::Duration::minutes(10),
                },
                NotApplied(OperationNotCurrent),
            ),
            (
                "different controller desire",
                controller_authority(),
                controller_authority(),
                NotApplied(OperationNotCurrent),
            ),
            (
                "current foreground attempt",
                foreground.clone(),
                foreground,
                Applied(()),
            ),
        ]
        .map(|(scenario, stored, supplied, expected)| Case {
            scenario,
            input: (stored, supplied),
            expect: Yields(expected),
        }),
        |(stored, supplied)| {
            let pool = &pool;
            async move {
                let request =
                    new_request(pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
                begin(pool, &request, &stored).await?;
                let assignment = assignment();
                save_assignment(pool, request.request_id, &assignment).await?;
                let mut txn = pool.begin().await?;
                let result = attach(&mut txn, request.request_id, &supplied, &assignment).await?;
                txn.commit().await?;
                let state = if result == Applied(()) {
                    AllocationState::Attached
                } else {
                    AllocationState::Pending
                };
                assert_eq!(
                    load(pool, request.request_id)
                        .await?
                        .expect("saved request")
                        .state,
                    state
                );
                Ok::<_, eyre::Report>(result)
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn foreground_writes_recheck_deadline_after_waiting_for_the_row(
    pool: PgPool,
) -> eyre::Result<()> {
    #[derive(Clone, Copy, Debug)]
    enum Write {
        Attach,
        CancelExpired,
    }
    for (write, expected_result, expected_state) in [
        (
            Write::Attach,
            NotApplied(OperationNotCurrent),
            AllocationState::Pending,
        ),
        (
            Write::CancelExpired,
            Applied(()),
            AllocationState::CancelRequested,
        ),
    ] {
        let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
        let deadline: DateTime<Utc> =
            sqlx::query_scalar("SELECT clock_timestamp() + interval '10 seconds'")
                .fetch_one(&pool)
                .await?;
        let authority = CompletionAuthority::Foreground {
            attempt_id: Uuid::new_v4(),
            attach_before: deadline,
        };
        begin(&pool, &request, &authority).await?;
        let assignment = assignment();
        save_assignment(&pool, request.request_id, &assignment).await?;
        let mut blocker = pool.begin().await?;
        sqlx::query(
            "SELECT allocation_id FROM resource_pool_allocations
            WHERE allocation_id = $1 FOR UPDATE",
        )
        .bind(request.request_id)
        .fetch_one(&mut *blocker)
        .await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let mut writer = pool.begin().await?;
        let write_attempt = async {
            let result = match write {
                Write::Attach => {
                    attach(&mut writer, request.request_id, &authority, &assignment).await?
                }
                Write::CancelExpired => {
                    request_cancel_if_expired(&mut writer, request.request_id).await?
                }
            };
            writer.commit().await?;
            Ok::<_, eyre::Report>(result)
        };
        let expire_before_unlock = async {
            wait_for_blocked_query(&pool, blocker_pid, "resource_pool_allocations").await;
            let still_valid: bool = sqlx::query_scalar("SELECT clock_timestamp() < $1")
                .bind(deadline)
                .fetch_one(&pool)
                .await?;
            assert!(
                still_valid,
                "the foreground write must start waiting before its deadline"
            );
            // The blocker changes no row, so only the passage of database time
            // can reject attachment or allow cancellation.
            sqlx::query(
                "SELECT pg_sleep(GREATEST(
                    EXTRACT(EPOCH FROM ($1::timestamptz - clock_timestamp())), 0
                ))",
            )
            .bind(deadline)
            .execute(&pool)
            .await?;
            blocker.commit().await?;
            Ok::<_, eyre::Report>(())
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::try_join!(write_attempt, expire_before_unlock)
        })
        .await??;
        assert_eq!(result, expected_result, "{write:?}");
        assert_eq!(
            load(&pool, request.request_id)
                .await?
                .expect("expired attempt")
                .state,
            expected_state,
            "{write:?}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn expiry_cancellation_preserves_ineligible_requests(pool: PgPool) -> eyre::Result<()> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&pool)
        .await?;
    let deadline = now + chrono::Duration::seconds(10);
    let foreground = |attach_before| CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
        attach_before,
    };
    let mut preserved = Vec::new();
    for (scenario, authority, state) in [
        (
            "pending controller request",
            controller_authority(),
            AllocationState::Pending,
        ),
        (
            "live foreground request",
            foreground(now + chrono::Duration::minutes(5)),
            AllocationState::Pending,
        ),
        (
            "attached foreground allocation after its deadline",
            foreground(deadline),
            AllocationState::Attached,
        ),
        (
            "foreground release request after its deadline",
            foreground(deadline),
            AllocationState::ReleaseRequested,
        ),
    ] {
        let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
        begin(&pool, &request, &authority).await?;
        if state != AllocationState::Pending {
            let assignment = assignment();
            attach_assignment(&pool, request.request_id, &authority, &assignment).await?;
            if state == AllocationState::ReleaseRequested {
                let mut txn = pool.begin().await?;
                assert_eq!(
                    request_release(&mut txn, request.request_id, &assignment.allocation_id)
                        .await?,
                    Applied(()),
                    "{scenario}"
                );
                txn.commit().await?;
            }
        }
        let saved = load(&pool, request.request_id).await?.expect(scenario);
        assert_eq!(saved.state, state, "{scenario}");
        preserved.push((scenario, saved));
    }

    let mut clock = pool.acquire().await?;
    sqlx::query(
        "SELECT pg_sleep(GREATEST(
            EXTRACT(EPOCH FROM ($1::timestamptz - clock_timestamp())), 0
        ))",
    )
    .bind(deadline)
    .execute(&mut *clock)
    .await?;
    wait_for_database_time_after(&mut clock, deadline).await?;
    drop(clock);
    for (scenario, saved) in preserved {
        let mut txn = pool.begin().await?;
        assert_eq!(
            request_cancel_if_expired(&mut txn, saved.allocation_id).await?,
            NotApplied(OperationNotCurrent),
            "{scenario}"
        );
        txn.commit().await?;
        assert_eq!(
            load(&pool, saved.allocation_id).await?,
            Some(saved),
            "{scenario}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn recovery_pages_are_bounded_ordered_and_filtered_by_state(
    pool: PgPool,
) -> eyre::Result<()> {
    let authority = controller_authority();
    for id in [4, 1, 3, 2] {
        let mut request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
        request.request_id = Uuid::from_u128(id);
        begin(&pool, &request, &authority).await?;
    }
    let mut txn = pool.begin().await?;
    assert_eq!(
        request_cancel(&mut txn, Uuid::from_u128(3)).await?,
        Applied(())
    );
    txn.commit().await?;
    let limit = NonZeroU16::new(2).expect("nonzero page size");
    let first = list(&pool, AllocationState::Pending, None, None, limit).await?;
    assert_eq!(
        first
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let second = list(
        &pool,
        AllocationState::Pending,
        Some(first[1].allocation_id),
        None,
        limit,
    )
    .await?;
    assert_eq!(
        second
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(
        list(
            &pool,
            AllocationState::Pending,
            Some(second[0].allocation_id),
            None,
            limit
        )
        .await?
        .is_empty()
    );
    let cancelled = list(&pool, AllocationState::CancelRequested, None, None, limit).await?;
    assert_eq!(
        cancelled
            .iter()
            .map(|row| row.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        vec![3]
    );
    let inventory = ResourcePool::new("integrated-recovery".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![71_i64], true).await?;
    allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "machine-1",
        None,
    )
    .await?;
    let integrated = allocation::list(&pool, inventory.name(), None, limit)
        .await?
        .pop()
        .expect("integrated allocation");
    assert_eq!(load(&pool, integrated.allocation_id).await?, None);

    let mut attached_request =
        new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    attached_request.request_id = Uuid::from_u128(5);
    let mut colliding_request = attached_request.clone();
    colliding_request.request_id = integrated.allocation_id;
    assert!(matches!(
        begin(&pool, &colliding_request, &authority).await,
        Err(RemoteAllocationError::RequestConflict(id)) if id == integrated.allocation_id
    ));
    assert_eq!(
        allocation::load(&pool, integrated.allocation_id).await?,
        Some(integrated)
    );
    begin(&pool, &attached_request, &authority).await?;
    attach_assignment(
        &pool,
        attached_request.request_id,
        &authority,
        &assignment(),
    )
    .await?;
    let attached = list(&pool, AllocationState::Attached, None, None, limit).await?;
    assert_eq!(
        attached
            .iter()
            .map(|row| row.allocation_id)
            .collect::<Vec<_>>(),
        vec![attached_request.request_id]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn attachment_requires_the_original_ready_assignment(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let pending = begin(&pool, &request, &authority).await?;
    let assignment = assignment();
    let mut txn = pool.begin().await?;
    assert_eq!(
        attach(&mut txn, request.request_id, &authority, &assignment).await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(pending));

    save_assignment(&pool, request.request_id, &assignment).await?;
    let ready = load(&pool, request.request_id)
        .await?
        .expect("saved assignment");
    let changed = AllocationAssignment {
        route_target_policy: Some(ProviderRouteTargetPolicy {
            mode: ProviderRouteTargetMode::Replace,
            imports: Some(vec![]),
            exports: Some(vec![]),
            peer_identity: vec![],
        }),
        ..assignment.clone()
    };
    let mut txn = pool.begin().await?;
    assert_eq!(
        record_result(
            &mut txn,
            request.request_id,
            &AllocationResult::Allocated(changed.clone())
        )
        .await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        attach(&mut txn, request.request_id, &authority, &changed).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        request_release(&mut txn, request.request_id, &assignment.allocation_id).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        confirm_cleanup(&mut txn, request.request_id, CleanupConfirmation::Released).await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(ready));

    let mut txn = pool.begin().await?;
    assert_eq!(
        attach(&mut txn, request.request_id, &authority, &assignment).await?,
        Applied(())
    );
    txn.commit().await?;
    let attached = load(&pool, request.request_id)
        .await?
        .expect("attached assignment");
    let mut txn = pool.begin().await?;
    assert_eq!(
        record_result(
            &mut txn,
            request.request_id,
            &AllocationResult::Allocated(assignment)
        )
        .await?,
        NotApplied(OperationNotCurrent)
    );
    txn.commit().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(attached));
    Ok(())
}

#[crate::sqlx_test]
async fn entity_deletion_commits_release_work_for_later_cleanup(pool: PgPool) -> eyre::Result<()> {
    sqlx::query("CREATE TABLE remote_test_entity (id UUID PRIMARY KEY)")
        .execute(&pool)
        .await?;
    let mut request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    request.selection = AllocationSelection::Exact(ResourcePoolValue::Integer(73));
    let authority = controller_authority();
    begin(&pool, &request, &authority).await?;
    let assignment = AllocationAssignment {
        route_target_policy: Some(ProviderRouteTargetPolicy {
            mode: ProviderRouteTargetMode::Replace,
            imports: Some(vec![
                RouteTargetConfig { asn: 64512, vni: 9 },
                RouteTargetConfig { asn: 64512, vni: 3 },
                RouteTargetConfig { asn: 64512, vni: 9 },
            ]),
            exports: Some(vec![]),
            peer_identity: vec![],
        }),
        ..assignment()
    };
    save_assignment(&pool, request.request_id, &assignment).await?;
    let mut create = pool.begin().await?;
    sqlx::query("INSERT INTO remote_test_entity VALUES ($1)")
        .bind(request.request_id)
        .execute(&mut *create)
        .await?;
    assert_eq!(
        attach(&mut create, request.request_id, &authority, &assignment).await?,
        Applied(())
    );
    create.commit().await?;
    let mut observer = pool.acquire().await?;
    let attached = load(&mut *observer, request.request_id)
        .await?
        .expect("attached entity");
    let allocation_id = request.request_id;
    let attached_allocation = allocation::load(&mut *observer, allocation_id)
        .await?
        .expect("attached allocation");
    assert_eq!(attached_allocation.state, AllocationState::Attached);

    for (scenario, commit) in [
        ("rolled back deletion", false),
        ("committed deletion", true),
    ] {
        let mut deletion = pool.begin().await?;
        sqlx::query("DELETE FROM remote_test_entity WHERE id = $1")
            .bind(request.request_id)
            .execute(&mut *deletion)
            .await?;
        assert_eq!(
            request_release(&mut deletion, request.request_id, &assignment.allocation_id).await?,
            Applied(())
        );
        if commit {
            deletion.commit().await?;
        } else {
            deletion.rollback().await?;
            assert_eq!(
                load(&mut *observer, request.request_id).await?,
                Some(attached.clone()),
                "{scenario}"
            );
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM remote_test_entity WHERE id = $1)")
                .bind(request.request_id)
                .fetch_one(&mut *observer)
                .await?;
        assert_eq!(exists, !commit, "{scenario}");
        let visible = allocation::load(&mut *observer, allocation_id)
            .await?
            .expect("allocation survives entity deletion");
        assert_saved_request(&visible, &request, &authority);
        assert_eq!(visible.source, attached_allocation.source, "{scenario}");
        assert_eq!(
            visible.created_at, attached_allocation.created_at,
            "{scenario}"
        );
        assert_eq!(
            visible.state,
            if commit {
                AllocationState::ReleaseRequested
            } else {
                AllocationState::Attached
            },
            "{scenario}"
        );
        assert_eq!(
            allocation::list(
                &mut *observer,
                &request.pool.logical_pool,
                None,
                NonZeroU16::new(10).expect("test page size")
            )
            .await?,
            vec![visible],
            "{scenario}"
        );
    }
    let pending_release = load(&mut *observer, request.request_id)
        .await?
        .expect("release work");
    assert_eq!(pending_release.state, AllocationState::ReleaseRequested);
    assert_saved_request(&pending_release, &request, &authority);
    let result = AllocationResult::Allocated(assignment);
    assert_eq!(saved_result(&pending_release), Some(&result));
    assert_eq!(
        list(
            &mut *observer,
            AllocationState::ReleaseRequested,
            None,
            None,
            NonZeroU16::new(1).expect("one row")
        )
        .await?,
        vec![pending_release.clone()]
    );

    let cleanup_requested = allocation::load(&mut *observer, allocation_id)
        .await?
        .expect("allocation waiting for cleanup");
    let mut rolled_back_cleanup = pool.begin().await?;
    assert_eq!(
        confirm_cleanup(
            &mut rolled_back_cleanup,
            request.request_id,
            CleanupConfirmation::Released
        )
        .await?,
        Applied(())
    );
    assert_eq!(
        allocation::load(rolled_back_cleanup.as_mut(), allocation_id).await?,
        None
    );
    rolled_back_cleanup.rollback().await?;
    assert_eq!(
        load(&mut *observer, request.request_id).await?,
        Some(pending_release)
    );
    assert_eq!(
        allocation::load(&mut *observer, allocation_id).await?,
        Some(cleanup_requested)
    );

    let mut cleanup = pool.begin().await?;
    assert_eq!(
        confirm_cleanup(
            &mut cleanup,
            request.request_id,
            CleanupConfirmation::Released
        )
        .await?,
        Applied(())
    );
    cleanup.commit().await?;
    let released = load(&mut *observer, request.request_id)
        .await?
        .expect("completed release");
    assert_eq!(released.state, AllocationState::Released);
    assert_eq!(saved_result(&released), Some(&result));
    assert_eq!(allocation::load(&mut *observer, allocation_id).await?, None);
    Ok(())
}

#[crate::sqlx_test]
async fn allocation_reads_survive_added_columns(pool: PgPool) -> eyre::Result<()> {
    let value = ResourcePoolValue::IpAddress("2001:db8::73".parse()?);
    let mut request =
        new_request(&pool, "manual-loopback", ResourcePoolValueDomain::IpAddress).await?;
    request.owner.owner_type = OwnerType::Machine;
    request.owner.slot = "loopback".to_owned();
    request.selection = AllocationSelection::Requested(value.clone());
    begin(&pool, &request, &controller_authority()).await?;
    let assignment = AllocationAssignment {
        value: value.clone(),
        assignment_mode: AssignmentMode::Manual,
        ..assignment()
    };
    save_assignment(&pool, request.request_id, &assignment).await?;
    let saved = load(&pool, request.request_id)
        .await?
        .expect("saved allocation");
    let allocation_id = request.request_id;
    let saved_allocation = allocation::load(&pool, allocation_id)
        .await?
        .expect("saved allocation");
    assert_eq!(
        saved_result(&saved_allocation),
        Some(&AllocationResult::Allocated(assignment))
    );
    let limit = NonZeroU16::new(1).expect("one row");
    let mut reader = pool.acquire().await?;
    assert_eq!(
        load(&mut *reader, request.request_id).await?,
        Some(saved.clone())
    );
    assert_eq!(
        list(&mut *reader, AllocationState::Pending, None, None, limit).await?,
        vec![saved.clone()]
    );
    assert_eq!(
        allocation::load(&mut *reader, allocation_id).await?,
        Some(saved_allocation.clone())
    );
    assert_eq!(
        allocation::list(&mut *reader, &request.pool.logical_pool, None, limit).await?,
        vec![saved_allocation.clone()]
    );
    assert!(reader.cached_statements_size() > 0);

    // Keep the API connection's prepared statements while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
        ALTER TABLE resource_pool_allocations ADD COLUMN test_added_column TEXT;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    assert_eq!(
        load(&mut *reader, request.request_id).await?,
        Some(saved.clone())
    );
    assert_eq!(
        list(&mut *reader, AllocationState::Pending, None, None, limit).await?,
        vec![saved]
    );
    assert_eq!(
        allocation::load(&mut *reader, allocation_id).await?,
        Some(saved_allocation.clone())
    );
    assert_eq!(
        allocation::list(&mut *reader, &request.pool.logical_pool, None, limit).await?,
        vec![saved_allocation]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn cleanup_waits_for_result_and_retains_its_terminal_allocation(
    pool: PgPool,
) -> eyre::Result<()> {
    let request = new_request(&pool, "late-result", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let pending = begin(&pool, &request, &authority).await?;
    let mut cancellation = pool.begin().await?;
    assert_eq!(
        request_cancel(&mut cancellation, request.request_id).await?,
        Applied(())
    );
    cancellation.commit().await?;

    let result = AllocationResult::Allocated(assignment());
    let mut provider_result = pool.begin().await?;
    assert_eq!(
        record_result(&mut provider_result, request.request_id, &result).await?,
        Applied(())
    );
    let allocation_id = request.request_id;
    let result_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(provider_result.as_mut())
        .await?;
    let mut cleanup = pool.begin().await?;
    let confirm = async {
        let applied = confirm_cleanup(
            &mut cleanup,
            request.request_id,
            CleanupConfirmation::Released,
        )
        .await?;
        cleanup.commit().await?;
        Ok::<_, eyre::Report>(applied)
    };
    let finish_result = async {
        // Keep cleanup blocked until the provider result commits. Its state
        // update must preserve the result written by the other transaction.
        wait_for_blocked_query(&pool, result_pid, "resource_pool_allocations").await;
        provider_result.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (applied, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(confirm, finish_result)
    })
    .await??;
    assert_eq!(applied, Applied(()));
    assert_eq!(allocation::load(&pool, allocation_id).await?, None);
    let released = load(&pool, request.request_id)
        .await?
        .expect("retained terminal allocation");
    assert_eq!(released.created_at, pending.created_at);
    assert_eq!(released.state, AllocationState::Released);
    assert_saved_request(&released, &request, &authority);
    assert_eq!(saved_result(&released), Some(&result));
    Ok(())
}

/// Long owner and provider strings must keep their exact text without bypassing
/// ownership checks when the database indexes their hashes.
#[crate::sqlx_test]
async fn opaque_provider_strings_survive_replay_release_and_recovery(
    pool: PgPool,
) -> eyre::Result<()> {
    // Random UUID text keeps long identities from compressing into short
    // B-tree keys, which would hide oversized-key failures.
    let long_identity: String = (0..256).map(|_| Uuid::new_v4().to_string()).collect();
    for (name, identity) in [
        // Unicode and surrounding spaces remain part of each identity.
        ("unicode-provider", " Provider é ".to_owned()),
        // Owner, slot and provider keys must fit even without compression.
        ("long-provider", format!(" {long_identity} ")),
    ] {
        let mut binding_txn = pool.begin().await?;
        let selection = ResourcePoolSelection {
            pool_name: name.to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("fabric".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: identity.clone(),
            remote_pool: "tenant-vnis".to_owned(),
        };
        let binding = bind_or_verify(&mut binding_txn, &selection).await?;
        assert_eq!(bind_or_verify(&mut binding_txn, &selection).await?, binding);
        binding_txn.commit().await?;
        assert_eq!(
            crate::resource_pool::binding::load(&pool, name).await?,
            Some(binding.clone())
        );
        assert!(
            crate::resource_pool::binding::all(&pool)
                .await?
                .contains(&binding)
        );

        let request = RemoteAllocationRequest {
            request_id: Uuid::new_v4(),
            binding_id: binding.binding_id,
            pool: RemotePoolIdentity {
                authority_id: identity.clone(),
                pool_id: identity.clone(),
                logical_pool: name.to_owned(),
            },
            owner: AllocationOwner {
                owner_type: OwnerType::Vpc,
                owner_id: identity.clone(),
                slot: identity.clone(),
                generation: 0,
            },
            eligibility_revision: "revision-1".to_owned(),
            selection: AllocationSelection::Automatic,
        };
        let authority = controller_authority();
        let pending = begin(&pool, &request, &authority).await?;
        assert_saved_request(&pending, &request, &authority);
        assert_eq!(begin(&pool, &request, &authority).await?, pending);

        // A different generation must still wait for this owner's pending slot.
        let newer = RemoteAllocationRequest {
            request_id: Uuid::new_v4(),
            owner: AllocationOwner {
                generation: 1,
                ..request.owner.clone()
            },
            ..request.clone()
        };
        assert!(matches!(
            begin(&pool, &newer, &authority).await,
            Err(RemoteAllocationError::SlotInUse(_))
        ));
        let assignment = AllocationAssignment {
            allocation_id: identity,
            ..assignment()
        };
        attach_assignment(&pool, request.request_id, &authority, &assignment).await?;
        let mut cleanup = pool.begin().await?;
        assert_eq!(
            request_release(
                &mut cleanup,
                request.request_id,
                assignment.allocation_id.trim()
            )
            .await?,
            NotApplied(OperationNotCurrent)
        );
        assert_eq!(
            request_release(&mut cleanup, request.request_id, &assignment.allocation_id).await?,
            Applied(())
        );
        assert_eq!(
            confirm_cleanup(
                &mut cleanup,
                request.request_id,
                CleanupConfirmation::Released
            )
            .await?,
            Applied(())
        );
        cleanup.commit().await?;
        let terminal = load(&pool, request.request_id)
            .await?
            .expect("retained opaque assignment");
        assert_eq!(terminal.state, AllocationState::Released);
        assert_saved_request(&terminal, &request, &authority);
        assert_eq!(
            saved_result(&terminal),
            Some(&AllocationResult::Allocated(assignment.clone()))
        );
        assert_eq!(begin(&pool, &request, &authority).await?, terminal);
        assert!(
            list(
                &pool,
                AllocationState::Released,
                None,
                None,
                NonZeroU16::new(10).unwrap()
            )
            .await?
            .contains(&terminal)
        );

        // Cleanup frees the slot, but the completed generation cannot be reused.
        let reused = RemoteAllocationRequest {
            request_id: Uuid::new_v4(),
            ..request.clone()
        };
        assert!(matches!(
            begin(&pool, &reused, &authority).await,
            Err(RemoteAllocationError::GenerationExists(_))
        ));
        begin(&pool, &newer, &authority).await?;
        let mut duplicate = pool.begin().await?;
        let error = record_result(
            &mut duplicate,
            newer.request_id,
            &AllocationResult::Allocated(assignment),
        )
        .await
        .expect_err("terminal handle remains unique");
        assert!(error.violates_constraint("resource_pool_allocations_handle"));
        duplicate.rollback().await?;

        let rejection = AllocationResult::Failed(AllocationFailure {
            code: AllocationFailureCode::FailedPrecondition,
            message: " Provider diagnostic é ".to_owned(),
        });
        let mut result_txn = pool.begin().await?;
        assert_eq!(
            record_result(&mut result_txn, newer.request_id, &rejection).await?,
            Applied(())
        );
        result_txn.commit().await?;
        assert_eq!(
            saved_result(
                &load(&pool, newer.request_id)
                    .await?
                    .expect("saved rejection")
            ),
            Some(&rejection)
        );
    }
    Ok(())
}

/// Rebuilding the identity indexes must preserve existing allocations while an
/// Integrated writer finishes, then accept long identities through normal writes.
#[crate::sqlx_test]
async fn provider_string_indexes_preserve_allocations_with_a_live_writer(
    pool: PgPool,
) -> eyre::Result<()> {
    const VERSION: i64 = 20261020025342;
    const MIGRATION: &str =
        include_str!("../../../migrations/20261020025342_resource_pool_provider_strings.sql");
    crate::migrations::migrate_before(&pool, VERSION).await;

    let request = new_request(&pool, "migration-remote", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    begin(&pool, &request, &authority).await?;
    let assignment = assignment();
    attach_assignment(&pool, request.request_id, &authority, &assignment).await?;
    let old_remote = load(&pool, request.request_id)
        .await?
        .expect("predecessor allocation");

    let inventory = ResourcePool::new("migration-integrated".to_owned(), ValueType::Integer);
    let mut txn = pool.begin().await?;
    populate(&inventory, &mut txn, vec![73_i64], true).await?;
    allocate(
        &inventory,
        &mut txn,
        OwnerType::Vpc,
        "integrated-owner",
        None,
    )
    .await?;
    let limit = NonZeroU16::new(10).unwrap();
    let old_integrated = allocation::list(&mut *txn, inventory.name(), None, limit).await?;
    assert_eq!(old_integrated.len(), 1);
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *txn)
        .await?;

    let migrations = sqlx::migrate!("./migrations");
    assert_eq!(
        migrations
            .iter()
            .find(|migration| migration.version == VERSION)
            .expect("provider string index migration")
            .sql
            .as_ref(),
        MIGRATION
    );
    let migrate = async {
        crate::migrations::migrate(&pool).await?;
        Ok::<_, eyre::Report>(())
    };
    let commit_writer = async {
        wait_for_blocked_query(&pool, writer_pid, "resource_pool_allocations_handle").await;
        txn.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    tokio::time::timeout(Duration::from_secs(40), async {
        tokio::try_join!(migrate, commit_writer)
    })
    .await??;

    assert_eq!(begin(&pool, &request, &authority).await?, old_remote);
    assert_eq!(
        allocation::list(&pool, inventory.name(), None, limit).await?,
        old_integrated
    );
    let mut release = pool.begin().await?;
    assert_eq!(
        request_release(&mut release, request.request_id, &assignment.allocation_id).await?,
        Applied(())
    );
    release.commit().await?;

    // Exercise the rebuilt owner indexes with text that cannot compress into
    // a short B-tree key; the old allocation must still reserve its handle.
    let long_identity: String = (0..256).map(|_| Uuid::new_v4().to_string()).collect();
    let newer = RemoteAllocationRequest {
        request_id: Uuid::new_v4(),
        owner: AllocationOwner {
            owner_id: long_identity.clone(),
            slot: long_identity,
            generation: 1,
            ..request.owner.clone()
        },
        ..request
    };
    let pending = begin(&pool, &newer, &authority).await?;
    assert_saved_request(&pending, &newer, &authority);
    assert_eq!(load(&pool, newer.request_id).await?, Some(pending));
    let mut duplicate = pool.begin().await?;
    let error = record_result(
        &mut duplicate,
        newer.request_id,
        &AllocationResult::Allocated(assignment),
    )
    .await
    .expect_err("migrated assignment still reserves its handle");
    assert!(error.violates_constraint("resource_pool_allocations_handle"));
    duplicate.rollback().await?;
    Ok(())
}
