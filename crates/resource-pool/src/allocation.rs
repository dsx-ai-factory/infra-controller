// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Prepare pool requests, resolve provider work, and allocate with entity writes.

#[cfg(test)]
mod tests;

use std::str::FromStr;
use std::time::Duration;

use db::resource_pool::{binding, remote as storage};
use db::work_lock_manager::AcquireLockError;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::allocation::{
    AllocationSource, AllocationState as StoredAllocationState, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationOwner, AllocationSelection, CompletionAuthority,
    ProviderRouteTargetPolicy, RemoteAllocationRequest, RemotePoolIdentity,
};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolValueDomain,
};
use sqlx::{PgPool, PgTransaction};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::remote::{RemoteResourcePool, RemoteResourcePoolError, wire};
use crate::transaction::DATABASE_WAIT_TIMEOUT;
use crate::value::{ResourcePoolEligibility, ValueConversionError, value_from_wire, value_to_wire};
use crate::{
    IntegratedResourcePoolBackend, ResourcePoolBackend, ResourcePoolBackendError,
    ResourcePoolWithBackend, database_transaction, registration,
};

// Lease contention and provider allocation share one budget. Callers leave
// additional time in their completion authority to validate and save the entity.
const ALLOCATION_TIMEOUT: Duration = Duration::from_secs(30);
const LEASE_RETRY_INTERVAL: Duration = Duration::from_millis(100);
// Saving cancellation includes connection acquisition, its write, and commit.
const SAVE_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

/// `PreparedAllocationRequest` saves a controller's request alongside its creation ID.
/// Commit that transaction before [`Self::resolve`] consumes this request and
/// returns the allocation to attach. An interrupted controller can replay the
/// request while the same creation ID is still wanted.
pub struct PreparedAllocationRequest<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error,
{
    pool: &'a ResourcePool<T>,
    state: PreparedState<'a, T>,
}

/// `PreparedState` keeps SQL inputs or the saved request needed for provider work.
enum PreparedState<'a, T> {
    Integrated {
        backend: &'a IntegratedResourcePoolBackend,
        owner_type: OwnerType,
        owner_id: String,
        requested: Option<T>,
    },
    Remote {
        backend: &'a RemoteResourcePool,
        request: Box<RemoteAllocationRequest>,
        authority: CompletionAuthority,
        eligibility: ResourcePoolEligibility,
    },
}

/// `Allocation` is ready to allocate in the caller's entity transaction.
/// It owns the saved request identity and provider result, so the caller can
/// save the entity with [`Self::allocate`] or abandon it with [`Self::cancel`].
/// Integrated selects its value during `allocate`, keeping reservation and
/// entity writes in the same transaction.
pub struct Allocation<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error,
{
    pool: &'a ResourcePool<T>,
    state: AllocationState<'a, T>,
}

/// `AllocationState` owns deferred SQL inputs or a checked provider assignment.
enum AllocationState<'a, T> {
    Integrated {
        backend: &'a IntegratedResourcePoolBackend,
        owner_type: OwnerType,
        owner_id: String,
        requested: Option<T>,
    },
    Remote {
        request_id: Uuid,
        authority: CompletionAuthority,
        assignment: AllocationAssignment,
        value: T,
    },
}

