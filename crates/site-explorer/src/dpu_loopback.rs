// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Share DPU loopback preparation, admission, and attachment between discovery
//! and Site Explorer.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use carbide_resource_pool::{
    Allocation, ResourcePoolBackendError, ResourcePoolRegistry, database_transaction,
};
use carbide_uuid::machine::MachineId;
use db::machine::MachineCreationIdentity;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::common::CommonPools;
use model::resource_pool::{OwnerType, ResourcePoolError};
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

/// The allocation purpose identifying a DPU's IPv4 underlay loopback.
pub const DPU_IPV4_LOOPBACK_SLOT: &str = "loopback-ipv4";

// Bound connection acquisition, every lookup, and commit together, including
// configured and reused allocations that need no provider call.
const PREPARATION_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

// Bound admission while a provider reservation is held, leaving time for
// attachment. The transaction retains its own statement limits.
const DPU_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `acquire_admin_admission` takes an admin-lock admission permit.
/// Acquire before opening the transaction and hold the permit through commit
/// or rollback, so waiting callers do not occupy database connections.
/// A pending provider reservation limits the wait to five seconds and observes
/// `cancel`, leaving the remaining reservation lifetime for SQL attachment.
/// Cancellation returns `ResourcePoolBackendError::Cancelled`; expiration
/// returns `ResourcePoolBackendError::DeadlineExceeded`. Without a provider
/// reservation, the wait has no deadline and ignores `cancel`.
pub async fn acquire_admin_admission(
    has_provider_reservation: bool,
    cancel: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ResourcePoolBackendError> {
    if !has_provider_reservation {
        return Ok(db::machine_interface::admin_lock_admission().await);
    }

    cancel
        .run_until_cancelled(tokio::time::timeout(
            DPU_ADMISSION_TIMEOUT,
            db::machine_interface::admin_lock_admission(),
        ))
        .await
        .ok_or(ResourcePoolBackendError::Cancelled {
            operation: "DPU admin lock admission",
        })?
        .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
            operation: "DPU admin lock admission",
        })
}

/// `PreparedDpuLoopback` holds one DPU's identity and selected-backend preparation.
/// Prepare and acquire admission before the entity transaction, check identity
/// before creation writes, then attach after interface writes. Roll back on
/// error before cancelling.
pub struct PreparedDpuLoopback<'a> {
    machine_id: MachineId,
    identity: MachineCreationIdentity,
    registry: &'a ResourcePoolRegistry,
    pools: &'a CommonPools,
    allocation: Option<Allocation<'a, IpAddr>>,
}

