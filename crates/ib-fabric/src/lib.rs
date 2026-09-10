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

pub mod config;
pub mod errors;
pub mod ib;
mod metrics;

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::emit;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::machine::HostMachineId;
use chrono::Utc;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{self, DatabaseError};
use health_report::HealthReportApplyMode;
use metrics::{
    FabricMetrics, IbFabricDataLoadFailed, IbFabricMonitorMetrics, IbMonitorIterationFinished,
    IbMonitorMachineStatusObservationFailed, IbMonitorPkeyReconciliationSkipped,
    IbMonitorSkuInactivePreloadFailed, UfmGuidPkeyChangeFinished, UfmOperation,
};
use model::ib::{IBNetwork, IBPort, IBPortMembership, IBPortState, IbMembership};
use model::ib_partition::{IBPartition, IbPartitionSearchFilter, PartitionKey};
use model::machine::infiniband::{
    MachineIbInterfaceStatusObservation, MachineInfinibandStatusObservation,
};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    HostHealthConfig, LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot,
};
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::config::IbFabricDefinition;
use crate::errors::{IbError, IbResult};
use crate::ib::{GetPartitionOptions, IBFabric, IBFabricManager, IBFabricManagerType};

type SkuInactiveDevicesCache = HashMap<String, Option<HashSet<u32>>>;

/// `IbFabricMonitor` monitors the health of all connected InfiniBand fabrics in periodic intervals
pub struct IbFabricMonitor {
    db_pool: PgPool,

    fabrics: HashMap<String, IbFabricDefinition>,
    metric_holder: Arc<metrics::MetricHolder>,
    /// API for interaction with Forge IBFabricManager
    fabric_manager: Arc<dyn IBFabricManager>,

    host_health: HostHealthConfig,
    work_lock_manager_handle: WorkLockManagerHandle,
}

impl IbFabricMonitor {
    const ITERATION_WORK_KEY: &'static str = "IbFabricMonitor::run_single_iteration";

    /// Create a IbFabricMonitor
    pub fn new(
        db_pool: PgPool,
        fabrics: HashMap<String, IbFabricDefinition>,
        meter: opentelemetry::metrics::Meter,
        fabric_manager: Arc<dyn IBFabricManager>,
        host_health: HostHealthConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        // We want to hold metrics for longer than the iteration interval, so there is continuity
        // in emitting metrics. However we want to avoid reporting outdated metrics in case
        // reporting gets stuck. Therefore round up the iteration interval by 1min.
        let hold_period = fabric_manager
            .get_config()
            .fabric_manager_run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(
            meter,
            hold_period,
            &fabrics
                .keys()
                .map(|fab| fab.as_str())
                .collect::<Vec<&str>>(),
        ));

