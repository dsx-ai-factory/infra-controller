// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Prepare and attach DPU ASNs, and backfill missing assignments at startup.

use std::time::Duration;

use carbide_resource_pool::remote::RemoteResourcePoolError;
use carbide_resource_pool::{
    Allocation, ResourcePoolBackendError, ResourcePoolRegistry, ResourcePoolWithBackend,
    database_transaction,
};
use carbide_uuid::machine::MachineId;
use db::machine::MachineCreationIdentity;
use db::{ConditionalWrite, DatabaseError, Transaction};
use model::resource_pool::common::CommonPools;
use model::resource_pool::{OwnerType, ResourcePoolError};
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio_util::sync::CancellationToken;

/// The allocation purpose identifying a DPU's FNN ASN.
pub const DPU_ASN_SLOT: &str = "fnn-asn";

// Bound remote lookups, the backfill scan and each backfill attachment, including
// acquiring a connection and committing. Native SQL retains its ordinary limits.
const DATABASE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

// Bound remote startup work independently of fleet size and individual provider
// attempts, leaving their request identities available for cleanup.
// Expiry cancels the active attempt before awaiting its bounded cleanup.
const REMOTE_BACKFILL_BUDGET: Duration = Duration::from_secs(60);

/// Creation allocates only new DPUs and permits failure; backfill requires an existing row.
#[derive(Clone, Copy)]
enum FailurePolicy {
    BestEffort,
    Required,
}

/// The ASN work to perform when the entity transaction reaches attachment.
enum AsnPreparation<'a> {
    /// No fill is needed, or the optional allocation already failed.
    Unneeded,
    /// Resolve Integrated inventory when the entity transaction reaches attachment.
    Integrated,
    /// Read the existing assignment under the entity transaction's locks.
    Owned,
    /// Attach this prepared allocation if the ASN is still missing.
    Allocation(Box<Allocation<'a, u32>>),
}

/// The work selected by the pre-transaction lookup.
enum AsnLookup<'a> {
    Unneeded,
    Integrated,
    Owned,
    Remote(ResourcePoolWithBackend<'a, u32>),
}

/// `PreparedDpuAsn` keeps one machine lifetime and its selected ASN preparation.
/// Check identity before creation, attach after interface writes, and cancel
/// after the entity transaction ends. Allocation failure is nonfatal for creation.
pub struct PreparedDpuAsn<'a> {
    machine_id: MachineId,
    identity: MachineCreationIdentity,
    registry: &'a ResourcePoolRegistry,
    pools: &'a CommonPools,
    preparation: AsnPreparation<'a>,
    failure_policy: FailurePolicy,
    cancel: CancellationToken,
}

impl<'a> PreparedDpuAsn<'a> {
    /// `prepare` plans ASN allocation for a new DPU before its entity transaction.
    /// Existing remote DPUs restore saved ownership only; startup backfill owns
    /// fresh fills. Allocation failures leave creation without an ASN and produce
    /// diagnostics. Lookup, identity and owning cancellation errors remain errors.
    /// Remote lookup shares five seconds, including acquisition and commit.
    /// Integrated lookup retains the database's ordinary timing limits.
    pub async fn prepare(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        cancel: &CancellationToken,
    ) -> Result<Self, ResourcePoolBackendError> {
        Self::prepare_with_policy(
            database,
            registry,
            pools,
            machine_id,
            cancel,
            FailurePolicy::BestEffort,
        )
        .await
    }

