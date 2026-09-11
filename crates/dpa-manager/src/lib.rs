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
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use carbide_dpa::DpaInfo;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::machine::{HostMachineId, MachineId};
use chrono::TimeDelta;
use db::db_read::PgPoolReader;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{self, TransactionVending};
use metrics::{DpaMonitorIterationFinished, DpaMonitorMetrics};
use model::dpa_interface::{DpaInterface, DpaInterfaceControllerState, DpaSearchConfig};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    HostHealthConfig, LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot,
};
use mqttea::client::MqtteaClient;
use sqlx::{PgConnection, PgPool, PgTransaction};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::config::EwEthersConfig;
use crate::errors::{DpaManagerError, DpaManagerResult};

mod card_handler;
pub mod config;
pub mod errors;
mod metrics;

#[cfg(test)]
pub(crate) use carbide_macros::sqlx_test;

pub struct DpaMonitor {
    pub(crate) db_services: DbServices,
    pub(crate) dpa_info: Arc<DpaInfo>,
    pub(crate) config: EwEthersConfig,
    host_health: HostHealthConfig,
    metric_holder: Arc<metrics::MetricHolder>,
    work_lock_manager_handle: WorkLockManagerHandle,
}

pub struct DbServices {
    db_pool: PgPool,
}

// This carries the result running the handler for a single dpa interface.
// If the dpa interface needs to a new state, the new state is returned.
// If we started a transaction in the handler, the transaction is returned.
pub(crate) struct HandlerResult {
    new_state: Option<DpaInterfaceControllerState>,
    txn: Option<PgTransaction<'static>>,
}

impl DpaMonitor {
    const ITERATION_WORK_KEY: &'static str = "DpaMonitor::run_single_iteration";

    pub fn new(
        db_pool: PgPool,
        _db_reader: PgPoolReader,
        dpa_info: Arc<DpaInfo>,
        _meter: opentelemetry::metrics::Meter,
        config: EwEthersConfig,
        host_health: HostHealthConfig,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        let hold_period = config
            .monitor_run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(_meter, hold_period));