        IbFabricMonitor {
            db_pool,
            fabrics,
            metric_holder,
            fabric_manager,
            host_health,
            work_lock_manager_handle,
        }
    }

    /// Start the IbFabricMonitor and return a [sending channel](tokio::sync::oneshot::Sender) that will stop the IbFabricMonitor when dropped.
    pub fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        if self.fabric_manager.get_config().manager_type != IBFabricManagerType::Disable {
            join_set
                .build_task()
                .name("ib_fabric_monitor")
                .spawn(async move { self.run(cancel_token).await })?;
        }

        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        let run_interval = self.fabric_manager.get_config().fabric_manager_run_interval;
        let timer = PeriodicTimer::new(run_interval);

        loop {
            let mut tick = timer.tick();
            // Failed passes emit `IbMonitorIterationFinished` next to their
            // latency. This loop only adjusts cadence; logging here would
            // duplicate the terminal diagnostic at a different level.
            if let Ok(num_changes) = self.run_single_iteration().await
                && num_changes > 0
            {
                // If any change has been applied to the IB fabric,
                // the status that has been collected in the last iteration is already outdated
                // Therefore run again as soon as possible.
                tick.set_interval(Duration::from_millis(1000));
            }

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("IbFabricMonitor stop was requested");
                    return;
                }
            }
        }
    }

    pub async fn run_single_iteration(&self) -> IbResult<usize> {
        let mut metrics = IbFabricMonitorMetrics::new();

        let num_changes = if let Ok(_lock) = self
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            tracing::trace!(
                lock = Self::ITERATION_WORK_KEY,
                "IbFabricMonitor acquired the lock",
            );

            let span_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));

            let check_ib_fabrics_span = tracing::span!(
                parent: None,
                tracing::Level::INFO,
                "check_ib_fabrics_and_apply_changes",
                span_id,
                otel.status_code = tracing::field::Empty,
                otel.status_message = tracing::field::Empty,
                num_fabrics = 0,
                fabric_metrics = tracing::field::Empty,
            );

            let res = self
                .check_ib_fabrics_and_apply_changes(&mut metrics)
                .instrument(check_ib_fabrics_span.clone())
                .await;
            check_ib_fabrics_span.record("num_fabrics", metrics.num_fabrics);
            check_ib_fabrics_span.record(
                "fabric_metrics",
                serde_json::to_string(&metrics.fabrics).unwrap_or_default(),
            );

            let num_changes = match &res {
                Ok(num_changes) => {
                    check_ib_fabrics_span.record("otel.status_code", "ok");
                    *num_changes
                }
                Err(e) => {
                    check_ib_fabrics_span.record("otel.status_code", "error");
                    // Writing this field will set the span status to error
                    // Therefore we only write it on errors
                    check_ib_fabrics_span.record("otel.status_message", format!("{e:?}"));
                    0
                }
            };

            check_ib_fabrics_span.in_scope(|| {
                carbide_instrument::emit(match res.as_ref().err() {
                    None => IbMonitorIterationFinished::Succeeded {
                        latency: metrics.recording_started_at.elapsed(),
                    },
                    Some(error) => IbMonitorIterationFinished::Failed {
                        latency: metrics.recording_started_at.elapsed(),
                        error: error.to_string(),
                    },
                });
            });

            // Cache all other metrics that have been captured in this iteration.
            // Those will be queried by OTEL on demand
            self.metric_holder.update_metrics(metrics);

            res?;

            num_changes
        } else {
            0
        };

        Ok(num_changes)
    }

    async fn check_ib_fabrics_and_apply_changes(
        &self,
        metrics: &mut IbFabricMonitorMetrics,
    ) -> IbResult<usize> {
        if self.fabric_manager.get_config().manager_type == IBFabricManagerType::Disable {
            return Ok(0);
        }

        let mut conn = self
            .db_pool
            .acquire()
            .await
            .map_err(|e| IbError::from(DatabaseError::new("acquire connection", e)))?;
        let snapshots = match self.get_all_snapshots(&mut conn).await {
            Ok(snapshots) => snapshots,
            Err(e) => {
                tracing::error!(error = %e, "Failed to load ManagedHost snapshots in IbFabricMonitor");
                // Record the same error for all fabrics, so that the problem is at least visible on dashboards
                for fabric in self.fabrics.keys() {
                    metrics.num_fabrics += 1;
                    let fabric_metrics = metrics.fabrics.entry(fabric.to_string()).or_default();
                    fabric_metrics.fabric_error = "ManagedHostSnapshotLoadingError".to_string();
                }
                return Err(e);
            }
        };

        let tenant_partitions = match get_tenant_partitions(&mut conn).await {
            Ok(snapshots) => snapshots,
            Err(e) => {
                tracing::error!(error = %e, "Failed to load Partition data in IbFabricMonitor");
                // Record the same error for all fabrics, so that the problem is at least visible on dashboards
                for fabric in self.fabrics.keys() {
                    metrics.num_fabrics += 1;
                    let fabric_metrics = metrics.fabrics.entry(fabric.to_string()).or_default();
                    fabric_metrics.fabric_error = "ManagedHostSnapshotLoadingError".to_string();
                }
                return Err(e);
            }
        };
        drop(conn); // Don't reuse the postgres connection later on. It might be stale

        // Create a reverse mapping from pkeys to partition IDs
        // That makes the lookup of which partition ID is associated with GUIDs
        // more efficient
        let mut partition_ids_by_pkey = HashMap::new();
        for (id, partition) in tenant_partitions.iter() {
            if let Some(pkey) = partition.status.as_ref().and_then(|s| s.pkey) {
                partition_ids_by_pkey.insert(pkey, *id);
            }
        }

        // One client per fabric for the whole iteration: the data-loading phase
        // below builds it and the change-application phase at the end reuses it.
        let mut fabric_clients: HashMap<String, Arc<dyn IBFabric>> = HashMap::new();
        let mut fabric_data: HashMap<String, FabricData> = HashMap::new();
        for (fabric, fabric_definition) in self.fabrics.iter() {
            let fabric_data = fabric_data.entry(fabric.to_string()).or_default();

            metrics.num_fabrics += 1;
            let fabric_metrics = metrics.fabrics.entry(fabric.to_string()).or_default();
            if let Some(conn) = load_single_fabric_data(
                self.fabric_manager.as_ref(),
                fabric,
                fabric_definition,
                fabric_data,
                fabric_metrics,
            )
            .await
            {
                fabric_clients.insert(fabric.clone(), conn);
            }
        }

        let sku_inactive_cache = preload_sku_inactive_devices(&self.db_pool, &snapshots)
            .await
            .unwrap_or_else(|e| {
                emit(IbMonitorSkuInactivePreloadFailed::new(e.to_string()));
                HashMap::new()
            });

        let machine_ids_by_guid = machine_ids_by_ib_guid(&snapshots);
        let current_fabrics_by_guid = current_fabrics_by_ib_guid(&fabric_data);
        let memberships_in_ufm = memberships_in_fabric_data(&fabric_data);
        let mut reports = Vec::new();
        for (machine, snapshot) in &snapshots {
            let mut snapshot_clone = snapshot.clone();
            match record_machine_infiniband_status_observation(
                &self.db_pool,
                &mut snapshot_clone,
                &tenant_partitions,
                &partition_ids_by_pkey,
                &fabric_data,
                &sku_inactive_cache,
                metrics,
            )
            .await
            {
                Ok(report) => reports.push(report),
                Err(e) => {
                    emit(IbMonitorMachineStatusObservationFailed::new(
                        e.to_string(),
                        machine.to_string(),
                    ));
                }
            }
        }

        // Query only retired records that match a membership UFM reports or a
        // membership this pass may add. Historical rows are not scanned into
        // the monitor.
        let memberships_to_check = memberships_in_ufm
            .iter()
            .chain(reports.iter().flat_map(|report| &report.needed_memberships))
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        // Stop this pass if the lookup fails. Continuing without these records
        // could restore a membership that must remain absent.
        let retired_memberships = db::retired_ib_membership::find_recorded_candidates(
            &self.db_pool,
            &memberships_to_check,
        )
        .await?;
        let retired_membership_changes = self
            .reconcile_retired_memberships(
                &mut fabric_clients,
                &memberships_in_ufm,
                &machine_ids_by_guid,
                &current_fabrics_by_guid,
                retired_memberships,
                &mut reports,
            )
            .await?;
        let membership_changes = apply_guid_pkey_changes(
            self.fabric_manager.as_ref(),
            &mut fabric_clients,
            &self.fabrics,
            &tenant_partitions,
            &partition_ids_by_pkey,
            reports,
        )
        .await?;

        Ok(retired_membership_changes + membership_changes)
    }

    /// `reconcile_retired_memberships` removes memberships that should stay
    /// retired and suppresses changes from stale `Machine` snapshots. A locked
    /// reread leaves exact live reuse alone for this pass.
    async fn reconcile_retired_memberships(
        &self,
        fabric_clients: &mut HashMap<String, Arc<dyn IBFabric>>,
        memberships_in_ufm: &HashSet<IbMembership>,
        machine_ids_by_guid: &HashMap<String, Option<HostMachineId>>,
        current_fabrics_by_guid: &HashMap<String, Option<String>>,
        retired_memberships: Vec<IbMembership>,
        reports: &mut [MachineIbStatusEvaluation],
    ) -> IbResult<usize> {
        let mut num_changes = 0;
        let mut memberships_to_suppress = Vec::new();
        let mut live_memberships = Vec::new();

        for retired_membership in retired_memberships {
            let Some(fabric_definition) = self.fabrics.get(&retired_membership.fabric) else {
                tracing::debug!(
                    fabric = %retired_membership.fabric,
                    guid = %retired_membership.guid,
                    pkey = %retired_membership.pkey,
                    "Skipping retired membership for unconfigured fabric"
                );
                memberships_to_suppress.push(retired_membership);
                continue;
            };
            if !is_pkey_in_managed_range(retired_membership.pkey, fabric_definition) {
                tracing::debug!(
                    fabric = %retired_membership.fabric,
                    guid = %retired_membership.guid,
                    pkey = %retired_membership.pkey,
                    "Skipping retired membership outside managed PKey range"
                );
                memberships_to_suppress.push(retired_membership);
                continue;
            }

            if let Some(duplicate_ownership) = suppress_duplicate_ownership_changes(
                current_fabrics_by_guid,
                machine_ids_by_guid,
                &retired_membership,
                reports,
            ) {
                let skipped = match duplicate_ownership {
                    DuplicateOwnership::Fabric => {
                        IbMonitorPkeyReconciliationSkipped::DuplicateFabricOwnership {
                            fabric: retired_membership.fabric.clone(),
                            guid: retired_membership.guid.clone(),
                            pkey: retired_membership.pkey.to_string(),
                        }
                    }
                    DuplicateOwnership::Machine => {
                        IbMonitorPkeyReconciliationSkipped::DuplicateMachineOwnership {
                            fabric: retired_membership.fabric.clone(),
                            guid: retired_membership.guid.clone(),
                            pkey: retired_membership.pkey.to_string(),
                        }
                    }
                };
                emit(skipped);
                continue;
            }

            // A unique report from another fabric proves that the earlier
            // `Machine` snapshot no longer owns this membership. Missing port
            // data still gets the locked database reread.
            let known_current_fabric_mismatch = current_fabrics_by_guid
                .get(&retired_membership.guid)
                .and_then(|fabric| fabric.as_deref())
                .is_some_and(|fabric| fabric != retired_membership.fabric.as_str());
            let current_state_still_needs_membership = if known_current_fabric_mismatch {
                false
            } else if let Some(Some(machine_id)) = machine_ids_by_guid.get(&retired_membership.guid)
            {
                match self
                    .membership_is_still_needed(*machine_id, &retired_membership)
                    .await
                {
                    Ok(still_needed) => still_needed,
                    Err(error) => {
                        emit(
                            IbMonitorPkeyReconciliationSkipped::MembershipStateLookupFailed {
                                fabric: retired_membership.fabric.clone(),
                                guid: retired_membership.guid.clone(),
                                pkey: retired_membership.pkey.to_string(),
                                machine_id: machine_id.to_string(),
                                error: error.to_string(),
                            },
                        );
                        // The monitor cannot tell whether the `Instance` still
                        // needs this membership. Suppress both its pending bind
                        // and unbind for this pass.
                        memberships_to_suppress.push(retired_membership);
                        continue;
                    }
                }
            } else {
                false
            };
            // Check live reuse before UFM presence. A live membership that UFM
            // has not bound yet must keep its pending bind.
            if current_state_still_needs_membership {
                live_memberships.push(retired_membership);
                continue;
            }

            // Keep the record after UFM no longer reports the membership. It
            // may be needed to remove an older bind that finishes later.
            if !memberships_in_ufm.contains(&retired_membership) {
                memberships_to_suppress.push(retired_membership);
                continue;
            }

            let conn = client_for_fabric(
                self.fabric_manager.as_ref(),
                fabric_clients,
                &retired_membership.fabric,
            )
            .await?;
            let result = conn
                .unbind_ib_ports(
                    retired_membership.pkey.into(),
                    vec![retired_membership.guid.clone()],
                )
                .await;
            UfmGuidPkeyChangeFinished::emit(
                &retired_membership.fabric,
                UfmOperation::UnbindGuidFromPkey,
                &retired_membership.guid,
                retired_membership.pkey,
                &result,
            );
            if result.is_ok() {
                num_changes += 1;
            }
            memberships_to_suppress.push(retired_membership);
        }

        let memberships_to_suppress = memberships_to_suppress.into_iter().collect::<HashSet<_>>();
        let live_memberships = live_memberships.into_iter().collect::<HashSet<_>>();

        // The locked reread is newer than each report's original `Machine`
        // snapshot. Keep a pending bind when the current `Instance` still needs
        // that exact membership. Drop unbinds from the older snapshot for live
        // memberships and for retired memberships already handled above.
        for report in reports {
            report.missing_guid_pkeys.retain(|(fabric, guid, pkey)| {
                !memberships_to_suppress.contains(&IbMembership {
                    fabric: fabric.clone(),
                    pkey: *pkey,
                    guid: guid.clone(),
                })
            });
            report.unexpected_guid_pkeys.retain(|(fabric, guid, pkey)| {
                let membership = IbMembership {
                    fabric: fabric.clone(),
                    pkey: *pkey,
                    guid: guid.clone(),
                };
                !live_memberships.contains(&membership)
                    && !memberships_to_suppress.contains(&membership)
            });
        }

        Ok(num_changes)
    }

    /// `membership_is_still_needed` checks current `Machine` and `Instance`
    /// state after waiting for the `Machine` update used by allocation and
    /// `force-delete` operations. UFM work remains outside this transaction. A
    /// later monitor pass corrects a UFM change that finishes after this check.
    async fn membership_is_still_needed(
        &self,
        machine_id: HostMachineId,
        membership: &IbMembership,
    ) -> IbResult<bool> {
        let mut txn = self
            .db_pool
            .begin()
            .await
            .map_err(|e| DatabaseError::new("begin retired IB membership transaction", e))?;

        let machine_exists = db::machine::find_one(
            txn.as_mut(),
            &machine_id,
            MachineSearchConfig {
                for_update: true,
                ..Default::default()
            },
        )
        .await?
        .is_some();
        if !machine_exists {
            txn.commit()
                .await
                .map_err(|e| DatabaseError::new("commit retired IB membership transaction", e))?;
            return Ok(false);
        }

        // After another transaction finishes its `Machine` update, READ
        // COMMITTED gives this query a new snapshot that includes that change.
        let snapshot = db::managed_host::load_snapshot(
            txn.as_mut(),
            &machine_id,
            LoadSnapshotOptions::default().with_host_health(self.host_health),
        )
        .await?;
        let membership_may_be_reused = snapshot.as_ref().is_some_and(|snapshot| {
            // During `ForceDeletion`, the state controller no longer manages
            // the `Machine`, so later state transitions may never clear a
            // stale membership. Keep it retired even while the old `Instance`
            // configuration remains visible. Other states require the current
            // hardware and fabric observation to identify the exact membership
            // before it can be reused.
            !matches!(snapshot.managed_state, ManagedHostState::ForceDeletion)
                && snapshot
                    .host_snapshot
                    .status
                    .hardware_info
                    .as_ref()
                    .is_some_and(|hardware_info| {
                        hardware_info
                            .infiniband_interfaces
                            .iter()
                            .any(|interface| interface.guid == membership.guid)
                    })
                && snapshot
                    .host_snapshot
                    .status
                    .infiniband_status_observation
                    .as_ref()
                    .into_iter()
                    .flat_map(|observation| &observation.ib_interfaces)
                    .any(|interface| {
                        interface.guid == membership.guid
                            && interface.fabric_id == membership.fabric
                    })
        });
        let mut still_live = false;
        if let Some(instance) = snapshot
            .as_ref()
            .filter(|snapshot| membership_may_be_reused && !snapshot.use_admin_network())
            .and_then(|snapshot| snapshot.instance.as_ref())
            .filter(|instance| instance.deleted.is_none())
        {
            for interface in &instance.config.infiniband.ib_interfaces {
                if interface.guid.as_deref() != Some(membership.guid.as_str()) {
                    continue;
                }
                let pkey = db::ib_partition::find_pkey_by_partition_id(
                    txn.as_mut(),
                    interface.ib_partition_id,
                )
                .await?
                .and_then(|pkey| PartitionKey::try_from(pkey).ok());
                if pkey == Some(membership.pkey) {
                    still_live = true;
                    break;
                }
            }
        }

        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("commit retired IB membership transaction", e))?;

        Ok(still_live)
    }

    async fn get_all_snapshots(
        &self,
        txn: &mut PgConnection,
    ) -> IbResult<HashMap<HostMachineId, ManagedHostStateSnapshot>> {
        let machine_ids = db::machine::find_machine_ids::<HostMachineId>(
            &mut *txn,
            MachineSearchConfig {
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

        db::managed_host::load_by_machine_ids(
            txn,
            &machine_ids,
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: true,
                host_health_config: self.host_health,
            },
        )
        .await
        .map_err(Into::into)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DuplicateOwnership {
    Fabric,
    Machine,
}

/// Suppresses pending bind and unbind changes for a GUID and PKey on every
/// fabric when more than one fabric or `Machine` claims the GUID. Returns the
/// duplicate owner kind so the caller can emit the matching Event.
fn suppress_duplicate_ownership_changes(
    current_fabrics_by_guid: &HashMap<String, Option<String>>,
    machine_ids_by_guid: &HashMap<String, Option<HostMachineId>>,
    membership: &IbMembership,
    reports: &mut [MachineIbStatusEvaluation],
) -> Option<DuplicateOwnership> {
    let duplicate_ownership = if matches!(current_fabrics_by_guid.get(&membership.guid), Some(None))
    {
        DuplicateOwnership::Fabric
    } else if matches!(machine_ids_by_guid.get(&membership.guid), Some(None)) {
        DuplicateOwnership::Machine
    } else {
        return None;
    };

    suppress_membership_changes_across_fabrics(reports, &membership.guid, membership.pkey);
    Some(duplicate_ownership)
}

/// Removes pending bind and unbind changes for a GUID and PKey on every
/// fabric.
fn suppress_membership_changes_across_fabrics(
    reports: &mut [MachineIbStatusEvaluation],
    guid: &str,
    pkey: PartitionKey,
) {
    for report in reports {
        report
            .missing_guid_pkeys
            .retain(|(_, candidate_guid, candidate_pkey)| {
                candidate_guid != guid || *candidate_pkey != pkey
            });
        report
            .unexpected_guid_pkeys
            .retain(|(_, candidate_guid, candidate_pkey)| {
                candidate_guid != guid || *candidate_pkey != pkey
            });
    }
}

/// Loads the health, port, and partition data for a single IB fabric over one
/// shared client connection, recording results in `fabric_data` and `fabric_metrics`.
///
/// Returns the fabric client so later phases of the monitor iteration can reuse
/// it instead of building a new one per operation. Returns `None` when no
/// client could be built. Failures of the individual data loads are recorded in
/// `fabric_metrics.fabric_error` and leave the corresponding `fabric_data`
/// fields unset, exactly like a failure of the load itself.
async fn load_single_fabric_data(
    fabric_manager: &dyn IBFabricManager,
    fabric: &str,
    fabric_definition: &IbFabricDefinition,
    fabric_data: &mut FabricData,
    fabric_metrics: &mut FabricMetrics,
) -> Option<Arc<dyn IBFabric>> {
    fabric_metrics.endpoints = fabric_definition.endpoints.clone();
    fabric_metrics.allow_insecure_fabric_configuration = fabric_manager
        .get_config()
        .allow_insecure_fabric_configuration;

    let conn = match fabric_manager.new_client(fabric).await {
        Ok(conn) => conn,
        Err(e) => {
            emit(IbFabricDataLoadFailed::build_client(
                fabric,
                &fabric_definition.endpoints,
                &e,
            ));
            fabric_metrics.fabric_error = e.to_string();
            return None;
        }
    };

    if let Err(e) = check_ib_fabric(conn.as_ref(), fabric_metrics).await {
        emit(IbFabricDataLoadFailed::health_check(
            fabric,
            &fabric_definition.endpoints,
            &e,
        ));
        fabric_metrics.fabric_error = e.to_string();
        // There's no point in loading other information case the fabric is down
        return Some(conn);
    }

    match get_ports_information(conn.as_ref(), fabric_metrics).await {
        Ok(ports) => {
            fabric_data.ports_by_guid = Some(ports);
        }
        Err(e) => {
            emit(IbFabricDataLoadFailed::load_ports(
                fabric,
                &fabric_definition.endpoints,
                &e,
            ));
            fabric_metrics.fabric_error = e.to_string();
            // There's no point in loading other information case the fabric is down
            return Some(conn);
        }
    }

    match get_partition_information(conn.as_ref(), fabric_metrics).await {
        Ok(partitions) => {
            fabric_data.partitions = Some(partitions);
        }
        Err(e) => {
            emit(IbFabricDataLoadFailed::load_partitions(
                fabric,
                &fabric_definition.endpoints,
                &e,
            ));
            fabric_metrics.fabric_error = e.to_string();
            // There's no point in loading other information case the fabric is down
            return Some(conn);
        }
    }

    // Derive Partitions by GUID
    fabric_data.derive_partitions_by_guid();

    Some(conn)
}

/// Checks the status of a single IB fabric over an established client connection
async fn check_ib_fabric(conn: &dyn IBFabric, metrics: &mut FabricMetrics) -> Result<(), IbError> {
    let version = conn.versions().await?;
    metrics.ufm_version = version.ufm_version;

    let config = conn.get_fabric_config().await?;
    metrics.subnet_prefix = config.subnet_prefix;
    metrics.m_key = config.m_key;
    metrics.sm_key = config.sm_key;
    metrics.sa_key = config.sa_key;
    metrics.m_key_per_port = config.m_key_per_port;

    // Check if any of the expected security settings is not configured
    // TODO: We are not checking whether the default partition is in restricted mode
    metrics.insecure_fabric_configuration = false;
    if parse_num(&metrics.m_key) == Some(0)
        || parse_num(&metrics.sm_key) == Some(1)
        || parse_num(&metrics.sa_key) == Some(1)
        || !metrics.m_key_per_port
    {
        metrics.insecure_fabric_configuration = true;
    }

    // Check if the default partition is in restricted mode
    let default_partition = conn
        .get_ib_network(
            PartitionKey::for_default_partition().into(),
            GetPartitionOptions {
                include_guids_data: true,
                include_qos_conf: true,
            },
        )
        .await?;
    if let Some(membership) = default_partition.membership {
        metrics.default_partition_membership = Some(membership.to_string());
        if membership == IBPortMembership::Full {
            metrics.insecure_fabric_configuration = true;
        }
    }

    Ok(())
}

#[derive(Debug, Default)]
struct FabricData {
    /// Ports by GUID. `None` if port data could not be loaded
    ports_by_guid: Option<HashMap<String, IBPort>>,
    /// Partitions by pkey. `None` if partition data could not be loaded
    partitions: Option<HashMap<u16, IBNetwork>>,
    /// Partitions associated with a single guid
    partition_ids_by_guid: Option<HashMap<String, HashSet<u16>>>,
}

impl FabricData {
    fn derive_partitions_by_guid(&mut self) {
        let Some(partitions) = self.partitions.as_ref() else {
            self.partition_ids_by_guid = None;
            return;
        };

        let mut partitions_by_guid: HashMap<String, HashSet<u16>> = HashMap::new();
        for (pkey, partition) in partitions.iter() {
            let Some(associated_guids) = partition.associated_guids.as_ref() else {
                // We can not correctly calculate partition_ids_by_guid if any partition has
                // incomplete GUID data
                self.partition_ids_by_guid = None;
                return;
            };

            for guid in associated_guids.iter() {
                let guid_partitions = partitions_by_guid.entry(guid.clone()).or_default();
                guid_partitions.insert(*pkey);
            }
        }

        self.partition_ids_by_guid = Some(partitions_by_guid);
    }
}

/// `machine_ids_by_ib_guid` maps hardware GUIDs from the original monitor
/// snapshot to the `Machine` that can be locked and read again. A `None` value
/// means that more than one `Machine` claims the GUID.
fn machine_ids_by_ib_guid(
    snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
) -> HashMap<String, Option<HostMachineId>> {
    unique_machine_ids_by_ib_guid(
        snapshots
            .iter()
            .filter_map(|(machine_id, snapshot)| {
                snapshot
                    .host_snapshot
                    .status
                    .hardware_info
                    .as_ref()
                    .map(|hardware_info| (machine_id, &hardware_info.infiniband_interfaces))
            })
            .flat_map(|(machine_id, interfaces)| {
                interfaces
                    .iter()
                    .map(|interface| (interface.guid.clone(), *machine_id))
            }),
    )
}

/// `unique_machine_ids_by_ib_guid` keeps unique GUID owners and marks duplicate
/// ownership by distinct machines as `None`.
fn unique_machine_ids_by_ib_guid(
    guid_owners: impl IntoIterator<Item = (String, HostMachineId)>,
) -> HashMap<String, Option<HostMachineId>> {
    let mut machine_ids_by_guid = HashMap::new();
    for (guid, machine_id) in guid_owners {
        machine_ids_by_guid
            .entry(guid)
            .and_modify(|current_machine_id: &mut Option<HostMachineId>| {
                if *current_machine_id != Some(machine_id) {
                    *current_machine_id = None;
                }
            })
            .or_insert(Some(machine_id));
    }
    machine_ids_by_guid
}

/// `current_fabrics_by_ib_guid` maps each GUID in the available UFM port data
/// to its current fabric. A `None` value means more than one available fabric
/// reported the same GUID. Fabrics with unavailable port data make no claim.
fn current_fabrics_by_ib_guid(
    data_by_fabric: &HashMap<String, FabricData>,
) -> HashMap<String, Option<String>> {
    let mut fabrics_by_guid = HashMap::new();
    for (fabric, data) in data_by_fabric {
        let Some(ports_by_guid) = data.ports_by_guid.as_ref() else {
            continue;
        };
        for guid in ports_by_guid.keys() {
            fabrics_by_guid
                .entry(guid.clone())
                .and_modify(|current_fabric: &mut Option<String>| {
                    if current_fabric.as_deref() != Some(fabric) {
                        *current_fabric = None;
                    }
                })
                .or_insert_with(|| Some(fabric.clone()));
        }
    }
    fabrics_by_guid
}

/// `memberships_in_fabric_data` builds the exact memberships UFM reported for
/// this monitor pass.
fn memberships_in_fabric_data(
    data_by_fabric: &HashMap<String, FabricData>,
) -> HashSet<IbMembership> {
    let mut memberships = HashSet::new();
    for (fabric, data) in data_by_fabric {
        let Some(partitions_by_guid) = data.partition_ids_by_guid.as_ref() else {
            continue;
        };
        for (guid, pkeys) in partitions_by_guid {
            for pkey in pkeys {
                let Ok(pkey) = PartitionKey::try_from(*pkey) else {
                    continue;
                };
                memberships.insert(IbMembership {
                    fabric: fabric.clone(),
                    pkey,
                    guid: guid.clone(),
                });
            }
        }
    }
    memberships
}

/// Return port information within a single IB fabric
async fn get_ports_information(
    conn: &dyn IBFabric,
    metrics: &mut FabricMetrics,
) -> Result<HashMap<String, IBPort>, IbError> {
    let ports = conn.find_ib_port(None).await?;
    let mut ports_by_state = HashMap::new();
    let mut ports_by_guid = HashMap::new();
    for port in ports.into_iter() {
        let state = match port.state.as_ref() {
            Some(state) => format!("{state:?}"),
            None => "unknown".to_string(),
        };
        *ports_by_state.entry(state).or_default() += 1;
        ports_by_guid.insert(port.guid.clone(), port);
    }
    metrics.ports_by_state = Some(ports_by_state);

    Ok(ports_by_guid)
}

/// Return partitioning information within a single IB fabric
async fn get_partition_information(
    conn: &dyn IBFabric,
    metrics: &mut FabricMetrics,
) -> Result<HashMap<u16, IBNetwork>, IbError> {
    // Due to the UFM bug we need to first get partition IDs and then query
    // each partition individually for additional data
    let partitions = conn
        .get_ib_networks(GetPartitionOptions {
            include_guids_data: false,
            include_qos_conf: true,
        })
        .await?;
    metrics.num_partitions = Some(partitions.len());

    let mut result = HashMap::new();
    for &pkey in partitions.keys() {
        match conn
            .get_ib_network(
                pkey,
                GetPartitionOptions {
                    include_guids_data: true,
                    include_qos_conf: true,
                },
            )
            .await
        {
            Ok(partition) => {
                result.insert(pkey, partition);
            }
            Err(IbError::NotFoundError { .. }) => continue, // Partition might have been deleted
            Err(e) => return Err(e),
        }
    }

    Ok(result)
}

/// Returns the client for a fabric, reusing one from `fabric_clients` when the
/// current iteration already built it. A client built here is retained in
/// `fabric_clients` so subsequent operations on the same fabric reuse it too.
///
/// The memo still earns its keep now that the manager caches built clients
/// across iterations: a hit here skips even the manager's per-call
/// secret-manager read, so each fabric's credentials are read once per
/// iteration. A miss is cheap -- the manager rebuilds a client only when the
/// fabric's credentials or endpoint change.
async fn client_for_fabric(
    fabric_manager: &dyn IBFabricManager,
    fabric_clients: &mut HashMap<String, Arc<dyn IBFabric>>,
    fabric: &str,
) -> Result<Arc<dyn IBFabric>, IbError> {
    if let Some(conn) = fabric_clients.get(fabric) {
        return Ok(conn.clone());
    }

    let conn = fabric_manager.new_client(fabric).await?;
    fabric_clients.insert(fabric.to_string(), conn.clone());
    Ok(conn)
}

/// Applies the GUID<->pkey binding changes that the per-machine status
/// evaluations found to be required. Each fabric is served by a single client
/// from `fabric_clients` for the whole batch, and every UFM call emits its
/// counter-backed Event at the call boundary.
///
/// Returns the number of successfully applied changes.
async fn apply_guid_pkey_changes(
    fabric_manager: &dyn IBFabricManager,
    fabric_clients: &mut HashMap<String, Arc<dyn IBFabric>>,
    fabrics: &HashMap<String, IbFabricDefinition>,
    tenant_partitions: &HashMap<IBPartitionId, IBPartition>,
    partition_ids_by_pkey: &HashMap<PartitionKey, IBPartitionId>,
    reports: Vec<MachineIbStatusEvaluation>,
) -> IbResult<usize> {
    let mut num_changes = 0;

    for report in reports {
        for (fabric, guid, pkey) in report.missing_guid_pkeys {
            let Some(partition_id) = partition_ids_by_pkey.get(&pkey) else {
                emit(IbMonitorPkeyReconciliationSkipped::NoPartitionIdForPkey {
                    pkey: pkey.to_string(),
                });
                continue;
            };
            let Some(partition) = tenant_partitions.get(partition_id) else {
                emit(IbMonitorPkeyReconciliationSkipped::PartitionMissing {
                    pkey: pkey.to_string(),
                });
                continue;
            };

            let conn = client_for_fabric(fabric_manager, fabric_clients, &fabric).await?;
            let result = conn
                .bind_ib_ports(partition.into(), vec![guid.clone()])
                .await;
            UfmGuidPkeyChangeFinished::emit(
                &fabric,
                UfmOperation::BindGuidToPkey,
                &guid,
                pkey,
                &result,
            );
            if result.is_ok() {
                num_changes += 1;
            }
        }

        for (fabric, guid, pkey) in report.unexpected_guid_pkeys {
            // Only unbind pkeys that are within this Carbide's managed range.
            // Pkeys outside the configured range should be left alone.
            // Note: We only enforce expected pkeys for GUIDs configured on the instance.
            // Unconfigured GUIDs with out-of-range pkeys will be ignored.
            let managed_pkey = fabrics
                .get(&fabric)
                .map(|f| is_pkey_in_managed_range(pkey, f))
                .unwrap_or(false);

            if !managed_pkey {
                tracing::debug!(
                    %fabric,
                    %guid,
                    %pkey,
                    "Skipping unbind for pkey outside managed range"
                );
                continue;
            }

            let conn = client_for_fabric(fabric_manager, fabric_clients, &fabric).await?;
            let result = conn.unbind_ib_ports(pkey.into(), vec![guid.clone()]).await;
            UfmGuidPkeyChangeFinished::emit(
                &fabric,
                UfmOperation::UnbindGuidFromPkey,
                &guid,
                pkey,
                &result,
            );
            if result.is_ok() {
                num_changes += 1;
            }
        }
    }

    Ok(num_changes)
}

/// Find all active partitions in order to determine pkeys
async fn get_tenant_partitions(
    txn: &mut PgConnection,
) -> Result<HashMap<IBPartitionId, IBPartition>, IbError> {
    let partition_ids = db::ib_partition::find_ids(
        &mut *txn,
        IbPartitionSearchFilter {
            tenant_org_id: None,
            name: None,
        },
    )
    .await?;

    const PAGE_SIZE: usize = 100;
    let mut result = HashMap::new();
    let mut offset = 0;
    while offset != partition_ids.len() {
        let page_size = PAGE_SIZE.min(partition_ids.len() - offset);
        let next_ids = &partition_ids[offset..offset + page_size];
        let partition_data = db::ib_partition::find_by(
            &mut *txn,
            db::ObjectColumnFilter::List(db::ib_partition::IdColumn, next_ids),
        )
        .await?;

        for partition in partition_data {
            result.insert(partition.id, partition);
        }

        offset += page_size;
    }

    Ok(result)
}

/// `MachineIbStatusEvaluation` holds missing and unexpected PKey changes,
/// unknown PKey observations, down ports, and expected memberships that
/// retirement reconciliation must protect from stale changes.
#[derive(Debug, Clone, Default)]
struct MachineIbStatusEvaluation {
    missing_guid_pkeys: Vec<(String, String, PartitionKey)>,
    unexpected_guid_pkeys: Vec<(String, String, PartitionKey)>,
    unknown_guid_pkeys: Vec<(String, String, PartitionKey)>,
    down_port_guids: Vec<String>,
    needed_memberships: Vec<IbMembership>,
}

async fn record_machine_infiniband_status_observation(
    db_pool: &PgPool,
    mh_snapshot: &mut ManagedHostStateSnapshot,
    tenant_partitions: &HashMap<IBPartitionId, IBPartition>,
    tenant_partition_ids_by_pkey: &HashMap<PartitionKey, IBPartitionId>,
    data_by_fabric: &HashMap<String, FabricData>,
    sku_inactive_cache: &SkuInactiveDevicesCache,
    metrics: &mut IbFabricMonitorMetrics,
) -> Result<MachineIbStatusEvaluation, IbError> {
    let mut result = MachineIbStatusEvaluation::default();

    if mh_snapshot.host_snapshot.status.hardware_info.is_none() {
        // Skip status update while hardware info is not available
        *metrics
            .num_machines_by_port_states
            .entry((0, 0))
            .or_default() += 1;
        *metrics
            .num_machines_by_ports_with_partitions
            .entry(0)
            .or_default() += 1;
        return Ok(result);
    }

    let machine_id = &mh_snapshot.host_snapshot.id;
    let ib_hw_info = &mh_snapshot
        .host_snapshot
        .status
        .hardware_info
        .as_ref()
        .unwrap()
        .infiniband_interfaces;

    // Determine what the expected configuration for each port on the host is
    // That is derived by the instance configuration
    // If there is no instance, then each interface should not have an associated partition
    let expected_ib_config = mh_snapshot
        .instance
        .as_ref()
        .map(|instance| &instance.config.infiniband);
    let mut expected_pkeys = HashMap::new();

    // If we are on the tenant network, then the pkey configuration is the
    // instances network configuration.
    //
    // If not (e.g. during instance termination, OR any zero-DPU host where
    // no tenant overlay exists in the first place), then there are no pkeys
    // expected on any interface.
    let use_tenant_network = !mh_snapshot.use_admin_network();
    if use_tenant_network && let Some(expected_ib_config) = expected_ib_config {
        for iface in expected_ib_config.ib_interfaces.iter() {
            let Some(guid) = iface.guid.as_ref() else {
                continue;
            };
            let Some(partition_data) = tenant_partitions.get(&iface.ib_partition_id) else {
                continue;
            };
            let Some(expected_pkey) = partition_data.status.as_ref().and_then(|s| s.pkey) else {
                continue;
            };
            expected_pkeys.insert(guid.clone(), expected_pkey);
        }
    }

    // SKU defines which ports are intentionally disconnected/inactive by hardware design
    let expected_inactive_devices = get_expected_inactive_devices_from_cache(
        sku_inactive_cache,
        mh_snapshot.host_snapshot.config.hw_sku.as_deref(),
    );

    // Use GUID as secondary key for stable ordering when slots are identical
    let mut sorted_ib_interfaces = ib_hw_info.to_vec();
    sorted_ib_interfaces.sort_by_key(|iface| {
        (
            iface
                .pci_properties
                .as_ref()
                .and_then(|p| p.slot.clone())
                .unwrap_or_default(),
            iface.guid.clone(), // Stable tie-breaker
        )
    });

    let guid_to_index: HashMap<String, u32> = sorted_ib_interfaces
        .into_iter()
        .enumerate()
        .map(|(idx, iface)| (iface.guid, idx as u32))
        .collect();

    // Fallback for port-down alerting when no SKU is assigned:
    // use the instance's IB config GUIDs to determine which ports the workload needs
    let instance_guids: Option<HashSet<String>> = mh_snapshot.instance.as_ref().map(|instance| {
        instance
            .config
            .infiniband
            .ib_interfaces
            .iter()
            .filter_map(|iface| iface.guid.as_ref().map(|g| g.to_lowercase()))
            .collect()
    });

    // The list of GUIDs that are part of this Machine
    let mut guids: Vec<String> = Vec::new();
    for ib_interface in ib_hw_info.iter() {
        guids.push(ib_interface.guid.clone());
    }

    let mut prev = mh_snapshot
        .host_snapshot
        .status
        .infiniband_status_observation
        .clone()
        .unwrap_or_default();

    let mut ib_interfaces_status: Vec<MachineIbInterfaceStatusObservation> =
        Vec::with_capacity(guids.len());

    let mut active_ports = 0;
    let mut ports_with_partitions = 0;

    for guid in guids.iter() {
        // Search for the GUID in all fabrics. Record the fabric where we found it, plus the actual data
        // Note: This only works since GUIDs are globally unique
        let mut found_port_data = None;
        for (fabric_id, fabric_data) in data_by_fabric.iter() {
            if let Some(port_data) = fabric_data
                .ports_by_guid
                .as_ref()
                .and_then(|ports_by_guid| ports_by_guid.get(guid))
            {
                found_port_data = Some((fabric_id, fabric_data, port_data));
                break;
            }
        }

        let (fabric_id, lid, associated_pkeys, associated_partition_ids) = match found_port_data {
            Some((fabric_id, fabric_data, port_data)) => {
                if let Some(expected_pkey) = expected_pkeys.get(guid) {
                    result.needed_memberships.push(IbMembership {
                        fabric: fabric_id.to_string(),
                        pkey: *expected_pkey,
                        guid: guid.to_string(),
                    });
                }

                // Look up the found port's associated PKeys. If UFM did not
                // return partition data, preserve that uncertainty as `None`
                // instead of treating it as an empty membership list.
                let associated_pkeys = match fabric_data.partition_ids_by_guid.as_ref() {
                    Some(partition_ids_by_guid) => match partition_ids_by_guid.get(guid) {
                        Some(partition_ids) => {
                            let mut ids = HashSet::new();
                            for id in partition_ids {
                                if let Ok(id) = PartitionKey::try_from(*id) {
                                    ids.insert(id);
                                }
                            }
                            Some(ids)
                        }
                        None => Some(HashSet::new()),
                    },
                    None => None,
                };

                let associated_partition_ids = match associated_pkeys.as_ref() {
                    Some(pkeys) => {
                        if !pkeys.is_empty() {
                            ports_with_partitions += 1;
                        }

                        // Translate associated_pkeys into associated_partition_ids
                        let mut associated_partition_ids = HashSet::new();
                        for pkey in pkeys {
                            match tenant_partition_ids_by_pkey.get(pkey) {
                                Some(partition_id) => {
                                    associated_partition_ids.insert(*partition_id);
                                }
                                None => {
                                    result.unknown_guid_pkeys.push((
                                        fabric_id.to_string(),
                                        guid.to_string(),
                                        *pkey,
                                    ));
                                }
                            }
                        }

                        // Determine which keys need to get added or removed
                        match expected_pkeys.get(guid) {
                            Some(expected_pkey) => {
                                // GUID should be associated with `expected_pkey`
                                if !pkeys.contains(expected_pkey) {
                                    result.missing_guid_pkeys.push((
                                        fabric_id.to_string(),
                                        guid.to_string(),
                                        *expected_pkey,
                                    ));
                                }
                                // Everything else is unexpected
                                for pkey in pkeys {
                                    if pkey != expected_pkey {
                                        result.unexpected_guid_pkeys.push((
                                            fabric_id.to_string(),
                                            guid.to_string(),
                                            *pkey,
                                        ));
                                    }
                                }
                            }
                            None => {
                                // All GUIDs are unexpected
                                for pkey in pkeys {
                                    result.unexpected_guid_pkeys.push((
                                        fabric_id.to_string(),
                                        guid.to_string(),
                                        *pkey,
                                    ));
                                }
                            }
                        }

                        Some(associated_partition_ids)
                    }
                    None => {
                        // We don't know what is associated, therefore we can't make
                        // a great decision about whether pkeys are missing or unexpected
                        None
                    }
                };

                let (lid, is_down) = if port_data.state == Some(IBPortState::Active) {
                    active_ports += 1;
                    (port_data.lid as u16, false)
                } else {
                    // Port is not active - check if we should track it as down
                    let should_track = should_track_port_as_down(
                        guid,
                        &guid_to_index,
                        expected_inactive_devices.as_ref(),
                        instance_guids.as_ref(),
                    );
                    if should_track {
                        result.down_port_guids.push(guid.clone());
                    }
                    (0xffff_u16, true)
                };

                if is_down {
                    tracing::debug!(
                        machine_id = %machine_id,
                        guid = %guid,
                        port_state = ?port_data.state,
                        "IB port is not active"
                    );
                }

                (fabric_id, lid, associated_pkeys, associated_partition_ids)
            }
            None => {
                // The port was not found on UFM. In this case we don't even try
                // to look up associated pkeys
                // Check if we should track it as down
                let should_track = should_track_port_as_down(
                    guid,
                    &guid_to_index,
                    expected_inactive_devices.as_ref(),
                    instance_guids.as_ref(),
                );
                if should_track {
                    result.down_port_guids.push(guid.clone());
                }

                // TODO: We should differentiate between "Can not communicate with fabric"
                // and "UFM definitely did not know about this GUID".
                (&String::new(), 0xffff_u16, None, None)
            }
        };

        ib_interfaces_status.push(MachineIbInterfaceStatusObservation {
            guid: guid.clone(),
            lid,
            fabric_id: fabric_id.to_string(),
            associated_pkeys,
            associated_partition_ids,
        });
    }

    *metrics
        .num_machines_by_port_states
        .entry((guids.len(), active_ports))
        .or_default() += 1;
    *metrics
        .num_machines_by_ports_with_partitions
        .entry(ports_with_partitions)
        .or_default() += 1;

    if !result.missing_guid_pkeys.is_empty() {
        metrics.num_machines_with_missing_pkeys += 1;
        tracing::warn!(
            machine_id = %machine_id,
            missing_guid_pkeys = ?result.missing_guid_pkeys,
            "Machine is missing pkeys on UFM",
        );
    }
    if !result.unexpected_guid_pkeys.is_empty() {
        metrics.num_machines_with_unexpected_pkeys += 1;
        tracing::warn!(
            machine_id = %machine_id,
            unexpected_guid_pkeys = ?result.unexpected_guid_pkeys,
            "Machine has unexpected registered pkeys on UFM",
        );
    }
    if !result.unknown_guid_pkeys.is_empty() {
        metrics.num_machines_with_unknown_pkeys += 1;
        tracing::warn!(
            machine_id = %machine_id,
            unknown_guid_pkeys = ?result.unknown_guid_pkeys,
            "Machine has registered pkeys on UFM that do not map to IB PartitionIDs",
        );
    }

    let has_existing_ib_port_down_alert = mh_snapshot
        .aggregate_health
        .alerts
        .iter()
        .any(|alert| alert.id.as_str() == "IbPortDown");

    if !result.down_port_guids.is_empty() {
        tracing::warn!(
            machine_id = %machine_id,
            down_ports = ?result.down_port_guids,
            total_port_count = guids.len(),
            "IB port(s) detected as down - setting PreventAllocations alert"
        );
        set_ib_port_down_alert(db_pool, machine_id, &result.down_port_guids, guids.len()).await?;
    } else if has_existing_ib_port_down_alert {
        tracing::info!(
            machine_id = %machine_id,
            "All IB ports are now active - clearing IbPortDown alert"
        );
        clear_ib_port_down_alert(db_pool, machine_id).await?;
    }

    let cur = MachineInfinibandStatusObservation {
        observed_at: Utc::now(),
        ib_interfaces: ib_interfaces_status,
    };

    // This allows to update a record ony in case of any changes.
    prev.observed_at = cur.observed_at;

    if let Some(alert) = mh_snapshot
        .aggregate_health
        .alerts
        .iter()
        .find(|alert| alert.id.as_str() == "IbCleanupPending")
    {
        let guids = parse_guids_from_alert(&alert.message);
        tracing::info!(
            machine_id = %machine_id,
            guids = ?guids,
            "Processing IbCleanupPending alert - checking if GUIDs are cleared from UFM"
        );
        let mut all_cleared = true;
        for guid in &guids {
            if result.missing_guid_pkeys.iter().any(|(_, g, _)| g == guid)
                || result
                    .unexpected_guid_pkeys
                    .iter()
                    .any(|(_, g, _)| g == guid)
                || result.unknown_guid_pkeys.iter().any(|(_, g, _)| g == guid)
            {
                all_cleared = false;
            }
        }
        if all_cleared {
            tracing::info!(
                machine_id = %machine_id,
                guids = ?guids,
                "All GUIDs cleared from UFM - clearing IbCleanupPending alert"
            );
            clear_ib_cleanup_alert(db_pool, machine_id).await?;
        } else {
            tracing::debug!(
                machine_id = %machine_id,
                "Not all GUIDs cleared yet - alert remains"
            );
        }
    }

    // Update Machine infiniband status in case any changes only
    // Vector of statuses is based on guids vector that is formed
    // from hardware_info.infiniband_interfaces[]
    // So it guarantees stable order between function calls
    if prev != cur {
        let mut conn = db_pool
            .acquire()
            .await
            .map_err(|e| DatabaseError::new("acquire connection", e))?;
        db::machine::update_infiniband_status_observation(&mut conn, machine_id, &cur).await?;
        metrics.num_machine_ib_status_updates += 1;
        mh_snapshot
            .host_snapshot
            .status
            .infiniband_status_observation = Some(cur);
    }

    Ok(result)
}

/// Clear the IbCleanupPending alert
async fn clear_ib_cleanup_alert(
    db_pool: &PgPool,
    machine_id: &HostMachineId,
) -> Result<(), IbError> {
    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| DatabaseError::new("acquire connection", e))?;

    db::machine::remove_health_report(
        &mut conn,
        machine_id,
        HealthReportApplyMode::Merge,
        "ib-cleanup-validation",
    )
    .await
    .map_err(|e| IbError::internal(format!("Failed to clear IB cleanup alert: {e}")))?;

    Ok(())
}

const IB_PORT_DOWN_OVERRIDE_SOURCE: &str = "ib-port-down-monitor";

async fn set_ib_port_down_alert(
    db_pool: &PgPool,
    machine_id: &HostMachineId,
    down_port_guids: &[String],
    total_ports: usize,
) -> Result<(), IbError> {
    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| DatabaseError::new("acquire connection", e))?;

    let alert =
        health_report::HealthProbeAlert::ib_port_down(down_port_guids.to_vec(), total_ports);
    let health_report = health_report::HealthReport {
        source: IB_PORT_DOWN_OVERRIDE_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(Utc::now()),
        successes: vec![],
        alerts: vec![alert],
    };

    db::machine::insert_health_report(
        &mut conn,
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
        false, // overwrite existing
    )
    .await
    .map_err(|e| IbError::internal(format!("Failed to set IB port down alert: {e}")))?;

    Ok(())
}

