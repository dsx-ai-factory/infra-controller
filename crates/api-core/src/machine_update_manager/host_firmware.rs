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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use carbide_firmware::FirmwareConfig;
use carbide_uuid::machine::HostMachineId;
use db::work_lock_manager::WorkLock;
use db::{self, desired_firmware, host_firmware_config};
use model::machine::ManagedHostStateSnapshot;
use model::machine_update_module::HOST_FW_UPDATE_HEALTH_REPORT_SOURCE;
use opentelemetry::metrics::Meter;
use sqlx::PgConnection;
use tokio::sync::Mutex;

use super::host_on_disable_list;
use super::machine_update_module::MachineUpdateModule;
use super::metrics::{FirmwareUpdatePhase, FirmwareUpdateProgress, FirmwareUpdateTarget};
use crate::CarbideResult;
use crate::cfg::file::CarbideConfig;

#[cfg(test)]
mod tests;

pub(super) struct HostFirmwareUpdate {
    pub(super) metrics: HostFirmwareUpdateMetrics,
    config: Arc<CarbideConfig>,
    firmware_config: FirmwareConfig,
    firmware_catalog_last_read: Arc<Mutex<Option<FirmwareCatalogMarker>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FirmwareCatalogMarker {
    firmware_dir_mod_time: Option<SystemTime>,
    host_firmware_config_summary: host_firmware_config::HostFirmwareConfigSummary,
}

#[async_trait]
impl MachineUpdateModule for HostFirmwareUpdate {
    async fn get_updates_in_progress(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        let current_updating_machines = db::machine::get_host_reprovisioning_machines(txn).await?;

        Ok(current_updating_machines.iter().map(|m| m.id).collect())
    }

    async fn start_updates(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        work_lock: &WorkLock,
        available_updates: i32,
        updating_host_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        let firmware_catalog_last_read = self.firmware_catalog_last_read.try_lock().ok();
        // Loading metadata can block on the filesystem. Finish that work before
        // taking the database fence; runtime overrides are read inside it.
        let disk_catalog = firmware_catalog_last_read.as_ref().map(|_| {
            (
                self.firmware_config.config_update_time(),
                self.firmware_config.create_snapshot(),
            )
        });
        let mut txn = super::update_transaction(pool, work_lock).await?;
        let mut published_catalog_marker = None;
        if let (Some(last_read), Some((firmware_dir_mod_time, disk_snapshot))) =
            (firmware_catalog_last_read.as_ref(), disk_catalog)
        {
            let catalog_marker = FirmwareCatalogMarker {
                firmware_dir_mod_time,
                host_firmware_config_summary: host_firmware_config::summary(&mut txn).await?,
            };
            if last_read.as_ref() != Some(&catalog_marker) {
                // Save the firmware config in an SQL table so that we can filter for hosts with non-matching firmware there.
                let overrides = host_firmware_config::list_configs(&mut txn).await?;
                let fw_config_snapshot = disk_snapshot.with_overrides(overrides);
                tracing::info!(
                    firmware_config_snapshot = ?fw_config_snapshot,
                    "Firmware config now",
                );
                let models = fw_config_snapshot.into_values().collect::<Vec<_>>();
                desired_firmware::snapshot_desired_firmware(&mut txn, &models).await?;
                published_catalog_marker = Some(catalog_marker);
            }
        }

        let machine_updates = db::host_machine_update::find_upgrade_needed(
            &mut txn,
            self.config.firmware_global.autoupdate,
            self.config.firmware_global.instance_updates_manual_tagging,
        )
        .await?;
        let mut updates_started = HashSet::default();
        self.metrics
            .pending_firmware_updates
            .store(machine_updates.len() as u64, Ordering::Relaxed);

        // Discovery can return a host once per BMC address. Only successful,
        // distinct admissions consume this pass's budget.
        for machine_update in machine_updates {
            if updates_started.len() as i32 >= available_updates {
                break;
            }
            if updating_host_machines.contains(&machine_update.id)
                || updates_started.contains(&machine_update.id)
                || host_on_disable_list(&self.config.firmware_global, &machine_update.id)
            {
                continue;
            }

            // Manual and rack writers do not share the manager's work lock.
            // Preserve any request they committed after candidate discovery.
            if !db::host_machine_update::try_trigger_host_reprovisioning_request(
                &mut txn,
                "Automated",
                &machine_update.id,
            )
            .await?
            {
                continue;
            }

            updates_started.insert(machine_update.id);
        }

        txn.commit().await?;
        for machine_id in &updates_started {
            carbide_instrument::emit(FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::Host,
                phase: FirmwareUpdatePhase::Started,
                machine_id: *machine_id,
                detail: String::new(),
            });
        }

