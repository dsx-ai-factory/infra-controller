// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Share DPU loopback preparation, admission, and attachment between discovery
//! and Site Explorer, and backfill missing IPv6 loopbacks.

use std::future::Future;
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
use sqlx::{PgConnection, PgPool};
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
    /// existing ownership is reused. An empty Integrated pool leaves IPv6
    /// optional. An acknowledged remote pool requires an allocation even if
    /// its allowed range is empty. A configured allocator's failure aborts preparation.
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
                Ipv6LoopbackRequest::Create(requested_loopback_v6),
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
        txn: &mut db::Transaction<'_>,
    ) -> Result<(), ResourcePoolBackendError> {
        let config = db::machine::get_network_config(&mut **txn, &self.machine_id).await?;
        let missing_ipv4 = config.value.loopback_ip.is_none();
        let missing_ipv6 = self.ipv6.is_some()
            && config.value.loopback_ip_v6.is_none()
            && self
                .registry
                .is_configured(txn, &self.pools.ethernet.pool_loopback_ip_v6)
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

/// Creation may prepare an absent machine. Backfill must find an existing DPU
/// before reserving anything, so deletion cannot turn repair into creation.
enum Ipv6LoopbackRequest {
    Create(Option<Ipv6Addr>),
    Backfill,
}

/// Preparation shared by foreground attachment and the IPv6-only backfill.
struct PreparedDpuIpv6Loopback<'a> {
    instrumentation: carbide_instrument::Instrumentation,
    identity: MachineCreationIdentity,
    allocation: Option<Allocation<'a, Ipv6Addr>>,
    requested: Option<Ipv6Addr>,
}