async fn clear_ib_port_down_alert(
    db_pool: &PgPool,
    machine_id: &HostMachineId,
) -> Result<(), IbError> {
    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| DatabaseError::new("acquire connection", e))?;

    db::machine::remove_health_report(
        &mut conn,
        machine_id,
        HealthReportApplyMode::Merge,
        IB_PORT_DOWN_OVERRIDE_SOURCE,
    )
    .await
    .map_err(|e| IbError::internal(format!("Failed to clear IB port down alert: {e}")))?;

    Ok(())
}

/// Should a down port be tracked for alerting?
/// Precedence:
/// 1. SKU exists: track if the port is not in `inactive_devices` (hardware truth)
/// 2. No SKU, but instance has IB config: track if the port is in the instance's IB config (workload truth)
/// 3. Neither SKU nor instance IB config: don't track
fn should_track_port_as_down(
    guid: &str,
    guid_to_index: &HashMap<String, u32>,
    expected_inactive_devices: Option<&HashSet<u32>>,
    instance_guids: Option<&HashSet<String>>,
) -> bool {
    if let Some(expected_inactive) = expected_inactive_devices {
        let port_index = guid_to_index.get(guid).copied().unwrap_or(u32::MAX);
        return !expected_inactive.contains(&port_index);
    }

    if let Some(guids) = instance_guids {
        return guids.contains(&guid.to_lowercase());
    }

    false
}