    /// Prepare new-DPU creation or existing-DPU backfill through the same slot.
    /// Validate a saved binding before skipping existing Integrated machines.
    async fn prepare_with_policy(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        cancel: &CancellationToken,
        failure_policy: FailurePolicy,
    ) -> Result<Self, ResourcePoolBackendError> {
        let pool = &pools.ethernet.pool_fnn_asn;
        let owner_id = machine_id.to_string();
        // A pool absent from the resolved registry can only load as Integrated;
        // get_or_load_backend still rejects an unresolved saved remote authority.
        let remote = registry
            .get(pool.name())
            .is_some_and(|selected| selected.backend.remote().is_some());
        let lookup = async {
            let mut txn = if remote {
                database_transaction(database).await?
            } else {
                Transaction::begin(database).await?
            };
            let identity = MachineCreationIdentity::capture(txn.as_pgconn(), &machine_id).await?;
            identity.check(txn.as_pgconn(), &machine_id).await?;
            let _ = registry
                .get_or_load_backend(txn.as_pgconn(), pool.name())
                .await?;
            let existing = identity.created().is_some();
            let decision =
                match (failure_policy, existing, remote) {
                    (FailurePolicy::Required, false, _)
                    | (FailurePolicy::BestEffort, true, false) => AsnLookup::Unneeded,
                    _ if existing
                        && db::machine::get_asn(txn.as_pgconn(), &machine_id)
                            .await?
                            .is_some() =>
                    {
                        AsnLookup::Unneeded
                    }
                    _ => {
                        if registry
                            .find_owned(
                                pool,
                                txn.as_pgconn(),
                                OwnerType::Machine,
                                &owner_id,
                                DPU_ASN_SLOT,
                            )
                            .await?
                            .is_some()
                        {
                            AsnLookup::Owned
                        } else if matches!(failure_policy, FailurePolicy::BestEffort) && existing {
                            AsnLookup::Unneeded
                        } else if remote {
                            let selected = registry
                                .select(txn.as_pgconn(), pool)
                                .await?
                                .ok_or_else(|| {
                                    DatabaseError::FailedPrecondition(format!(
                                        "configured ASN pool `{}` is missing",
                                        pool.name(),
                                    ))
                                })?;
                            AsnLookup::Remote(selected)
                        } else {
                            AsnLookup::Integrated
                        }
                    }
                };
            txn.commit().await?;
            Ok::<_, ResourcePoolBackendError>((identity, decision))
        };
        let (identity, decision) = if remote {
            cancel
                .run_until_cancelled(tokio::time::timeout(DATABASE_STEP_TIMEOUT, lookup))
                .await
                .ok_or(ResourcePoolBackendError::Cancelled {
                    operation: "DPU ASN lookup",
                })?
                .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
                    operation: "DPU ASN lookup",
                })??
        } else {
            cancel.run_until_cancelled(lookup).await.ok_or(
                ResourcePoolBackendError::Cancelled {
                    operation: "DPU ASN lookup",
                },
            )??
        };
        let preparation = match decision {
            AsnLookup::Unneeded => AsnPreparation::Unneeded,
            AsnLookup::Integrated => AsnPreparation::Integrated,
            AsnLookup::Owned => AsnPreparation::Owned,
            AsnLookup::Remote(selected) => {
                match selected
                    .prepare_allocation(
                        database,
                        OwnerType::Machine,
                        &owner_id,
                        DPU_ASN_SLOT,
                        None,
                        cancel,
                    )
                    .await
                {
                    Ok(allocation) => AsnPreparation::Allocation(Box::new(allocation)),
                    Err(error) => {
                        handle_allocation_failure(failure_policy, pools, &owner_id, error, cancel)
                            .map_err(|error| *error)?;
                        AsnPreparation::Unneeded
                    }
                }
            }
        };
        Ok(Self {
            machine_id,
            identity,
            registry,
            pools,
            preparation,
            failure_policy,
            cancel: cancel.clone(),
        })
    }

    /// A new provider assignment needs a short admission deadline before attachment.
    /// Integrated work, saved ownership and failed preparation hold no such reservation.
    pub fn has_provider_reservation(&self) -> bool {
        matches!(&self.preparation, AsnPreparation::Allocation(allocation)
            if allocation.value().is_some())
    }

    /// The hardware ID whose lifetime this preparation checked.
    pub(crate) fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    /// `check_identity` rejects deletion or replacement before creation writes.
    /// An absent machine must be checked under the caller's deletion fence,
    /// held through creation and commit, as for loopback preparation.
    pub async fn check_identity(&self, txn: &mut PgConnection) -> Result<(), DatabaseError> {
        self.identity.check(txn, &self.machine_id).await
    }

    /// `attach` fills only a missing ASN on the same live machine, after interface
    /// writes. Already assigned, absent or force-deleting targets need no fill;
    /// observed replacement lifetimes are errors; a losing conditional write skips
    /// attachment. Needed writes use the existing nonblocking group lock. A busy
    /// group returns an error so the caller retries the entity transaction.
    /// The assignment savepoint rolls back failed allocation or a losing write.
    /// Creation records allocation failures and continues; backfill returns them.
    pub async fn attach(
        &self,
        txn: &mut PgTransaction<'_>,
    ) -> Result<(), ResourcePoolBackendError> {
        if matches!(&self.preparation, AsnPreparation::Unneeded) {
            return Ok(());
        }
        let current = MachineCreationIdentity::capture(&mut **txn, &self.machine_id).await?;
        let Some(created) = current.created() else {
            return Ok(());
        };
        if self
            .identity
            .created()
            .is_some_and(|expected| expected != created)
        {
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine {} lifetime changed during ASN preparation",
                self.machine_id,
            ))
            .into());
        }
        if db::machine::get_asn(&mut **txn, &self.machine_id)
            .await?
            .is_some()
        {
            return Ok(());
        }
        let owner_id = self.machine_id.to_string();
        let mut assignment = Transaction::begin_inner(txn).await?;
        if !db::machine::try_lock_group_for_update(assignment.as_pgconn(), &self.machine_id).await?
        {
            assignment.rollback().await?;
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine group for {} changed or is busy; retry",
                self.machine_id,
            ))
            .into());
        }
        if db::machine::get_asn(assignment.as_pgconn(), &self.machine_id)
            .await?
            .is_some()
        {
            assignment.rollback().await?;
            return Ok(());
        }
        let owned = self
            .registry
            .find_owned(
                &self.pools.ethernet.pool_fnn_asn,
                assignment.as_pgconn(),
                OwnerType::Machine,
                &owner_id,
                DPU_ASN_SLOT,
            )
            .await?;
        let asn = match owned {
            Some(owned) => *owned.value(),
            None => {
                let result = match &self.preparation {
                    AsnPreparation::Allocation(allocation) => {
                        allocation.allocate(&mut assignment).await
                    }
                    AsnPreparation::Integrated => {
                        let pool = &self.pools.ethernet.pool_fnn_asn;
                        match self
                            .registry
                            .get_or_load_backend(assignment.as_pgconn(), pool.name())
                            .await?
                        {
                            Some(backend) => backend
                                .require_integrated()?
                                .allocate(
                                    pool,
                                    assignment.as_pgconn(),
                                    OwnerType::Machine,
                                    &owner_id,
                                    None,
                                )
                                .await
                                .map_err(Into::into),
                            None => {
                                Err(db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                                    ResourcePoolError::MissingAllocationRange {
                                        pool_name: pool.name().to_owned(),
                                        auto_assign: true,
                                    },
                                )
                                .into())
                            }
                        }
                    }
                    AsnPreparation::Owned | AsnPreparation::Unneeded => {
                        assignment.rollback().await?;
                        return Ok(());
                    }
                };
                match result {
                    Ok(asn) => asn,
                    Err(error) => {
                        assignment.rollback().await?;
                        return handle_allocation_failure(
                            self.failure_policy,
                            self.pools,
                            &owner_id,
                            error,
                            &self.cancel,
                        )
                        .map_err(|error| *error);
                    }
                }
            }
        };
        match db::machine::try_update_asn_for_created(
            assignment.as_pgconn(),
            &self.machine_id,
            created,
            asn,
        )
        .await?
        {
            ConditionalWrite::Applied(()) => assignment.commit().await?,
            ConditionalWrite::NotApplied(_) => assignment.rollback().await?,
        }
        Ok(())
    }

    /// `cancel` records cleanup for this exact unused attempt after the entity
    /// transaction ends. A successfully attached allocation stays owned.
    pub async fn cancel(&self, database: &PgPool) -> Result<(), ResourcePoolBackendError> {
        if let AsnPreparation::Allocation(allocation) = &self.preparation {
            allocation.cancel(database).await?;
        }
        Ok(())
    }
}