        Self {
            db_services: DbServices { db_pool },
            dpa_info,
            config,
            host_health,
            work_lock_manager_handle,
            metric_holder,
        }
    }

    pub fn start(
        mut self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        join_set
            .build_task()
            .name("dpa-monitor")
            .spawn(async move { self.run(cancel_token).await })?;

        Ok(())
    }

    pub async fn run(&mut self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.monitor_run_interval);
        loop {
            let mut tick = timer.tick();
            // `run_single_iteration` records a failed pass before returning,
            // so this loop only needs a successful result to adjust cadence.
            if let Ok(num_changes) = self.run_single_iteration().await
                && num_changes > 0
            {
                // Decrease the interval if changes have been made.
                tick.set_interval(Duration::from_millis(1000));
            }

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("DpaMonitor stop was requested");
                    return;
                }
            }
        }
    }

    pub async fn run_single_iteration(&mut self) -> DpaManagerResult<usize> {
        let mut metrics = DpaMonitorMetrics::new();
        let span_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
        let check_dpa_span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "dpa-monitor",
            span_id,
        );
        let result = self
            .run_single_iteration_inner(&mut metrics)
            .instrument(check_dpa_span.clone())
            .await;
        check_dpa_span.record("metrics", metrics.to_string());
        check_dpa_span.in_scope(|| {
            carbide_instrument::emit(match result.as_ref().err() {
                None => DpaMonitorIterationFinished::Succeeded {
                    latency: metrics.recording_started_at.elapsed(),
                },
                Some(error) => DpaMonitorIterationFinished::Failed {
                    latency: metrics.recording_started_at.elapsed(),
                    error: error.to_string(),
                },
            });
        });
        self.metric_holder.update_metrics(metrics);
        result
    }

    async fn run_single_iteration_inner(
        &mut self,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<usize> {
        let _lock = match self
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "DpaMonitor failed to acquire work lock: Another instance of carbide running?",
                );
                return Ok(0);
            }
        };
        tracing::info!(
            lock = Self::ITERATION_WORK_KEY,
            "DpaMonitor acquired the lock",
        );

        let mut txn = self.db_services.db_pool.txn_begin().await?;

        let mut snapshots = match self.get_all_snapshots(&mut txn).await {
            Ok(snapshots) => snapshots,
            Err(e) => {
                tracing::error!(error = %e, "run_single_iteration_inner: Failed to load ManagedHost snapshots in IbFabricMonitor");
                // Record the same error for all fabrics, so that the problem is at least visible on dashboards
                return Err(e);
            }
        };

        txn.commit().await?;

        for mh in snapshots.values_mut() {
            metrics.num_machines_scanned += 1;

            // If the machine does not have any dpa interfaces, we can skip it.
            if mh.dpa_interface_snapshots.is_empty() {
                tracing::info!("run_single_iteration_inner: skipping, no dpa interfaces");
                continue;
            }

            // If the machine is an instance, increment the number of instances scanned.
            if mh.instance.is_some() {
                metrics.num_instances_scanned += 1;
            }

            for idx in 0..mh.dpa_interface_snapshots.len() {
                metrics.num_dpa_interfaces_scanned += 1;

                let controller_state = mh.dpa_interface_snapshots[idx].controller_state.clone();

                // Look at this DPA interface and see if we need to transition it to a new state.
                // This will return a new state if we need to transition to a new state, or None if we can stay in the current state.
                // We build an array of dpa interfaces and new state.
                // After examining all the dpa interfaces in all the machines, we will update the DB with the new states in another loop
                let handler_result = self.handle_dpa_interface(mh, idx, metrics).await?;

                let new_state = handler_result.new_state;
                let txn = handler_result.txn;

                if let Some(new_state) = new_state {
                    let new_version = controller_state.version.increment();

                    let mut txn =
                        match txn {
                            Some(t) => t,
                            None => self.db_services.db_pool.begin().await.map_err(|e| {
                                db::AnnotatedSqlxError::new("dpa_monitor begin txn", e)
                            })?,
                        };

                    let applied = db::dpa_interface::try_update_controller_state(
                        &mut txn,
                        mh.dpa_interface_snapshots[idx].id,
                        controller_state.version,
                        new_version,
                        &new_state,
                    )
                    .await?;

                    if applied {
                        txn.commit().await.map_err(|e| {
                            db::AnnotatedSqlxError::new("dpa_monitor commit txn", e)
                        })?;
                    } else {
                        // Lost the controller-state CAS: another writer advanced
                        // this interface since the snapshot loaded. The transition did not apply, so
                        // drop the txn to roll back any bookkeeping the handler
                        // staged in it. re-read on the next tick
                        drop(txn);
                    }
                } else if let Some(txn) = txn {
                    txn.commit()
                        .await
                        .map_err(|e| db::AnnotatedSqlxError::new("dpa_monitor commit txn", e))?;
                }
            }
        }

        Ok(0)
    }

    // This should return a txn if we started one, an indication of whether state is changing,
    // and if so, the new state.
    // We should:
    //    1. Go through the state transitions for the card.
    //    2. Send heartbeats in Ready and Assigned states if necessary.
    //    3. If the DPA is in ASSIGNED state, go through the attachments.
    //    4.    If we are not an instance, then, we need to do ResetVNI.
    //    5.    If we are an instance, then, we need to do SetVNI.
    //    6. We need a way for machine statehandler to determine if congig is done.
    async fn handle_dpa_interface(
        &mut self,
        mh: &mut ManagedHostStateSnapshot,
        idx: usize,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<HandlerResult> {
        use card_handler::handler_for;

        if matches!(mh.managed_state, ManagedHostState::Decommissioning { .. }) {
            return Ok(HandlerResult {
                new_state: None,
                txn: None,
            });
        }

        let interface_type = mh.dpa_interface_snapshots[idx].interface_type;
        let handler = handler_for(interface_type);
        let controller_state = mh.dpa_interface_snapshots[idx]
            .controller_state
            .value
            .clone();

        match controller_state {
            DpaInterfaceControllerState::Provisioning => {
                handler.handle_provisioning(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::Ready => {
                handler.handle_ready(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::Unlocking => {
                handler.handle_unlocking(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::ApplyFirmware => {
                handler.handle_apply_firmware(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::ApplyProfile => {
                handler.handle_apply_profile(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::Locking => {
                handler.handle_locking(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::Assigned => {
                handler.handle_assigned(self, mh, idx, metrics).await
            }
            DpaInterfaceControllerState::RotateKeyUnlocking => {
                handler
                    .handle_rotate_key_unlocking(self, mh, idx, metrics)
                    .await
            }
            DpaInterfaceControllerState::RotateKeyLocking => {
                handler
                    .handle_rotate_key_locking(self, mh, idx, metrics)
                    .await
            }
        }
    }

    async fn get_all_snapshots(
        &self,
        txn: &mut PgConnection,
    ) -> DpaManagerResult<HashMap<MachineId, ManagedHostStateSnapshot>> {
        let machine_ids = db::machine::find_machine_ids(
            &mut *txn,
            MachineSearchConfig {
                include_predicted_host: true,
                ..Default::default()
            },
        )
        .await?;

        let mut res = db::managed_host::load_by_machine_ids(
            txn,
            &machine_ids,
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: true,
                host_health_config: self.host_health,
            },
        )
        .await
        .map_err(Into::<DpaManagerError>::into)?;

        // Load every host's DPA interfaces in one query rather than one per
        // machine. Query by each entry's `host_snapshot.id` — the same key the
        // assignment below looks up — collected up front so the batch call
        // doesn't conflict with the mutable borrow of `res` below. Duplicates
        // in the slice are harmless for `= ANY($1)`, and the non-consuming
        // `get` keeps the assignment correct even when two entries resolve to
        // the same host snapshot.
        let machine_ids: Vec<HostMachineId> = res.values().map(|mh| mh.host_snapshot.id).collect();
        let dpa_search_config = DpaSearchConfig {
            only_svpc: false,
            only_astra: false,
        };
        let dpa_snapshots_by_machine =
            db::dpa_interface::find_by_machine_ids(&mut *txn, &machine_ids, dpa_search_config)
                .await
                .map_err(Into::<DpaManagerError>::into)?;

        for mh in res.values_mut() {
            mh.dpa_interface_snapshots = dpa_snapshots_by_machine
                .get(&mh.host_snapshot.id)
                .cloned()
                .unwrap_or_default();
        }

        Ok(res)
    }

    // Determine if we need to do a heartbeat or if we need to
    // send a SetVni command because the DPA and Carbide are out of sync.
    // If so, call send_set_vni_command to send the heart beat or set vni
    pub(crate) async fn do_heartbeat<'a>(
        &mut self,
        state: &DpaInterface,
        client: Arc<MqtteaClient>,
        dpa_info: &Arc<DpaInfo>,
        hb_interval: TimeDelta,
        vni: u32,
        metrics: &mut DpaMonitorMetrics,
    ) -> DpaManagerResult<Option<PgTransaction<'a>>> {
        // We are in the Ready or Assigned state and we continue to be in the same state.
        // In this state, we will send SetVni command to the DPA if
        //    (1) if the heartbeat interval has elapsed since the heartbeat
        //    (2) The DPA sent us an ack and it looks like the DPA lost its config (due to powercycle potentially)
        // Heartbeat is identified by the revision being se to the sentinel value "NIL"
        // When we send a heartbeat below, we update the last_hb_time for the interface entry.

        // XXX TODO XXX
        // Verify with the FW team how the card behaves if it loses its config after a powercyle.
        // If we send it a heartbeat with NIL as the revision, but with a valid VNI (since its a part
        // of a tenancy), will it echo back the VNI? Or does the reply alway carry whatever VNI it is using?
        // If it just echoes back the VNI, we have to send it a SetVni command with the VNI to use.
        // XXX TODO XXX

        let Some(next_hb_time) = state.last_hb_time.checked_add_signed(hb_interval) else {
            // checked_add_signed returns None if the addition overflows
            return Ok(None);
        };

        if chrono::Utc::now() < next_hb_time {
            return Ok(None);
        }

        let txn = self
            .send_set_vni_command(state, client, dpa_info, vni, true, "NIL".to_string())
            .await?;

        metrics.num_heartbeats_sent += 1;

        Ok(txn)
    }

    // Send a SetVni command to the DPA. The SetVni command could be a heart beat (identified by
    // revision being "NIL"). If needs_vni is true, get the VNI to use from the DB. Otherwise, vni
    // sent is 0.
    pub(crate) async fn send_set_vni_command<'a>(
        &self,
        state: &DpaInterface,
        client: Arc<MqtteaClient>,
        dpa_info: &Arc<DpaInfo>, // dpa_info contains the subnet_ip and subnet_mask to use for the SetVni command
        vni: u32,
        heart_beat: bool,
        revision_str: String,
    ) -> DpaManagerResult<Option<PgTransaction<'a>>> {
        let services = &self.db_services;

        // Send a heartbeat command, indicated by the revision string being "NIL".
        match carbide_dpa::send_dpa_command(
            client,
            dpa_info,
            state.mac_address.to_string(),
            revision_str,
            vni as i32,
        )
        .await
        {
            Ok(()) => {
                if heart_beat {
                    let mut txn =
                        services.db_pool.begin().await.map_err(|e| {
                            db::AnnotatedSqlxError::new("dpa_monitor hb begin txn", e)
                        })?;
                    let res = db::dpa_interface::update_last_hb_time(state, &mut txn).await;
                    if let Err(error) = res {
                        tracing::error!(
                            dpa_interface_id = %state.id,
                            error = %error,
                            "Error updating DPA interface heartbeat time",
                        );
                    }
                    Ok(Some(txn))
                } else {
                    Ok(None)
                }
            }
            Err(_e) => Ok(None),
        }
    }
}