async fn preload_sku_inactive_devices(
    db_pool: &PgPool,
    snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
) -> Result<SkuInactiveDevicesCache, IbError> {
    let sku_ids: Vec<&str> = snapshots
        .values()
        .filter_map(|snap| snap.host_snapshot.config.hw_sku.as_deref())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if sku_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut conn = db_pool
        .acquire()
        .await
        .map_err(|e| DatabaseError::new("acquire connection", e))?;

    let skus = db::sku::find(&mut conn, &sku_ids)
        .await
        .map_err(|e| IbError::internal(format!("Failed to load SKUs: {e}")))?;

    let mut cache: SkuInactiveDevicesCache = HashMap::new();
    for sku in skus {
        let inactive = if sku.components.infiniband_devices.is_empty() {
            // SKU has no IB devices - skip monitoring for machines with this SKU
            None
        } else {
            Some(
                sku.components
                    .infiniband_devices
                    .into_iter()
                    .flat_map(|ib_dev| ib_dev.inactive_devices)
                    .collect(),
            )
        };
        cache.insert(sku.id, inactive);
    }

    Ok(cache)
}

/// Returns:
/// - None if no SKU assigned, SKU not in cache, or SKU has no IB devices
///   (in these cases, IB port down monitoring should be skipped)
/// - Some(set) with the set of port indices expected to be inactive per SKU
fn get_expected_inactive_devices_from_cache(
    sku_cache: &SkuInactiveDevicesCache,
    hw_sku: Option<&str>,
) -> Option<HashSet<u32>> {
    let sku_id = hw_sku?;
    sku_cache.get(sku_id).cloned().flatten()
}