impl<'a, T> ResourcePoolWithBackend<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    /// `prepare_allocation` gets an allocation ready for the entity transaction.
    /// Finish any caller transaction first: remote preparation commits its own
    /// request before waiting for the provider. Integrated performs no I/O here.
    /// `None` selects an automatic value; `Some(value)` requests a manual value.
    /// `slot` distinguishes allocations within the owner, such as `vni`.
    /// Finish cleanup of a superseded remote request before switching pools for
    /// that owner and slot. Integrated preparation does not read remote history.
    ///
    /// Remote database preparation and commit share five seconds, followed by
    /// a 30-second provider budget. Cancellation stops these waits. Attachment
    /// expires 60 seconds after preparation on the database clock, so a retry
    /// cannot attach a previous caller's allocation. On failure, we try to save
    /// cancellation before returning the original error; recovery finishes it.
    pub async fn prepare_allocation(
        &self,
        database: &PgPool,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
        requested: Option<T>,
        cancel: &CancellationToken,
    ) -> Result<Allocation<'a, T>, ResourcePoolBackendError> {
        if let ResourcePoolBackend::Integrated(backend) = self.backend {
            return Ok(Allocation {
                pool: self.pool,
                state: AllocationState::Integrated {
                    backend,
                    owner_type,
                    owner_id: owner_id.to_owned(),
                    requested,
                },
            });
        }

        let mut request_id = None;
        let result = async {
            let prepared = cancel
                .run_until_cancelled(tokio::time::timeout(DATABASE_WAIT_TIMEOUT, async {
                    let mut txn = database_transaction(database).await?;
                    let query = "SELECT clock_timestamp() + interval '60 seconds'";
                    let attach_before = sqlx::query_scalar(query)
                        .fetch_one(txn.as_pgconn())
                        .await
                        .map_err(|error| DatabaseError::query(query, error))?;
                    let prepared = self
                        .prepare_request(
                            &mut txn,
                            owner_type,
                            owner_id,
                            slot,
                            requested,
                            CompletionAuthority::Foreground {
                                attempt_id: Uuid::new_v4(),
                                attach_before,
                            },
                        )
                        .await?;
                    // Losing the commit response does not prove rollback. Keep
                    // the ID outside this future so timeout can request cleanup.
                    if let PreparedState::Remote { request, .. } = &prepared.state {
                        request_id = Some(request.request_id);
                    }
                    txn.commit().await?;
                    Ok::<_, ResourcePoolBackendError>(prepared)
                }))
                .await
                .ok_or(RemoteResourcePoolError::Cancelled)?
                .map_err(|_| RemoteResourcePoolError::Incomplete)??;
            prepared.resolve(database, cancel).await
        }
        .await;

        // Resolution already cancels values that cannot fit the pool type.
        if let Err(error) = &result
            && !matches!(error, ResourcePoolBackendError::Value(_))
            && let Some(request_id) = request_id
            && let Err(cleanup_error) = cancel_request(database, request_id).await
        {
            tracing::error!(%owner_id, %request_id, error = %cleanup_error,
                "could not save cancellation after allocation request failed");
        }
        result
    }

    /// `prepare_request` records a new allocation attempt in the caller's transaction.
    /// `None` selects automatic values; `Some(value)` requests the manual value.
    /// `slot` distinguishes remote allocations within the owner, such as `vni`
    /// or `loopback`. Integrated keeps its existing pool/owner identity and
    /// performs no database writes during preparation.
    ///
    /// A controller can replay its matching Pending request and original accepted
    /// definition. Changed inputs within the same controller desire return
    /// `RequestChanged`; unfinished cleanup returns `CleanupPending`. Another
    /// desire or foreground request cannot replace an occupied slot.
    /// Confirmed cancellation or release permits a new request and generation.
    /// Before switching pools, finish cleanup of the old remote request;
    /// Integrated preparation does not check that request's history.
    /// The caller commits this transaction before resolving the prepared request.
    pub async fn prepare_request(
        &self,
        txn: &mut PgTransaction<'_>,
        owner_type: OwnerType,
        owner_id: &str,
        slot: &str,
        requested: Option<T>,
        authority: CompletionAuthority,
    ) -> Result<PreparedAllocationRequest<'a, T>, ResourcePoolBackendError> {
        let remote = match self.backend {
            ResourcePoolBackend::Integrated(backend) => {
                return Ok(PreparedAllocationRequest {
                    pool: self.pool,
                    state: PreparedState::Integrated {
                        backend,
                        owner_type,
                        owner_id: owner_id.to_owned(),
                        requested,
                    },
                });
            }
            ResourcePoolBackend::Grpc(remote) => remote,
        };
        let binding = binding::load(&mut **txn, self.pool.name())
            .await?
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "resource pool binding",
                id: self.pool.name().to_owned(),
            })?;
        if binding.backend_kind != ResourcePoolBackendKind::Grpc
            || binding.authority_id != remote.authority_id()
            || binding.value_domain != ResourcePoolValueDomain::from(self.pool.value_type)
        {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "selected backend or value domain differs from the saved binding",
            )
            .into());
        }
        let selection = requested
            .as_ref()
            .map(|value| {
                model_value(binding.value_domain, value).map(AllocationSelection::Requested)
            })
            .transpose()?
            .unwrap_or(AllocationSelection::Automatic);
        let previous =
            storage::latest_for_owner_slot(&mut **txn, owner_type, owner_id, slot).await?;
        let (generation, replay) = if let Some(previous) = previous {
            let AllocationSource::Remote {
                binding_id,
                request,
                ..
            } = previous.source
            else {
                return Err(RemoteResourcePoolError::InvalidRecord(
                    "remote allocation history contains an integrated allocation",
                )
                .into());
            };
            match previous.state {
                StoredAllocationState::Cancelled | StoredAllocationState::Released => {
                    let generation = request.generation.checked_add(1).ok_or_else(|| {
                        DatabaseError::FailedPrecondition(
                            "resource pool allocation generation limit reached".to_owned(),
                        )
                    })?;
                    (generation, None)
                }
                StoredAllocationState::Pending
                    if matches!(authority, CompletionAuthority::Controller { .. })
                        && request.completion_authority == authority =>
                {
                    if binding_id != binding.binding_id
                        || previous.pool_name != binding.pool_name
                        || request.selection != selection
                    {
                        return Err(ResourcePoolBackendError::RequestChanged);
                    }
                    (request.generation, Some((previous.allocation_id, request)))
                }
                StoredAllocationState::CancelRequested
                | StoredAllocationState::ReleaseRequested => {
                    return Err(ResourcePoolBackendError::CleanupPending);
                }
                StoredAllocationState::Attached | StoredAllocationState::Pending => {
                    return Err(DatabaseError::FailedPrecondition(format!(
                        "{owner_type} `{owner_id}` already has an allocation or unfinished request for slot `{slot}`",
                    ))
                    .into());
                }
            }
        } else {
            (0, None)
        };

        let revision = replay
            .as_ref()
            .map(|(_, request)| {
                request.eligibility_revision.parse().map_err(|_| {
                    RemoteResourcePoolError::InvalidRecord(
                        "saved request has an invalid eligibility revision",
                    )
                })
            })
            .transpose()?;
        let definition = registration::accepted_in_transaction(txn, &binding, revision)
            .await?
            .ok_or_else(|| {
                DatabaseError::FailedPrecondition(format!(
                    "resource pool `{}` has no accepted definition for this allocation request",
                    binding.pool_name,
                ))
            })?;
        let pool = definition
            .pool
            .ok_or(RemoteResourcePoolError::InvalidRecord(
                "accepted pool definition has no pool identity",
            ))?;
        if let Some((_, saved)) = &replay
            && (saved.authority_id != pool.authority_id
                || saved.pool_id != pool.pool_id
                || saved.eligibility_revision != definition.eligibility_revision)
        {
            return Err(RemoteResourcePoolError::InvalidRecord(
                "saved request differs from its accepted pool identity",
            )
            .into());
        }
        let eligibility = ResourcePoolEligibility {
            domain: binding.value_domain,
            entries: definition.eligibility,
        };
        let request = RemoteAllocationRequest {
            request_id: replay.map_or_else(Uuid::new_v4, |(request_id, _)| request_id),
            binding_id: binding.binding_id,
            pool: RemotePoolIdentity {
                authority_id: pool.authority_id,
                pool_id: pool.pool_id,
                logical_pool: binding.pool_name,
            },
            owner: AllocationOwner {
                owner_type,
                owner_id: owner_id.to_owned(),
                slot: slot.to_owned(),
                generation,
            },
            eligibility_revision: definition.eligibility_revision,
            selection,
        };
        // Controller callers can save their creation record here too, so
        // removing it can find and cancel the associated provider request.
        storage::begin(txn, &request, &authority)
            .await
            .map_err(RemoteResourcePoolError::from)?;
        Ok(PreparedAllocationRequest {
            pool: self.pool,
            state: PreparedState::Remote {
                backend: remote,
                request: Box::new(request),
                authority,
                eligibility,
            },
        })
    }
}

