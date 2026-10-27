// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Execute saved remote allocations and finish their cleanup after a restart.
//!
//! The foreground saves intent before provider I/O and returns a validated
//! assignment. Its consumer still owns admission and atomic entity attachment.
//! Recovery expires foreground attachment deadlines, saves recovered results,
//! and finishes cancellation or release already recorded. It never creates an
//! entity or dispatches a new allocation. Consumers use
//! [`OwnedAllocation::release`](crate::OwnedAllocation::release) to save release
//! intent in their entity transaction. Recovery performs provider cleanup after
//! that transaction commits.
//! API startup owns the recovery task and cleanup sampler; both stop with its
//! cancellation token.

mod metrics;
#[cfg(test)]
mod tests;
pub(crate) mod wire;

use std::future::Future;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::Duration;

use carbide_resource_pool_api::v1;
use db::resource_pool::{binding, remote as storage};
use db::work_lock_manager::{AcquireLockError, WorkLock, WorkLockManagerHandle};
use db::{ConditionalWrite, DatabaseError, Transaction};
pub use metrics::start_cleanup_metrics;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationFailureCode, AllocationResult, AllocationSelection,
    CleanupConfirmation, CompletionAuthority, RemoteAllocationParameters, RemoteAllocationRequest,
};
use model::resource_pool::{ResourcePoolBackendKind, ResourcePoolBinding, ResourcePoolValueDomain};
use sqlx::PgPool;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use v1::operation_status::State;

use crate::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError};
use crate::transaction::{DATABASE_WAIT_TIMEOUT, database_transaction};
use crate::value::{ResourcePoolEligibility, ValueConversionError};

// Match the client's 32 admitted calls without adding a waiting queue. One
// coordinator permit covers database work, provider calls, and retry backoff.
const MAX_OPERATIONS: usize = 32;
const FOREGROUND_ROUNDS: usize = 3;
const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(30);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(30);
const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const PAGE_SIZE: NonZeroU16 = NonZeroU16::new(16).unwrap();

/// `RecoveryCursor` keeps the last ID read and where this pass must finish.
/// Requests added above `through` wait for the next pass, so earlier requests
/// are revisited even when new requests keep arriving.
#[derive(Clone, Copy, Debug)]
struct RecoveryCursor {
    after: Uuid,
    through: Uuid,
}

/// `RemoteResourcePool` executes requests for one known provider authority.
/// Clones share admission capacity and the provider's connection pool.
#[derive(Clone)]
pub struct RemoteResourcePool {
    pool: PgPool,
    work_locks: WorkLockManagerHandle,
    authority_id: String,
    provider: GrpcResourcePoolBackend,
    operations: Arc<Semaphore>,
}

