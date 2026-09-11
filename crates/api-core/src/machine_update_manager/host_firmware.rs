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
use db::{self, desired_firmware, host_firmware_config};
use model::machine::ManagedHostStateSnapshot;
use model::machine_update_module::HOST_FW_UPDATE_HEALTH_REPORT_SOURCE;
use opentelemetry::metrics::Meter;
use sqlx::PgConnection;
use tokio::sync::Mutex;

use super::machine_update_module::MachineUpdateModule;
use super::metrics::{FirmwareUpdatePhase, FirmwareUpdateProgress, FirmwareUpdateTarget};
use crate::CarbideResult;
use crate::cfg::file::CarbideConfig;

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
        available_updates: i32,
        updating_host_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        let mut txn = db::Transaction::begin(pool).await?;
        if let Ok(mut firmware_catalog_last_read) = self.firmware_catalog_last_read.try_lock() {
            let catalog_marker = self.firmware_catalog_marker(&mut txn).await?;
            if firmware_catalog_last_read.as_ref() != Some(&catalog_marker) {
                // Save the firmware config in an SQL table so that we can filter for hosts with non-matching firmware there.
                let fw_config_snapshot = self.effective_firmware_config_snapshot(&mut txn).await?;
                tracing::info!(
                    firmware_config_snapshot = ?fw_config_snapshot,
                    "Firmware config now",
                );
                let models = fw_config_snapshot.into_values().collect::<Vec<_>>();
                desired_firmware::snapshot_desired_firmware(&mut txn, &models).await?;
                *firmware_catalog_last_read = Some(catalog_marker);
            }
        }

        let machine_updates = self.check_for_updates(&mut txn, available_updates).await?;
        let mut updates_started = HashSet::default();
        self.metrics
            .pending_firmware_updates
            .store(machine_updates.len() as u64, Ordering::Relaxed);

        for machine_update in machine_updates.iter() {
            if updating_host_machines.contains(machine_update) {
                continue;
            }

            db::host_machine_update::trigger_host_reprovisioning_request(
                &mut txn,
                "Automated",
                machine_update,
            )
            .await?;

            // Counted after the trigger succeeds; the commit below spans the
            // whole batch, so a later DB error can re-count machines on the
            // next pass -- the same repeat-on-retry the old log line had.
            carbide_instrument::emit(FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::Host,
                phase: FirmwareUpdatePhase::Started,
                machine_id: *machine_update,
                detail: String::new(),
            });
            updates_started.insert(*machine_update);
        }

        txn.commit().await?;
        Ok(updates_started)
    }

    async fn clear_completed_updates(&self, txn: &mut PgConnection) -> CarbideResult<()> {
        let completed = db::host_machine_update::find_completed_updates(txn).await?;

        for machine in completed {
            db::machine::remove_health_report(
                txn,
                &machine,
                health_report::HealthReportApplyMode::Merge,
                HOST_FW_UPDATE_HEALTH_REPORT_SOURCE,
            )
            .await?;
            db::machine::update_update_complete(&machine, true, txn).await?;
            carbide_instrument::emit(FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::Host,
                phase: FirmwareUpdatePhase::Completed,
                machine_id: machine,
                detail: String::new(),
            });
        }
        Ok(())
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

    async fn firmware_catalog_marker(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<FirmwareCatalogMarker> {
        Ok(FirmwareCatalogMarker {
            firmware_dir_mod_time: self.firmware_config.config_update_time(),
            host_firmware_config_summary: host_firmware_config::summary(txn).await?,
        })
    }

    async fn effective_firmware_config_snapshot(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<carbide_firmware::FirmwareConfigSnapshot> {
        let host_firmware_configs = host_firmware_config::list_configs(txn).await?;
        Ok(self
            .firmware_config
            .create_snapshot_with_overrides(host_firmware_configs))
    }

    pub(super) async fn check_for_updates(
        &self,
        txn: &mut PgConnection,
        mut available_updates: i32,
    ) -> CarbideResult<Vec<HostMachineId>> {
        let mut machines = vec![];
        if available_updates == 0 {
            return Ok(machines);
        };
        // find_upgrade_needed filters for just things that need upgrades
        for update_needed in db::host_machine_update::find_upgrade_needed(
            txn,
            self.config.firmware_global.autoupdate,
            self.config.firmware_global.instance_updates_manual_tagging,
        )
        .await?
        {
            if available_updates == 0 {
                return Ok(machines);
            };
            if self
                .config
                .firmware_global
                .host_disable_autoupdate
                .iter()
                .any(|x| **x == update_needed.id.to_string())
            {
                // This machine is specifically disabled
                break;
            }
            available_updates -= 1;
            machines.push(update_needed.id);
        }
        Ok(machines)
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