/// Parse GUIDs from IbCleanupPending alert message
/// Returns Vec<String> of GUIDs
/// Format: "IB port cleanup pending - IB Monitor will unbind. GUIDs: X; Y; Z"
fn parse_guids_from_alert(message: &str) -> Vec<String> {
    let Some(guids_str) =
        message.strip_prefix("IB port cleanup pending - IB Monitor will unbind. GUIDs: ")
    else {
        return Vec::new();
    };

    guids_str
        .split("; ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parses a u64 string in hexadecimal or decimal format
fn parse_num(input: &str) -> Option<u64> {
    match input.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => input.parse().ok(),
    }
}

/// Checks if a pkey is within the managed pkey ranges for a fabric.
/// Returns true if the pkey falls within any of the configured ranges.
fn is_pkey_in_managed_range(pkey: PartitionKey, fabric_definition: &IbFabricDefinition) -> bool {
    let pkey_value = u16::from(pkey) as u64;
    fabric_definition
        .pkeys
        .iter()
        .filter_map(|r| Some(parse_num(&r.start)?..parse_num(&r.end)?))
        .any(|range| range.contains(&pkey_value))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};

    use super::*;

    /// `fabric_data_with_ports` builds a complete UFM port inventory for the
    /// fabric ownership tests.
    fn fabric_data_with_ports(guids: &[&str]) -> FabricData {
        FabricData {
            ports_by_guid: Some(
                guids
                    .iter()
                    .enumerate()
                    .map(|(index, guid)| {
                        (
                            (*guid).to_string(),
                            IBPort {
                                name: (*guid).to_string(),
                                guid: (*guid).to_string(),
                                lid: index as i32 + 1,
                                state: Some(IBPortState::Active),
                            },
                        )
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    /// `current_fabric_mapping_uses_available_unique_port_ownership` verifies
    /// that unavailable fabric data does not erase a known GUID location.
    #[test]
    fn current_fabric_mapping_uses_available_unique_port_ownership() {
        let guid = "moved-guid";
        value_scenarios!(
            run = |data_by_fabric: HashMap<String, FabricData>| {
                current_fabrics_by_ib_guid(&data_by_fabric)
                    .get(guid)
                    .cloned()
            };
            "one available fabric claims the GUID" {
                HashMap::from([
                    (
                        "fabric-a".to_string(),
                        FabricData {
                            ports_by_guid: Some(HashMap::new()),
                            ..Default::default()
                        },
                    ),
                    ("fabric-b".to_string(), fabric_data_with_ports(&[guid])),
                ]) => Some(Some("fabric-b".to_string())),
            }

            "two available fabrics claim the GUID" {
                HashMap::from([
                    ("fabric-a".to_string(), fabric_data_with_ports(&[guid])),
                    ("fabric-b".to_string(), fabric_data_with_ports(&[guid])),
                ]) => Some(None),
            }

            "retired fabric data is unavailable" {
                HashMap::from([
                    ("fabric-a".to_string(), FabricData::default()),
                    ("fabric-b".to_string(), fabric_data_with_ports(&[guid])),
                ]) => Some(Some("fabric-b".to_string())),
            }

            "unrelated fabric data is unavailable" {
                HashMap::from([
                    ("fabric-a".to_string(), fabric_data_with_ports(&[guid])),
                    ("fabric-b".to_string(), FabricData::default()),
                ]) => Some(Some("fabric-a".to_string())),
            }

            "no available fabric claims the GUID" {
                HashMap::from([(
                    "fabric-a".to_string(),
                    FabricData {
                        ports_by_guid: None,
                        partition_ids_by_guid: Some(HashMap::from([(
                            guid.to_string(),
                            HashSet::from([50]),
                        )])),
                        ..Default::default()
                    },
                )]) => None,
            }
        );
    }

    /// `duplicate_ownership_suppression_applies_to_every_reported_fabric`
    /// verifies that both production duplicate ownership branches suppress
    /// the same GUID and PKey on a fabric other than the retired membership.
    #[test]
    fn duplicate_ownership_suppression_applies_to_every_reported_fabric() {
        let pkey = PartitionKey::try_from(0x101).expect("valid PKey");
        let other_pkey = PartitionKey::try_from(0x102).expect("valid PKey");
        let duplicate_guid = "duplicate-guid";
        let retained_changes = vec![
            (
                "fabric-b".to_string(),
                duplicate_guid.to_string(),
                other_pkey,
            ),
            ("fabric-b".to_string(), "other-guid".to_string(), pkey),
        ];
        let unchanged_missing = [
            vec![("fabric-b".to_string(), duplicate_guid.to_string(), pkey)],
            retained_changes.clone(),
        ]
        .concat();
        let unchanged_unexpected = [
            vec![
                ("fabric-a".to_string(), duplicate_guid.to_string(), pkey),
                ("fabric-b".to_string(), duplicate_guid.to_string(), pkey),
            ],
            retained_changes.clone(),
        ]
        .concat();
        value_scenarios!(
            run = |(current_fabrics_by_guid, machine_ids_by_guid): (
                HashMap<String, Option<String>>,
                HashMap<String, Option<HostMachineId>>,
            )| {
                let mut reports = vec![MachineIbStatusEvaluation {
                    missing_guid_pkeys: unchanged_missing.clone(),
                    unexpected_guid_pkeys: unchanged_unexpected.clone(),
                    ..Default::default()
                }];
                let duplicate_ownership = suppress_duplicate_ownership_changes(
                    &current_fabrics_by_guid,
                    &machine_ids_by_guid,
                    &IbMembership {
                        fabric: "fabric-a".to_string(),
                        pkey,
                        guid: duplicate_guid.to_string(),
                    },
                    &mut reports,
                );
                (
                    duplicate_ownership,
                    reports[0].missing_guid_pkeys.clone(),
                    reports[0].unexpected_guid_pkeys.clone(),
                )
            };
            "duplicate fabric ownership" {
                (
                    HashMap::from([(duplicate_guid.to_string(), None)]),
                    HashMap::new(),
                ) => (
                    Some(DuplicateOwnership::Fabric),
                    retained_changes.clone(),
                    retained_changes.clone(),
                ),
            }
            "duplicate Machine ownership" {
                (
                    HashMap::from([(
                        duplicate_guid.to_string(),
                        Some("fabric-b".to_string()),
                    )]),
                    HashMap::from([(duplicate_guid.to_string(), None)]),
                ) => (
                    Some(DuplicateOwnership::Machine),
                    retained_changes.clone(),
                    retained_changes.clone(),
                ),
            }
            "fabric ownership takes precedence when both are duplicate" {
                (
                    HashMap::from([(duplicate_guid.to_string(), None)]),
                    HashMap::from([(duplicate_guid.to_string(), None)]),
                ) => (
                    Some(DuplicateOwnership::Fabric),
                    retained_changes.clone(),
                    retained_changes,
                ),
            }
            "unique ownership leaves changes alone" {
                (
                    HashMap::from([(
                        duplicate_guid.to_string(),
                        Some("fabric-b".to_string()),
                    )]),
                    HashMap::new(),
                ) => (
                    None,
                    unchanged_missing.clone(),
                    unchanged_unexpected.clone(),
                ),
            }
        );
    }

    /// `machine_guid_mapping_rejects_duplicate_owners` verifies that a repeated
    /// claim from one `Machine` stays unique while a second owner prevents
    /// either one from being selected.
    #[test]
    fn machine_guid_mapping_rejects_duplicate_owners() {
        let first = MachineId::new(MachineIdSource::Tpm, [1; 32], MachineType::Host)
            .try_into()
            .unwrap();
        let second = MachineId::new(MachineIdSource::Tpm, [2; 32], MachineType::Host)
            .try_into()
            .unwrap();
        value_scenarios!(
            run = |machine_ids: Vec<HostMachineId>| {
                unique_machine_ids_by_ib_guid(
                    machine_ids
                        .into_iter()
                        .map(|machine_id| ("guid".to_string(), machine_id)),
                )
                .get("guid")
                .copied()
            };
            "one Machine claims the GUID" {
                vec![first] => Some(Some(first)),
            }

            "one Machine repeats its claim" {
                vec![first, first] => Some(Some(first)),
            }

            "two Machines claim the GUID" {
                vec![first, second] => Some(None),
            }
        );
    }

    #[test]
    fn parses_numbers() {
        value_scenarios!(parse_num:
            "hex" {
                "0x0000000000000000" => Some(0),
                "0x0000000000000001" => Some(1),
                "0x00" => Some(0),
                "0x01" => Some(1),
                "0xff" => Some(255),
            }

            "decimal" {
                "0" => Some(0),
                "1" => Some(1),
                "2303" => Some(2303),
            }

            "invalid" {
                "" => None,
                "0x" => None,
                "not-a-number" => None,
            }
        );
    }

    #[test]
    fn parses_cleanup_alert_guids() {
        value_scenarios!(parse_guids_from_alert:
            "valid alerts" {
                "IB port cleanup pending - IB Monitor will unbind. GUIDs: 946dae03006104f8" => vec![
                    "946dae03006104f8".to_string(),
                ],
                "IB port cleanup pending - IB Monitor will unbind. GUIDs: 946dae03006104f8; abc123def4567890; fedcba9876543210" => vec![
                    "946dae03006104f8".to_string(),
                    "abc123def4567890".to_string(),
                    "fedcba9876543210".to_string(),
                ],
                "IB port cleanup pending - IB Monitor will unbind. GUIDs:  abc  ;  def  " => vec![
                    "abc".to_string(),
                    "def".to_string(),
                ],
            }

            "ignored messages" {
                "Wrong prefix message" => Vec::<String>::new(),
                "IB port cleanup pending - IB Monitor will unbind. GUIDs: " => Vec::<String>::new(),
            }
        );
    }

    // ============================================================
    // Unit Tests for HealthProbeAlert::ib_port_down
    // ============================================================

    // What a built `ib_port_down` alert is expected to expose. The alert type
    // isn't PartialEq and its message is free-form, so the table projects each
    // alert onto these checked properties -- the id, that the message names the
    // "<down> of <total>" count and every down GUID, that the alert prevents
    // allocations, and that the tenant message is present and counts the ports.
    #[derive(Debug, PartialEq)]
    struct PortDownAlertView {
        id: String,
        message_names_count: bool,
        message_names_all_guids: bool,
        prevents_allocations: bool,
        tenant_message_counts_ports: bool,
    }

    // One row of inputs to `ib_port_down`: the down GUIDs and the host's total
    // port count.
    struct PortDownAlert {
        down_guids: Vec<String>,
        total_ports: usize,
    }

    #[test]
    fn ib_port_down_alert_reports_down_ports() {
        fn view(input: PortDownAlert) -> PortDownAlertView {
            let alert = health_report::HealthProbeAlert::ib_port_down(
                input.down_guids.clone(),
                input.total_ports,
            );
            let count_phrase = format!("{} of {}", input.down_guids.len(), input.total_ports);
            PortDownAlertView {
                id: alert.id.as_str().to_string(),
                message_names_count: alert.message.contains(&count_phrase),
                message_names_all_guids: input
                    .down_guids
                    .iter()
                    .all(|guid| alert.message.contains(guid)),
                prevents_allocations: alert
                    .classifications
                    .contains(&health_report::HealthAlertClassification::prevent_allocations()),
                tenant_message_counts_ports: alert
                    .tenant_message
                    .as_ref()
                    .is_some_and(|m| m.contains(&format!("{} port(s)", input.down_guids.len()))),
            }
        }

        value_scenarios!(
            run = view;
            "single down port" {
                PortDownAlert {
                    down_guids: vec!["946dae03006104f8".to_string()],
                    total_ports: 8,
                } => PortDownAlertView {
                    id: "IbPortDown".to_string(),
                    message_names_count: true,
                    message_names_all_guids: true,
                    prevents_allocations: true,
                    tenant_message_counts_ports: true,
                },
            }

            "multiple down ports" {
                PortDownAlert {
                    down_guids: vec![
                        "946dae03006104f8".to_string(),
                        "abc123def4567890".to_string(),
                    ],
                    total_ports: 8,
                } => PortDownAlertView {
                    id: "IbPortDown".to_string(),
                    message_names_count: true,
                    message_names_all_guids: true,
                    prevents_allocations: true,
                    tenant_message_counts_ports: true,
                },
            }
        );
    }

    // ============================================================
    // Unit Tests for is_pkey_in_managed_range
    // ============================================================

    fn make_fabric_definition(ranges: Vec<(&str, &str)>) -> IbFabricDefinition {
        use model::resource_pool::define::Range;
        IbFabricDefinition {
            endpoints: vec![],
            pkeys: ranges
                .into_iter()
                .map(|(start, end)| Range {
                    start: start.to_string(),
                    end: end.to_string(),
                    auto_assign: true,
                })
                .collect(),
        }
    }

    #[test]
    fn checks_managed_pkey_ranges() {
        value_scenarios!(
            run = |(pkey, ranges): (u16, Vec<(&str, &str)>)| {
                let fabric = make_fabric_definition(ranges);
                let pkey = PartitionKey::try_from(pkey).unwrap();
                is_pkey_in_managed_range(pkey, &fabric)
            };
            "decimal range" {
                (256, vec![("256", "2303")]) => true,
                (2302, vec![("256", "2303")]) => true,
                (1280, vec![("256", "2303")]) => true,
                (255, vec![("256", "2303")]) => false,
                (2303, vec![("256", "2303")]) => false,
                (0x5000, vec![("256", "2303")]) => false,
            }

            "hex range" {
                (0x100, vec![("0x100", "0x8FF")]) => true,
                (0x8FE, vec![("0x100", "0x8FF")]) => true,
                (0x8FF, vec![("0x100", "0x8FF")]) => false,
            }

            "multiple ranges" {
                (150, vec![("100", "200"), ("500", "600")]) => true,
                (550, vec![("100", "200"), ("500", "600")]) => true,
                (300, vec![("100", "200"), ("500", "600")]) => false,
            }

            "empty or invalid ranges" {
                (256, vec![]) => false,
                (256, vec![("not-a-number", "300")]) => false,
                (256, vec![("100", "not-a-number")]) => false,
            }
        );
    }

    // ============================================================
    // Unit Tests for should_track_port_as_down
    // ============================================================

    // One row for `should_track_port_as_down`: the GUID under test, the
    // GUID->index map of the host's ports, and the two optional precedence
    // inputs -- the SKU's expected-inactive port indices and the instance's
    // configured GUIDs. Owned here so the closure can hand out the borrows the
    // function takes.
    struct TrackPortDown {
        guid: &'static str,
        guid_to_index: &'static [(&'static str, u32)],
        expected_inactive: Option<&'static [u32]>,
        instance_guids: Option<&'static [&'static str]>,
    }

    #[test]
    fn should_track_port_as_down_follows_sku_then_instance_precedence() {
        fn track(row: TrackPortDown) -> bool {
            let guid_to_index: HashMap<String, u32> = row
                .guid_to_index
                .iter()
                .map(|(g, i)| (g.to_string(), *i))
                .collect();
            let expected_inactive: Option<HashSet<u32>> = row
                .expected_inactive
                .map(|indices| indices.iter().copied().collect());
            let instance_guids: Option<HashSet<String>> = row
                .instance_guids
                .map(|guids| guids.iter().map(|g| g.to_string()).collect());

            should_track_port_as_down(
                row.guid,
                &guid_to_index,
                expected_inactive.as_ref(),
                instance_guids.as_ref(),
            )
        }

        value_scenarios!(
            run = track;
            // --- SKU takes precedence when present ---
            "SKU present, port not in inactive set -> track" {
                TrackPortDown {
                    guid: "guid1",
                    guid_to_index: &[("guid1", 0), ("guid2", 1)],
                    expected_inactive: Some(&[2, 3]),
                    instance_guids: None,
                } => true,
            }

            "SKU present, port in inactive set -> do not track" {
                TrackPortDown {
                    guid: "guid3",
                    guid_to_index: &[("guid1", 0), ("guid2", 1), ("guid3", 2)],
                    expected_inactive: Some(&[2]),
                    instance_guids: None,
                } => false,
            }

            "SKU says up overrides instance not using it -> track" {
                TrackPortDown {
                    guid: "guid3",
                    guid_to_index: &[("guid1", 0), ("guid2", 1), ("guid3", 2)],
                    expected_inactive: Some(&[]),
                    instance_guids: Some(&["guid1", "guid2"]),
                } => true,
            }

            "SKU says inactive overrides instance using it -> do not track" {
                TrackPortDown {
                    guid: "guid2",
                    guid_to_index: &[("guid1", 0), ("guid2", 1)],
                    expected_inactive: Some(&[1]),
                    instance_guids: Some(&["guid1", "guid2"]),
                } => false,
            }

            "all ports inactive by SKU, guid1 -> do not track" {
                TrackPortDown {
                    guid: "guid1",
                    guid_to_index: &[("guid1", 0), ("guid2", 1)],
                    expected_inactive: Some(&[0, 1]),
                    instance_guids: None,
                } => false,
            }

            "all ports inactive by SKU, guid2 -> do not track" {
                TrackPortDown {
                    guid: "guid2",
                    guid_to_index: &[("guid1", 0), ("guid2", 1)],
                    expected_inactive: Some(&[0, 1]),
                    instance_guids: None,
                } => false,
            }

            "unknown GUID with SKU gets u32::MAX, not inactive -> track (fail-open)" {
                TrackPortDown {
                    guid: "unknown_guid",
                    guid_to_index: &[],
                    expected_inactive: Some(&[0, 1, 2]),
                    instance_guids: None,
                } => true,
            }

            // --- Instance fallback when no SKU ---
            "no SKU, instance uses this port -> track" {
                TrackPortDown {
                    guid: "guid1",
                    guid_to_index: &[("guid1", 0), ("guid2", 1)],
                    expected_inactive: None,
                    instance_guids: Some(&["guid1", "guid2"]),
                } => true,
            }

            "no SKU, instance does not use this port -> do not track" {
                TrackPortDown {
                    guid: "guid3",
                    guid_to_index: &[("guid1", 0), ("guid2", 1), ("guid3", 2)],
                    expected_inactive: None,
                    instance_guids: Some(&["guid1", "guid2"]),
                } => false,
            }

            "no SKU, instance match is case-insensitive -> track" {
                TrackPortDown {
                    guid: "GUID1",
                    guid_to_index: &[("GUID1", 0)],
                    expected_inactive: None,
                    instance_guids: Some(&["guid1"]),
                } => true,
            }

            "no SKU, empty instance config -> do not track" {
                TrackPortDown {
                    guid: "guid1",
                    guid_to_index: &[("guid1", 0)],
                    expected_inactive: None,
                    instance_guids: Some(&[]),
                } => false,
            }

            // --- Neither SKU nor instance (a SKU present but with no IB devices
            // reaches this the same way -- both arrive as `expected_inactive: None`) ---
            "neither SKU nor instance -> do not track" {
                TrackPortDown {
                    guid: "guid1",
                    guid_to_index: &[("guid1", 0)],
                    expected_inactive: None,
                    instance_guids: None,
                } => false,
            }
        );
    }

    mod fabric_load_failures {
        use std::convert::Infallible;

        use async_trait::async_trait;
        use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
        use carbide_test_support::Outcome::Yields;
        use carbide_test_support::{Case, check_cases_async};
        use model::ib::IBQosConf;

        use super::*;
        use crate::ib::{
            Filter, IBFabricConfig, IBFabricManagerConfig, IBFabricRawResponse, IBFabricVersions,
        };

        const ERROR: &str = "failed to call IBFabricManager: simulated failure";
        const FABRIC: &str = "fabric-1";
        const METRIC: &str = "carbide_ib_monitor_partial_failures_total";

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum FabricLoadFailure {
            BuildClient,
            HealthCheck,
            LoadPorts,
            LoadPartitions,
        }

        fn simulated_failure() -> IbError {
            IbError::IBFabricError("simulated failure".to_string())
        }

        struct FailingFabricManager {
            failure: FabricLoadFailure,
        }

        #[async_trait]
        impl IBFabricManager for FailingFabricManager {
            async fn new_client(&self, _fabric_name: &str) -> Result<Arc<dyn IBFabric>, IbError> {
                if self.failure == FabricLoadFailure::BuildClient {
                    return Err(simulated_failure());
                }
                Ok(Arc::new(FailingFabric {
                    failure: self.failure,
                }))
            }

            fn get_config(&self) -> IBFabricManagerConfig {
                IBFabricManagerConfig::default()
            }
        }

        struct FailingFabric {
            failure: FabricLoadFailure,
        }

        #[async_trait]
        impl IBFabric for FailingFabric {
            async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError> {
                Ok(IBFabricConfig::default())
            }

            async fn update_partition_qos_conf(
                &self,
                _pkey: u16,
                _qos_conf: &IBQosConf,
            ) -> Result<(), IbError> {
                unreachable!("fabric data loading does not update partition QoS")
            }

            async fn get_ib_networks(
                &self,
                _options: GetPartitionOptions,
            ) -> Result<HashMap<u16, IBNetwork>, IbError> {
                if self.failure == FabricLoadFailure::LoadPartitions {
                    return Err(simulated_failure());
                }
                Ok(HashMap::new())
            }

            async fn get_ib_network(
                &self,
                pkey: u16,
                _options: GetPartitionOptions,
            ) -> Result<IBNetwork, IbError> {
                Ok(IBNetwork {
                    name: "default".to_string(),
                    pkey,
                    ipoib: false,
                    qos_conf: None,
                    associated_guids: Some(HashSet::new()),
                    membership: None,
                })
            }

            async fn bind_ib_ports(
                &self,
                _ibnetwork: IBNetwork,
                _ports: Vec<String>,
            ) -> Result<(), IbError> {
                unreachable!("fabric data loading does not bind ports")
            }

            async fn unbind_ib_ports(&self, _pkey: u16, _id: Vec<String>) -> Result<(), IbError> {
                unreachable!("fabric data loading does not unbind ports")
            }

            async fn find_ib_port(&self, _filter: Option<Filter>) -> Result<Vec<IBPort>, IbError> {
                if self.failure == FabricLoadFailure::LoadPorts {
                    return Err(simulated_failure());
                }
                Ok(Vec::new())
            }

            async fn versions(&self) -> Result<IBFabricVersions, IbError> {
                if self.failure == FabricLoadFailure::HealthCheck {
                    return Err(simulated_failure());
                }
                Ok(IBFabricVersions {
                    ufm_version: "test".to_string(),
                })
            }

            async fn raw_get(&self, _path: &str) -> Result<IBFabricRawResponse, IbError> {
                unreachable!("fabric data loading does not make raw requests")
            }
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            client_returned: bool,
            fabric_error: String,
            log_count: usize,
            event_name: Option<String>,
            failure_stage: Option<String>,
            counter_delta: f64,
        }

        #[tokio::test]
        async fn fabric_load_failures_update_gauge_state_and_emit_the_stage_event() {
            check_cases_async(
                [
                    Case {
                        scenario: "client build",
                        input: FabricLoadFailure::BuildClient,
                        expect: Yields(Observation {
                            client_returned: false,
                            fabric_error: ERROR.to_string(),
                            log_count: 1,
                            event_name: Some("ib_fabric_data_load_failed".to_string()),
                            failure_stage: Some("build_client".to_string()),
                            counter_delta: 1.0,
                        }),
                    },
                    Case {
                        scenario: "health check",
                        input: FabricLoadFailure::HealthCheck,
                        expect: Yields(Observation {
                            client_returned: true,
                            fabric_error: ERROR.to_string(),
                            log_count: 1,
                            event_name: Some("ib_fabric_data_load_failed".to_string()),
                            failure_stage: Some("health_check".to_string()),
                            counter_delta: 1.0,
                        }),
                    },
                    Case {
                        scenario: "port load",
                        input: FabricLoadFailure::LoadPorts,
                        expect: Yields(Observation {
                            client_returned: true,
                            fabric_error: ERROR.to_string(),
                            log_count: 1,
                            event_name: Some("ib_fabric_data_load_failed".to_string()),
                            failure_stage: Some("load_ports".to_string()),
                            counter_delta: 1.0,
                        }),
                    },
                    Case {
                        scenario: "partition load",
                        input: FabricLoadFailure::LoadPartitions,
                        expect: Yields(Observation {
                            client_returned: true,
                            fabric_error: ERROR.to_string(),
                            log_count: 1,
                            event_name: Some("ib_fabric_data_load_failed".to_string()),
                            failure_stage: Some("load_partitions".to_string()),
                            counter_delta: 1.0,
                        }),
                    },
                ],
                |failure| async move {
                    let manager = FailingFabricManager { failure };
                    let definition = IbFabricDefinition {
                        endpoints: vec!["https://ufm-1".to_string()],
                        pkeys: Vec::new(),
                    };
                    let mut fabric_data = FabricData::default();
                    let mut fabric_metrics = FabricMetrics::default();
                    let metrics = MetricsCapture::start();
                    let (client, logs) = capture_logs_async(load_single_fabric_data(
                        &manager,
                        FABRIC,
                        &definition,
                        &mut fabric_data,
                        &mut fabric_metrics,
                    ))
                    .await;
                    let log = logs.first().expect("fabric failure Event logged");
                    let failure_stage = log.field("failure_stage").map(str::to_string);

                    Ok::<_, Infallible>(Observation {
                        client_returned: client.is_some(),
                        fabric_error: fabric_metrics.fabric_error,
                        log_count: logs.len(),
                        event_name: log.field("event_name").map(str::to_string),
                        counter_delta: metrics.counter_delta(
                            METRIC,
                            &[(
                                "failure_stage",
                                failure_stage.as_deref().expect("failure stage label"),
                            )],
                        ),
                        failure_stage,
                    })
                },
            )
            .await;
        }
    }

    // ============================================================
    // Unit Tests for per-iteration fabric client reuse
    // ============================================================

    mod client_reuse {
        use carbide_instrument::testing::MetricsCapture;

        use super::*;
        use crate::ib::fakes::{CountingFabricManager, make_partition};

        /// One fabric's worth of pending changes: three binds + two unbinds
        /// against pkey 0x101 on `fabric`.
        fn five_pending_changes(
            fabric: &str,
            pkey: PartitionKey,
        ) -> Vec<MachineIbStatusEvaluation> {
            vec![MachineIbStatusEvaluation {
                missing_guid_pkeys: vec![
                    (fabric.to_string(), "guid-1".to_string(), pkey),
                    (fabric.to_string(), "guid-2".to_string(), pkey),
                    (fabric.to_string(), "guid-3".to_string(), pkey),
                ],
                unexpected_guid_pkeys: vec![
                    (fabric.to_string(), "guid-4".to_string(), pkey),
                    (fabric.to_string(), "guid-5".to_string(), pkey),
                ],
                unknown_guid_pkeys: vec![],
                down_port_guids: vec![],
                needed_memberships: vec![],
            }]
        }

        /// Counts the client builds needed to apply a batch of five GUID<->pkey
        /// changes (three binds + two unbinds) on one fabric.
        ///
        /// Before the per-iteration client reuse this scope built 5 clients
        /// (one per GUID change); now the whole batch shares one.
        #[tokio::test]
        async fn applying_guid_pkey_changes_builds_one_client_per_fabric() {
            const FABRIC: &str = "batched-change-fabric";

            let manager = CountingFabricManager::new();
            let pkey = PartitionKey::try_from(0x101).expect("valid pkey");
            let partition = make_partition(Some(0x101), false);
            let partition_id = partition.id;
            let tenant_partitions = HashMap::from([(partition_id, partition)]);
            let partition_ids_by_pkey = HashMap::from([(pkey, partition_id)]);
            let fabrics = HashMap::from([(
                FABRIC.to_string(),
                make_fabric_definition(vec![("0x100", "0x8FF")]),
            )]);
            let mut fabric_clients = HashMap::new();
            let event_metrics = MetricsCapture::start();

            let num_changes = apply_guid_pkey_changes(
                &manager,
                &mut fabric_clients,
                &fabrics,
                &tenant_partitions,
                &partition_ids_by_pkey,
                five_pending_changes(FABRIC, pkey),
            )
            .await
            .expect("applying changes against stub fabric");

            assert_eq!(num_changes, 5, "all five changes applied");
            assert_eq!(
                event_metrics.counter_delta(
                    "carbide_ib_monitor_ufm_changes_applied_total",
                    &[
                        ("fabric", FABRIC),
                        ("operation", "bind_guid_to_pkey"),
                        ("status", "ok"),
                    ],
                ),
                3.0,
            );
            assert_eq!(
                event_metrics.counter_delta(
                    "carbide_ib_monitor_ufm_changes_applied_total",
                    &[
                        ("fabric", FABRIC),
                        ("operation", "unbind_guid_from_pkey"),
                        ("status", "ok"),
                    ],
                ),
                2.0,
            );
            assert_eq!(
                manager.build_count(),
                1,
                "AFTER: one client build serves all five GUID changes"
            );
        }

        /// A client built by the data-loading phase is reused by the
        /// change-application phase: a full per-fabric iteration builds exactly
        /// one client.
        #[tokio::test]
        async fn monitor_iteration_builds_one_client_per_fabric() {
            const FABRIC: &str = "reused-client-fabric";

            let manager = CountingFabricManager::new();
            let definition = make_fabric_definition(vec![("0x100", "0x8FF")]);
            let pkey = PartitionKey::try_from(0x101).expect("valid pkey");
            let partition = make_partition(Some(0x101), false);
            let partition_id = partition.id;
            let tenant_partitions = HashMap::from([(partition_id, partition)]);
            let partition_ids_by_pkey = HashMap::from([(pkey, partition_id)]);
            let fabrics = HashMap::from([(FABRIC.to_string(), definition.clone())]);
            let mut fabric_data = FabricData::default();
            let mut fabric_metrics = FabricMetrics::default();
            let mut fabric_clients = HashMap::new();

            // Data-loading phase.
            let conn = load_single_fabric_data(
                &manager,
                FABRIC,
                &definition,
                &mut fabric_data,
                &mut fabric_metrics,
            )
            .await
            .expect("client for reachable stub fabric");
            fabric_clients.insert(FABRIC.to_string(), conn);

            assert!(fabric_data.ports_by_guid.is_some());
            assert!(fabric_data.partitions.is_some());
            assert_eq!(
                manager.build_count(),
                1,
                "AFTER: one client build loads health + ports + partitions"
            );

            // Change-application phase reuses the data-loading client.
            let num_changes = apply_guid_pkey_changes(
                &manager,
                &mut fabric_clients,
                &fabrics,
                &tenant_partitions,
                &partition_ids_by_pkey,
                five_pending_changes(FABRIC, pkey),
            )
            .await
            .expect("applying changes against stub fabric");

            assert_eq!(num_changes, 5, "all five changes applied");
            assert_eq!(
                manager.build_count(),
                1,
                "AFTER: the whole per-fabric iteration shares one client build"
            );
        }
    }

    // ============================================================
    // Integration Tests - TODO
    // ============================================================
    // Integration tests deferred until after code review.
    //
    // Planned test scenarios:
    // 1. Alert found + alert GUIDs NOT in result lists → Alert cleared
    // 2. Alert found + alert GUIDs present in unexpected_guid_pkeys → Alert NOT cleared (retry next iteration)
    // 3. Alert found + alert GUIDs present in missing_guid_pkeys → Alert NOT cleared
    // 4. Alert found + alert GUIDs present in unknown_guid_pkeys → Alert NOT cleared
    // 5. No alert present → No action taken
    // 6. Alert with malformed message → Gracefully ignored (no GUIDs extracted)
    // 7. Multiple GUIDs in single alert → All GUIDs checked for clearing condition
}