/// `RemoteResourcePoolError` preserves storage and provider failures without
/// treating an uncertain call as exhaustion or confirmed cleanup.
#[derive(Debug, thiserror::Error)]
pub enum RemoteResourcePoolError {
    /// A database operation or transaction failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// Intent conflicts with an existing request, generation, slot, or binding.
    #[error(transparent)]
    Storage(Box<storage::RemoteAllocationError>),
    /// A provider call failed; its source retains the protocol status.
    #[error(transparent)]
    Provider(#[from] GrpcResourcePoolError),
    /// The operation lease could not be acquired.
    #[error(transparent)]
    Acquire(#[from] AcquireLockError),
    /// A value or eligibility declaration is invalid.
    #[error(transparent)]
    Value(#[from] ValueConversionError),
    /// The supplied or saved record cannot be executed by this coordinator.
    #[error("invalid remote allocation record: {0}")]
    InvalidRecord(&'static str),
    /// A provider response differs from the complete saved request.
    #[error("invalid remote allocation response: {0}")]
    InvalidResponse(&'static str),
    /// All coordinator slots are occupied; no work was queued.
    #[error("remote resource pool is busy")]
    Busy,
    /// The caller or recovery task stopped waiting. Durable cleanup may remain.
    #[error("remote resource pool operation cancelled")]
    Cancelled,
    /// Attachment or cleanup no longer permits the requested transition.
    #[error("remote allocation is no longer current")]
    NotCurrent,
    /// A bounded wait expired, or allocation or cleanup remains unconfirmed.
    #[error("remote allocation has not completed")]
    Incomplete,
    /// The provider saved a definite rejection for this immutable request.
    #[error("remote allocation rejected: {0:?}")]
    Rejected(AllocationFailureCode),
}

impl From<storage::RemoteAllocationError> for RemoteResourcePoolError {
    fn from(error: storage::RemoteAllocationError) -> Self {
        match error {
            storage::RemoteAllocationError::Database(error) => Self::Database(error),
            error => Self::Storage(Box::new(error)),
        }
    }
}

impl RemoteResourcePool {
    /// `new` shares at most 32 operations across clones, with immediate `Busy`
    /// on excess admission. `authority_id` is the provider's already resolved
    /// stable identity; selecting or discovering a provider belongs to the caller.
    pub fn new(
        pool: PgPool,
        work_locks: WorkLockManagerHandle,
        authority_id: String,
        provider: GrpcResourcePoolBackend,
    ) -> Self {
        Self {
            pool,
            work_locks,
            authority_id,
            provider,
            operations: Arc::new(Semaphore::new(MAX_OPERATIONS)),
        }
    }

    /// `authority_id` identifies the provider used by this coordinator.
    /// Registry resolution checks it against the pool's saved binding.
    pub fn authority_id(&self) -> &str {
        &self.authority_id
    }

    /// `provider` borrows the shared client used to register this provider's pools.
    /// Registration and allocation therefore use the same connection and credentials.
    pub fn provider(&self) -> &GrpcResourcePoolBackend {
        &self.provider
    }

    /// `allocate` saves automatic or manual intent, then recovers or executes
    /// that exact request. It attempts at most three rounds with 100/200 ms
    /// backoff. Each round recovers and may allocate once. A dispatch returning
    /// InvalidArgument, FailedPrecondition, or ResourceExhausted gets one extra
    /// recovery to observe a saved rejection. Authentication and authorization
    /// failures are not retried. Each provider call has the client's ten-second
    /// deadline, within a shared 30-second budget starting at method entry.
    /// Binding lookup, lease acquisition, and intent persistence also share that
    /// budget and each wait at most five seconds. Cleanup afterward gets a
    /// separate five-second attempt, for at most 35 seconds of local waiting.
    /// Caller cancellation can stop execution earlier.
    ///
    /// After confirmed intent persistence, execution errors request cancellation
    /// while the lease and database remain available. An interrupted intent
    /// commit may still persist; its foreground deadline permits later cleanup,
    /// while a controller must replay or cancel its own outstanding request.
    /// Dropping the future likewise leaves durable work for recovery. Controller
    /// authority permits later attachment; it does not extend this call's retry
    /// budget. After cancellation, the controller waits for cleanup before
    /// starting a new generation.
    ///
    /// `eligibility` must describe the request's accepted revision. A returned
    /// assignment is not attachment permission: the consumer must recheck its
    /// admission before calling [`Allocation::allocate`](crate::Allocation::allocate)
    /// in the entity transaction. That operation checks completion authority
    /// through `db::resource_pool::remote::attach`. Attached and terminal replays fail.
    pub async fn allocate(
        &self,
        request: &RemoteAllocationRequest,
        authority: &CompletionAuthority,
        eligibility: &ResourcePoolEligibility,
        cancel: &CancellationToken,
    ) -> Result<AllocationAssignment, RemoteResourcePoolError> {
        if matches!(request.selection, AllocationSelection::Exact(_)) {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "exact selection requires allocate_exact",
            ));
        }
        self.execute(request, authority, eligibility, cancel).await
    }

    /// `allocate_exact` has the same persistence and attachment contract as
    /// `allocate`, but requires `Exact` selection and preserves either accepted
    /// assignment mode without changing the saved request.
    pub async fn allocate_exact(
        &self,
        request: &RemoteAllocationRequest,
        authority: &CompletionAuthority,
        eligibility: &ResourcePoolEligibility,
        cancel: &CancellationToken,
    ) -> Result<AllocationAssignment, RemoteResourcePoolError> {
        if !matches!(request.selection, AllocationSelection::Exact(_)) {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "allocate_exact requires exact selection",
            ));
        }
        self.execute(request, authority, eligibility, cancel).await
    }

    async fn execute(
        &self,
        request: &RemoteAllocationRequest,
        authority: &CompletionAuthority,
        eligibility: &ResourcePoolEligibility,
        cancel: &CancellationToken,
    ) -> Result<AllocationAssignment, RemoteResourcePoolError> {
        let deadline = Instant::now() + FOREGROUND_TIMEOUT;
        let _permit = self
            .operations
            .try_acquire()
            .map_err(|_| RemoteResourcePoolError::Busy)?;
        check_cancelled(cancel)?;
        let binding = wait(
            cancel,
            remaining(deadline)?.min(DATABASE_WAIT_TIMEOUT),
            self.load_binding(&request.pool.logical_pool, request.binding_id),
        )
        .await?;
        if request.pool.authority_id != self.authority_id
            || binding.value_domain != eligibility.domain
        {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "request authority or eligibility domain differs from the binding",
            ));
        }
        if let AllocationSelection::Requested(value) | AllocationSelection::Exact(value) =
            &request.selection
            && value_domain(value) != eligibility.domain
        {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "selection differs from the bound value domain",
            ));
        }

        let work_lock = self
            .acquire(
                request.request_id,
                cancel,
                remaining(deadline)?.min(DATABASE_WAIT_TIMEOUT),
            )
            .await?;
        // Preparation may already have saved this request. Under the lease,
        // insert or verify its intent before any provider call. Cancellation is
        // safe only after begin confirms that the saved inputs match this caller.
        wait(
            cancel,
            remaining(deadline)?.min(DATABASE_WAIT_TIMEOUT),
            self.save_intent(&work_lock, request, authority),
        )
        .await?;
        let result = wait(
            cancel,
            deadline.saturating_duration_since(Instant::now()),
            self.execute_locked(
                request.request_id,
                eligibility,
                &work_lock,
                cancel,
                deadline,
            ),
        )
        .await;
        if let Err(error) = &result
            && !matches!(
                error,
                RemoteResourcePoolError::NotCurrent | RemoteResourcePoolError::Rejected(_)
            )
        {
            self.abandon(&work_lock, request.request_id).await;
        }
        result
    }

    async fn execute_locked(
        &self,
        request_id: Uuid,
        eligibility: &ResourcePoolEligibility,
        work_lock: &WorkLock,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<AllocationAssignment, RemoteResourcePoolError> {
        for round in 0..FOREGROUND_ROUNDS {
            check_cancelled(cancel)?;
            remaining(deadline)?;
            let record = self.expire_and_load(work_lock, request_id).await?;
            if record.state != AllocationState::Pending {
                return Err(RemoteResourcePoolError::NotCurrent);
            }
            match &record.source {
                AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(assignment)),
                    ..
                } => {
                    validate_saved(&record, assignment, eligibility)?;
                    remaining(deadline)?;
                    return Ok(assignment.clone());
                }
                AllocationSource::Remote {
                    result: Some(AllocationResult::Failed(failure)),
                    ..
                } => {
                    self.request_cancel(work_lock, request_id).await?;
                    return Err(RemoteResourcePoolError::Rejected(failure.code));
                }
                _ => {}
            }
            match self
                .foreground_round(&record, eligibility, work_lock, cancel, deadline)
                .await
            {
                Ok(Some(assignment)) => return Ok(assignment),
                Ok(None) => {}
                Err(RemoteResourcePoolError::Provider(error)) if retryable(&error) => {
                    if round + 1 == FOREGROUND_ROUNDS {
                        return Err(RemoteResourcePoolError::Provider(error));
                    }
                }
                Err(error) => return Err(error),
            }
            if round + 1 < FOREGROUND_ROUNDS {
                tokio::time::sleep(Duration::from_millis(100 << round)).await;
            }
        }
        Err(RemoteResourcePoolError::Incomplete)
    }

    async fn foreground_round(
        &self,
        record: &ResourcePoolAllocation,
        eligibility: &ResourcePoolEligibility,
        work_lock: &WorkLock,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<Option<AllocationAssignment>, RemoteResourcePoolError> {
        remaining(deadline)?;
        let status = match self.recover(record).await? {
            State::Unknown(_) => {
                remaining(deadline)?;
                match self.dispatch(record, cancel).await {
                    Ok(allocation) => State::Allocated(allocation),
                    Err(error)
                        if matches!(
                            error.provider_code(),
                            Some(
                                tonic::Code::InvalidArgument
                                    | tonic::Code::FailedPrecondition
                                    | tonic::Code::ResourceExhausted
                            )
                        ) =>
                    {
                        remaining(deadline)?;
                        match self.recover(record).await? {
                            State::Unknown(_) | State::Pending(_) => return Err(error),
                            status => status,
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            status => status,
        };
        let allocation = match status {
            State::Allocated(allocation) => allocation,
            State::Unknown(_) | State::Pending(_) => return Ok(None),
            State::Failed(failure) => {
                let failure = wire::failure(failure)?;
                let code = failure.code;
                self.save_result(
                    work_lock,
                    record.allocation_id,
                    &AllocationResult::Failed(failure),
                )
                .await?;
                self.request_cancel(work_lock, record.allocation_id).await?;
                return Err(RemoteResourcePoolError::Rejected(code));
            }
            State::Cancelled(_) | State::Released(_) => {
                self.request_cancel(work_lock, record.allocation_id).await?;
                self.confirm_status(work_lock, record.allocation_id, status)
                    .await?;
                return Err(RemoteResourcePoolError::NotCurrent);
            }
        };
        eligibility.validate_allocation(&allocation)?;
        let assignment = wire::assignment(record, allocation)?;
        self.save_result(
            work_lock,
            record.allocation_id,
            &AllocationResult::Allocated(assignment.clone()),
        )
        .await?;
        if self
            .expire_and_load(work_lock, record.allocation_id)
            .await?
            .state
            != AllocationState::Pending
        {
            return Err(RemoteResourcePoolError::NotCurrent);
        }
        remaining(deadline)?;
        Ok(Some(assignment))
    }

    async fn dispatch(
        &self,
        record: &ResourcePoolAllocation,
        cancel: &CancellationToken,
    ) -> Result<v1::Allocation, RemoteResourcePoolError> {
        // A ready recovery reply can reach this point before the enclosing wait
        // polls cancellation again. Check before starting provider allocation.
        check_cancelled(cancel)?;
        let operation = Some(wire::operation(record)?);
        let selection = &parameters(record)?.selection;
        let allocation = match selection {
            AllocationSelection::Automatic | AllocationSelection::Requested(_) => {
                let requested_value = match selection {
                    AllocationSelection::Requested(value) => Some(wire::value(value)?),
                    _ => None,
                };
                call(self.provider.allocate(v1::AllocateRequest {
                    operation,
                    requested_value,
                }))
                .await?
                .allocation
            }
            AllocationSelection::Exact(value) => {
                call(self.provider.allocate_exact(v1::AllocateExactRequest {
                    operation,
                    value: Some(wire::value(value)?),
                }))
                .await?
                .allocation
            }
        };
        allocation.ok_or(RemoteResourcePoolError::InvalidResponse(
            "allocation is required",
        ))
    }

    /// `reconcile` performs one step for the saved request, with at most two
    /// provider calls and no allocation dispatch or entity creation. It expires
    /// foreground attachment using database time and validates recovered request
    /// identity and value domain. Before attachment, the consumer must validate
    /// the value against its accepted eligibility revision and recheck entity admission.
    ///
    /// Other authorities, held leases, and terminal or attached records are
    /// skipped. Pending records with a saved assignment need only expiry checks;
    /// a saved failure requests cancellation without contacting the provider.
    /// Cancellation first calls `Cancel`; a `Pending` reply is followed by
    /// `Recover`. Unknown requests, missing handles, and false release replies
    /// never count as cleanup confirmation. An interrupted step leaves saved
    /// work for a later pass.
    /// The initial read and lease acquisition each wait at most five seconds.
    /// After acquisition the step waits at most 30 seconds. Every wait observes
    /// cancellation. An interrupted acquisition may leave a lease without a
    /// guard until the manager's configured expiry; no provider work starts
    /// until acquisition returns its guard.
    pub async fn reconcile(
        &self,
        request_id: Uuid,
        cancel: &CancellationToken,
    ) -> Result<(), RemoteResourcePoolError> {
        let _permit = self
            .operations
            .try_acquire()
            .map_err(|_| RemoteResourcePoolError::Busy)?;
        check_cancelled(cancel)?;
        let record = wait(cancel, DATABASE_WAIT_TIMEOUT, self.load(request_id)).await?;
        if parameters(&record)?.authority_id != self.authority_id {
            return Ok(());
        }
        let work_lock = match self
            .acquire(request_id, cancel, DATABASE_WAIT_TIMEOUT)
            .await
        {
            Ok(work_lock) => work_lock,
            Err(RemoteResourcePoolError::Acquire(AcquireLockError::WorkAlreadyLocked(_))) => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        wait(
            cancel,
            RECONCILE_TIMEOUT,
            self.reconcile_locked(request_id, &work_lock),
        )
        .await
    }

    async fn acquire(
        &self,
        request_id: Uuid,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<WorkLock, RemoteResourcePoolError> {
        // The manager may finish acquisition after this caller stops waiting.
        // Without a returned guard that lease receives no keepalives and expires.
        // This can delay the same request's retry, but cannot dispatch provider
        // work: we only proceed after receiving the guard.
        wait(
            cancel,
            timeout,
            self.work_locks.try_acquire_lock(work_key(request_id)),
        )
        .await
    }

    async fn reconcile_locked(
        &self,
        request_id: Uuid,
        work_lock: &WorkLock,
    ) -> Result<(), RemoteResourcePoolError> {
        let record = self.expire_and_load(work_lock, request_id).await?;
        let binding_id = match &record.source {
            AllocationSource::Remote { binding_id, .. } => *binding_id,
            _ => {
                return Err(RemoteResourcePoolError::InvalidRecord(
                    "allocation is not remote",
                ));
            }
        };
        let binding = self.load_binding(&record.pool_name, binding_id).await?;
        match record.state {
            AllocationState::Pending => {
                self.recover_pending(&record, binding.value_domain, work_lock)
                    .await
            }
            AllocationState::CancelRequested => self.cancel_pending(&record, work_lock).await,
            AllocationState::ReleaseRequested => self.release_pending(&record, work_lock).await,
            AllocationState::Attached | AllocationState::Cancelled | AllocationState::Released => {
                Ok(())
            }
        }
    }

    async fn recover_pending(
        &self,
        record: &ResourcePoolAllocation,
        domain: ResourcePoolValueDomain,
        work_lock: &WorkLock,
    ) -> Result<(), RemoteResourcePoolError> {
        match &record.source {
            AllocationSource::Remote {
                result: Some(AllocationResult::Allocated(_)),
                ..
            } => return Ok(()),
            AllocationSource::Remote {
                result: Some(AllocationResult::Failed(_)),
                ..
            } => return self.request_cancel(work_lock, record.allocation_id).await,
            _ => {}
        }
        let status = match self.recover(record).await {
            Err(RemoteResourcePoolError::Provider(GrpcResourcePoolError::InvalidResponse(
                reason,
            ))) => {
                self.request_cancel(work_lock, record.allocation_id).await?;
                return Err(RemoteResourcePoolError::InvalidResponse(reason));
            }
            result => result?,
        };
        match status {
            State::Allocated(allocation) => {
                let assignment = match wire::assignment(record, allocation) {
                    Ok(assignment) if value_domain(&assignment.value) == domain => assignment,
                    Ok(_) => {
                        self.request_cancel(work_lock, record.allocation_id).await?;
                        return Err(RemoteResourcePoolError::InvalidResponse(
                            "allocation differs from the bound value domain",
                        ));
                    }
                    Err(error) => {
                        self.request_cancel(work_lock, record.allocation_id).await?;
                        return Err(error);
                    }
                };
                self.save_result(
                    work_lock,
                    record.allocation_id,
                    &AllocationResult::Allocated(assignment),
                )
                .await
            }
            State::Failed(failure) => {
                self.save_result(
                    work_lock,
                    record.allocation_id,
                    &AllocationResult::Failed(wire::failure(failure)?),
                )
                .await?;
                self.request_cancel(work_lock, record.allocation_id).await
            }
            State::Cancelled(_) | State::Released(_) => {
                self.request_cancel(work_lock, record.allocation_id).await?;
                self.confirm_status(work_lock, record.allocation_id, status)
                    .await
            }
            State::Unknown(_) | State::Pending(_) => Ok(()),
        }
    }

    async fn cancel_pending(
        &self,
        record: &ResourcePoolAllocation,
        work_lock: &WorkLock,
    ) -> Result<(), RemoteResourcePoolError> {
        let response = call(self.provider.cancel(v1::CancelRequest {
            operation_identity: Some(wire::operation_id(record)?),
        }))
        .await?;
        let status = match state(response.operation_status)? {
            // `Cancel` returning `Pending` means cleanup is still running.
            // Check `Recover` for completion before leaving it for the next pass.
            State::Pending(_) => self.recover(record).await?,
            status => status,
        };
        self.confirm_status(work_lock, record.allocation_id, status)
            .await
    }

    async fn release_pending(
        &self,
        record: &ResourcePoolAllocation,
        work_lock: &WorkLock,
    ) -> Result<(), RemoteResourcePoolError> {
        match self.recover(record).await? {
            status @ State::Released(_) => {
                return self
                    .confirm_status(work_lock, record.allocation_id, status)
                    .await;
            }
            State::Allocated(allocation) => {
                let assignment = wire::assignment(record, allocation)?;
                if !matches!(&record.source, AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(saved)), ..
                } if saved == &assignment)
                {
                    return Err(RemoteResourcePoolError::InvalidResponse(
                        "release recovery differs from the saved assignment",
                    ));
                }
            }
            _ => return Err(RemoteResourcePoolError::Incomplete),
        }
        if !call(self.provider.release(wire::release(record)?))
            .await?
            .applied
        {
            return Err(RemoteResourcePoolError::Incomplete);
        }
        self.confirm_cleanup(
            work_lock,
            record.allocation_id,
            CleanupConfirmation::Released,
        )
        .await
    }

    async fn recover(
        &self,
        record: &ResourcePoolAllocation,
    ) -> Result<State, RemoteResourcePoolError> {
        let response = call(self.provider.recover(v1::RecoverRequest {
            operation_identity: Some(wire::operation_id(record)?),
        }))
        .await?;
        state(response.operation_status)
    }

    async fn load(
        &self,
        request_id: Uuid,
    ) -> Result<ResourcePoolAllocation, RemoteResourcePoolError> {
        let mut txn = database_transaction(&self.pool).await?;
        let record = storage::load(&mut txn, request_id).await?;
        txn.commit().await?;
        record.ok_or(RemoteResourcePoolError::NotCurrent)
    }

    async fn load_binding(
        &self,
        pool_name: &str,
        binding_id: Uuid,
    ) -> Result<ResourcePoolBinding, RemoteResourcePoolError> {
        let mut txn = database_transaction(&self.pool).await?;
        let binding = binding::load(&mut txn, pool_name).await?;
        txn.commit().await?;
        let binding = binding.ok_or(RemoteResourcePoolError::InvalidRecord(
            "pool binding is missing",
        ))?;
        if binding.binding_id != binding_id
            || binding.backend_kind != ResourcePoolBackendKind::Grpc
            || binding.authority_id != self.authority_id
        {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "pool binding differs from the original authority",
            ));
        }
        Ok(binding)
    }

    /// `next_page` reads one bounded page and advances its cursor after success.
    /// A new pass captures its end before reading, then keeps that end across pages.
    async fn next_page(
        &self,
        state: AllocationState,
        cursor: &mut Option<RecoveryCursor>,
    ) -> Result<Vec<ResourcePoolAllocation>, DatabaseError> {
        let mut txn = database_transaction(&self.pool).await?;
        let (after, through) = match *cursor {
            Some(cursor) => (Some(cursor.after), Some(cursor.through)),
            None => (None, storage::last_request_id(&mut txn, state).await?),
        };
        let records = match through {
            Some(through) => {
                storage::list(&mut txn, state, after, Some(through), PAGE_SIZE).await?
            }
            None => Vec::new(),
        };
        txn.commit().await?;
        // Advance only after a successful read. An error retries the same page;
        // reaching the saved boundary starts a new pass on the next round.
        *cursor = match (records.last(), through) {
            (Some(last), Some(through))
                if records.len() == usize::from(PAGE_SIZE.get())
                    && last.allocation_id < through =>
            {
                Some(RecoveryCursor {
                    after: last.allocation_id,
                    through,
                })
            }
            _ => None,
        };
        Ok(records)
    }

    async fn transaction(
        &self,
        work_lock: &WorkLock,
    ) -> Result<Transaction<'_>, RemoteResourcePoolError> {
        let mut txn = database_transaction(&self.pool).await?;
        work_lock.fence_transaction(&mut txn).await?;
        Ok(txn)
    }

    async fn save_intent(
        &self,
        work_lock: &WorkLock,
        request: &RemoteAllocationRequest,
        authority: &CompletionAuthority,
    ) -> Result<(), RemoteResourcePoolError> {
        let mut txn = self.transaction(work_lock).await?;
        storage::begin(&mut txn, request, authority).await?;
        txn.commit().await?;
        Ok(())
    }

    async fn save_result(
        &self,
        work_lock: &WorkLock,
        request_id: Uuid,
        result: &AllocationResult,
    ) -> Result<(), RemoteResourcePoolError> {
        let mut txn = self.transaction(work_lock).await?;
        let write = match storage::record_result(&mut txn, request_id, result).await {
            Ok(write) => write,
            Err(error) => {
                // A reused provider handle cannot be attached. End the failed
                // transaction before recording cancellation on a fresh one.
                let cancel_required = error.violates_constraint("resource_pool_allocations_handle");
                txn.rollback_or_log("remote allocation result write").await;
                if cancel_required {
                    self.abandon(work_lock, request_id).await;
                }
                return Err(error.into());
            }
        };
        if matches!(write, ConditionalWrite::NotApplied(_)) {
            txn.rollback_or_log("remote allocation result no longer current")
                .await;
            self.abandon(work_lock, request_id).await;
            return Err(RemoteResourcePoolError::NotCurrent);
        }
        txn.commit().await?;
        Ok(())
    }

    async fn request_cancel(
        &self,
        work_lock: &WorkLock,
        request_id: Uuid,
    ) -> Result<(), RemoteResourcePoolError> {
        let mut txn = self.transaction(work_lock).await?;
        applied(storage::request_cancel(&mut txn, request_id).await?)?;
        txn.commit().await?;
        Ok(())
    }

    async fn expire_and_load(
        &self,
        work_lock: &WorkLock,
        request_id: Uuid,
    ) -> Result<ResourcePoolAllocation, RemoteResourcePoolError> {
        let mut txn = self.transaction(work_lock).await?;
        // Read the row after checking its deadline so callers see any
        // cancellation recorded here, or the unchanged state if it did not expire.
        let _ = storage::request_cancel_if_expired(&mut txn, request_id).await?;
        let record = storage::load(&mut txn, request_id).await?;
        txn.commit().await?;
        record.ok_or(RemoteResourcePoolError::NotCurrent)
    }

    async fn confirm_cleanup(
        &self,
        work_lock: &WorkLock,
        request_id: Uuid,
        confirmation: CleanupConfirmation,
    ) -> Result<(), RemoteResourcePoolError> {
        let mut txn = self.transaction(work_lock).await?;
        applied(storage::confirm_cleanup(&mut txn, request_id, confirmation).await?)?;
        txn.commit().await?;
        Ok(())
    }

    async fn confirm_status(
        &self,
        work_lock: &WorkLock,
        request_id: Uuid,
        status: State,
    ) -> Result<(), RemoteResourcePoolError> {
        match status {
            State::Cancelled(_) => {
                self.confirm_cleanup(work_lock, request_id, CleanupConfirmation::Cancelled)
                    .await
            }
            State::Released(_) => {
                self.confirm_cleanup(work_lock, request_id, CleanupConfirmation::Released)
                    .await
            }
            State::Pending(_) => Ok(()),
            _ => Err(RemoteResourcePoolError::InvalidResponse(
                "cleanup is not confirmed",
            )),
        }
    }

    async fn abandon(&self, work_lock: &WorkLock, request_id: Uuid) {
        // The caller may already be cancelled. Give its durable cleanup intent
        // one bounded write attempt while retaining the same work guard.
        match tokio::time::timeout(
            CANCELLATION_TIMEOUT,
            self.request_cancel(work_lock, request_id),
        )
        .await
        {
            Ok(Ok(()) | Err(RemoteResourcePoolError::NotCurrent)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    %request_id,
                    %error,
                    grpc_status_code = ?error.provider_code(),
                    "Could not record remote allocation cancellation"
                );
            }
            Err(error) => {
                tracing::warn!(%request_id, %error, "Timed out recording remote allocation cancellation");
            }
        }
    }

    /// `start` spawns recovery in the caller's task set until cancellation.
    /// Each round reads one page of 16 records for each active state. A pass ends
    /// at the last UUID present when it began, so new arrivals cannot prevent
    /// earlier requests from being retried. After each round it waits five
    /// seconds; provider calls remain bounded by their own deadlines and
    /// observe cancellation. Returns an error if the recovery task cannot be spawned.
    pub fn start(self, tasks: &mut JoinSet<()>, cancel: CancellationToken) -> std::io::Result<()> {
        tasks
            .build_task()
            .name("resource_pool_recovery")
            .spawn(async move { self.run(cancel).await })?;
        Ok(())
    }

    async fn run(&self, cancel: CancellationToken) {
        let states = [
            AllocationState::Pending,
            AllocationState::CancelRequested,
            AllocationState::ReleaseRequested,
        ];
        let mut cursors = [None; 3];
        while !cancel.is_cancelled() {
            for (state, cursor) in states.iter().zip(cursors.iter_mut()) {
                let Some(page) = cancel
                    .run_until_cancelled(tokio::time::timeout(
                        DATABASE_WAIT_TIMEOUT,
                        self.next_page(*state, cursor),
                    ))
                    .await
                else {
                    return;
                };
                let records = match page {
                    Ok(Ok(records)) => records,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            allocation_state = ?state,
                            cursor = ?cursor,
                            %error,
                            "Could not read remote allocation recovery page"
                        );
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            allocation_state = ?state,
                            cursor = ?cursor,
                            %error,
                            "Timed out reading remote allocation recovery page"
                        );
                        continue;
                    }
                };
                for record in records {
                    if cancel.is_cancelled() {
                        return;
                    }
                    if let Err(error) = self.reconcile(record.allocation_id, &cancel).await {
                        if matches!(error, RemoteResourcePoolError::Cancelled) {
                            return;
                        }
                        tracing::warn!(
                            request_id = %record.allocation_id,
                            allocation_state = ?record.state,
                            %error,
                            grpc_status_code = ?error.provider_code(),
                            "Remote allocation recovery remains incomplete"
                        );
                    }
                }
            }
            if cancel
                .run_until_cancelled(tokio::time::sleep(RECONCILE_INTERVAL))
                .await
                .is_none()
            {
                return;
            }
        }
    }
}

