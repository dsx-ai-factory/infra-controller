// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Cancel unfinished allocations before their consumer forgets its saved IDs.

use std::time::Duration;

use db::resource_pool::remote as storage;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::AllocationState;
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::remote::RemoteResourcePoolError;
use crate::{ResourcePoolBackendError, ResourcePoolRegistry, database_transaction};

// Bound the caller's wait while background recovery continues cleanup. The caller
// keeps its saved creation IDs until it can confirm that cleanup finished.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(5);

/// `AllocationCleanup` identifies one attempt selected for cleanup, or its absence.
/// Commit the cancellation before calling [`Self::wait`], then recheck
/// [`Self::is_complete`] under the consumer's lock before removing creation IDs.
/// A newer attempt may have started while cleanup ran.
pub struct AllocationCleanup {
    owner_type: OwnerType,
    owner_id: String,
    slot: String,
    request_id: Option<Uuid>,
}

impl AllocationCleanup {
    /// `wait` finishes cleanup of the captured attempt through its original backend.
    /// Commit the transaction from [`ResourcePoolRegistry::request_cancellation`]
    /// first. This method never selects a newer request for the same owner.
    /// A handle with no captured request returns immediately, as does a request
    /// already `Cancelled` or `Released`. A missing, `Pending`, or `Attached`
    /// captured request returns `NotCurrent`.
    ///
    /// The wait is bounded by thirty seconds, including database and provider
    /// work. Expiry returns `Incomplete`, and cancellation returns `Cancelled`.
    /// `Busy` is retried; other reconciliation errors return immediately,
    /// including `Incomplete` when release remains unconfirmed. Saved cleanup
    /// intent remains available for retry, and the same handle can wait again.
    /// A missing original backend returns a failed precondition without changing
    /// the request.
    pub async fn wait(
        &self,
        registry: &ResourcePoolRegistry,
        database: &PgPool,
        cancel: &CancellationToken,
    ) -> Result<(), ResourcePoolBackendError> {
        let Some(request_id) = self.request_id else {
            return Ok(());
        };

        cancel
            .run_until_cancelled(tokio::time::timeout(CLEANUP_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                let saved = storage::load(&mut txn, request_id)
                    .await?
                    .ok_or(RemoteResourcePoolError::NotCurrent)?;
                txn.commit().await?;
                match saved.state {
                    AllocationState::Cancelled | AllocationState::Released => return Ok(()),
                    AllocationState::CancelRequested | AllocationState::ReleaseRequested => {}
                    _ => return Err(RemoteResourcePoolError::NotCurrent.into()),
                }
                let remote = registry
                    .get(&saved.pool_name)
                    .and_then(|selected| selected.backend.remote())
                    .ok_or_else(|| {
                        DatabaseError::FailedPrecondition(format!(
                            "resource pool `{}` needs its original backend to finish cleanup",
                            saved.pool_name,
                        ))
                    })?;
                loop {
                    // Box the large coordinator future so the enclosing
                    // timeout and caller futures stay small.
                    match Box::pin(remote.reconcile(request_id, cancel)).await {
                        Ok(()) | Err(RemoteResourcePoolError::Busy) => {}
                        Err(error) => return Err(error.into()),
                    }
                    let mut txn = database_transaction(database).await?;
                    let current = storage::load(&mut txn, request_id)
                        .await?
                        .ok_or(RemoteResourcePoolError::NotCurrent)?;
                    txn.commit().await?;
                    match current.state {
                        AllocationState::Cancelled | AllocationState::Released => return Ok(()),
                        AllocationState::CancelRequested | AllocationState::ReleaseRequested => {}
                        _ => return Err(RemoteResourcePoolError::NotCurrent.into()),
                    }
                    tokio::time::sleep(CLEANUP_INTERVAL).await;
                }
            }))
            .await
            .ok_or(RemoteResourcePoolError::Cancelled)?
            .map_err(|_| RemoteResourcePoolError::Incomplete)?
    }

    /// `is_complete` confirms that the same attempt is still terminal, or that
    /// an owner with no saved attempt still has none. It performs no provider I/O.
    /// Hold the consumer's creation lock and also recheck its current identity
    /// and desire before removing creation IDs.
    pub async fn is_complete(&self, txn: &mut PgConnection) -> Result<bool, DatabaseError> {
        let latest =
            storage::latest_for_owner_slot(txn, self.owner_type, &self.owner_id, &self.slot)
                .await?;
        Ok(match (self.request_id, latest) {
            (None, None) => true,
            (Some(request_id), Some(latest)) => {
                latest.allocation_id == request_id
                    && matches!(
                        latest.state,
                        AllocationState::Cancelled | AllocationState::Released
                    )
            }
            _ => false,
        })
    }
}

impl ResourcePoolRegistry {
    /// `request_cancellation` captures an owner's latest attempt and requests
    /// cancellation of a `Pending` request in the caller's transaction.
    /// Cleanup and terminal requests are captured unchanged; an `Attached` request
    /// returns `NotCurrent`. If there is no saved request, the handle records
    /// that absence.
    ///
    /// Preparation and cancellation for the same owner and slot must use the same
    /// consumer creation lock. Participating attachment and removal paths acquire
    /// that lock before allocation row locks, which remain held until commit or
    /// rollback. Replacement outside this lock must use a new owner ID.
    ///
    /// This method performs no provider I/O and never commits. It sets no database
    /// deadline of its own; the caller must bound its transaction. Rollback
    /// preserves the previous request state. Commit before [`AllocationCleanup::wait`].
    /// The handle identifies the observed attempt, not whether the conditional
    /// write applied. If another writer changed its state, `wait` checks whether
    /// cleanup can proceed. Attached allocations need ownership lookup and release.
    /// Integrated needs only rollback of its entity transaction when no remote
    /// request remains.
    ///
    /// After waiting, recheck current identity, desire, and
    /// [`AllocationCleanup::is_complete`] under the creation lock before forgetting
    /// creation IDs. A new attempt may have started after the capture committed.
    pub async fn request_cancellation(
        &self,
        txn: &mut PgTransaction<'_>,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
    ) -> Result<AllocationCleanup, ResourcePoolBackendError> {
        let saved = storage::latest_for_owner_slot(&mut **txn, owner_type, owner_id, slot).await?;
        let request_id = match saved {
            None => None,
            Some(saved) => {
                match saved.state {
                    AllocationState::Attached => {
                        return Err(RemoteResourcePoolError::NotCurrent.into());
                    }
                    AllocationState::Pending => {
                        // Capture the observed identity even if another writer changed
                        // its state. `wait` checks that state before provider cleanup.
                        match storage::request_cancel(txn, saved.allocation_id).await? {
                            ConditionalWrite::Applied(()) | ConditionalWrite::NotApplied(_) => {}
                        }
                    }
                    AllocationState::CancelRequested
                    | AllocationState::ReleaseRequested
                    | AllocationState::Cancelled
                    | AllocationState::Released => {}
                }
                Some(saved.allocation_id)
            }
        };
        Ok(AllocationCleanup {
            owner_type,
            owner_id: owner_id.to_owned(),
            slot: slot.to_owned(),
            request_id,
        })
    }
}
