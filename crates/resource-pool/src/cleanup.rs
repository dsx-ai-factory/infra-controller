// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Cancel unfinished allocations before their consumer forgets its saved IDs.

use std::time::Duration;

use db::DatabaseError;
use db::resource_pool::remote as storage;
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::AllocationState;
use sqlx::{PgConnection, PgPool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::remote::RemoteResourcePoolError;
use crate::{ResourcePoolBackendError, ResourcePoolRegistry, database_transaction};

// The caller must confirm cleanup before forgetting saved creation IDs. Bound
// this wait so a later startup can resume unfinished work with the same owner.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(5);
const SAVE_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

/// `AllocationCleanup` identifies the attempt whose cleanup finished.
/// Recheck it under the consumer's lock before removing saved creation IDs:
/// another caller may have started a new generation while cleanup ran.
pub struct AllocationCleanup {
    owner_type: OwnerType,
    owner_id: String,
    slot: String,
    request_id: Option<Uuid>,
}

impl AllocationCleanup {
    /// `is_complete` confirms that the same attempt is still terminal, or that
    /// an owner with no saved attempt still has none. It performs no provider I/O.
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
    /// `cancel_pending_allocation` withdraws an owner's unfinished request and
    /// waits for confirmed cleanup through its original backend. Attached
    /// allocations must use ownership lookup and release instead.
    ///
    /// Saving cancellation takes at most five seconds, followed by at most
    /// thirty seconds of cleanup. Cancellation stops both waits. No request
    /// means no external work needs cleanup; Integrated allocation rolls back
    /// with the entity transaction and never creates a pending request.
    /// The returned check must pass under the consumer's lock before it forgets
    /// the IDs used for this attempt.
    pub async fn cancel_pending_allocation(
        &self,
        database: &PgPool,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
        cancel: &CancellationToken,
    ) -> Result<AllocationCleanup, ResourcePoolBackendError> {
        let saved = cancel
            .run_until_cancelled(tokio::time::timeout(SAVE_CANCELLATION_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                let saved =
                    storage::latest_for_owner_slot(&mut txn, owner_type, owner_id, slot).await?;
                if let Some(saved) = &saved {
                    // An ambiguous entity commit may already have attached it.
                    // Cancellation never undoes a successful entity allocation.
                    match storage::request_cancel(&mut txn, saved.allocation_id).await? {
                        db::ConditionalWrite::Applied(()) | db::ConditionalWrite::NotApplied(_) => {
                        }
                    }
                }
                txn.commit().await?;
                Ok::<_, DatabaseError>(saved)
            }))
            .await
            .ok_or(RemoteResourcePoolError::Cancelled)?
            .map_err(|_| RemoteResourcePoolError::Incomplete)??;

        if let Some(saved) = &saved
            && !matches!(
                saved.state,
                AllocationState::Cancelled | AllocationState::Released
            )
        {
            let remote = self
                .get(&saved.pool_name)
                .and_then(|selected| selected.backend.remote())
                .ok_or_else(|| {
                    DatabaseError::FailedPrecondition(format!(
                        "resource pool `{}` needs its original backend to finish cleanup",
                        saved.pool_name,
                    ))
                })?;
            cancel
                .run_until_cancelled(tokio::time::timeout(CLEANUP_TIMEOUT, async {
                    loop {
                        match remote.reconcile(saved.allocation_id, cancel).await {
                            Ok(()) | Err(RemoteResourcePoolError::Busy) => {}
                            Err(error) => return Err(error),
                        }
                        let mut txn = database_transaction(database).await?;
                        let current = storage::load(&mut txn, saved.allocation_id)
                            .await?
                            .ok_or(RemoteResourcePoolError::NotCurrent)?;
                        txn.commit().await?;
                        match current.state {
                            AllocationState::Cancelled | AllocationState::Released => return Ok(()),
                            AllocationState::CancelRequested
                            | AllocationState::ReleaseRequested => {}
                            _ => return Err(RemoteResourcePoolError::NotCurrent),
                        }
                        tokio::time::sleep(CLEANUP_INTERVAL).await;
                    }
                }))
                .await
                .ok_or(RemoteResourcePoolError::Cancelled)?
                .map_err(|_| RemoteResourcePoolError::Incomplete)??;
        }
        Ok(AllocationCleanup {
            owner_type,
            owner_id: owner_id.to_owned(),
            slot: slot.to_owned(),
            request_id: saved.map(|saved| saved.allocation_id),
        })
    }
}
