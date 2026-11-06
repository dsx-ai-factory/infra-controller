// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Prepare DPU ASNs outside entity transactions and attach the selected ownership.

use std::time::Duration;

use carbide_resource_pool::remote::RemoteResourcePoolError;
use carbide_resource_pool::{
    Allocation, ResourcePoolBackend, ResourcePoolBackendError, ResourcePoolRegistry,
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

// Bound each lookup, backfill scan and backfill attachment, including acquiring
// a connection and committing. Entity transactions retain their caller's bounds.
const DATABASE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

// One minute permits a complete shared provider attempt (30 seconds) and its
// database steps without letting fleet size determine API startup time.
// Expiry cancels the active attempt before awaiting its bounded cleanup.
const REMOTE_BACKFILL_BUDGET: Duration = Duration::from_secs(60);

/// Creation permits an absent ASN; backfill reports allocation failures.
#[derive(Clone, Copy)]
enum FailurePolicy {
    BestEffort,
    Required,
}

/// The ASN work to perform when the entity transaction reaches attachment.
enum AsnPreparation<'a> {
    /// No fill is needed, or the optional allocation already failed.
    Unneeded,
    /// Report missing Integrated inventory if creation reaches attachment.
    Unconfigured,
    /// Read the existing assignment under the entity transaction's locks.
    Owned,
    /// Attach this prepared allocation if the ASN is still missing.
    Allocation(Box<Allocation<'a, u32>>),
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
    remote_attempt: bool,
    failure_policy: FailurePolicy,
    cancel: CancellationToken,
}

impl<'a> PreparedDpuAsn<'a> {
    /// `prepare` resolves a missing ASN before the entity transaction. Integrated
    /// allocation is attempted only for a new machine; existing remote ownership
    /// is reused during repair. Allocation failures leave creation without an ASN
    /// and produce diagnostics. Lookup, identity and owning cancellation errors
    /// remain errors. The initial lookup shares five seconds, including acquiring
    /// a connection and committing. Present provider route-target policy is rejected.
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

    /// Prepare the same slot for creation or backfill, preserving their failure
    /// policies. Resolve saved backend identity before applying native shortcuts.
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
        let (identity, selected, owned, missing_pool, remote_attempt) = cancel
            .run_until_cancelled(tokio::time::timeout(DATABASE_STEP_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                let identity =
                    MachineCreationIdentity::capture(txn.as_pgconn(), &machine_id).await?;
                identity.check(txn.as_pgconn(), &machine_id).await?;
                let remote = registry
                    .get_or_load_backend(txn.as_pgconn(), pool.name())
                    .await?
                    .is_some_and(|backend| backend.remote().is_some());
                if (matches!(failure_policy, FailurePolicy::Required)
                    && identity.created().is_none())
                    || (matches!(failure_policy, FailurePolicy::BestEffort)
                        && identity.created().is_some()
                        && !remote)
                {
                    txn.commit().await?;
                    return Ok((identity, None, false, false, false));
                }
                let configured = identity.created().is_some()
                    && db::machine::get_asn(txn.as_pgconn(), &machine_id)
                        .await?
                        .is_some();
                let owned = if configured {
                    false
                } else {
                    registry
                        .find_owned(
                            pool,
                            txn.as_pgconn(),
                            OwnerType::Machine,
                            &owner_id,
                            DPU_ASN_SLOT,
                        )
                        .await?
                        .is_some()
                };
                let selected = if configured || owned {
                    None
                } else {
                    registry.select(txn.as_pgconn(), pool).await?
                };
                let missing_pool = !configured && !owned && selected.is_none();
                txn.commit().await?;
                let remote_attempt = remote && selected.is_some();
                Ok::<_, ResourcePoolBackendError>((
                    identity,
                    selected,
                    owned,
                    missing_pool,
                    remote_attempt,
                ))
            }))
            .await
            .ok_or(ResourcePoolBackendError::Cancelled {
                operation: "DPU ASN lookup",
            })?
            .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
                operation: "DPU ASN lookup",
            })??;
        let preparation = if let Some(selected) = selected {
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
        } else if missing_pool {
            AsnPreparation::Unconfigured
        } else if owned {
            AsnPreparation::Owned
        } else {
            AsnPreparation::Unneeded
        };
        Ok(Self {
            machine_id,
            identity,
            registry,
            pools,
            preparation,
            remote_attempt,
            failure_policy,
            cancel: cancel.clone(),
        })
    }

    /// Whether remote preparation needs bounded, cancellable admission, including
    /// an optional attempt that failed. Integrated and reused ownership keep their
    /// existing admission policy.
    pub fn needs_bounded_admission(&self) -> bool {
        self.remote_attempt
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
    /// replacement lifetimes are errors. Needed writes use the existing nonblocking
    /// group lock; a busy group requires the caller to roll back and retry.
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
        if matches!(&self.preparation, AsnPreparation::Unconfigured) {
            return handle_allocation_failure(
                self.failure_policy,
                self.pools,
                &owner_id,
                db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                    ResourcePoolError::MissingAllocationRange {
                        pool_name: self.pools.ethernet.pool_fnn_asn.name().to_owned(),
                        auto_assign: true,
                    },
                )
                .into(),
                &self.cancel,
            )
            .map_err(|error| *error);
        }
        let mut assignment = Transaction::begin_inner(txn).await?;
        if !db::machine::try_lock_group_for_update(assignment.as_pgconn(), &self.machine_id).await?
        {
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
                let AsnPreparation::Allocation(allocation) = &self.preparation else {
                    assignment.rollback().await?;
                    return Ok(());
                };
                match allocation.allocate(&mut assignment).await {
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
/// savepoints. Remote preparation runs outside transactions, and each assignment
/// commits separately;
/// unavailable or obsolete slots are reported without starving later DPUs.
/// Remote scanning and each attachment share five seconds with their connection
/// acquisition and commit, and observe cancellation. Provider preparation keeps
/// the shared allocator's separate deadline. The remote pass has a one-minute
/// budget; expiry cancels its active attempt and awaits bounded cleanup.
/// Budget expiry returns `DeadlineExceeded`, even if earlier rows committed;
/// cleanup can extend completion beyond that processing budget. Missing remote
/// ASNs are retried by discovery, Site Explorer repair, or a later startup.
/// This function starts no recurring task.
pub async fn update_dpu_asns(
    database: &PgPool,
    registry: &ResourcePoolRegistry,
    pools: &CommonPools,
    cancel: &CancellationToken,
) -> Result<(), ResourcePoolBackendError> {
    let remote_ids = cancel
        .run_until_cancelled(tokio::time::timeout(DATABASE_STEP_TIMEOUT, async {
            let mut txn = database_transaction(database).await?;
            let remote = matches!(
                registry
                    .get_or_load_backend(txn.as_pgconn(), pools.ethernet.pool_fnn_asn.name())
                    .await?,
                Some(ResourcePoolBackend::Grpc(_))
            );
            let ids = if remote {
                Some(db::machine::find_dpu_ids_without_asn(txn.as_pgconn()).await?)
            } else {
                None
            };
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
    let Some(machine_ids) = remote_ids else {
        return db::machine::update_dpu_asns(database, pools)
            .await
            .map_err(Into::into);
    };
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
    matches!(
        error,
        ResourcePoolBackendError::CleanupPending
            | ResourcePoolBackendError::RequestChanged
            | ResourcePoolBackendError::Value(_)
            | ResourcePoolBackendError::Database(
                DatabaseError::FailedPrecondition(_) | DatabaseError::NotFoundError { .. }
            )
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