impl<'a, T> PreparedAllocationRequest<'a, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    /// `resolve` completes provider work without holding the caller's transaction.
    /// Integrated resolution does no I/O. Remote allocation and lease retries
    /// share a 30-second budget; cancellation stops both. Only a held work lease
    /// is retried here, every 100ms, using the same saved request and authority.
    /// Recovery skips held leases; this caller waits because it needs the result.
    /// A remote value must fit `T`; conversion failure requests cancellation.
    /// Provider errors normally request cancellation; wait for that cleanup
    /// before preparing another attempt. An interrupted wait can leave a Pending
    /// request, which a controller can replay with the same desire and inputs.
    /// Consumer-specific value and policy checks remain with callers.
    pub async fn resolve(
        self,
        database: &PgPool,
        cancel: &CancellationToken,
    ) -> Result<Allocation<'a, T>, ResourcePoolBackendError> {
        let state = match self.state {
            PreparedState::Integrated {
                backend,
                owner_type,
                owner_id,
                requested,
            } => AllocationState::Integrated {
                backend,
                owner_type,
                owner_id,
                requested,
            },
            PreparedState::Remote {
                backend,
                request,
                authority,
                eligibility,
            } => {
                let assignment = cancel
                    .run_until_cancelled(tokio::time::timeout(ALLOCATION_TIMEOUT, async {
                        loop {
                            // Box the large coordinator future so the enclosing
                            // timeout and caller futures stay small.
                            match Box::pin(backend.allocate(
                                &request,
                                &authority,
                                &eligibility,
                                cancel,
                            ))
                            .await
                            {
                                Err(RemoteResourcePoolError::Acquire(
                                    AcquireLockError::WorkAlreadyLocked(_),
                                )) => tokio::time::sleep(LEASE_RETRY_INTERVAL).await,
                                result => return result,
                            }
                        }
                    }))
                    .await
                    .ok_or(RemoteResourcePoolError::Cancelled)?
                    .map_err(|_| RemoteResourcePoolError::Incomplete)??;
                let value = match wire::value(&assignment.value)
                    .and_then(|value| value_from_wire(eligibility.domain, &value))
                {
                    Ok(value) => value,
                    Err(error) => {
                        // Replaying this result cannot make it fit T. Preserve
                        // the conversion error even if saving cleanup fails.
                        if let Err(cleanup_error) =
                            cancel_request(database, request.request_id).await
                        {
                            tracing::error!(request_id = %request.request_id, error = %cleanup_error,
                                "could not cancel an allocation with an unusable value");
                        }
                        return Err(error.into());
                    }
                };
                AllocationState::Remote {
                    request_id: request.request_id,
                    authority,
                    assignment,
                    value,
                }
            }
        };
        Ok(Allocation {
            pool: self.pool,
            state,
        })
    }
}

