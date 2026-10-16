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
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationFailure, AllocationFailureCode, AllocationOwner,
    AllocationResult, AllocationSelection, CleanupConfirmation, CompletionAuthority,
    ProviderRouteTargetMode, ProviderRouteTargetPolicy, RemoteAllocationRequest,
    RemoteOperationState, RemotePoolIdentity,
};
use model::resource_pool::{
    OwnerType, ResourcePoolBackendKind, ResourcePoolSelection, ResourcePoolValueDomain,
};
use model::vpc::routing_profile::RouteTargetConfig;
use sqlx::{Acquire, Connection, PgPool};
use uuid::Uuid;

use super::{
    OperationNotCurrent, RemoteAllocationError, attach, begin, confirm_cleanup, list, load,
    record_result, request_cancel, request_release,
};
use crate::ConditionalWrite::{Applied, NotApplied};
use crate::db_read::DbReader;
use crate::resource_pool::allocation;
use crate::resource_pool::binding::bind_or_verify;
use crate::test_support::postgres::wait_for_blocked_query;

async fn allocation_id_for_request(
    db: impl DbReader<'_>,
    request_id: Uuid,
) -> eyre::Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE remote_request_id = $1",
    )
    .bind(request_id)
    .fetch_optional(db)
    .await?)
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
            site_id: Uuid::new_v4(),
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
async fn saved_request_and_result_survive_entity_rollback(pool: PgPool) -> eyre::Result<()> {
    sqlx::query("CREATE TABLE remote_test_entity (id UUID PRIMARY KEY)")
        .execute(&pool)
        .await?;
    let mut request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    request.owner.generation = u64::MAX;
    let authority = controller_authority();
    let saved = begin(&pool, &request, &authority).await?;
    assert_eq!(saved.state, RemoteOperationState::Pending);
    assert_eq!(saved.result, None);
    assert_eq!(
        allocation_id_for_request(&pool, request.request_id).await?,
        None
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
    let rolled_back_id = allocation_id_for_request(rolled_back_result.as_mut(), request.request_id)
        .await?
        .expect("uncommitted allocation record");
    assert!(
        allocation::load(rolled_back_result.as_mut(), rolled_back_id)
            .await?
            .is_some()
    );
    rolled_back_result.rollback().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(saved));
    assert_eq!(allocation::load(&pool, rolled_back_id).await?, None);

    let mut result_txn = pool.begin().await?;
    let (transaction_started, before_record): (DateTime<Utc>, DateTime<Utc>) =
        sqlx::query_as("SELECT transaction_timestamp(), clock_timestamp()")
            .fetch_one(result_txn.as_mut())
            .await?;
    assert_eq!(
        record_result(&mut result_txn, request.request_id, &result).await?,
        Applied(())
    );
    let allocation_id = allocation_id_for_request(result_txn.as_mut(), request.request_id)
        .await?
        .expect("saved allocation record");
    let reserved = allocation::load(result_txn.as_mut(), allocation_id)
        .await?
        .expect("saved allocation");
    assert_eq!(
        reserved.source,
        AllocationSource::Remote {
            request_id: request.request_id
        }
    );
    assert_eq!(reserved.pool_name, request.pool.logical_pool);
    assert_eq!(reserved.owner_type, request.owner.owner_type);
    assert_eq!(reserved.owner_id, request.owner.owner_id);
    assert_eq!(reserved.value, assignment.value);
    assert_eq!(reserved.assignment_mode, Some(assignment.assignment_mode));
    assert_eq!(reserved.allocated_at, None);
    assert_eq!(reserved.state, AllocationState::Reserved);
    let after_record: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(result_txn.as_mut())
        .await?;
    assert!(
        transaction_started < reserved.recorded_at
            && before_record <= reserved.recorded_at
            && reserved.recorded_at <= after_record,
        "the allocation record uses insertion time, not transaction start time"
    );
    result_txn.commit().await?;

    let mut replay_txn = pool.begin().await?;
    assert_eq!(
        record_result(&mut replay_txn, request.request_id, &result).await?,
        Applied(())
    );
    let different = AllocationResult::Failed(AllocationFailure {
        code: AllocationFailureCode::ResourceExhausted,
        message: "no available VNIs".to_owned(),
    });
    assert_eq!(
        record_result(&mut replay_txn, request.request_id, &different).await?,
        NotApplied(OperationNotCurrent)
    );
    assert_eq!(
        allocation_id_for_request(replay_txn.as_mut(), request.request_id).await?,
        Some(allocation_id)
    );
    assert_eq!(
        allocation::load(replay_txn.as_mut(), allocation_id).await?,
        Some(reserved.clone())
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

    // Another connection sees the committed request while entity creation is
    // still in progress. Rolling that creation back must not erase the request.
    let mut observer = pool.acquire().await?;
    let observer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *observer)
        .await?;
    assert_ne!(entity_pid, observer_pid);
    let before_rollback = load(&mut *observer, request.request_id)
        .await?
        .expect("saved request");
    assert_eq!(before_rollback.request, request);
    assert_eq!(before_rollback.completion_authority, authority);
    assert_eq!(before_rollback.result, Some(result));
    assert_eq!(before_rollback.state, RemoteOperationState::Pending);
    assert_eq!(
        allocation::load(&mut *observer, allocation_id).await?,
        Some(reserved.clone())
    );
    entity_txn.rollback().await?;
    assert_eq!(
        allocation::load(&mut *observer, allocation_id).await?,
        Some(reserved)
    );
    assert_eq!(
        load(&mut *observer, request.request_id).await?,
        Some(before_rollback)
    );
    let entity_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM remote_test_entity)")
            .fetch_one(&mut *observer)
            .await?;
    assert!(!entity_exists);
    Ok(())
}

