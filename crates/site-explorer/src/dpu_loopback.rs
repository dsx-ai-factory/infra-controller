// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Share DPU loopback preparation, admission, and attachment between discovery
//! and Site Explorer, and backfill missing IPv6 loopbacks.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use carbide_resource_pool::remote::RemoteResourcePoolError;
use carbide_resource_pool::{
    Allocation, ResourcePoolBackendError, ResourcePoolRegistry, database_transaction,
};
use carbide_uuid::machine::MachineId;
use db::machine::MachineCreationIdentity;
use db::resource_pool::remote::RemoteAllocationError;
use db::{ConditionalWrite, DatabaseError};
use model::resource_pool::common::{CommonPools, LOOPBACK_IP_V6};
use model::resource_pool::{OwnerType, ResourcePoolError};
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

/// The allocation purpose identifying a DPU's IPv4 underlay loopback.
pub const DPU_IPV4_LOOPBACK_SLOT: &str = "loopback-ipv4";

/// The allocation purpose identifying a DPU's IPv6 underlay loopback.
pub const DPU_IPV6_LOOPBACK_SLOT: &str = "loopback-ipv6";

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
/// reservation, the wait has no deadline but still observes `cancel`.
pub async fn acquire_admin_admission(
    has_provider_reservation: bool,
    cancel: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ResourcePoolBackendError> {
    if !has_provider_reservation {
        return cancel
            .run_until_cancelled(db::machine_interface::admin_lock_admission())
            .await
            .ok_or(ResourcePoolBackendError::Cancelled {
                operation: "DPU admin lock admission",
            });
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

/// `PreparedDpuLoopback` holds one DPU's IPv4 preparation and optional IPv6
/// preparation, including the machine lifetime captured by each lookup.
/// Prepare and acquire admission before the entity transaction, check identity
/// before creation writes, then attach after interface writes. Roll back on
/// error before cancelling.
pub struct PreparedDpuLoopback<'a> {
    instrumentation: carbide_instrument::Instrumentation,
    machine_id: MachineId,
    identity: MachineCreationIdentity,
    registry: &'a ResourcePoolRegistry,
    pools: &'a CommonPools,
    allocation: Option<Allocation<'a, IpAddr>>,
    ipv6: Option<PreparedDpuIpv6Loopback<'a>>,
}

impl<'a> PreparedDpuLoopback<'a> {
    /// Uses the supplied Event instrumentation; retain its provider through attachment.
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
        instrumentation: carbide_instrument::Instrumentation,
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
                                        &instrumentation,
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
            instrumentation,
            machine_id,
            identity,
            registry,
            pools,
            allocation,
            ipv6: None,
        })
    }

    /// `prepare_with_ipv6` prepares both loopbacks concurrently for discovery
    /// and new inventory. Requested addresses apply only to a new machine;
    /// existing ownership is reused. IPv6 remains optional when its eligibility
    /// contains no values. A configured allocator's failure aborts preparation.
    /// Successful sibling preparation is cancelled if the other fails.
    /// Both address families record Events through the supplied instrumentation.
    #[allow(clippy::too_many_arguments)] // Pool dependencies and requested addresses share one preparation.
    pub async fn prepare_with_ipv6(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        requested_loopback_v4: Option<Ipv4Addr>,
        requested_loopback_v6: Option<Ipv6Addr>,
        cancel: &CancellationToken,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Result<Self, ResourcePoolBackendError> {
        let (ipv4, ipv6) = tokio::join!(
            Self::prepare(
                database,
                registry,
                pools,
                machine_id,
                requested_loopback_v4,
                cancel,
                instrumentation.clone(),
            ),
            PreparedDpuIpv6Loopback::prepare(
                database,
                registry,
                pools,
                machine_id,
                requested_loopback_v6,
                cancel,
                instrumentation,
            ),
        );
        match (ipv4, ipv6) {
            (Ok(mut prepared), Ok(ipv6)) => {
                prepared.ipv6 = Some(ipv6);
                Ok(prepared)
            }
            (Ok(prepared), Err(error)) => {
                if let Err(cleanup_error) = prepared.cancel(database).await {
                    tracing::error!(%machine_id, error = %cleanup_error, "Could not save DPU loopback cancellation");
                }
                Err(error)
            }
            (Err(error), Ok(prepared)) => {
                if let Err(cleanup_error) = prepared.cancel(database).await {
                    tracing::error!(%machine_id, error = %cleanup_error, "Could not save DPU IPv6 loopback cancellation");
                }
                Err(error)
            }
            (Err(error), Err(ipv6_error)) => {
                tracing::error!(%machine_id, error = %ipv6_error, "DPU IPv6 loopback preparation also failed");
                Err(error)
            }
        }
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
            || self.ipv6.as_ref().is_some_and(|ipv6| {
                ipv6.allocation
                    .as_ref()
                    .and_then(Allocation::value)
                    .is_some()
            })
    }

    /// `check_identity` rejects deletion or replacement before the caller writes
    /// machines and interfaces. For an absent machine, repeat this under the
    /// caller's deletion fence and hold that fence through creation and commit.
    pub async fn check_identity(&self, txn: &mut PgConnection) -> Result<(), DatabaseError> {
        self.identity.check(&mut *txn, &self.machine_id).await?;
        if let Some(ipv6) = &self.ipv6 {
            ipv6.identity.check(txn, &self.machine_id).await?;
        }
        Ok(())
    }

    /// `attach` fills missing DPU loopbacks in one conditional configuration
    /// write. IPv4 and IPv6 use their selected pool backends. IPv6 is included
    /// only after `prepare_with_ipv6`; plain `prepare` keeps existing-host repair
    /// IPv4-only. Call after interface writes and commit together.
    /// A busy machine group returns an error: roll back the transaction before
    /// retrying, then cancel any unused provider preparation.
    /// The final write checks row creation time, configuration version, and
    /// ForceDeletion; a rejection must roll back allocation attachment as well.
    pub async fn attach(
        &self,
        txn: &mut PgTransaction<'_>,
    ) -> Result<(), ResourcePoolBackendError> {
        let config = db::machine::get_network_config(&mut **txn, &self.machine_id).await?;
        let missing_ipv4 = config.value.loopback_ip.is_none();
        let missing_ipv6 = self.ipv6.is_some()
            && config.value.loopback_ip_v6.is_none()
            && self
                .registry
                .has_values(txn, &self.pools.ethernet.pool_loopback_ip_v6)
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
                                &self.instrumentation,
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
        if missing_ipv6
            && config.loopback_ip_v6.is_none()
            && let Some(ipv6) = &self.ipv6
        {
            if ipv6
                .identity
                .created()
                .is_some_and(|expected| expected != created)
            {
                return Err(DatabaseError::FailedPrecondition(format!(
                    "machine {} lifetime changed during IPv6 loopback preparation",
                    self.machine_id,
                ))
                .into());
            }
            config.loopback_ip_v6 = ipv6
                .allocate(self.registry, self.pools, txn, &owner_id)
                .await?;
            changed |= config.loopback_ip_v6.is_some();
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
        let (ipv4, ipv6) = tokio::join!(
            async {
                if let Some(allocation) = &self.allocation {
                    allocation.cancel(database).await?;
                }
                Ok::<_, ResourcePoolBackendError>(())
            },
            async {
                if let Some(ipv6) = &self.ipv6 {
                    ipv6.cancel(database).await?;
                }
                Ok::<_, ResourcePoolBackendError>(())
            },
        );
        match (ipv4, ipv6) {
            (Err(error), Err(ipv6_error)) => {
                tracing::error!(machine_id = %self.machine_id, error = %ipv6_error, "DPU IPv6 loopback cancellation also failed");
                Err(error)
            }
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

/// Preparation shared by foreground attachment and the IPv6-only backfill.
struct PreparedDpuIpv6Loopback<'a> {
    instrumentation: carbide_instrument::Instrumentation,
    identity: MachineCreationIdentity,
    allocation: Option<Allocation<'a, Ipv6Addr>>,
}

impl<'a> PreparedDpuIpv6Loopback<'a> {
    /// Reuses configured or owned addresses before preparing a new allocation.
    /// Capture the machine lifetime so deletion or replacement cannot inherit it.
    async fn prepare(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        requested: Option<Ipv6Addr>,
        cancel: &CancellationToken,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Result<Self, ResourcePoolBackendError> {
        let pool = &pools.ethernet.pool_loopback_ip_v6;
        let owner_id = machine_id.to_string();
        let (identity, selected) = cancel
            .run_until_cancelled(tokio::time::timeout(PREPARATION_LOOKUP_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                let identity =
                    MachineCreationIdentity::capture(txn.as_pgconn(), &machine_id).await?;
                identity.check(txn.as_pgconn(), &machine_id).await?;
                let configured = if identity.created().is_some() {
                    db::machine::get_network_config(txn.as_pgconn(), &machine_id)
                        .await?
                        .value
                        .loopback_ip_v6
                        .is_some()
                } else {
                    false
                };
                let selected = if configured
                    || registry
                        .find_owned(
                            pool,
                            txn.as_pgconn(),
                            OwnerType::Machine,
                            &owner_id,
                            DPU_IPV6_LOOPBACK_SLOT,
                        )
                        .await?
                        .is_some()
                {
                    None
                } else if registry.has_values(txn.as_pgconn(), pool).await? {
                    Some(
                        registry
                            .select(txn.as_pgconn(), pool)
                            .await?
                            .ok_or_else(|| {
                                DatabaseError::FailedPrecondition(format!(
                                    "resource pool {LOOPBACK_IP_V6} disappeared during preparation"
                                ))
                            })?,
                    )
                } else {
                    if requested.is_some() && identity.created().is_none() {
                        return Err(DatabaseError::FailedPrecondition(format!(
                            "requested IPv6 loopback requires a configured {LOOPBACK_IP_V6} pool"
                        ))
                        .into());
                    }
                    None
                };
                txn.commit().await?;
                Ok::<_, ResourcePoolBackendError>((identity, selected))
            }))
            .await
            .ok_or(ResourcePoolBackendError::Cancelled {
                operation: "DPU IPv6 loopback lookup",
            })?
            .map_err(|_| ResourcePoolBackendError::DeadlineExceeded {
                operation: "DPU IPv6 loopback lookup",
            })??;
        let allocation = match selected {
            Some(selected) => Some(
                selected
                    .prepare_allocation(
                        database,
                        OwnerType::Machine,
                        &owner_id,
                        DPU_IPV6_LOOPBACK_SLOT,
                        requested.filter(|_| identity.created().is_none()),
                        cancel,
                    )
                    .await?,
            ),
            None => None,
        };
        Ok(Self {
            instrumentation,
            identity,
            allocation,
        })
    }

    /// Attaches the same preparation in each fresh entity transaction. Existing
    /// ownership wins if another caller filled this DPU while preparation ran.
    async fn allocate(
        &self,
        registry: &ResourcePoolRegistry,
        pools: &CommonPools,
        txn: &mut PgTransaction<'_>,
        owner_id: &str,
    ) -> Result<Option<Ipv6Addr>, ResourcePoolBackendError> {
        let pool = &pools.ethernet.pool_loopback_ip_v6;
        if !registry.has_values(txn, pool).await? {
            return Ok(None);
        }
        if let Some(owned) = registry
            .find_owned(
                pool,
                txn,
                OwnerType::Machine,
                owner_id,
                DPU_IPV6_LOOPBACK_SLOT,
            )
            .await?
        {
            return Ok(Some(*owned.value()));
        }
        let allocation = self.allocation.as_ref().ok_or_else(|| {
            DatabaseError::FailedPrecondition(format!(
                "DPU {owner_id} IPv6 loopback ownership changed during preparation"
            ))
        })?;
        match allocation.allocate(txn).await {
            Ok(value) => Ok(Some(value)),
            Err(ResourcePoolBackendError::Integrated(error)) => {
                db::resource_pool::emit_allocation_failure(
                    &self.instrumentation,
                    pool.value_type,
                    owner_id,
                    false,
                    LOOPBACK_IP_V6,
                    &error,
                );
                if matches!(
                    error,
                    db::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                        ResourcePoolError::Empty
                    )
                ) {
                    return Err(
                        DatabaseError::ResourceExhausted(format!("pool {LOOPBACK_IP_V6}")).into(),
                    );
                }
                Err(error.into())
            }
            Err(error) => Err(error),
        }
    }

    async fn cancel(&self, database: &PgPool) -> Result<(), ResourcePoolBackendError> {
        if let Some(allocation) = &self.allocation {
            allocation.cancel(database).await?;
        }
        Ok(())
    }
}

/// Fills missing optional IPv6 loopbacks through the configured pool backend.
/// Empty allowed eligibility leaves DPUs IPv4-only; provider availability and
/// free capacity do not determine whether the pool is configured.
///
/// Each DPU is prepared once outside its attachment transactions. The complete
/// ownership lookup has a five-second budget, including connection acquisition
/// and any Integrated reservation-row wait. Remote request persistence and
/// provider resolution retain the shared allocator's separate budgets.
///
/// Each DPU gets at most three fresh transactions without backoff. Configuration
/// and version are reloaded because writing one sibling advances the whole group.
/// Visible ForceDeletion is skipped; target/group contention rolls back the
/// Integrated reservation or remote attachment and retries without waiting for
/// Machine locks. Remote preparation survives retries and is cancelled afterward
/// if unused. The final write checks the captured creation time as well as version
/// and deletion. Earlier DPU commits survive later failures.
///
/// Unfinished owner-slot work and exhausted lock/version failures are collected
/// while later DPUs are tried. The pass returns the first such failure afterward.
/// Provider, pool-configuration, deadline and database failures stop the pass.
/// A deletion recheck also skips failed preparation for ForceDeletion or an
/// absent DPU. A failed or cancelled recheck takes precedence and logs the
/// preparation failure.
/// Cancellation interrupts the scan, pre-checks and active attempts, then bounded
/// cleanup saves cancellation for the exact unused preparation after the entity
/// transaction is dropped. A commit that already completed remains durable.
/// Scan and attachment statements have a five-second limit, preserving a shorter
/// database setting. Standalone deletion checks retain the database's limit.
/// There is no fleet-wide deadline, so a pass scales with the missing DPU count.
///
/// Startup logs failures without scheduling another pass. A later startup,
/// pool-growth request, or DPU rediscovery can fill a remaining loopback.
/// Allocation failures use the caller's instrumentation throughout the pass.
pub async fn update_dpu_loopback_ips_v6(
    db_pool: &PgPool,
    registry: &ResourcePoolRegistry,
    common_pools: &CommonPools,
    cancel: &CancellationToken,
    instrumentation: &carbide_instrument::Instrumentation,
) -> Result<(), ResourcePoolBackendError> {
    const MAX_UPDATE_ATTEMPTS: usize = 3;

    let dpu_machine_ids = cancel
        .run_until_cancelled(async {
            let mut txn = database_transaction(db_pool).await?;
            let ids = if registry
                .has_values(txn.as_pgconn(), &common_pools.ethernet.pool_loopback_ip_v6)
                .await?
            {
                db::machine::find_dpu_ids_without_loopback_ipv6(txn.as_pgconn()).await?
            } else {
                Vec::new()
            };
            txn.commit().await?;
            Ok::<_, ResourcePoolBackendError>(ids)
        })
        .await
        .ok_or(ResourcePoolBackendError::Cancelled {
            operation: "DPU IPv6 loopback backfill scan",
        })??;

    if !dpu_machine_ids.is_empty() {
        tracing::info!(
            dpu_count = dpu_machine_ids.len(),
            "Updating missing IPv6 loopback IPs for DPUs"
        );
    }

    let mut first_failure = None;

    for dpu_machine_id in dpu_machine_ids {
        // Avoid provider work for an already absent or deleting DPU. Later
        // checks still fence deletion that commits after this read.
        if cancel
            .run_until_cancelled(db::machine::is_force_deleting_or_deleted(
                db_pool,
                &dpu_machine_id,
            ))
            .await
            .ok_or(ResourcePoolBackendError::Cancelled {
                operation: "DPU IPv6 loopback backfill pre-check",
            })??
        {
            continue;
        }
        let prepared = match PreparedDpuIpv6Loopback::prepare(
            db_pool,
            registry,
            common_pools,
            dpu_machine_id,
            None,
            cancel,
            instrumentation.clone(),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let deletion_recheck = match cancel
                    .run_until_cancelled(db::machine::is_force_deleting_or_deleted(
                        db_pool,
                        &dpu_machine_id,
                    ))
                    .await
                {
                    Some(result) => result.map_err(ResourcePoolBackendError::from),
                    None => Err(ResourcePoolBackendError::Cancelled {
                        operation: "DPU IPv6 loopback backfill deletion recheck",
                    }),
                };
                match deletion_recheck {
                    Ok(true) => continue,
                    Ok(false) if is_backfill_slot_conflict(&error) && !cancel.is_cancelled() => {
                        tracing::warn!(machine_id = %dpu_machine_id, error = %error,
                            "DPU IPv6 loopback slot is still in use; continuing backfill");
                        first_failure.get_or_insert(error);
                        continue;
                    }
                    Ok(false) => return Err(error),
                    Err(recheck_error) => {
                        tracing::error!(
                            machine_id = %dpu_machine_id,
                            preparation_error = %error,
                            error = %recheck_error,
                            "Could not recheck DPU deletion after IPv6 loopback preparation failed"
                        );
                        return Err(recheck_error);
                    }
                }
            }
        };
        let result = cancel
            .run_until_cancelled(async {
                let Some(created) = prepared.identity.created() else {
                    return Ok(None);
                };
                let mut last_failure = None;

                for _ in 0..MAX_UPDATE_ATTEMPTS {
                    let mut txn = database_transaction(db_pool).await?;
                    let (mut network_config, network_config_version) =
                        match db::machine::get_network_config(txn.as_pgconn(), &dpu_machine_id)
                            .await
                        {
                            Ok(network_config) => network_config.take(),
                            Err(error) if error.is_not_found() => {
                                // Force deletion can remove a DPU after the initial ID
                                // scan. It no longer needs a reservation.
                                txn.commit().await?;
                                last_failure = None;
                                break;
                            }
                            Err(error) => return Err(ResourcePoolBackendError::from(error)),
                        };
                    if network_config.loopback_ip_v6.is_some() {
                        txn.commit().await?;
                        last_failure = None;
                        break;
                    }

                    // A committed deletion needs no reservation, even when its row is
                    // busy or the pool is exhausted. This read takes no Machine lock.
                    if db::machine::is_force_deleting_or_deleted(txn.as_pgconn(), &dpu_machine_id)
                        .await?
                    {
                        txn.commit().await?;
                        last_failure = None;
                        break;
                    }

                    let allocated = prepared
                        .allocate(
                            registry,
                            common_pools,
                            &mut txn,
                            &dpu_machine_id.to_string(),
                        )
                        .await?;
                    let loopback_ip_v6 = allocated.ok_or_else(|| {
                        DatabaseError::FailedPrecondition(format!(
                            concat!(
                                "resource pool {} disappeared during ",
                                "DPU IPv6 loopback backfill"
                            ),
                            LOOPBACK_IP_V6
                        ))
                    })?;
                    network_config.loopback_ip_v6 = Some(loopback_ip_v6);

                    // Roll back the Integrated reservation or remote attachment
                    // rather than wait for a target whose group may need it.
                    let admitted = db::machine::try_lock_dpu_for_loopback_backfill(
                        txn.as_pgconn(),
                        &dpu_machine_id,
                    )
                    .await;
                    match admitted {
                        Ok(true) => {}
                        Ok(false) => {
                            txn.rollback().await?;
                            last_failure = None;
                            break;
                        }
                        Err(error) if error.is_lock_not_available() => {
                            txn.rollback().await?;
                            last_failure = Some(error);
                            continue;
                        }
                        Err(error) => return Err(ResourcePoolBackendError::from(error)),
                    }

                    match db::machine::try_update_network_config_for_created(
                        txn.as_pgconn(),
                        &dpu_machine_id,
                        created,
                        network_config_version,
                        &network_config,
                    )
                    .await
                    {
                        Ok(ConditionalWrite::Applied(())) => {
                            txn.commit().await?;
                            last_failure = None;
                            break;
                        }
                        Ok(ConditionalWrite::NotApplied(_)) => {}
                        Err(error) if error.is_lock_not_available() => {
                            // Roll back the reservation/attachment before reloading.
                            // The same remote preparation remains available.
                            txn.rollback().await?;
                            last_failure = Some(error);
                            continue;
                        }
                        Err(error) => return Err(ResourcePoolBackendError::from(error)),
                    }

                    txn.rollback().await?;
                    last_failure = Some(DatabaseError::ConcurrentModificationError(
                        "machine",
                        network_config_version.to_string(),
                    ));
                }

                Ok::<_, ResourcePoolBackendError>(last_failure)
            })
            .await
            .unwrap_or(Err(ResourcePoolBackendError::Cancelled {
                operation: "DPU IPv6 loopback backfill attachment",
            }));
        if let Err(error) = prepared.cancel(db_pool).await {
            tracing::error!(machine_id = %dpu_machine_id, error = %error, "Could not save DPU IPv6 loopback cancellation");
        }
        if let Some(error) = result? {
            // One busy DPU should not postpone every row after it in the scan.
            // Remember the first failure, continue the pass, and report it
            // once the remaining DPUs have had their own chance to reconcile.
            first_failure.get_or_insert(error.into());
        }
    }

    if let Some(error) = first_failure {
        return Err(error);
    }

    Ok(())
}

/// Only unfinished work for this owner slot permits trying later DPUs.
/// Pool configuration, provider and infrastructure failures still stop the pass.
fn is_backfill_slot_conflict(error: &ResourcePoolBackendError) -> bool {
    match error {
        ResourcePoolBackendError::CleanupPending => true,
        ResourcePoolBackendError::Remote(RemoteResourcePoolError::Storage(error)) => matches!(
            error.as_ref(),
            RemoteAllocationError::GenerationExists(_) | RemoteAllocationError::SlotInUse(_)
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    use carbide_test_harness::prelude::{TestHarness, sqlx_test, sqlx_testing};
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    /// Slot races permit later DPUs, but an unacknowledged pool definition must
    /// stop the pass even though both errors map to FailedPrecondition.
    #[test]
    fn ipv6_backfill_distinguishes_slot_races_from_pool_configuration() {
        carbide_test_support::value_scenarios!(
            run = |error| is_backfill_slot_conflict(&error);
            "a competing generation permits later DPUs" {
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::from(
                    RemoteAllocationError::GenerationExists(DatabaseError::FailedPrecondition(
                        "competing generation".into(),
                    )),
                )) => true,
            }
            "a missing accepted definition stops the pass" {
                ResourcePoolBackendError::Database(DatabaseError::FailedPrecondition(
                    "resource pool lo-ip-v6 has no accepted definition".into(),
                )) => false,
            }
        );
    }

    /// The private lookup and initial scan must stop while connection acquisition
    /// is pending, and leave the same connection pool usable afterward.
    #[sqlx_test]
    async fn ipv6_lookup_deadline_and_cancellation_stop_pool_wait(database: PgPool) {
        let harness = TestHarness::builder(database.clone()).build().await;
        let api = harness.api();
        let single = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(database.connect_options().as_ref().clone())
            .await
            .unwrap();
        let registry = ResourcePoolRegistry::default();
        let machine_id = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
            .parse()
            .unwrap();
        enum Stop {
            LookupDeadline,
            LookupCancellation,
            ScanCancellation,
        }
        for stop in [
            Stop::LookupDeadline,
            Stop::LookupCancellation,
            Stop::ScanCancellation,
        ] {
            let held = single.acquire().await.unwrap();
            let cancel = CancellationToken::new();
            let mut operation = Box::pin(async {
                if matches!(stop, Stop::ScanCancellation) {
                    update_dpu_loopback_ips_v6(
                        &single,
                        &registry,
                        api.common_pools(),
                        &cancel,
                        api.instrumentation(),
                    )
                    .await
                } else {
                    PreparedDpuIpv6Loopback::prepare(
                        &single,
                        &registry,
                        api.common_pools(),
                        machine_id,
                        None,
                        &cancel,
                        api.instrumentation().clone(),
                    )
                    .await
                    .map(drop)
                }
            });
            assert!(
                poll_fn(|cx| Poll::Ready(operation.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            if matches!(stop, Stop::LookupDeadline) {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(6)).await;
                tokio::time::resume();
            } else {
                cancel.cancel();
            }
            let error = tokio::time::timeout(Duration::from_secs(1), operation)
                .await
                .expect("lookup or scan must stop while the connection is held")
                .expect_err("the interrupted database wait must fail");
            assert!(matches!(
                (stop, error),
                (
                    Stop::LookupDeadline,
                    ResourcePoolBackendError::DeadlineExceeded {
                        operation: "DPU IPv6 loopback lookup"
                    }
                ) | (
                    Stop::LookupCancellation,
                    ResourcePoolBackendError::Cancelled {
                        operation: "DPU IPv6 loopback lookup"
                    }
                ) | (
                    Stop::ScanCancellation,
                    ResourcePoolBackendError::Cancelled {
                        operation: "DPU IPv6 loopback backfill scan"
                    }
                )
            ));
            drop(held);
            assert_eq!(
                sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&single)
                    .await
                    .unwrap(),
                1
            );
        }
        single.close().await;
    }
}