impl<T> Allocation<'_, T>
where
    T: ToString + FromStr + Send + Sync + 'static,
    T::Err: std::error::Error,
{
    /// `value` exposes a remote value for consumer validation before attachment.
    /// Integrated returns `None` because it selects inventory during `allocate`.
    pub fn value(&self) -> Option<&T> {
        match &self.state {
            AllocationState::Integrated { .. } => None,
            AllocationState::Remote { value, .. } => Some(value),
        }
    }

    /// `route_target_policy` returns routing instructions from the provider.
    /// Consumers must apply supported instructions or reject them explicitly.
    pub fn route_target_policy(&self) -> Option<&ProviderRouteTargetPolicy> {
        match &self.state {
            AllocationState::Integrated { .. } => None,
            AllocationState::Remote { assignment, .. } => assignment.route_target_policy.as_ref(),
        }
    }

    /// `allocate` reserves or attaches the value in the caller's transaction.
    /// Integrated forwards to its SQL allocator. Remote attachment verifies the
    /// exact saved assignment and completion authority, including its deadline.
    /// Callers recheck entity admission and controller desire in this transaction,
    /// save the entity before committing, and roll back all writes on error.
    /// This method performs no provider I/O and never commits independently.
    /// Use it once per committed entity; a rolled-back transaction may retry it.
    pub async fn allocate(&self, txn: &mut PgTransaction<'_>) -> Result<T, ResourcePoolBackendError>
    where
        T: Clone,
    {
        match &self.state {
            AllocationState::Integrated {
                backend,
                owner_type,
                owner_id,
                requested,
            } => Ok(backend
                .allocate(self.pool, txn, *owner_type, owner_id, requested.clone())
                .await?),
            AllocationState::Remote {
                request_id,
                authority,
                assignment,
                value,
            } => match storage::attach(txn, *request_id, authority, assignment).await? {
                ConditionalWrite::Applied(()) => Ok(value.clone()),
                ConditionalWrite::NotApplied(_) => Err(RemoteResourcePoolError::NotCurrent.into()),
            },
        }
    }

    /// `cancel` abandons a request when the caller cannot save its entity.
    /// Integrated needs only the caller's rollback. Remote cancellation uses a
    /// separate five-second database transaction, so roll back entity writes
    /// first. An attached request stays intact if its commit actually succeeded.
    /// Report cleanup errors separately to preserve the original creation error.
    pub async fn cancel(&self, database: &PgPool) -> Result<(), ResourcePoolBackendError> {
        let AllocationState::Remote { request_id, .. } = &self.state else {
            return Ok(());
        };
        cancel_request(database, *request_id).await
    }
}

/// `cancel_request` bounds connection acquisition, the cancellation write, and
/// commit together. An already attached request must retain its allocation.
async fn cancel_request(
    database: &PgPool,
    request_id: Uuid,
) -> Result<(), ResourcePoolBackendError> {
    tokio::time::timeout(SAVE_CANCELLATION_TIMEOUT, async {
        let mut txn = database_transaction(database).await?;
        match storage::request_cancel(&mut txn, request_id).await? {
            ConditionalWrite::Applied(()) | ConditionalWrite::NotApplied(_) => {}
        }
        txn.commit().await
    })
    .await
    .map_err(|_| RemoteResourcePoolError::Incomplete)??;
    Ok(())
}

/// `model_value` preserves the pool's domain when saving a typed selection.
fn model_value<T: ToString>(
    domain: ResourcePoolValueDomain,
    value: &T,
) -> Result<ResourcePoolValue, ValueConversionError> {
    let value = value_to_wire(domain, value)?;
    Ok(match domain {
        ResourcePoolValueDomain::Integer => {
            ResourcePoolValue::Integer(value_from_wire(domain, &value)?)
        }
        ResourcePoolValueDomain::IpAddress => {
            ResourcePoolValue::IpAddress(value_from_wire(domain, &value)?)
        }
        ResourcePoolValueDomain::Ipv6Prefix => {
            ResourcePoolValue::Ipv6Prefix(value_from_wire(domain, &value)?)
        }
    })
}