/// Keep creation's allocation failure nonfatal while preserving native diagnostics.
/// Owning cancellation and backfill failures remain errors.
fn handle_allocation_failure(
    policy: FailurePolicy,
    pools: &CommonPools,
    owner_id: &str,
    error: ResourcePoolBackendError,
    cancel: &CancellationToken,
) -> Result<(), Box<ResourcePoolBackendError>> {
    // Shutdown still stops admission when it races an optional provider error.
    if cancel.is_cancelled() {
        return Err(ResourcePoolBackendError::Cancelled {
            operation: "DPU ASN allocation",
        }
        .into());
    }
    if matches!(policy, FailurePolicy::Required) || is_owning_cancellation(&error) {
        return Err(error.into());
    }
    match &error {
        ResourcePoolBackendError::Integrated(error) => {
            db::resource_pool::emit_best_effort_dpu_asn_allocation_failure(
                pools.ethernet.pool_fnn_asn.value_type,
                owner_id,
                error,
            );
        }
        _ => tracing::info!(stable_machine_id = %owner_id, error = %error,
            "Failed to allocate ASN for DPU"),
    }
    Ok(())
}

/// `update_dpu_asns` uses the selected backend to backfill missing ASNs.
/// Integrated retains its existing eligibility, batch transaction and assignment
/// savepoints and ordinary database limits. Owning cancellation stops the native
/// batch without adding a deadline; its transaction rolls back after in-flight SQL
/// finishes. Remote preparation runs outside transactions; assignments commit
/// separately. Classified slot and provider failures permit later DPUs to proceed;
/// database and corrupt ownership failures stop the pass.
/// Remote scanning and each attachment share five seconds with their connection
/// acquisition and commit, and observe cancellation. Provider preparation keeps
/// the shared allocator's separate deadline. The remote pass has a one-minute
/// budget; expiry cancels its active attempt and awaits bounded cleanup.
/// Budget expiry returns `DeadlineExceeded`, even if earlier rows committed;
/// cleanup can extend completion beyond that processing budget. Missing remote
/// ASNs are retried by a later startup. Discovery and Site Explorer repair restore
/// saved ownership without making fresh allocations for existing DPUs.
/// This function starts no recurring task.
pub async fn update_dpu_asns(
    database: &PgPool,
    registry: &ResourcePoolRegistry,
    pools: &CommonPools,
    cancel: &CancellationToken,
) -> Result<(), ResourcePoolBackendError> {
    let pool = &pools.ethernet.pool_fnn_asn;
    let remote = registry
        .get(pool.name())
        .is_some_and(|selected| selected.backend.remote().is_some());
    if !remote {
        let native = async {
            // Cached Integrated selection goes straight to the unchanged native batch.
            // Validate an uncached binding with ordinary SQL timing before fallback.
            if registry.get(pool.name()).is_none() {
                let mut txn = Transaction::begin(database).await?;
                let _ = registry
                    .get_or_load_backend(txn.as_pgconn(), pool.name())
                    .await?;
                txn.commit().await?;
            }
            db::machine::update_dpu_asns(database, pools)
                .await
                .map_err(ResourcePoolBackendError::from)
        };
        return cancel.run_until_cancelled(native).await.ok_or(
            ResourcePoolBackendError::Cancelled {
                operation: "DPU ASN backfill",
            },
        )?;
    }
    let machine_ids = cancel
        .run_until_cancelled(tokio::time::timeout(DATABASE_STEP_TIMEOUT, async {
            let mut txn = database_transaction(database).await?;
            let ids = db::machine::find_dpu_ids_without_asn(txn.as_pgconn()).await?;
            txn.commit().await?;
            Ok::<_, ResourcePoolBackendError>(ids)
        }))
        .await
        .ok_or(ResourcePoolBackendError::Cancelled {
            operation: "DPU ASN backfill lookup",
        })?
        .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
            operation: "DPU ASN backfill lookup",
        })??;
    let pass_cancel = cancel.child_token();
    let mut pass = Box::pin(backfill_remote_asns(
        database,
        registry,
        pools,
        machine_ids,
        &pass_cancel,
    ));
    tokio::select! {
        result = &mut pass => result,
        _ = tokio::time::sleep(REMOTE_BACKFILL_BUDGET) => {
            pass_cancel.cancel();
            if let Err(error) = pass.await
                && !is_owning_cancellation(&error) {
                tracing::warn!(error = %error, "DPU ASN backfill ended after its startup budget");
            }
            Err(ResourcePoolBackendError::DeadlineExceeded { operation: "DPU ASN backfill" })
        }
    }
}