impl<'a> PreparedDpuIpv6Loopback<'a> {
    /// Reuses configured or owned addresses before preparing a new allocation.
    /// Capture the machine lifetime so deletion or replacement cannot inherit it.
    async fn prepare(
        database: &PgPool,
        registry: &'a ResourcePoolRegistry,
        pools: &'a CommonPools,
        machine_id: MachineId,
        request: Ipv6LoopbackRequest,
        cancel: &CancellationToken,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Result<Self, ResourcePoolBackendError> {
        let requested = match request {
            Ipv6LoopbackRequest::Create(requested) => requested,
            Ipv6LoopbackRequest::Backfill => None,
        };
        let pool = &pools.ethernet.pool_loopback_ip_v6;
        let owner_id = machine_id.to_string();
        let (identity, selected) = cancel
            .run_until_cancelled(tokio::time::timeout(PREPARATION_LOOKUP_TIMEOUT, async {
                let mut txn = database_transaction(database).await?;
                let identity =
                    MachineCreationIdentity::capture(txn.as_pgconn(), &machine_id).await?;
                identity.check(txn.as_pgconn(), &machine_id).await?;
                if matches!(request, Ipv6LoopbackRequest::Backfill) && identity.created().is_none()
                {
                    return Err(DatabaseError::NotFoundError {
                        kind: "machine",
                        id: machine_id.to_string(),
                    }
                    .into());
                }
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
                } else if registry.is_configured(txn.as_pgconn(), pool).await? {
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
            requested: requested.filter(|_| identity.created().is_none()),
        })
    }

    /// Attaches the same preparation in each fresh entity transaction. Existing
    /// ownership wins if another caller filled this DPU while preparation ran.
    async fn allocate(
        &self,
        registry: &ResourcePoolRegistry,
        pools: &CommonPools,
        txn: &mut db::Transaction<'_>,
        owner_id: &str,
    ) -> Result<Option<Ipv6Addr>, ResourcePoolBackendError> {
        let pool = &pools.ethernet.pool_loopback_ip_v6;
        if !registry.is_configured(txn, pool).await? {
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
        let allocated = match &self.allocation {
            Some(allocation) => allocation.allocate(txn).await,
            None => {
                // Integrated growth may add the first values after preparation.
                // Its reservation still belongs in this entity transaction.
                // A remote allocator must have been prepared before attachment.
                let backend = registry
                    .get_or_load_backend(txn, pool.name())
                    .await?
                    .ok_or_else(|| {
                        DatabaseError::FailedPrecondition(format!(
                            "resource pool {LOOPBACK_IP_V6} disappeared during attachment"
                        ))
                    })?;
                backend
                    .require_integrated()?
                    .allocate(pool, txn, OwnerType::Machine, owner_id, self.requested)
                    .await
                    .map_err(ResourcePoolBackendError::from)
            }
        };
        match allocated {
            Ok(value) => Ok(Some(value)),
            Err(ResourcePoolBackendError::Integrated(error)) => {
                db::resource_pool::emit_allocation_failure(
                    &self.instrumentation,
                    pool.value_type,
                    owner_id,
                    self.requested.is_some(),
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
/// An empty Integrated pool leaves DPUs IPv4-only. An acknowledged remote pool
/// requires allocation, including when it allows no values. Provider availability
/// and free capacity do not determine whether the pool is configured.
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
/// Changed or unfinished owner-slot work and exhausted lock/version/lifetime
/// failures are collected while later DPUs are tried. The pass returns the first
/// such failure afterward.
/// Ownership lock contention also permits later DPUs. Provider, pool-configuration,
/// deadline and other database failures stop the pass.
/// A deletion recheck also skips failed preparation for ForceDeletion or an
/// absent DPU. A failed or cancelled recheck takes precedence and logs the
/// preparation failure.
/// Cancellation interrupts the scan, pre-checks and active attempts, then bounded
/// cleanup saves cancellation for the exact unused preparation after the entity
/// transaction is dropped. Failure to save that cancellation is logged without
/// changing the attachment result. A completed commit remains in effect.
/// The scan, each DPU's attachment (including its retries), and each standalone
/// recheck have separate five-second client deadlines, including connection
/// acquisition. Scan and attachment statements also have a five-second limit,
/// preserving a shorter database setting.
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

    let dpu_machine_ids = backfill_step(cancel, "DPU IPv6 loopback backfill scan", async {
        let mut txn = database_transaction(db_pool).await?;
        let ids = if registry
            .is_configured(txn.as_pgconn(), &common_pools.ethernet.pool_loopback_ip_v6)
            .await?
        {
            db::machine::find_dpu_ids_without_loopback_ipv6(txn.as_pgconn()).await?
        } else {
            Vec::new()
        };
        txn.commit().await?;
        Ok::<_, ResourcePoolBackendError>(ids)
    })
    .await?;

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
        if backfill_step(
            cancel,
            "DPU IPv6 loopback backfill pre-check",
            db::machine::is_force_deleting_or_deleted(db_pool, &dpu_machine_id),
        )
        .await?
        {
            continue;
        }
        let prepared = match PreparedDpuIpv6Loopback::prepare(
            db_pool,
            registry,
            common_pools,
            dpu_machine_id,
            Ipv6LoopbackRequest::Backfill,
            cancel,
            instrumentation.clone(),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let owner_conflict = is_backfill_owner_conflict(&error) && !cancel.is_cancelled();
                let still_needed = backfill_step(
                    cancel,
                    "DPU IPv6 loopback backfill preparation recheck",
                    async {
                        if owner_conflict {
                            // Read deletion and completion together so a deleting
                            // DPU cannot retain a stale owner-slot failure.
                            db::machine::needs_ipv6_loopback(db_pool, &dpu_machine_id).await
                        } else {
                            db::machine::is_force_deleting_or_deleted(db_pool, &dpu_machine_id)
                                .await
                                .map(|deleted| !deleted)
                        }
                    },
                )
                .await;
                match still_needed {
                    Ok(false) => continue,
                    Ok(true) if owner_conflict => {
                        tracing::warn!(machine_id = %dpu_machine_id, error = %error,
                            "DPU IPv6 loopback allocation changed or is still in progress; continuing backfill");
                        first_failure.get_or_insert(error);
                        continue;
                    }
                    Ok(true) => return Err(error),
                    Err(recheck_error) => {
                        tracing::error!(
                            machine_id = %dpu_machine_id,
                            preparation_error = %error,
                            error = %recheck_error,
                            "Could not recheck DPU state after IPv6 loopback preparation failed"
                        );
                        return Err(recheck_error);
                    }
                }
            }
        };
        let result = backfill_step(cancel, "DPU IPv6 loopback backfill attachment", async {
            let Some(created) = prepared.identity.created() else {
                return Ok(None);
            };
            let mut last_failure = None;

            for _ in 0..MAX_UPDATE_ATTEMPTS {
                let mut txn = database_transaction(db_pool).await?;
                let (mut network_config, network_config_version) =
                    match db::machine::get_network_config(txn.as_pgconn(), &dpu_machine_id).await {
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

                let allocated = match prepared
                    .allocate(
                        registry,
                        common_pools,
                        &mut txn,
                        &dpu_machine_id.to_string(),
                    )
                    .await
                {
                    Ok(allocated) => allocated,
                    Err(error) => {
                        // Release an aborted ownership lookup before cleanup or
                        // work on another DPU uses this connection.
                        txn.rollback().await?;
                        return Err(error);
                    }
                };
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

                // The agent can acknowledge an OVS restart without changing
                // the config version. Preserve that acknowledgement under
                // the row lock instead of writing back our earlier flag.
                network_config.use_admin_network_changed =
                    db::machine::get_network_config(txn.as_pgconn(), &dpu_machine_id)
                        .await?
                        .value
                        .use_admin_network_changed;

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
        .await;
        if let Err(error) = prepared.cancel(db_pool).await {
            tracing::error!(machine_id = %dpu_machine_id, error = %error, "Could not save DPU IPv6 loopback cancellation");
        }
        match result {
            Err(error) if is_backfill_owner_conflict(&error) && !cancel.is_cancelled() => {
                // Attachment can meet the same owner-slot contention as preparation.
                // Keep the failure only if this DPU still needs an address.
                match backfill_step(
                    cancel,
                    "DPU IPv6 loopback backfill attachment recheck",
                    db::machine::needs_ipv6_loopback(db_pool, &dpu_machine_id),
                )
                .await
                {
                    Ok(true) => {
                        first_failure.get_or_insert(error);
                    }
                    Ok(false) => {}
                    Err(recheck_error) => {
                        tracing::error!(
                            machine_id = %dpu_machine_id,
                            attachment_error = %error,
                            error = %recheck_error,
                            "Could not recheck DPU state after IPv6 loopback attachment failed"
                        );
                        return Err(recheck_error);
                    }
                }
            }
            Ok(Some(error)) => {
                // Keep the first exhausted retry failure while later DPUs get a turn.
                first_failure.get_or_insert(error.into());
            }
            Ok(None) => {}
            Err(error) => return Err(error),
        }
    }

    if let Some(error) = first_failure {
        return Err(error);
    }

    Ok(())
}

/// Bound each backfill step, including connection acquisition and lost replies.
/// Dropping the step releases its transaction before unused-allocation cleanup.
async fn backfill_step<T, E: Into<ResourcePoolBackendError>>(
    cancel: &CancellationToken,
    operation: &'static str,
    read: impl Future<Output = Result<T, E>>,
) -> Result<T, ResourcePoolBackendError> {
    cancel
        .run_until_cancelled(tokio::time::timeout(PREPARATION_LOOKUP_TIMEOUT, read))
        .await
        .ok_or(ResourcePoolBackendError::Cancelled { operation })?
        .map_err(|_| ResourcePoolBackendError::DeadlineExceeded { operation })?
        .map_err(Into::into)
}

/// Changed or unfinished work for this owner permits trying later DPUs.
/// Pool configuration, provider and infrastructure failures still stop the pass.
fn is_backfill_owner_conflict(error: &ResourcePoolBackendError) -> bool {
    match error {
        ResourcePoolBackendError::Database(error)
        | ResourcePoolBackendError::Remote(RemoteResourcePoolError::Database(error)) => {
            error.is_lock_not_available()
        }
        ResourcePoolBackendError::CleanupPending
        | ResourcePoolBackendError::Remote(RemoteResourcePoolError::NotCurrent) => true,
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
            run = |error| is_backfill_owner_conflict(&error);
            "a competing generation permits later DPUs" {
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::from(
                    RemoteAllocationError::GenerationExists(DatabaseError::FailedPrecondition(
                        "competing generation".into(),
                    )),
                )) => true,
            }
            "a cancelled request for a replacement permits later DPUs" {
                ResourcePoolBackendError::Remote(RemoteResourcePoolError::NotCurrent) => true,
            }
            "a missing accepted definition stops the pass" {
                ResourcePoolBackendError::Database(DatabaseError::FailedPrecondition(
                    "resource pool lo-ip-v6 has no accepted definition".into(),
                )) => false,
            }
        );
    }

    /// Request insertion wraps SQL errors differently from ownership lookup.
    /// Only lock contention may let backfill continue past this owner's request.
    #[sqlx_test]
    async fn ipv6_backfill_classifies_remote_insert_errors(database: PgPool) {
        for (query, expected) in [
            // A slot insertion waiting on another writer should not block later DPUs.
            (
                "DO $$ BEGIN RAISE EXCEPTION 'injected lock contention' USING ERRCODE = '55P03'; END $$",
                true,
            ),
            // Other database failures still stop the pass, even through this wrapper.
            (
                "DO $$ BEGIN RAISE EXCEPTION 'injected connection failure' USING ERRCODE = '08006'; END $$",
                false,
            ),
        ] {
            let sql_error = sqlx::query(query)
                .execute(&database)
                .await
                .expect_err("injected SQL failure");
            let error = ResourcePoolBackendError::Remote(RemoteResourcePoolError::from(
                RemoteAllocationError::Database(DatabaseError::query(query, sql_error)),
            ));
            assert_eq!(is_backfill_owner_conflict(&error), expected, "{error}");
        }
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
            ScanDeadline,
            RecheckDeadline,
            RecheckCancellation,
        }
        for stop in [
            Stop::LookupDeadline,
            Stop::LookupCancellation,
            Stop::ScanCancellation,
            // The scan needs its own client deadline before a transaction exists.
            Stop::ScanDeadline,
            // Standalone rechecks have no transaction statement timeout.
            Stop::RecheckDeadline,
            Stop::RecheckCancellation,
        ] {
            let held = single.acquire().await.unwrap();
            let cancel = CancellationToken::new();
            let mut operation = Box::pin(async {
                if matches!(stop, Stop::ScanCancellation | Stop::ScanDeadline) {
                    update_dpu_loopback_ips_v6(
                        &single,
                        &registry,
                        api.common_pools(),
                        &cancel,
                        api.instrumentation(),
                    )
                    .await
                } else if matches!(stop, Stop::RecheckDeadline | Stop::RecheckCancellation) {
                    backfill_step(
                        &cancel,
                        "DPU IPv6 loopback backfill deletion recheck",
                        db::machine::is_force_deleting_or_deleted(&single, &machine_id),
                    )
                    .await
                    .map(drop)
                } else {
                    PreparedDpuIpv6Loopback::prepare(
                        &single,
                        &registry,
                        api.common_pools(),
                        machine_id,
                        Ipv6LoopbackRequest::Create(None),
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
            if matches!(
                stop,
                Stop::LookupDeadline | Stop::RecheckDeadline | Stop::ScanDeadline
            ) {
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
                    Stop::ScanDeadline,
                    ResourcePoolBackendError::DeadlineExceeded {
                        operation: "DPU IPv6 loopback backfill scan"
                    }
                ) | (
                    Stop::ScanCancellation,
                    ResourcePoolBackendError::Cancelled {
                        operation: "DPU IPv6 loopback backfill scan"
                    }
                ) | (
                    Stop::RecheckDeadline,
                    ResourcePoolBackendError::DeadlineExceeded {
                        operation: "DPU IPv6 loopback backfill deletion recheck"
                    }
                ) | (
                    Stop::RecheckCancellation,
                    ResourcePoolBackendError::Cancelled {
                        operation: "DPU IPv6 loopback backfill deletion recheck"
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

        // Backfill may lose its candidate after scanning. Unlike discovery,
        // it must not prepare a new machine's reservation for that absent ID.
        let result = PreparedDpuIpv6Loopback::prepare(
            &database,
            &registry,
            api.common_pools(),
            machine_id,
            Ipv6LoopbackRequest::Backfill,
            &CancellationToken::new(),
            api.instrumentation().clone(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ResourcePoolBackendError::Database(
                DatabaseError::NotFoundError {
                    kind: "machine",
                    ..
                }
            ))
        ));
    }
}