impl<'a> PreparedDpuLoopback<'a> {
    /// `prepare` captures the machine lifetime and resolves a missing IPv4
    /// reservation without holding the caller's transaction. Existing attached
    /// ownership is reused, including when its configuration field was cleared.
    /// Concurrent foreground attempts can fail with the occupied-slot error;
    /// a later caller retries against the winner's saved ownership. Initial
    /// lookup, including pool acquisition and commit, shares five seconds and
    /// observes cancellation even when no provider call is needed.
    /// `requested_loopback_v4` applies only to a new machine; existing machines
    /// retain their ownership or automatic repair. The caller must lock and
    /// recheck its reservation intent before any creation or interface writes.
    pub async fn prepare(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        requested_loopback_v4: Option<Ipv4Addr>,
        cancel: &CancellationToken,
    ) -> Result<Self, ResourcePoolBackendError> {
        let pool = &pools.ethernet.pool_loopback_ip;
        let owner_id = machine_id.to_string();
        let (identity, selected) =
            cancel
                .run_until_cancelled(tokio::time::timeout(PREPARATION_LOOKUP_TIMEOUT, async {
                    let mut txn = database_transaction(database).await?;
                    let identity =
                        MachineCreationIdentity::capture(txn.as_pgconn(), &machine_id).await?;
                    identity.check(txn.as_pgconn(), &machine_id).await?;
                    let configured = if identity.created().is_some() {
                        db::machine::get_network_config(txn.as_pgconn(), &machine_id)
                            .await?
                            .value
                            .loopback_ip
                            .is_some()
                    } else {
                        false
                    };
                    let owned = if configured {
                        false
                    } else {
                        registry
                            .find_owned(
                                pool,
                                txn.as_pgconn(),
                                OwnerType::Machine,
                                &owner_id,
                                DPU_IPV4_LOOPBACK_SLOT,
                            )
                            .await?
                            .is_some()
                    };
                    let selected =
                        if configured || owned {
                            None
                        } else {
                            Some(registry.select(txn.as_pgconn(), pool).await?.ok_or_else(
                                || {
                                    let error =
                                        db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                                            ResourcePoolError::Empty,
                                        );
                                    db::resource_pool::emit_allocation_failure(
                                        pool.value_type,
                                        &owner_id,
                                        false,
                                        "lo-ip",
                                        &error,
                                    );
                                    DatabaseError::ResourceExhausted("pool lo-ip".to_owned())
                                },
                            )?)
                        };
                    txn.commit().await?;
                    Ok::<_, ResourcePoolBackendError>((identity, selected))
                }))
                .await
                .ok_or(ResourcePoolBackendError::Cancelled {
                    operation: "DPU loopback lookup",
                })?
                .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
                    operation: "DPU loopback lookup",
                })??;
        let allocation = match selected {
            Some(selected) => Some(
                selected
                    .prepare_allocation(
                        database,
                        OwnerType::Machine,
                        &owner_id,
                        DPU_IPV4_LOOPBACK_SLOT,
                        requested_loopback_v4
                            .filter(|_| identity.created().is_none())
                            .map(IpAddr::V4),
                        cancel,
                    )
                    .await?,
            ),
            None => None,
        };
        Ok(Self {
            machine_id,
            identity,
            registry,
            pools,
            allocation,
        })
    }

    /// Returns the machine owning this exact preparation for cleanup diagnostics.
    pub(crate) fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    /// `has_provider_reservation` is true when a provider allocation is waiting
    /// for attachment. Integrated selects its value in the transaction, and
    /// reused ownership needs no new reservation; neither needs that deadline.
    pub fn has_provider_reservation(&self) -> bool {
        self.allocation
            .as_ref()
            .and_then(Allocation::value)
            .is_some()
    }

    /// `check_identity` rejects deletion or replacement before the caller writes
    /// machines and interfaces. For an absent machine, repeat this under the
    /// caller's deletion fence and hold that fence through creation and commit.
    pub async fn check_identity(&self, txn: &mut PgConnection) -> Result<(), DatabaseError> {
        self.identity.check(txn, &self.machine_id).await
    }

    /// `attach` fills missing DPU loopbacks in one conditional configuration
    /// write. IPv4 uses the selected backend. `fill_missing_ipv6` preserves the
    /// caller's existing optional Integrated IPv6 fill; pass false for an IPv4
    /// repair of an existing Site Explorer machine. Call after interface writes
    /// and commit together.
    /// A busy machine group returns an error: roll back the transaction before
    /// retrying, then cancel any unused provider preparation.
    /// The final write checks row creation time, configuration version, and
    /// ForceDeletion; a rejection must roll back allocation attachment as well.
    pub async fn attach(
        &self,
        txn: &mut PgTransaction<'_>,
        fill_missing_ipv6: bool,
    ) -> Result<(), ResourcePoolBackendError> {
        let config = db::machine::get_network_config(&mut **txn, &self.machine_id).await?;
        let missing_ipv4 = config.value.loopback_ip.is_none();
        let missing_ipv6 = fill_missing_ipv6
            && config.value.loopback_ip_v6.is_none()
            && db::resource_pool::pool_has_rows(
                txn,
                self.pools.ethernet.pool_loopback_ip_v6.name(),
            )
            .await?;
        if missing_ipv4 || missing_ipv6 {
            // Interface writes come first. Do not wait for group rows while
            // holding a DPU: an existing host writer may need that DPU next.
            if !db::machine::try_lock_group_for_update(txn, &self.machine_id).await? {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "machine group for {} changed or is busy; retry",
                    self.machine_id,
                ))
                .into());
            }
        }
        let current = MachineCreationIdentity::capture(&mut **txn, &self.machine_id).await?;
        current.check(&mut **txn, &self.machine_id).await?;
        let created = current
            .created()
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "machine",
                id: self.machine_id.to_string(),
            })?;
        if self
            .identity
            .created()
            .is_some_and(|expected| expected != created)
        {
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine {} lifetime changed during loopback preparation",
                self.machine_id,
            ))
            .into());
        }
        if !missing_ipv4 && !missing_ipv6 {
            return Ok(());
        }
        // A writer may have completed the fill while we waited for the group.
        let (mut config, version) = db::machine::get_network_config(&mut **txn, &self.machine_id)
            .await?
            .take();
        let mut changed = false;
        let owner_id = self.machine_id.to_string();
        if config.loopback_ip.is_none() {
            let owned = self
                .registry
                .find_owned(
                    &self.pools.ethernet.pool_loopback_ip,
                    txn,
                    OwnerType::Machine,
                    &owner_id,
                    DPU_IPV4_LOOPBACK_SLOT,
                )
                .await?;
            let value = match owned {
                Some(owned) => *owned.value(),
                None => {
                    let allocation = self.allocation.as_ref().ok_or_else(|| {
                        DatabaseError::FailedPrecondition(format!(
                            "DPU {owner_id} loopback ownership changed during preparation",
                        ))
                    })?;
                    match allocation.allocate(txn).await {
                        Ok(value) => value,
                        Err(ResourcePoolBackendError::Integrated(error)) => {
                            db::resource_pool::emit_allocation_failure(
                                self.pools.ethernet.pool_loopback_ip.value_type,
                                &owner_id,
                                false,
                                "lo-ip",
                                &error,
                            );
                            if matches!(
                                error,
                                db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                                    ResourcePoolError::Empty
                                )
                            ) {
                                return Err(DatabaseError::ResourceExhausted(
                                    "pool lo-ip".to_owned(),
                                )
                                .into());
                            }
                            return Err(error.into());
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            config.loopback_ip = Some(value);
            changed = true;
        }
        if fill_missing_ipv6
            && config.loopback_ip_v6.is_none()
            && let Some(value) =
                db::machine::allocate_loopback_ip_v6(self.pools, txn, &owner_id, None).await?
        {
            config.loopback_ip_v6 = Some(value);
            changed = true;
        }
        if changed
            && matches!(
                db::machine::try_update_network_config_for_created(
                    txn,
                    &self.machine_id,
                    created,
                    version,
                    &config,
                )
                .await?,
                ConditionalWrite::NotApplied(_)
            )
        {
            return Err(
                DatabaseError::ConcurrentModificationError("machine", version.to_string()).into(),
            );
        }
        Ok(())
    }

    /// `cancel` saves cancellation for this exact unused preparation after the
    /// entity transaction ends. An allocation attached by a successful commit
    /// remains owned; roll back failed entity writes before calling this.
    pub async fn cancel(&self, database: &PgPool) -> Result<(), ResourcePoolBackendError> {
        if let Some(allocation) = &self.allocation {
            allocation.cancel(database).await?;
        }
        Ok(())
    }
}