/// Try each scanned DPU and retain the first per-slot error for startup diagnostics.
/// Cancellation, database failures and corrupt ownership stop the pass.
async fn backfill_remote_asns(
    database: &PgPool,
    registry: &ResourcePoolRegistry,
    pools: &CommonPools,
    machine_ids: Vec<MachineId>,
    cancel: &CancellationToken,
) -> Result<(), ResourcePoolBackendError> {
    let mut first_error = None;
    for machine_id in machine_ids {
        let result = backfill_remote_asn(database, registry, pools, machine_id, cancel).await;
        if let Err(error) = result {
            if !is_entry_failure(&error) || cancel.is_cancelled() {
                return Err(error);
            }
            tracing::warn!(%machine_id, error = %error, "Could not backfill DPU ASN");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Prepare outside SQL, fill only the same live DPU, and clean up any unused attempt.
async fn backfill_remote_asn(
    database: &PgPool,
    registry: &ResourcePoolRegistry,
    pools: &CommonPools,
    machine_id: MachineId,
    cancel: &CancellationToken,
) -> Result<(), ResourcePoolBackendError> {
    let prepared = PreparedDpuAsn::prepare_with_policy(
        database,
        registry,
        pools,
        machine_id,
        cancel,
        FailurePolicy::Required,
    )
    .await?;
    let result = async {
        // Backfill never creates rows; an absent capture is skipped before
        // provider preparation and cannot authorize a later ingestion.
        if prepared.identity.created().is_none() {
            return Ok(());
        }
        cancel
            .run_until_cancelled(tokio::time::timeout(DATABASE_STEP_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                prepared.attach(&mut txn).await?;
                txn.commit().await?;
                Ok::<_, ResourcePoolBackendError>(())
            }))
            .await
            .ok_or(ResourcePoolBackendError::Cancelled {
                operation: "DPU ASN backfill attachment",
            })?
            .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
                operation: "DPU ASN backfill attachment",
            })?
    }
    .await;
    if let Err(error) = prepared.cancel(database).await {
        tracing::error!(%machine_id, error = %error, "Could not save DPU ASN cancellation");
    }
    result
}

