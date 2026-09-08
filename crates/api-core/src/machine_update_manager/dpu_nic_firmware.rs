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
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use carbide_machine_controller::dpf::DpfOperations;
use carbide_uuid::machine::{HostMachineId, MachineId};
use db::dpu_machine_update;
use model::dpu_machine_update::{DpuMachineUpdate, OutdatedDpfDpu};
use model::machine::ManagedHostStateSnapshot;
use sqlx::PgConnection;

use super::dpu_nic_firmware_metrics::DpuNicFirmwareUpdateMetrics;
use super::machine_update_module::MachineUpdateModule;
use super::metrics::{
    FirmwareUpdateFailed, FirmwareUpdateFailureCause, FirmwareUpdatePhase, FirmwareUpdateProgress,
    FirmwareUpdateTarget,
};
use crate::cfg::file::CarbideConfig;
use crate::machine_update_manager::MachineUpdateManager;
use crate::{CarbideResult, DatabaseError};

/// DpuNicFirmwareUpdate is a module used [MachineUpdateManager](crate::machine_update_manager::MachineUpdateManager)
/// to ensure that DPU NIC firmware matches the expected version of the carbide release.
///
/// Config used from [CarbideConfig](crate::cfg::CarbideConfig)
/// * `dpu_config.dpu_nic_firmware_update_versions` lists the accepted DPU NIC firmware versions.
///
/// Note that if the version does not match in either direction, the DPU will be updated.
pub(crate) struct DpuNicFirmwareUpdate {
    pub(crate) metrics: Option<DpuNicFirmwareUpdateMetrics>,
    pub(crate) config: Arc<CarbideConfig>,
    /// DPF handle for discovering outdated DPF-managed DPUs. `None` when DPF
    /// is disabled in config; in that case `find_outdated_dpus_dpf` is not called.
    pub(crate) dpf: Option<Arc<dyn DpfOperations>>,
    /// Wrong-version outcomes already counted, keyed by DPU and the version it
    /// landed on: the condition persists across manager passes (the update
    /// marker stays until an operator intervenes or a new update runs), and a
    /// counter must record the outcome once, not once per poll. An entry
    /// clears when the DPU's markers clear, so a later attempt that lands
    /// wrong again is a new outcome. In-memory: a restart re-reports at most
    /// once per stuck DPU.
    pub(crate) reported_wrong_versions: std::sync::Mutex<HashSet<(MachineId, String)>>,
}

#[async_trait]
impl MachineUpdateModule for DpuNicFirmwareUpdate {
    async fn get_updates_in_progress(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<MachineId>> {
        let current_updating_machines =
            match dpu_machine_update::get_reprovisioning_machines(txn).await {
                Ok(current_updating_machines) => current_updating_machines,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Error getting outstanding reprovisioning count",
                    );
                    vec![]
                }
            };