        if let (Some(mut last_read), Some(catalog_marker)) =
            (firmware_catalog_last_read, published_catalog_marker)
        {
            *last_read = Some(catalog_marker);
        }
        Ok(updates_started)
    }

    async fn clear_completed_updates(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<Vec<FirmwareUpdateProgress>> {
        let completed = db::host_machine_update::find_completed_updates(txn).await?;

        for machine in &completed {
            db::machine::remove_health_report(
                txn,
                machine,
                health_report::HealthReportApplyMode::Merge,
                HOST_FW_UPDATE_HEALTH_REPORT_SOURCE,
            )
            .await?;
            db::machine::update_update_complete(machine, true, txn).await?;
        }
        Ok(completed
            .into_iter()
            .map(|machine_id| FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::Host,
                phase: FirmwareUpdatePhase::Completed,
                machine_id,
                detail: String::new(),
            })
            .collect())
    }

    async fn update_metrics(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()> {
        let exhausted_retries = snapshots
            .values()
            .filter(|snapshot| snapshot.managed_state.host_repro_retries_exhausted())
            .count();
        self.metrics
            .exhausted_reprovision_retries
            .store(exhausted_retries as u64, Ordering::Relaxed);

        let mut txn = db::Transaction::begin(pool).await?;
        match db::host_machine_update::find_upgrade_needed(
            &mut txn,
            self.config.firmware_global.autoupdate,
            self.config.firmware_global.instance_updates_manual_tagging,
        )
        .await
        {
            Ok(upgrade_needed) => {
                self.metrics
                    .pending_firmware_updates
                    .store(upgrade_needed.len() as u64, Ordering::Relaxed);
            }
            Err(e) => tracing::warn!(error=%e, "Error geting host upgrade needed for metrics"),
        };
        match db::host_machine_update::find_upgrade_in_progress(&mut txn).await {
            Ok(upgrade_in_progress) => {
                self.metrics
                    .active_firmware_updates
                    .store(upgrade_in_progress.len() as u64, Ordering::Relaxed);
            }
            Err(e) => tracing::warn!(error=%e, "Error geting host upgrade in progress for metrics"),
        };
        txn.commit().await?;
        Ok(())
    }
}

impl HostFirmwareUpdate {
    pub(super) fn new(
        config: Arc<CarbideConfig>,
        meter: opentelemetry::metrics::Meter,
        firmware_config: FirmwareConfig,
    ) -> Option<Self> {
        tracing::info!(?firmware_config, "Using firmware configuration",);

        let metrics = HostFirmwareUpdateMetrics::new();
        metrics.register_callbacks(&meter);

        Some(Self {
            firmware_config,
            config,
            metrics,
            firmware_catalog_last_read: Arc::new(Mutex::new(None)),
        })
    }
}

impl fmt::Display for HostFirmwareUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFirmwareUpdate")
    }
}

pub(super) struct HostFirmwareUpdateMetrics {
    pub(super) pending_firmware_updates: Arc<AtomicU64>,
    pub(super) active_firmware_updates: Arc<AtomicU64>,
    pub(super) exhausted_reprovision_retries: Arc<AtomicU64>,
}

impl HostFirmwareUpdateMetrics {
    pub(super) fn new() -> Self {
        HostFirmwareUpdateMetrics {
            pending_firmware_updates: Arc::new(AtomicU64::new(0)),
            active_firmware_updates: Arc::new(AtomicU64::new(0)),
            exhausted_reprovision_retries: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn register_callbacks(&self, meter: &Meter) {
        let pending_firmware_updates = self.pending_firmware_updates.clone();
        let active_firmware_updates = self.active_firmware_updates.clone();
        let exhausted_reprovision_retries = self.exhausted_reprovision_retries.clone();
        meter
            .u64_observable_gauge("carbide_pending_host_firmware_update_count")
            .with_description("Number of host machines in the system that need a firmware update.")
            .with_callback(move |observer| {
                observer.observe(pending_firmware_updates.load(Ordering::Relaxed), &[])
            })
            .build();
        meter
            .u64_observable_gauge("carbide_active_host_firmware_update_count")
            .with_description(
                "Number of host machines in the system currently working on updating their firmware.",
            )
            .with_callback(move |observer|
                observer.observe(active_firmware_updates.load(Ordering::Relaxed), &[]))
            .build();
        meter
            .u64_observable_gauge("carbide_exhausted_reprovision_retry_count")
            .with_description(
                "Number of host machines in the system whose host firmware upgrade retry budget is exhausted.",
            )
            .with_callback(move |observer|
                observer.observe(exhausted_reprovision_retries.load(Ordering::Relaxed), &[]))
            .build();
    }
}