/// Distinguish local shutdown from a cancellation status returned by the provider.
fn is_owning_cancellation(error: &ResourcePoolBackendError) -> bool {
    matches!(
        error,
        ResourcePoolBackendError::Cancelled { .. }
            | ResourcePoolBackendError::Remote(RemoteResourcePoolError::Cancelled)
    )
}

/// Whether a DPU's slot or provider failure permits trying later DPUs.
/// Infrastructure and corrupt ownership errors still stop the pass.
fn is_entry_failure(error: &ResourcePoolBackendError) -> bool {
    if let ResourcePoolBackendError::Database(error) = error
        && error.is_not_found()
    {
        return true;
    }
    if let ResourcePoolBackendError::Remote(RemoteResourcePoolError::Storage(error)) = error {
        return matches!(
            error.as_ref(),
            db::resource_pool::remote::RemoteAllocationError::GenerationExists(_)
                | db::resource_pool::remote::RemoteAllocationError::SlotInUse(_)
        );
    }
    matches!(
        error,
        ResourcePoolBackendError::CleanupPending
            | ResourcePoolBackendError::RequestChanged
            | ResourcePoolBackendError::Value(_)
            | ResourcePoolBackendError::Database(DatabaseError::FailedPrecondition(_))
            | ResourcePoolBackendError::Remote(
                RemoteResourcePoolError::Busy
                    | RemoteResourcePoolError::Incomplete
                    | RemoteResourcePoolError::NotCurrent
                    | RemoteResourcePoolError::Rejected(_)
                    | RemoteResourcePoolError::Provider(_)
                    | RemoteResourcePoolError::InvalidResponse(_)
            )
    )
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use db::resource_pool::remote::RemoteAllocationError;

    use super::*;

    #[test]
    fn owner_slot_races_allow_later_backfill_entries() {
        let conflict = || DatabaseError::FailedPrecondition("competing owner slot".into());
        value_scenarios!(
            run = |error| is_entry_failure(&ResourcePoolBackendError::Remote(
                RemoteResourcePoolError::from(error),
            ));
            "a competing generation permits later DPUs" {
                RemoteAllocationError::GenerationExists(conflict()) => true,
            }
            "an unfinished slot permits later DPUs" {
                RemoteAllocationError::SlotInUse(conflict()) => true,
            }
            "a corrupt binding stops the pass" {
                RemoteAllocationError::BindingMismatch(Default::default()) => false,
            }
        );
    }
}