#[crate::sqlx_test]
async fn request_replay_rejects_changed_inputs(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "vpc-vni", ResourcePoolValueDomain::Integer).await?;
    let authority = controller_authority();
    let saved = begin(&pool, &request, &authority).await?;
    assert_eq!(begin(&pool, &request, &authority).await?, saved);

    let mut provider = request.clone();
    provider.pool.pool_id = "another-provider-pool".to_owned();
    let mut owner = request.clone();
    owner.owner.generation += 1;
    let mut eligibility = request.clone();
    eligibility.eligibility_revision = "eligible-vnis-2".to_owned();
    let mut selection = request.clone();
    selection.selection = AllocationSelection::Exact(ResourcePoolValue::Integer(73));
    check_cases_async(
        [
            ("provider identity", provider, authority.clone()),
            ("owner generation", owner, authority.clone()),
            ("eligibility revision", eligibility, authority.clone()),
            ("allocation method", selection, authority.clone()),
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
    check_cases_async(
        [
            ("different authority", wrong_authority),
            ("different logical pool", wrong_pool),
            ("absent binding", absent_binding),
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
async fn committed_failure_is_immutable_and_creates_no_allocation(
    pool: PgPool,
) -> eyre::Result<()> {
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
            async move {
                let mut txn = pool.begin().await?;
                let applied = record_result(&mut txn, request.request_id, &result).await?;
                txn.commit().await?;
                let rejected = load(pool, request.request_id)
                    .await?
                    .expect("saved rejection");
                assert_eq!(rejected.request, saved.request);
                assert_eq!(rejected.completion_authority, saved.completion_authority);
                assert_eq!(rejected.created_at, saved.created_at);
                assert_eq!(rejected.state, RemoteOperationState::Pending);
                assert_eq!(rejected.result.as_ref(), Some(rejection));
                assert_eq!(
                    allocation_id_for_request(pool, request.request_id).await?,
                    None
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
    let late_allocation_id = allocation_id_for_request(&pool, replacement.request_id)
        .await?
        .expect("late assignment allocation record");
    assert_eq!(
        allocation::load(&pool, late_allocation_id)
            .await?
            .expect("late allocation")
            .state,
        AllocationState::CleanupRequested
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
    assert_eq!(cleaned_up.state, RemoteOperationState::Released);
    assert_eq!(cleaned_up.result, Some(late_result));
    assert_eq!(allocation::load(&pool, late_allocation_id).await?, None);
    assert_eq!(begin(&pool, &replacement, &authority).await?, cleaned_up);

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
        RemoteOperationState::Pending
    );
    assert_eq!(
        load(&pool, second_slot.request_id)
            .await?
            .expect("other VPC slot")
            .state,
        RemoteOperationState::Pending
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
    assert_eq!(terminal.state, RemoteOperationState::Cancelled);
    assert_eq!(terminal.result, None);
    assert_eq!(
        allocation_id_for_request(&pool, second_slot.request_id).await?,
        None
    );
    assert_eq!(begin(&pool, &second_slot, &authority).await?, terminal);
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
    let Some(AllocationResult::Allocated(old_assignment)) = old.result.clone() else {
        panic!("the old worker must load its attached allocation");
    };
    let old_request_id = old.request.request_id;
    let old_allocation_id = allocation_id_for_request(&mut *stale_worker, old_request_id)
        .await?
        .expect("old allocation record");
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
        Some("resource_pool_operations_handle")
    );
    reused_handle.rollback().await?;
    assert_eq!(load(&pool, newer.request_id).await?, Some(pending));
    assert_eq!(
        allocation_id_for_request(&pool, newer.request_id).await?,
        None
    );
    assert_eq!(load(&pool, old_request_id).await?, Some(old));

    // Handles belong to the issuing pool. Another pool may use the same string.
    let mut other_pool = newer.clone();
    other_pool.request_id = Uuid::new_v4();
    other_pool.owner.owner_id = Uuid::new_v4().to_string();
    other_pool.pool.pool_id = "provider-pool-18".to_owned();
    begin(&pool, &other_pool, &authority).await?;
    save_assignment(&pool, other_pool.request_id, &old_assignment).await?;
    assert_eq!(
        load(&pool, other_pool.request_id)
            .await?
            .expect("other pool allocation")
            .result,
        Some(old_result.clone())
    );

    let new_assignment = assignment();
    assert_eq!(new_assignment.value, old_assignment.value);
    attach_assignment(&pool, newer.request_id, &authority, &new_assignment).await?;
    let newest = load(&pool, newer.request_id)
        .await?
        .expect("new assignment");
    let new_allocation_id = allocation_id_for_request(&pool, newer.request_id)
        .await?
        .expect("new allocation record");
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
    assert_eq!(
        confirm_cleanup(&mut txn, old_request_id, CleanupConfirmation::Released).await?,
        Applied(())
    );
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
    assert_eq!(allocation::load(&pool, old_allocation_id).await?, None);
    assert_eq!(
        allocation::load(&pool, new_allocation_id).await?,
        Some(new_allocation)
    );
    let terminal = load(&pool, old_request_id)
        .await?
        .expect("released attempt");
    assert_eq!(terminal.state, RemoteOperationState::Released);
    assert_eq!(terminal.result, Some(old_result.clone()));

    // Finishing cleanup removes the allocation record but keeps the provider
    // handle reserved, so a later generation cannot claim the old assignment.
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
    .expect_err("a terminal operation still reserves its provider handle");
    let crate::DatabaseError::Sqlx(error) = error else {
        panic!("expected the provider handle constraint: {error}");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .expect("PostgreSQL error")
            .constraint(),
        Some("resource_pool_operations_handle")
    );
    reused_terminal_handle.rollback().await?;
    assert_eq!(load(&pool, old_request_id).await?, Some(terminal));
    assert_eq!(
        load(&pool, after_cleanup.request_id).await?,
        Some(pending_after_cleanup)
    );
    assert_eq!(
        allocation_id_for_request(&pool, after_cleanup.request_id).await?,
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
        sqlx::query("LOCK TABLE resource_pool_operations IN SHARE MODE")
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
                     AND strpos(query, 'INSERT INTO resource_pool_operations') > 0",
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
            assert_eq!(load(&pool, winner.request.request_id).await?, Some(winner));
            assert_eq!(load(&pool, loser).await?, None);
        }
    }
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
            second.rollback().await?;
            Ok::<_, eyre::Report>(result)
        };
        let commit_first = async {
            wait_for_blocked_query(&pool, first_pid, "resource_pool_operations").await;
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
            RemoteOperationState::Attached
        } else {
            RemoteOperationState::CancelRequested
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
    let foreground = CompletionAuthority::Foreground {
        attempt_id: Uuid::new_v4(),
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
                    RemoteOperationState::Attached
                } else {
                    RemoteOperationState::Pending
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
async fn attachment_rechecks_deadline_after_waiting_for_the_row(pool: PgPool) -> eyre::Result<()> {
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
    sqlx::query("SELECT request_id FROM resource_pool_operations WHERE request_id = $1 FOR UPDATE")
        .bind(request.request_id)
        .fetch_one(&mut *blocker)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let mut attaching = pool.begin().await?;
    let attachment = async {
        let result = attach(&mut attaching, request.request_id, &authority, &assignment).await?;
        attaching.commit().await?;
        Ok::<_, eyre::Report>(result)
    };
    let expire_before_unlock = async {
        wait_for_blocked_query(&pool, blocker_pid, "resource_pool_operations").await;
        let still_valid: bool = sqlx::query_scalar("SELECT clock_timestamp() < $1")
            .bind(deadline)
            .fetch_one(&pool)
            .await?;
        assert!(
            still_valid,
            "the attachment must start waiting before its deadline"
        );
        // Expire against PostgreSQL's clock, which is also used by `attach`.
        // The blocker changes no row, so only the passage of time can reject it.
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
        tokio::try_join!(attachment, expire_before_unlock)
    })
    .await??;
    assert_eq!(result, NotApplied(OperationNotCurrent));
    assert_eq!(
        load(&pool, request.request_id)
            .await?
            .expect("expired attempt")
            .state,
        RemoteOperationState::Pending
    );
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
    let first = list(&pool, RemoteOperationState::Pending, None, limit).await?;
    assert_eq!(
        first
            .iter()
            .map(|row| row.request.request_id.as_u128())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let second = list(
        &pool,
        RemoteOperationState::Pending,
        Some(first[1].request.request_id),
        limit,
    )
    .await?;
    assert_eq!(
        second
            .iter()
            .map(|row| row.request.request_id.as_u128())
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(
        list(
            &pool,
            RemoteOperationState::Pending,
            Some(second[0].request.request_id),
            limit
        )
        .await?
        .is_empty()
    );
    let cancelled = list(&pool, RemoteOperationState::CancelRequested, None, limit).await?;
    assert_eq!(
        cancelled
            .iter()
            .map(|row| row.request.request_id.as_u128())
            .collect::<Vec<_>>(),
        vec![3]
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
    let allocation_id = allocation_id_for_request(&mut *observer, request.request_id)
        .await?
        .expect("attached allocation record");
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
        let expected = model::resource_pool::allocation::ResourcePoolAllocation {
            state: if commit {
                AllocationState::CleanupRequested
            } else {
                AllocationState::Attached
            },
            ..attached_allocation.clone()
        };
        assert_eq!(
            allocation::load(&mut *observer, allocation_id).await?,
            Some(expected),
            "{scenario}"
        );
    }
    let pending_release = load(&mut *observer, request.request_id)
        .await?
        .expect("release work");
    assert_eq!(
        pending_release.state,
        RemoteOperationState::ReleaseRequested
    );
    assert_eq!(pending_release.request, request);
    let result = AllocationResult::Allocated(assignment);
    assert_eq!(pending_release.result, Some(result.clone()));
    assert_eq!(
        list(
            &mut *observer,
            RemoteOperationState::ReleaseRequested,
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
    assert_eq!(released.state, RemoteOperationState::Released);
    assert_eq!(released.result, Some(result));
    assert_eq!(allocation::load(&mut *observer, allocation_id).await?, None);
    Ok(())
}

#[crate::sqlx_test]
async fn operation_and_allocation_reads_survive_added_columns(pool: PgPool) -> eyre::Result<()> {
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
        .expect("saved operation");
    let allocation_id = allocation_id_for_request(&pool, request.request_id)
        .await?
        .expect("saved allocation record");
    let saved_allocation = allocation::load(&pool, allocation_id)
        .await?
        .expect("saved allocation");
    assert_eq!(saved_allocation.value, value);
    assert_eq!(
        saved_allocation.assignment_mode,
        Some(AssignmentMode::Manual)
    );
    let limit = NonZeroU16::new(1).expect("one row");
    let mut reader = pool.acquire().await?;
    assert_eq!(
        load(&mut *reader, request.request_id).await?,
        Some(saved.clone())
    );
    assert_eq!(
        list(&mut *reader, RemoteOperationState::Pending, None, limit).await?,
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
        ALTER TABLE resource_pool_operations ADD COLUMN test_added_column TEXT;
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
        list(&mut *reader, RemoteOperationState::Pending, None, limit).await?,
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
async fn allocation_record_failures_preserve_remote_operations(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "atomic-result", ResourcePoolValueDomain::Integer).await?;
    let pending = begin(&pool, &request, &controller_authority()).await?;
    let result = AllocationResult::Allocated(assignment());
    sqlx::query(
        "ALTER TABLE resource_pool_allocations
         ADD CONSTRAINT reject_test_pool CHECK (pool_name <> 'atomic-result')",
    )
    .execute(&pool)
    .await?;

    // `record_result` also accepts an autocommit connection. If inserting the
    // allocation record fails, the original result must roll back with it.
    let mut connection = pool.acquire().await?;
    let error = record_result(&mut connection, request.request_id, &result)
        .await
        .expect_err("allocation record rejection must reject the result write");
    assert!(error.to_string().contains("reject_test_pool"), "{error}");
    assert_eq!(load(&pool, request.request_id).await?, Some(pending));
    assert_eq!(
        allocation_id_for_request(&pool, request.request_id).await?,
        None
    );
    sqlx::query("ALTER TABLE resource_pool_allocations DROP CONSTRAINT reject_test_pool")
        .execute(&pool)
        .await?;
    assert_eq!(
        record_result(&mut connection, request.request_id, &result).await?,
        Applied(())
    );
    let allocation_id = allocation_id_for_request(&pool, request.request_id)
        .await?
        .expect("saved allocation record");
    let mut cancellation = pool.begin().await?;
    assert_eq!(
        request_cancel(&mut cancellation, request.request_id).await?,
        Applied(())
    );
    cancellation.commit().await?;
    let before_cleanup = load(&pool, request.request_id)
        .await?
        .expect("cancellation request");
    let allocation = allocation::load(&pool, allocation_id)
        .await?
        .expect("allocation waiting for cleanup");

    sqlx::raw_sql(
        "CREATE FUNCTION reject_test_remote_allocation_delete() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'test remote allocation record deletion failed'; END $$;
         CREATE TRIGGER reject_test_remote_allocation_delete BEFORE DELETE ON resource_pool_allocations
         FOR EACH ROW EXECUTE FUNCTION reject_test_remote_allocation_delete();",
    )
    .execute(&pool)
    .await?;
    let mut cleanup = pool.begin().await?;
    let error = confirm_cleanup(
        &mut cleanup,
        request.request_id,
        CleanupConfirmation::Released,
    )
    .await
    .expect_err("allocation record deletion failure must reject terminal cleanup");
    assert!(
        error
            .to_string()
            .contains("test remote allocation record deletion failed"),
        "{error}"
    );
    cleanup.rollback().await?;
    assert_eq!(load(&pool, request.request_id).await?, Some(before_cleanup));
    assert_eq!(
        allocation::load(&pool, allocation_id).await?,
        Some(allocation)
    );
    Ok(())
}

#[crate::sqlx_test]
async fn cleanup_waits_for_result_and_removes_its_allocation_record(
    pool: PgPool,
) -> eyre::Result<()> {
    let request = new_request(&pool, "late-result", ResourcePoolValueDomain::Integer).await?;
    begin(&pool, &request, &controller_authority()).await?;
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
    let allocation_id = allocation_id_for_request(provider_result.as_mut(), request.request_id)
        .await?
        .expect("uncommitted allocation record");
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
        // Cleanup must wait for this result, then take a new statement snapshot
        // that includes the allocation record before deleting it.
        wait_for_blocked_query(&pool, result_pid, "resource_pool_operations").await;
        provider_result.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (applied, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(confirm, finish_result)
    })
    .await??;
    assert_eq!(applied, Applied(()));
    assert_eq!(allocation::load(&pool, allocation_id).await?, None);
    assert_eq!(
        allocation_id_for_request(&pool, request.request_id).await?,
        None
    );
    let released = load(&pool, request.request_id)
        .await?
        .expect("retained terminal operation");
    assert_eq!(released.state, RemoteOperationState::Released);
    assert_eq!(released.request, request);
    assert_eq!(released.result, Some(result));
    Ok(())
}

#[crate::sqlx_test]
async fn shared_reads_reject_allocations_for_terminal_operations(pool: PgPool) -> eyre::Result<()> {
    let request = new_request(&pool, "corrupt-terminal", ResourcePoolValueDomain::Integer).await?;
    begin(&pool, &request, &controller_authority()).await?;
    save_assignment(&pool, request.request_id, &assignment()).await?;
    let allocation_id = allocation_id_for_request(&pool, request.request_id)
        .await?
        .expect("saved allocation record");
    // Bypass cleanup to leave an allocation record that contradicts its operation.
    sqlx::query("UPDATE resource_pool_operations SET state = 'released' WHERE request_id = $1")
        .bind(request.request_id)
        .execute(&pool)
        .await?;
    for (operation, error) in [
        (
            "load",
            allocation::load(&pool, allocation_id)
                .await
                .expect_err("terminal operation has an active allocation record"),
        ),
        (
            "list",
            allocation::list(
                &pool,
                &request.pool.logical_pool,
                None,
                NonZeroU16::new(1).unwrap(),
            )
            .await
            .expect_err("terminal operation has an active allocation record"),
        ),
    ] {
        let crate::DatabaseError::Sqlx(error) = error else {
            panic!("{operation}: expected an allocation decode error: {error}");
        };
        let sqlx::Error::Decode(source) = error.source else {
            panic!("{operation}: expected an allocation decode error: {error:?}");
        };
        let message = source.to_string();
        assert!(
            message.contains(&allocation_id.to_string()),
            "{operation}: {message}"
        );
        assert!(
            message.contains(&request.request_id.to_string()),
            "{operation}: {message}"
        );
    }
    Ok(())
}