impl RemoteResourcePoolError {
    fn provider_code(&self) -> Option<tonic::Code> {
        match self {
            Self::Provider(GrpcResourcePoolError::Rpc(error)) => Some(error.code()),
            _ => None,
        }
    }
}

fn work_key(request_id: Uuid) -> String {
    format!("resource-pool:{request_id}")
}

fn parameters(
    record: &ResourcePoolAllocation,
) -> Result<&RemoteAllocationParameters, RemoteResourcePoolError> {
    match &record.source {
        AllocationSource::Remote { request, .. } => Ok(request),
        _ => Err(RemoteResourcePoolError::InvalidRecord(
            "allocation is not remote",
        )),
    }
}

fn state(status: Option<v1::OperationStatus>) -> Result<State, RemoteResourcePoolError> {
    status
        .and_then(|status| status.state)
        .ok_or(RemoteResourcePoolError::InvalidResponse(
            "operation status is required",
        ))
}

fn applied(
    result: ConditionalWrite<(), storage::OperationNotCurrent>,
) -> Result<(), RemoteResourcePoolError> {
    match result {
        ConditionalWrite::Applied(()) => Ok(()),
        ConditionalWrite::NotApplied(_) => Err(RemoteResourcePoolError::NotCurrent),
    }
}

fn check_cancelled(cancel: &CancellationToken) -> Result<(), RemoteResourcePoolError> {
    if cancel.is_cancelled() {
        Err(RemoteResourcePoolError::Cancelled)
    } else {
        Ok(())
    }
}