        Ok(current_updating_machines
            .iter()
            .map(|mu| mu.host_machine_id.into())
            .collect())
    }

    async fn start_updates(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        available_updates: i32,
        updating_host_machines: &HashSet<MachineId>,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<MachineId>> {
        let machine_updates: Vec<DpuMachineUpdate> = self
            .check_for_updates(snapshots, available_updates)
            .await
            .into_iter()
            .filter(|u| updating_host_machines.get(&u.host_machine_id).is_none())
            .collect();

        // The outcome is vec<DpuMachineUpdate>, let's convert it to HashMap<host_machine_id, vec<DpuMachineUpdate>>
        // This way we can run our loop based on host_machine id.
        let mut host_machine_updates: HashMap<MachineId, Vec<DpuMachineUpdate>> = HashMap::new();

        for machine_update in machine_updates {
            host_machine_updates
                .entry(machine_update.host_machine_id.into())
                .or_default()
                .push(machine_update);
        }

        let mut updates_started = HashSet::default();

        for (host_machine_id, machine_updates) in host_machine_updates {
            if updating_host_machines.contains(&host_machine_id) {
                continue;
            }

            let dpu_update_string = machine_updates.iter().fold("".to_string(), |output, dpu| {
                output + format!("{} ({}) ", dpu.dpu_machine_id, dpu.firmware_version).as_str()
            });

            // If the reprovisioning failed to update the database for a
            // given {dpu,host}_machine_id, log it as a warning and don't
            // add it to updates_started.

            let mut txn = db::Transaction::begin(pool).await?;
            if let Err(reprovisioning_err) =
                dpu_machine_update::trigger_reprovisioning_for_managed_host(
                    &mut txn,
                    &machine_updates,
                )
                .await
            {
                match reprovisioning_err {
                    DatabaseError::NotFoundError { id, .. } => {
                        carbide_instrument::emit(FirmwareUpdateFailed {
                            target: FirmwareUpdateTarget::DpuNic,
                            cause: FirmwareUpdateFailureCause::NoUpdateMatch,
                            machine_id: host_machine_id,
                            unmatched_dpu_machine_id: id,
                            firmware_version: String::new(),
                        });
                        continue;
                    }
                    _ => {
                        return Err(reprovisioning_err.into());
                    }
                }
            }

            txn.commit().await?;

            // Counted only once the trigger is committed: a DPU that left
            // ready between snapshot and trigger is a NoUpdateMatch failure,
            // not a started update.
            carbide_instrument::emit(FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::DpuNic,
                phase: FirmwareUpdatePhase::Started,
                machine_id: host_machine_id,
                detail: dpu_update_string,
            });
            updates_started.insert(host_machine_id);
        }

        Ok(updates_started)
    }

    async fn clear_completed_updates(&self, txn: &mut PgConnection) -> CarbideResult<()> {
        let updated_machines =
            dpu_machine_update::get_updated_machines(txn, self.config.host_health).await?;
        tracing::debug!(
            updated_machine_count = updated_machines.len(),
            "found updated machines",
        );
        for updated_machine in updated_machines {
            // DPF picks update targets by the DPUDeployment's expected BFB, not
            // by `dpu_nic_firmware_update_versions` — `find_outdated_dpus` skips
            // DPF hosts outright. Holding a DPF host's reported NIC firmware
            // against that list therefore never matches, which used to leave the
            // HostUpdateInProgress marker (and its prevent_allocations alert)
            // pinned forever after a successful reprovision. The reprovision
            // completing is the completion signal for DPF hosts.
            if updated_machine.dpf_managed
                || self
                    .config
                    .dpu_config
                    .dpu_nic_firmware_update_versions
                    .contains(&updated_machine.firmware_version)
            {
                if let Err(e) =
                    MachineUpdateManager::remove_machine_update_markers(txn, &updated_machine).await
                {
                    tracing::warn!(
                        machine_id = %updated_machine.dpu_machine_id,
                        error = %e,
                        "Failed to remove machine update markers",
                    );
                } else if let Ok(mut reported) = self.reported_wrong_versions.lock() {
                    reported
                        .retain(|(dpu, _)| dpu != updated_machine.dpu_machine_id.as_machine_id());
                }
            } else if self
                .reported_wrong_versions
                .lock()
                .is_ok_and(|mut reported| {
                    reported.insert((
                        updated_machine.dpu_machine_id.into(),
                        updated_machine.firmware_version.clone(),
                    ))
                })
            {
                carbide_instrument::emit(FirmwareUpdateFailed {
                    target: FirmwareUpdateTarget::DpuNic,
                    cause: FirmwareUpdateFailureCause::WrongVersionAfterUpdate,
                    machine_id: updated_machine.dpu_machine_id.into(),
                    unmatched_dpu_machine_id: String::new(),
                    firmware_version: updated_machine.firmware_version,
                });
            }
        }
        Ok(())
    }

    async fn update_metrics(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()> {
        let dpf_outdated = self.fetch_dpf_outdated().await;
        match DpuMachineUpdate::find_available_outdated_dpus(
            None,
            &self.config.dpu_config.dpu_nic_firmware_update_versions,
            snapshots,
            &dpf_outdated,
        ) {
            Ok(outdated_dpus) => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .pending_firmware_updates
                        .store(outdated_dpus.len() as u64, Ordering::Relaxed);
                }
            }
            Err(e) => tracing::warn!(error=%e, "Error geting outdated dpus for metrics"),
        }

        let outdated_dpus = DpuMachineUpdate::find_unavailable_outdated_dpus(
            &self.config.dpu_config.dpu_nic_firmware_update_versions,
            snapshots,
        )?;
        if let Some(metrics) = &self.metrics {
            metrics
                .unavailable_dpu_updates
                .store(outdated_dpus.len() as u64, Ordering::Relaxed);
        }

        let mut txn = db::Transaction::begin(pool).await?;
        match dpu_machine_update::get_fw_updates_running_count(&mut txn).await {
            Ok(count) => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .running_dpu_updates
                        .store(count as u64, Ordering::Relaxed);
                }
                txn.commit().await?;
            }
            Err(e) => tracing::warn!(
                error = %e,
                "Error getting running upgrade count for metrics",
            ),
        }

        Ok(())
    }
}

impl DpuNicFirmwareUpdate {
    pub(crate) fn new(
        config: Arc<CarbideConfig>,
        meter: opentelemetry::metrics::Meter,
        dpf: Option<Arc<dyn DpfOperations>>,
    ) -> Option<Self> {
        if !config
            .dpu_config
            .dpu_nic_firmware_reprovision_update_enabled
        {
            return None;
        }

        let mut metrics = DpuNicFirmwareUpdateMetrics::new();
        metrics.register_callbacks(&meter);
        Some(DpuNicFirmwareUpdate {
            reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
            metrics: Some(metrics),
            config,
            dpf,
        })
    }

    pub(crate) async fn check_for_updates(
        &self,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
        available_updates: i32,
    ) -> Vec<DpuMachineUpdate> {
        let dpf_outdated = self.fetch_dpf_outdated().await;
        match DpuMachineUpdate::find_available_outdated_dpus(
            Some(available_updates),
            &self.config.dpu_config.dpu_nic_firmware_update_versions,
            snapshots,
            &dpf_outdated,
        ) {
            Ok(machine_updates) => machine_updates,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to find machines needing updates",
                );
                vec![]
            }
        }
    }

    /// Best-effort fetch of DPF-managed outdated DPUs. Returns an empty vec
    /// when DPF is disabled or the query fails — callers should treat the
    /// result as "what we know right now" rather than authoritative.
    async fn fetch_dpf_outdated(&self) -> Vec<OutdatedDpfDpu> {
        let Some(dpf) = self.dpf.as_ref() else {
            return vec![];
        };
        match dpf.find_outdated_dpus_dpf().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch DPF outdated DPUs");
                vec![]
            }
        }
    }
}

impl fmt::Display for DpuNicFirmwareUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DpuNicFirmwareUpdate")
    }
}