fn remaining(deadline: Instant) -> Result<Duration, RemoteResourcePoolError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(RemoteResourcePoolError::Incomplete)
}

fn call<T>(
    future: impl Future<Output = Result<T, GrpcResourcePoolError>>,
) -> impl Future<Output = Result<T, RemoteResourcePoolError>> {
    // Keep the large gRPC future out of the coordinator's nested stack frames.
    // Box it before creating the async block so even an unpolled call stays small.
    let future = Box::pin(future);
    async move { future.await.map_err(Into::into) }
}

// Stop local waiting, including blocked SQL. SQLx may still finish the query or
// commit before its queued rollback; only persisted state proves what changed.
async fn wait<T, E>(
    cancel: &CancellationToken,
    timeout: Duration,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, RemoteResourcePoolError>
where
    RemoteResourcePoolError: From<E>,
{
    match cancel
        .run_until_cancelled(tokio::time::timeout(timeout, future))
        .await
    {
        None => Err(RemoteResourcePoolError::Cancelled),
        Some(Err(_)) => Err(RemoteResourcePoolError::Incomplete),
        Some(Ok(result)) => result.map_err(Into::into),
    }
}

fn retryable(error: &GrpcResourcePoolError) -> bool {
    // Each retry recovers the saved operation before deciding whether allocation
    // can be resent. Tonic can report transport failures as `Internal`, `Unknown`,
    // or `Cancelled`; none proves the provider rejected the operation.
    match error {
        GrpcResourcePoolError::Connect(_)
        | GrpcResourcePoolError::Busy
        | GrpcResourcePoolError::DeadlineExceeded => true,
        GrpcResourcePoolError::Rpc(error) => matches!(
            error.code(),
            tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::Aborted
                | tonic::Code::ResourceExhausted
                | tonic::Code::Internal
                | tonic::Code::Unknown
                | tonic::Code::Cancelled
        ),
        _ => false,
    }
}

fn value_domain(value: &ResourcePoolValue) -> ResourcePoolValueDomain {
    match value {
        ResourcePoolValue::Integer(_) => ResourcePoolValueDomain::Integer,
        ResourcePoolValue::IpAddress(_) => ResourcePoolValueDomain::IpAddress,
        ResourcePoolValue::Ipv6Prefix(_) => ResourcePoolValueDomain::Ipv6Prefix,
    }
}

// Recovery can save an assignment without the accepted eligibility. Before
// returning it to the caller, check that its value and mode satisfy those rules.
fn validate_saved(
    record: &ResourcePoolAllocation,
    assignment: &AllocationAssignment,
    eligibility: &ResourcePoolEligibility,
) -> Result<(), RemoteResourcePoolError> {
    eligibility.validate_value(
        &wire::value(&assignment.value)?,
        match assignment.assignment_mode {
            AssignmentMode::Automatic => v1::AssignmentMode::Automatic,
            AssignmentMode::Manual => v1::AssignmentMode::Manual,
        } as i32,
    )?;
    wire::validate_selection(&parameters(record)?.selection, assignment)
}
