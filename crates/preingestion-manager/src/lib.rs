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

mod bfb_rshim_copier;
mod config;
mod errors;
mod metrics;
mod rack_firmware;

use std::collections::HashMap;
use std::default::Default;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use carbide_firmware::{
    FirmwareConfigSnapshot, FirmwareDownloader, ResolvedFirmwareArtifactSource,
    resolve_files_firmware_artifact,
};
use carbide_instrument::emit;
use carbide_rack_controller::firmware_object::FirmwareObjectFetcher;
use carbide_redfish::libredfish::conv::IntoLibredfish;
use carbide_redfish::libredfish::{RedfishClientCreationError, RedfishClientPool};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials,
};
use carbide_utils::periodic_timer::PeriodicTimer;
use chrono::{DateTime, Utc};
use component_manager::compute_tray_manager::ComputeTrayManager;
pub use config::PreingestionManagerConfig;
use db::work_lock_manager::WorkLockManagerHandle;
use db::{DatabaseError, WithTransaction};
use futures_util::FutureExt;
use libredfish::model::task::TaskState;
use libredfish::model::update_service::TransferProtocolType;
use libredfish::{PowerState, Redfish, RedfishError, SystemPowerControl};
use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use model::rack_type::RackProfileConfig;
use model::site_explorer::{
    BlueFieldOperatingMode, ExploredEndpoint, InitialBmcResetPhase, InitialResetPhase,
    PowerDrainState, PreingestionState, TimeSyncResetPhase,
};
use opentelemetry::metrics::Meter;
use sqlx::PgPool;
use tokio::fs::File;
use tokio::io::AsyncBufReadExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::bfb_rshim_copier::BfbRshimCopier;
use crate::errors::{PreingestionManagerError, PreingestionManagerResult};
use crate::metrics::{
    BfbCopyFinished, FirmwareComponentLabel, FirmwareUpgradeTaskFinished, FirmwareUploadFinished,
    FirmwareUploadMethod, MultipartFirmwareUploadUnsupported, PowerControlLog, PowerControlStep,
    PowerOperation, PreingestionMetrics, UpgradeTaskFinalState, instrument_power_op,
};

const NOT_FOUND: u16 = 404;
const LEGACY_NO_URL_SENTINEL: &str = "file://dev/null";

/// Fallback timeout for BFB copy. SSH layer has 30-min timeout that should fire first;
/// this catches edge cases where the task dies without reporting.
const BFB_COPY_TIMEOUT_MINS: i64 = 35;

/// Minimum wait time before checking if BFB installation completed.
const BFB_INSTALLATION_MIN_WAIT_MINS: i64 = 10;

/// Timeout for waiting for DPU to report valid firmware after BFB installation.
const BFB_INSTALLATION_TIMEOUT_MINS: i64 = 45;

/// How many times to attempt issuing the one-shot `InitialBMCReset` before
/// giving up and proceeding with preingestion without it. Failing to issue the
/// reset shouldn't block ingestion: the BMC is presumably still up, we just
/// lose the inventory refresh.
const INITIAL_BMC_RESET_MAX_ATTEMPTS: u32 = 3;

/// How many times to attempt configuring site NTP servers before giving up.
const SET_NTP_SERVERS_MAX_ATTEMPTS: u32 = 3;
/// How long to wait for NTP servers to converge before checking time sync.
const SET_NTP_SERVERS_CONVERGENCE_WAIT: chrono::TimeDelta = chrono::TimeDelta::minutes(1);

pub struct PreingestionManager {
    static_info: Arc<PreingestionManagerStatic>,
    metric_holder: Arc<metrics::MetricHolder>,
    database_connection: PgPool,
}

#[derive(Clone)]
struct PreingestionManagerStatic {
    config: PreingestionManagerConfig,
    redfish_client_pool: Arc<dyn RedfishClientPool>,
    downloader: FirmwareDownloader,
    upload_limiter: Arc<Semaphore>,
    upgrade_script_state: Arc<UpdateScriptManager>,
    credential_reader: Option<Arc<dyn CredentialReader>>,
    work_lock_manager_handle: WorkLockManagerHandle,
    bfb_rshim_copier: Arc<BfbRshimCopier>,
    bfb_copy_state: Arc<BfbCopyManager>,
    bfb_copy_limiter: Arc<Semaphore>,
    ntp_servers: Vec<Ipv4Addr>,
    rack_firmware: Option<RackFirmwareDependencies>,
}

#[derive(Clone)]
struct RackFirmwareDependencies {
    rack_profiles: RackProfileConfig,
    compute_tray: Arc<dyn ComputeTrayManager>,
    firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,
}

impl PreingestionManager {
    const ITERATION_WORK_KEY: &'static str = "PreingestionManager::run_single_iteration";

    // Note on allow(too_many_arguments): It's normal for long-running services like this, which
    // integrate a lot of other services together (redfish, credentialss, config, work manager,
    // etc.) to require a lot of arguments. We can't really avoid this.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database_connection: sqlx::PgPool,
        config: PreingestionManagerConfig,
        redfish_client_pool: Arc<dyn RedfishClientPool>,
        meter: Meter,
        downloader: Option<FirmwareDownloader>,
        upload_limiter: Option<Arc<Semaphore>>,
        credential_reader: Option<Arc<dyn CredentialReader>>,
        work_lock_manager_handle: WorkLockManagerHandle,
        ntp_servers: Vec<Ipv4Addr>,
    ) -> PreingestionManager {
        let hold_period = config
            .run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, hold_period));

        let bfb_rshim_copier = Arc::new(BfbRshimCopier::new(credential_reader.clone()));

        PreingestionManager {
            static_info: Arc::new(PreingestionManagerStatic {
                redfish_client_pool,
                downloader: downloader.unwrap_or_default(),
                upload_limiter: upload_limiter.unwrap_or(Arc::new(Semaphore::new(5))),
                upgrade_script_state: Default::default(),
                credential_reader,
                work_lock_manager_handle,
                bfb_rshim_copier,
                bfb_copy_state: Default::default(),
                bfb_copy_limiter: Arc::new(Semaphore::new(config.max_concurrent_bfb_copies)),
                ntp_servers,
                rack_firmware: None,
                config,
            }),
            metric_holder,
            database_connection,
        }
    }

    /// Enable rack-profile firmware-object handling with an RMS compute-tray backend.
    pub fn with_rack_firmware(
        mut self,
        rack_profiles: RackProfileConfig,
        compute_tray: Arc<dyn ComputeTrayManager>,
        firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,
    ) -> Self {
        Arc::make_mut(&mut self.static_info).rack_firmware = Some(RackFirmwareDependencies {
            rack_profiles,
            compute_tray,
            firmware_object_fetcher,
        });

        self
    }

    pub fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        join_set
            .build_task()
            .name("preingestion_manager")
            .spawn(async move { self.run(cancel_token).await })?;
        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.static_info.config.run_interval);
        loop {
            let tick = timer.tick();
            let res = self.run_single_iteration().await;

            if let Err(e) = &res {
                tracing::warn!(error = %e, "Preingestion manager error");
            }

            // If we were able to go through everything (few or no uploads), or if we ran into a database error,
            // we will wait before checking if new state changes need to happen.
            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("Preingestion manager stop was requested");
                    return;
                }
            }
        }
    }

    /// run_single_iteration runs a single iteration of the state machine across all explored endpoints in the preingestion state.
    /// Returns true if we stopped early due to a timeout.
    pub async fn run_single_iteration(&self) -> PreingestionManagerResult<()> {
        let mut metrics = PreingestionMetrics::new();
        let db = self.database_connection.clone();

        let _work_lock = match self
            .static_info
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(e) => {
                // Unable to obtain the lock, we'll sleep and try again later.  There must be another instance of carbide-api running.
                tracing::warn!(
                    work_key = Self::ITERATION_WORK_KEY,
                    error = %e,
                    "Unable to acquire lock; will try again on next iteration"
                );
                return Ok(());
            }
        };

        let items = db::explored_endpoints::find_preingest_not_waiting_not_error(&db)
            .boxed()
            .await?;

        if !items.is_empty() && items.len() < 3 {
            // Show states if a modest amount, just count otherwise
            tracing::debug!(
                item_count = items.len(),
                items = ?items
                    .iter()
                    .map(|x| format!("({}: {:?})", x.address, x.preingestion_state))
                    .collect::<Vec<String>>(),
                "Preingestion manager working on items"
            );
        } else {
            tracing::debug!(
                item_count = items.len(),
                "Preingestion manager working on items"
            )
        }

        // Limit the number of concurrent preingestion tasks.
        // This does not affect how many endpoints are done in a single iteration, it just avoids opening
        // too many simultaneous postgres transactions, which can cause things to deadlock.
        let limit_sem = Arc::new(Semaphore::new(self.static_info.config.concurrency_limit));
        let mut task_set = JoinSet::new();

        for endpoint in items.into_iter() {
            let permit = limit_sem.clone().acquire_owned().await.unwrap();
            let static_info = self.static_info.clone();
            let db = db.clone();
            let _abort_handle = task_set
                .build_task()
                .name(&format!("preingestion {}", endpoint.address))
                .spawn(async move {
                    let _permit = permit; // retain semaphore until we're done
                    one_endpoint(&db, &endpoint, static_info).await
                });
        }

        while let Some(result) = task_set.join_next().await {
            match result {
                Ok(result) => match result {
                    Ok(result) => {
                        if result.delayed_upgrade {
                            metrics.delayed_uploading += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Error handling preingestion update");
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "Error handling preingestion update");
                }
            }
        }

        metrics.machines_in_preingestion =
            db::explored_endpoints::count_preingest_not_waiting_not_error(&db)
                .await?
                .max(0) as usize;
        metrics.waiting_for_installation = db::explored_endpoints::count_preingest_installing(&db)
            .await?
            .max(0) as usize;

        tracing::debug!(
            machines_in_preingestion = metrics.machines_in_preingestion,
            waiting_for_installation = metrics.waiting_for_installation,
            delayed_uploading = metrics.delayed_uploading,
            "Preingestion metrics updated"
        );
        self.metric_holder.update_metrics(metrics);

        Ok(())
    }
}

struct EndpointResult {
    delayed_upgrade: bool,
}

async fn one_endpoint(
    db: &PgPool,
    endpoint: &ExploredEndpoint,
    static_info: Arc<PreingestionManagerStatic>,
) -> PreingestionManagerResult<EndpointResult> {
    tracing::debug!(?endpoint, "Running preingestion");

    // BFB-related preingestion doesn't work for a DPU running in NIC
    // mode -- the Arm OS doesn't boot, so the `in_bfb_installation_wait`
    // call to `check_dpu_console_install_complete` (which literally greps
    // the microcom for strings) never succeeds, and we eventually will
    // hit SLA and fail.
    //
    // Soo, we need to skip past a couple of BFB states:
    // - BfbRecoveryNeeded -- don't even enter to begin with.
    // - BfbInstallationWait -- if we've gotten to this point,
    //   but we're in NIC mode, just transition ahead (we'll be=
    //   waiting forever otherwise).
    //
    // ..but, intentionally leave `BfbPlatformPowercycle` and
    // `BfbCopyInProgress` alone (if they're in it). They're
    // mid-flight states where skipping risks leaving
    // the host powered off, or leaving the spawned SSH copy
    // task orphaned. Existing timeouts will surface if something
    // fails, but that's fine -- those should succeed regardless
    // of NIC mode or DPU mode.
    //
    // This basically just mirrors the `in_bfb_platform_powercycle`
    // post-install handling, letting the site-explorer pairing and
    // remediation loops to continue on.
    let endpoint_host_bmc_ip = match &endpoint.preingestion_state {
        PreingestionState::BfbRecoveryNeeded { host_bmc_ip, .. }
        | PreingestionState::BfbInstallationWait { host_bmc_ip, .. } => Some(*host_bmc_ip),
        _ => None,
    };
    if let Some(host_bmc_ip) = endpoint_host_bmc_ip
        && endpoint.report.bluefield_operating_mode() == Some(BlueFieldOperatingMode::Nic)
    {
        tracing::info!(
            bmc_ip_address = %endpoint.address,
            host_bmc_ip_address = %host_bmc_ip,
            previous_state = ?endpoint.preingestion_state,
            "DPU is in NIC mode; skipping BFB preingestion path and marking complete",
        );
        db.with_txn(|txn| {
            async move {
                db::explored_endpoints::set_preingestion_complete(endpoint.address, txn).await?;
                db::explored_endpoints::set_waiting_for_explorer_refresh(endpoint.address, txn)
                    .await?;
                db::explored_endpoints::set_pause_remediation(host_bmc_ip, false, txn).await?;
                Ok::<(), DatabaseError>(())
            }
            .boxed()
        })
        .await??;
        return Ok(EndpointResult {
            delayed_upgrade: false,
        });
    }

    // Main state machine match.
    let delayed_upgrade = match &endpoint.preingestion_state {
        PreingestionState::Initial => {
            // Kick off a one-shot BMC reset before the time-sync / firmware
            // checks so pairing sees a stable BMC.
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_initial_bmc_reset(
                    endpoint.address,
                    InitialBmcResetPhase::Start { attempts: 0 },
                    txn,
                )
                .boxed()
            })
            .await??;
            false
        }
        PreingestionState::InitialBMCReset { phase } => {
            static_info.initial_bmc_reset(db, endpoint, phase).await?
        }
        PreingestionState::SetNtpServers { set_at, attempts } => {
            static_info
                .set_ntp_servers(db, endpoint, set_at.as_ref(), *attempts)
                .await?
        }
        PreingestionState::RecheckVersionsAfterFailure { .. } => {
            static_info
                .start_firmware_uploads_or_continue(db, endpoint, true)
                .await?
        }
        PreingestionState::RecheckVersions => {
            static_info
                .start_firmware_uploads_or_continue(db, endpoint, true)
                .await?
        }
        PreingestionState::InitialReset { phase, last_time } => {
            static_info
                .pre_update_resets(db, endpoint, Some(phase), Some(last_time))
                .await?;
            false
        }
        PreingestionState::TimeSyncReset {
            phase,
            last_time,
            attempt,
        } => {
            static_info
                .time_sync_resets(db, endpoint, phase, Some(last_time), *attempt)
                .await?;
            false
        }
        PreingestionState::RackFirmwareUpdateWait { .. } => {
            static_info.wait_for_rack_firmware(db, endpoint).await?;
            false
        }
        PreingestionState::UpgradeFirmwareWait {
            task_id,
            final_version,
            upgrade_type,
            power_drains_needed,
            firmware_number,
        } => {
            static_info
                .in_upgrade_firmware_wait(
                    db,
                    &InUpgradeFirmwareWaitArgs {
                        endpoint,
                        task_id,
                        final_version,
                        upgrade_type,
                        power_drains_needed,
                        firmware_number: firmware_number.unwrap_or(0),
                    },
                )
                .await?;
            false
        }
        details @ PreingestionState::ResetForNewFirmware { .. } => {
            static_info
                .in_reset_for_new_firmware(db, endpoint, details)
                .await?;
            false
        }
        PreingestionState::NewFirmwareReportedWait {
            final_version,
            upgrade_type,
            previous_reset_time,
        } => {
            static_info
                .in_new_firmware_reported_wait(
                    db,
                    endpoint,
                    final_version,
                    upgrade_type,
                    previous_reset_time,
                )
                .await?;
            false
        }
        PreingestionState::ScriptRunning => {
            static_info.waiting_for_script(db, endpoint).await?;
            false
        }
        PreingestionState::BfbRecoveryNeeded {
            reason: _,
            host_bmc_ip,
            pre_copy_powercycle,
        } => {
            static_info
                .in_bfb_recovery_needed(db, endpoint, host_bmc_ip, *pre_copy_powercycle)
                .await?;
            false
        }
        PreingestionState::BfbPlatformPowercycle {
            host_bmc_ip,
            phase,
            post_install,
        } => {
            static_info
                .in_bfb_platform_powercycle(db, endpoint, host_bmc_ip, phase, *post_install)
                .await?;
            false
        }
        PreingestionState::BfbCopyInProgress {
            started_at,
            host_bmc_ip,
        } => {
            static_info
                .in_bfb_copy_in_progress(db, endpoint, started_at, host_bmc_ip)
                .await?;
            false
        }
        PreingestionState::BfbInstallationWait {
            started_at,
            host_bmc_ip,
        } => {
            static_info
                .in_bfb_installation_wait(db, endpoint, started_at, host_bmc_ip)
                .await?;
            false
        }
        PreingestionState::Complete => {
            // This should have been filtered out by the query that got us this list.
            tracing::warn!(
                ?endpoint,
                "Endpoint showed complete preingestion and should not have been here"
            );
            false
        }
        PreingestionState::Failed { .. } => {
            // There was a serious failure, we never automatically leave this state and wait for a force delete
            false
        }
    };

    Ok(EndpointResult { delayed_upgrade })
}

impl PreingestionManagerStatic {
    async fn firmware_config_snapshot(
        &self,
        db: &PgPool,
    ) -> PreingestionManagerResult<FirmwareConfigSnapshot> {
        let host_firmware_configs = db::host_firmware_config::list_configs(db).await?;
        Ok(self
            .config
            .firmware
            .create_snapshot_with_overrides(host_firmware_configs))
    }

    /// find_fw_info_for_host looks up the firmware config for the given endpoint
    async fn find_fw_info_for_host(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<Option<Firmware>> {
        Ok(self
            .firmware_config_snapshot(db)
            .await?
            .find_fw_info_for_host(endpoint))
    }

    /// check_firmware_versions_below_preingestion will check if we actually need to do firmware upgrades before
    /// ingestion can happen, and either kick them off if so otherwise move on.
    async fn check_firmware_versions_below_preingestion(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<bool> {
        if self.start_rack_firmware(db, endpoint).await? {
            return Ok(false);
        }

        // First, we need to check if it's appropriate to upgrade at this point or wait until later.
        let fw_info = match self.find_fw_info_for_host(db, endpoint).await? {
            None => {
                tracing::debug!(
                    bmc_ip_address = %endpoint.address,
                    "No matching firmware info found during preingestion check"
                );
                // No desired firmware description found for this host, nothing to do.
                // This is the expected path for DPUs.
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_complete(endpoint.address, txn).boxed()
                })
                .await??;
                return Ok(false);
            }
            Some(fw_info) => fw_info,
        };
        for (fwtype, desc) in &fw_info.components {
            if let Some(min_preingestion) = &desc.preingest_upgrade_when_below
                && let Some(current) = endpoint.find_version(&fw_info, *fwtype)
            {
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    firmware_type = ?fwtype,
                    ?min_preingestion,
                    ?current,
                    "Checking firmware version against preingestion minimum"
                );

                if version_compare::compare(current, min_preingestion)
                    .is_ok_and(|c| c == version_compare::Cmp::Lt)
                {
                    tracing::info!(
                        bmc_ip_address = %endpoint.address,
                        firmware_type = ?fwtype,
                        "Starting firmware upload during preingestion check"
                    );
                    // One or both of the versions are low enough to absolutely need upgrades first - do them both while we're at it.
                    let delayed_upgrade = self
                        .start_firmware_uploads_or_continue(db, endpoint, false)
                        .await?;
                    return Ok(delayed_upgrade);
                } else {
                    tracing::info!(
                        bmc_ip_address = %endpoint.address,
                        firmware_type = ?fwtype,
                        "Firmware version satisfies preingestion minimum"
                    );
                }
            }
        }

        tracing::debug!(
            bmc_ip_address = %endpoint.address,
            "Firmware versions satisfy preingestion requirements; marking complete"
        );
        // Good enough for now at least, proceed with ingestion.
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_complete(endpoint.address, txn).boxed()
        })
        .await??;
        Ok(false)
    }

    /// start_firmware_uploads_or_continue will start a firmware upgrade if any of the endpoint's versions
    /// do not match the desired version.  If they all match, it will continue on.  The upload must complete
    /// before we return; this means only one upload happens at a time, but we don't expect that doing multiples
    /// would make a significant difference, as we're limited by our own upload bandwidth.
    async fn start_firmware_uploads_or_continue(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        repeat: bool,
    ) -> PreingestionManagerResult<bool> {
        if self.start_rack_firmware(db, endpoint).await? {
            return Ok(false);
        }

        if endpoint.waiting_for_explorer_refresh {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                "Waiting for explorer refresh before continuing firmware uploads"
            );
            // We've updated something and are waiting for site explorer to get back around to it
            return Ok(false);
        }

        // Determine if auto updates should be enabled.
        // We can't check machine IDs here as they may not be available yet, so use the global value only.
        if !self.config.autoupdate {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                "Automatic firmware updates disabled"
            );
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_complete(endpoint.address, txn).boxed()
            })
            .await??;
            return Ok(false);
        }

        let fw_info = match self.find_fw_info_for_host(db, endpoint).await? {
            None => {
                // No desired firmware description found for this host
                tracing::debug!(
                    bmc_ip_address = %endpoint.address,
                    "No firmware info found while starting firmware uploads"
                );

                return Ok(false);
            }
            Some(fw_info) => fw_info,
        };

        // Specifying ordering is optional, and defaults to first BMC then UEFI.
        let mut ordering = fw_info.ordering().clone();
        if ordering.is_empty() {
            ordering.push(FirmwareComponentType::Bmc);
            ordering.push(FirmwareComponentType::Uefi);
        }
        for upgrade_type in ordering {
            let (done, delayed_upgrade) = self
                .start_upgrade_if_needed(db, endpoint, &fw_info, upgrade_type, repeat)
                .await?;

            if done {
                // We started something and need to wait now, or we had a valid reason not to start and will retry later.
                // In the former case only, the state has been updated.
                return Ok(delayed_upgrade);
            }
        }

        tracing::debug!(
            bmc_ip_address = %endpoint.address,
            "No further firmware updates needed"
        );

        // Nothing needed to be updated, we're complete.
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_complete(endpoint.address, txn).boxed()
        })
        .await??;

        Ok(false)
    }

    /// First bool is true if started an upgrade, or for some other reason shouldn't check for updating other firmwares.  Second is if we delayed the update.
    async fn start_upgrade_if_needed(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        fw_info: &Firmware,
        fw_type: FirmwareComponentType,
        repeat: bool,
    ) -> Result<(bool, bool), DatabaseError> {
        {
            match need_upgrade(endpoint, fw_info, fw_type) {
                None => {
                    tracing::debug!(
                        bmc_ip_address = %endpoint.address,
                        firmware_type = ?fw_type,
                        "Firmware upgrade not needed"
                    );
                    Ok((false, false))
                }
                Some(to_install) => {
                    if to_install.script.is_some() {
                        self.by_script(db, endpoint.address, &to_install).await?;
                        return Ok((true, false));
                    }
                    let Ok(_active) = self.upload_limiter.try_acquire() else {
                        tracing::debug!(
                            bmc_ip_address = %endpoint.address,
                            ?to_install,
                            "Deferring firmware installation because too many uploads are active"
                        );
                        return Ok((true, true)); // Don't check others
                    };

                    if !repeat && to_install.pre_update_resets {
                        self.pre_update_resets(db, endpoint, None, None).await?;
                        return Ok((true, false));
                    }

                    tracing::info!(
                        bmc_ip_address = %endpoint.address,
                        ?to_install,
                        "Installing firmware"
                    );

                    self.initiate_update(endpoint, &to_install, &fw_type, 0, db)
                        .await?;

                    // initiate_update only returned an error for database issues.  If it truly succeeded, it updated
                    // the database with a new state.  If the firmware download was not yet complete or we had a Redfish
                    // problem (BMC is down or died in the middle, etc.) it will not have returned an error, but it will
                    // not have updated the state either; we will retry the update on the next go.  Either way, we return
                    // true so that we won't try updating other firmware.

                    Ok((true, false))
                }
            }
        }
    }

    /// in_upgrade_firmware_wait triggers when we are waiting for installation of firmware after an upload.
    async fn in_upgrade_firmware_wait(
        &self,
        db: &PgPool,
        args: &InUpgradeFirmwareWaitArgs<'_>,
    ) -> PreingestionManagerResult<()> {
        let (endpoint, task_id, final_version, upgrade_type, power_drains_needed, firmware_number) = (
            args.endpoint,
            args.task_id,
            args.final_version,
            args.upgrade_type,
            args.power_drains_needed,
            args.firmware_number,
        );

        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Redfish connection failed"
                );
                return Ok(());
            }
        };

        match redfish_client.get_task(task_id).await {
            Ok(task_info) => {
                match task_info.task_state {
                    Some(TaskState::New)
                    | Some(TaskState::Starting)
                    | Some(TaskState::Running)
                    | Some(TaskState::Pending) => {
                        tracing::debug!(
                            bmc_ip_address = %endpoint.address,
                            task_state = ?task_info.task_state,
                            task_messages = ?task_info.messages,
                            "Firmware upgrade task not yet complete"
                        );
                    }
                    Some(TaskState::Completed) => {
                        // Task has completed, update is done and we can clean up.  Site explorer will ingest this next time it runs on this endpoint.
                        //
                        // The completion event fires only once the state moves
                        // on (the next chain task is stored, or the transition
                        // commits): this same Completed task is re-observed on
                        // every pass until then, and a rolled-back transition
                        // never happened.

                        // If we have multiple firmware files to be uploaded, do the next one.
                        if let Some(fw_info) = self.find_fw_info_for_host(db, endpoint).await?
                            && let Some(component_info) = fw_info.components.get(upgrade_type)
                            && let Some(selected_firmware) =
                                select_firmware_for_artifact_continuation(
                                    component_info,
                                    final_version,
                                )
                        {
                            let firmware_number = firmware_number + 1;
                            if usize::try_from(firmware_number).is_ok_and(|firmware_number| {
                                firmware_number < selected_firmware.artifact_count()
                            }) {
                                tracing::info!(
                                    bmc_ip_address = %endpoint.address,
                                    ?selected_firmware,
                                    firmware_number,
                                    "Installing firmware chain step"
                                );

                                if self
                                    .initiate_update(
                                        endpoint,
                                        selected_firmware,
                                        upgrade_type,
                                        firmware_number,
                                        db,
                                    )
                                    .await?
                                {
                                    emit(FirmwareUpgradeTaskFinished::Ok {
                                        firmware: FirmwareComponentLabel(*upgrade_type),
                                        final_state: UpgradeTaskFinalState::Completed,
                                    });
                                }
                                return Ok(());
                            }
                        }
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "Marking firmware upgrade Redfish task complete"
                        );
                        db.with_txn(|txn| {
                            db::explored_endpoints::set_preingestion_reset_for_new_firmware(
                                endpoint.address,
                                final_version,
                                upgrade_type,
                                *power_drains_needed,
                                None,
                                None,
                                txn,
                            )
                            .boxed()
                        })
                        .await??;
                        emit(FirmwareUpgradeTaskFinished::Ok {
                            firmware: FirmwareComponentLabel(*upgrade_type),
                            final_state: UpgradeTaskFinalState::Completed,
                        });
                        // Can immediately process as that new state
                        return self
                            .in_reset_for_new_firmware(
                                db,
                                endpoint,
                                &PreingestionState::ResetForNewFirmware {
                                    final_version: final_version.to_string(),
                                    upgrade_type: *upgrade_type,
                                    power_drains_needed: *power_drains_needed,
                                    delay_until: None,
                                    last_power_drain_operation: None,
                                },
                            )
                            .await;
                    }
                    Some(
                        state @ (TaskState::Exception
                        | TaskState::Interrupted
                        | TaskState::Killed
                        | TaskState::Cancelled),
                    ) => {
                        let task_message = task_info
                            .messages
                            .last()
                            .map_or(String::new(), |m| m.message.clone());
                        let msg = format!(
                            "Failure in firmware upgrade for {}: {} {:?}",
                            endpoint.address, state, task_message,
                        );

                        db.with_txn(|txn| {
                            async move {
                                // Wait for site explorer to refresh it then try again after that.
                                db::explored_endpoints::set_preingestion_recheck_versions_reason(
                                    endpoint.address,
                                    msg,
                                    txn,
                                )
                                .await?;
                                db::explored_endpoints::re_explore_if_version_matches(
                                    endpoint.address,
                                    endpoint.report_version,
                                    txn,
                                )
                                .await?;

                                // We need site explorer to requery the version
                                db::explored_endpoints::set_waiting_for_explorer_refresh(
                                    endpoint.address,
                                    txn,
                                )
                                .await?;
                                Ok::<_, DatabaseError>(())
                            }
                            .boxed()
                        })
                        .await??;
                        // The event owns the historical WARN line and counts the
                        // failure, so an endpoint failing repeatedly is visible
                        // as a moving error series. It fires only after the
                        // transition commits -- a rolled-back pass re-observes
                        // this task and retries.
                        emit(FirmwareUpgradeTaskFinished::Error {
                            firmware: FirmwareComponentLabel(*upgrade_type),
                            final_state: UpgradeTaskFinalState::from_failed_task_state(state),

                            address: endpoint.address,
                            error: task_message,
                        });
                    }
                    _ => {
                        // Unexpected state
                        tracing::warn!(
                            bmc_ip_address = %endpoint.address,
                            task_state = ?task_info.task_state,
                            "Unrecognized firmware upgrade task state"
                        );
                    }
                };
            }
            Err(e) => match e {
                RedfishError::HTTPErrorCode { status_code, .. } => {
                    if status_code == NOT_FOUND {
                        // Dells (maybe others) have been observed to not have report the job any more after completing a host reboot for a UEFI upgrade.  If we get a 404 but see that we're at the right version, we're done with that upgrade.
                        if let Some(fw_info) = self.find_fw_info_for_host(db, endpoint).await?
                            && let Some(current_version) =
                                endpoint.find_version(&fw_info, *upgrade_type)
                            && current_version == final_version
                        {
                            tracing::debug!(
                                bmc_ip_address = %endpoint.address,
                                "Marking missing firmware upgrade Redfish task complete"
                            );
                            db.with_txn(|txn| {
                                db::explored_endpoints::set_preingestion_recheck_versions(
                                    endpoint.address,
                                    txn,
                                )
                                .boxed()
                            })
                            .await??;
                            // The BMC dropped the task record after finishing;
                            // the confirmed new version is the completion
                            // evidence, so this task still counts as completed.
                            emit(FirmwareUpgradeTaskFinished::Ok {
                                firmware: FirmwareComponentLabel(*upgrade_type),
                                final_state: UpgradeTaskFinalState::Completed,
                            });
                        }
                    }
                }
                _ => {
                    tracing::warn!(
                        bmc_ip_address = %endpoint.address,
                        error = %e,
                        "Getting Redfish task failed"
                    );
                }
            },
        };
        Ok(())
    }

    async fn in_reset_for_new_firmware(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        state: &PreingestionState,
    ) -> PreingestionManagerResult<()> {
        let (
            final_version,
            upgrade_type,
            power_drains_needed,
            delay_until,
            last_power_drain_operation,
        ) = match state {
            PreingestionState::ResetForNewFirmware {
                final_version,
                upgrade_type,
                power_drains_needed,
                delay_until,
                last_power_drain_operation,
            } => (
                final_version,
                upgrade_type,
                power_drains_needed,
                delay_until,
                last_power_drain_operation,
            ),
            _ => {
                return Err(PreingestionManagerError::InvalidArgument(
                    "Wrong enum in_reset_for_new_firmware".to_string(),
                ));
            }
        };

        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                tracing::error!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Redfish connection failed"
                );
                return Ok(());
            }
        };

        let mut need_wait = false;
        // Still not reporting the new version.
        // If this is the UEFI, we need to request a reboot.  Otherwise, we just need to keep waiting.
        // The version reported doesn't update until the end of the UEFI portion of the boot, which can be quite a long wait.

        if let Some(power_drains_needed) = power_drains_needed {
            if let Some(delay_until) = delay_until
                && *delay_until > chrono::Utc::now().timestamp()
            {
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    ?last_power_drain_operation,
                    "Waiting after power drain operation"
                );
                return Ok(());
            }

            match last_power_drain_operation {
                None | Some(PowerDrainState::On) => {
                    // The 1000 is for unit tests; values above this will skip delays.
                    if *power_drains_needed == 0 || *power_drains_needed == 1000 {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "Firmware upgrade power drains complete"
                        );
                        // This path, and only this path of the match, exits the match and lets us proceed.  All others should return after updating state.
                        need_wait = false; // We've reset multiple times already and should be reporting the new version
                    } else {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            required_power_drain_count = *power_drains_needed,
                            "Firmware upgrade task complete; initiating required power drain"
                        );
                        if instrument_power_op(
                            PowerOperation::ForceOff,
                            redfish_client.power(SystemPowerControl::ForceOff),
                            PowerControlLog::Step {
                                bmc_ip_address: endpoint.address,
                                step: PowerControlStep::PowerOff,
                            },
                        )
                        .await
                        .is_err()
                        {
                            return Ok(());
                        }

                        // Wait 60 seconds after powering off to do AC powercycle
                        let delay = if *power_drains_needed < 1000 {
                            time::Duration::seconds(60)
                        } else {
                            time::Duration::seconds(0)
                        };
                        db.with_txn(|txn| {
                            db::explored_endpoints::set_preingestion_reset_for_new_firmware(
                                endpoint.address,
                                final_version,
                                upgrade_type,
                                Some(*power_drains_needed),
                                Some(delay),
                                Some(PowerDrainState::Off),
                                txn,
                            )
                            .boxed()
                        })
                        .await??;
                        return Ok(());
                    }
                }
                Some(PowerDrainState::Off) => {
                    if endpoint.report.vendor.unwrap_or_default().is_lenovo() {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "Starting AC power cycle"
                        );
                        match redfish_client.get_power_state().await {
                            Ok(power_state) if power_state != PowerState::Off => {
                                tracing::warn!(
                                    bmc_ip_address = %endpoint.address,
                                    %power_state,
                                    "ACPowercycle requires chassis to be Off, forcing off first"
                                );
                                if instrument_power_op(
                                    PowerOperation::ForceOff,
                                    redfish_client.power(SystemPowerControl::ForceOff),
                                    PowerControlLog::Step {
                                        bmc_ip_address: endpoint.address,
                                        step: PowerControlStep::AcPowercyclePrerequisite,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    return Ok(());
                                }
                                let delay = if *power_drains_needed < 1000 {
                                    time::Duration::seconds(60)
                                } else {
                                    time::Duration::seconds(0)
                                };
                                db.with_txn(|txn| {
                                    db::explored_endpoints::set_preingestion_reset_for_new_firmware(
                                        endpoint.address,
                                        final_version,
                                        upgrade_type,
                                        Some(*power_drains_needed),
                                        Some(delay),
                                        Some(PowerDrainState::Off),
                                        txn,
                                    )
                                    .boxed()
                                })
                                .await??;
                                return Ok(());
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::error!(
                                    bmc_ip_address = %endpoint.address,
                                    error = %e,
                                    "Failed to get power state"
                                );
                                return Ok(());
                            }
                        }
                        if instrument_power_op(
                            PowerOperation::AcPowercycle,
                            redfish_client.power(SystemPowerControl::ACPowercycle),
                            PowerControlLog::Step {
                                bmc_ip_address: endpoint.address,
                                step: PowerControlStep::AcPowercycle,
                            },
                        )
                        .await
                        .is_err()
                        {
                            return Ok(());
                        }
                    }
                    let delay = if *power_drains_needed < 1000 {
                        time::Duration::seconds(90)
                    } else {
                        time::Duration::seconds(0)
                    };
                    db.with_txn(|txn| {
                        db::explored_endpoints::set_preingestion_reset_for_new_firmware(
                            endpoint.address,
                            final_version,
                            upgrade_type,
                            Some(*power_drains_needed),
                            Some(delay),
                            Some(PowerDrainState::Powercycle),
                            txn,
                        )
                        .boxed()
                    })
                    .await??;
                    return Ok(());
                }
                Some(PowerDrainState::Powercycle) => {
                    tracing::info!(bmc_ip_address = %endpoint.address, "Turning system back on");
                    if instrument_power_op(
                        PowerOperation::On,
                        redfish_client.power(SystemPowerControl::On),
                        PowerControlLog::Step {
                            bmc_ip_address: endpoint.address,
                            step: PowerControlStep::PowerOn,
                        },
                    )
                    .await
                    .is_err()
                    {
                        return Ok(());
                    }
                    let delay = if *power_drains_needed < 1000 {
                        time::Duration::seconds(5)
                    } else {
                        time::Duration::seconds(0)
                    };
                    db.with_txn(|txn| {
                        db::explored_endpoints::set_preingestion_reset_for_new_firmware(
                            endpoint.address,
                            final_version,
                            upgrade_type,
                            Some(*power_drains_needed - 1),
                            Some(delay),
                            Some(PowerDrainState::On),
                            txn,
                        )
                        .boxed()
                    })
                    .await??;
                    return Ok(());
                }
            };
        } else if upgrade_type.is_uefi() {
            tracing::info!(
                bmc_ip_address = %endpoint.address,
                "Firmware upgrade task complete; initiating required reboot"
            );
            if instrument_power_op(
                PowerOperation::ForceRestart,
                redfish_client.power(SystemPowerControl::ForceRestart),
                PowerControlLog::Step {
                    bmc_ip_address: endpoint.address,
                    step: PowerControlStep::UefiReboot,
                },
            )
            .await
            .is_err()
            {
                return Ok(());
            }
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_new_reported_wait(
                    endpoint.address,
                    final_version,
                    upgrade_type,
                    txn,
                )
                .boxed()
            })
            .await??;

            need_wait = true;
        }
        // Lenovo and Nvidia DPU BMC needs to be manually reset after the update
        let bmc_vendor = endpoint
            .report
            .vendor
            .unwrap_or(bmc_vendor::BMCVendor::Unknown);
        if upgrade_type.is_bmc() && (bmc_vendor.is_lenovo() || bmc_vendor.is_nvidia()) {
            tracing::info!(
                bmc_ip_address = %endpoint.address,
                "Firmware upgrade task complete; initiating required BMC reboot"
            );
            if instrument_power_op(
                PowerOperation::BmcReset,
                redfish_client.bmc_reset(None),
                PowerControlLog::Step {
                    bmc_ip_address: endpoint.address,
                    step: PowerControlStep::BmcReboot,
                },
            )
            .await
            .is_err()
            {
                return Ok(());
            }
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_new_reported_wait(
                    endpoint.address,
                    final_version,
                    upgrade_type,
                    txn,
                )
                .boxed()
            })
            .await??;
            // Will not be reporting the new version yet, we need to wait.
            need_wait = true;
        }
        if *upgrade_type == FirmwareComponentType::HGXBmc
            || *upgrade_type == FirmwareComponentType::Gpu
        {
            // Needs a host power reset
            // DGX models only had an "off", GB200 (and presumably later ones) has an actual AC powercycle.
            let poweroff_style = if redfish_client.ac_powercycle_supported_by_power() {
                SystemPowerControl::ACPowercycle
            } else {
                SystemPowerControl::ForceOff
            };
            if instrument_power_op(
                poweroff_style.into(),
                redfish_client.power(poweroff_style),
                PowerControlLog::Step {
                    bmc_ip_address: endpoint.address,
                    step: PowerControlStep::PowerOff,
                },
            )
            .await
            .is_err()
            {
                return Ok(());
            }
            tokio::time::sleep(self.config.hgx_bmc_gpu_reboot_delay).await;
            if instrument_power_op(
                PowerOperation::On,
                redfish_client.power(SystemPowerControl::On),
                PowerControlLog::Step {
                    bmc_ip_address: endpoint.address,
                    step: PowerControlStep::PowerOn,
                },
            )
            .await
            .is_err()
            {
                return Ok(());
            }
            // Does not need a wait
        }

        if need_wait {
            db.with_txn(|txn| {
                db::explored_endpoints::set_waiting_for_explorer_refresh(endpoint.address, txn)
                    .boxed()
            })
            .await??;
            return Ok(());
        } else if *upgrade_type == FirmwareComponentType::Cec {
            // The reset is best-effort; the Event records either failure before this continues.
            instrument_power_op(
                PowerOperation::ChassisReset,
                redfish_client.chassis_reset("Bluefield_ERoT", SystemPowerControl::GracefulRestart),
                PowerControlLog::Step {
                    bmc_ip_address: endpoint.address,
                    step: PowerControlStep::CecChassisReset,
                },
            )
            .await
            .ok();
        }
        // No need for resets or reboots, go right to waiting for the new version to show up, and we might as well check right away.
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_new_reported_wait(
                endpoint.address,
                final_version,
                upgrade_type,
                txn,
            )
            .boxed()
        })
        .await??;

        self.in_new_firmware_reported_wait(db, endpoint, final_version, upgrade_type, &None)
            .await
    }

    async fn in_new_firmware_reported_wait(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        final_version: &str,
        upgrade_type: &FirmwareComponentType,
        previous_reset_time: &Option<i64>,
    ) -> PreingestionManagerResult<()> {
        if let Some(fw_info) = self.find_fw_info_for_host(db, endpoint).await? {
            if let Some(current_version) = endpoint.find_version(&fw_info, *upgrade_type) {
                if current_version != final_version {
                    // Still not reporting the new version.
                    if !self.config.no_reset_retries
                        && let Some(previous_reset_time) = previous_reset_time
                        && previous_reset_time + 30 * 60 <= Utc::now().timestamp()
                    {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            ?upgrade_type,
                            "Firmware upgrade has taken more than 30 minutes to report the new version; resetting again"
                        );
                        let state = &PreingestionState::ResetForNewFirmware {
                            final_version: final_version.to_string(),
                            upgrade_type: *upgrade_type,
                            power_drains_needed: None,
                            delay_until: None,
                            last_power_drain_operation: None,
                        };
                        return Box::pin(self.in_reset_for_new_firmware(db, endpoint, state)).await;
                    }
                    db.with_txn(|txn| {
                        db::explored_endpoints::set_waiting_for_explorer_refresh(
                            endpoint.address,
                            txn,
                        )
                        .boxed()
                    })
                    .await??;
                    tracing::info!(
                        bmc_ip_address = %endpoint.address,
                        %upgrade_type,
                        current_version,
                        final_version,
                        "Firmware upgrade task complete but reported version has not updated"
                    );
                    return Ok(());
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    current_version,
                    "Firmware upgrade now reports the new version"
                );
            } else {
                // This path should only happen if something strange happened with the version definitions
                tracing::error!(
                    bmc_ip_address = %endpoint.address,
                    firmware_info = ?fw_info,
                    ?upgrade_type,
                    "Could not find current version while waiting for firmware upgrade"
                );
                // Make sure we wait for the new version
                db.with_txn(|txn| {
                    db::explored_endpoints::set_waiting_for_explorer_refresh(endpoint.address, txn)
                        .boxed()
                })
                .await??;
            }
        } else {
            // This path should only happen if something strange happened with the version definitions
            tracing::error!(
                bmc_ip_address = %endpoint.address,
                vendor = ?endpoint.report.vendor,
                systems = ?endpoint.report.systems,
                "Could not find firmware info while waiting for firmware upgrade"
            );
            // Make sure we wait for the new version
            db.with_txn(|txn| {
                db::explored_endpoints::set_waiting_for_explorer_refresh(endpoint.address, txn)
                    .boxed()
            })
            .await??;
        }

        // Go back to checking versions as there may be other things that need upgrading
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_recheck_versions(endpoint.address, txn).boxed()
        })
        .await??;

        Ok(())
    }

    async fn run_initial_checks(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<bool> {
        match self.check_bmc_time_sync(db, endpoint).await {
            Ok(true) => {
                // Time is in sync, proceed with firmware version check
                self.check_firmware_versions_below_preingestion(db, endpoint)
                    .await
            }
            Ok(false) => {
                // Time is not in sync. The reset sequence powers off the host
                // and resets the BMC, which must never happen on an
                // ingested/paired (potentially tenant-assigned) machine. This
                // mirrors the managed-host guard site-explorer already applies
                // before its own remediation.
                if self.is_ingested_host(db, endpoint).await {
                    tracing::info!(
                        bmc_ip_address = %endpoint.address,
                        "BMC time is out of sync but the host is already ingested; skipping time-sync remediation"
                    );
                    return self
                        .check_firmware_versions_below_preingestion(db, endpoint)
                        .await;
                }
                // Time is not in sync, initiate reset sequence
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    "BMC time is out of sync; initiating reset to fix time synchronization"
                );
                self.time_sync_resets(db, endpoint, &TimeSyncResetPhase::Start, None, 0)
                    .await
            }
            Err(e) => {
                if let PreingestionManagerError::Internal { message } = e {
                    tracing::error!(
                        bmc_ip_address = %endpoint.address,
                        error = %message,
                        "Internal error checking BMC time sync; failing preingestion"
                    );
                    db.with_txn(|txn| {
                        db::explored_endpoints::set_preingestion_failed(
                            endpoint.address,
                            format!("Failed to check BMC time sync: {message}"),
                            txn,
                        )
                        .boxed()
                    })
                    .await??;
                } else {
                    tracing::warn!(
                        bmc_ip_address = %endpoint.address,
                        error = %e,
                        "Retryable error checking BMC time sync; will retry later"
                    );
                }
                Ok(false)
            }
        }
    }

    async fn initial_bmc_reset(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        phase: &InitialBmcResetPhase,
    ) -> PreingestionManagerResult<bool> {
        match phase {
            InitialBmcResetPhase::Start { attempts } => {
                let redfish_client = match self
                    .redfish_client_pool
                    .create_client_for_ingested_host(endpoint.address, db)
                    .await
                {
                    Ok(client) => client,
                    Err(e) => {
                        tracing::warn!(
                            bmc_ip_address = %endpoint.address,
                            error = %e,
                            "Redfish connection failed; will retry initial BMC reset"
                        );
                        return Ok(false);
                    }
                };
                let next = attempts + 1;
                if instrument_power_op(
                    PowerOperation::BmcReset,
                    redfish_client.bmc_reset(None),
                    PowerControlLog::InitialBmcReset {
                        bmc_ip_address: endpoint.address,
                        attempt: next,
                        max_attempts: INITIAL_BMC_RESET_MAX_ATTEMPTS,
                    },
                )
                .await
                .is_err()
                {
                    if next >= INITIAL_BMC_RESET_MAX_ATTEMPTS {
                        db.with_txn(|txn| {
                            db::explored_endpoints::set_preingestion_set_ntp_servers(
                                endpoint.address,
                                None,
                                0,
                                txn,
                            )
                            .boxed()
                        })
                        .await??;
                        return Ok(false);
                    }
                    db.with_txn(|txn| {
                        db::explored_endpoints::set_preingestion_initial_bmc_reset(
                            endpoint.address,
                            InitialBmcResetPhase::Start { attempts: next },
                            txn,
                        )
                        .boxed()
                    })
                    .await??;
                    return Ok(false);
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Initial BMC reset initiated; polling for BMC return"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_initial_bmc_reset(
                        endpoint.address,
                        InitialBmcResetPhase::WaitForBmc,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(false)
            }
            InitialBmcResetPhase::WaitForBmc => {
                let redfish_client = match self
                    .redfish_client_pool
                    .create_client_for_ingested_host(endpoint.address, db)
                    .await
                {
                    Ok(client) => client,
                    Err(e) => {
                        tracing::warn!(
                            bmc_ip_address = %endpoint.address,
                            error = %e,
                            "Redfish connection failed; will retry waiting for BMC"
                        );
                        return Ok(false);
                    }
                };
                match redfish_client.get_service_root().await {
                    Ok(_) => {
                        // BMC is back. Wait for a fresh exploration before running
                        // checks, so pairing/ingestion reads the post-reset
                        // inventory (e.g. a DPU that reappeared), not the stale
                        // pre-reset report.
                        let address = endpoint.address;
                        db.with_txn(|txn| {
                            async move {
                                db::explored_endpoints::set_preingestion_initial_bmc_reset(
                                    address,
                                    InitialBmcResetPhase::WaitForExplorerRefresh,
                                    txn,
                                )
                                .await?;
                                db::explored_endpoints::set_waiting_for_explorer_refresh(
                                    address, txn,
                                )
                                .await?;
                                Ok::<_, DatabaseError>(())
                            }
                            .boxed()
                        })
                        .await??;
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "BMC came back after initial reset; awaiting fresh exploration report before continuing"
                        );
                        Ok(false)
                    }
                    Err(e) => {
                        // An unreachable BMC is never a reason to move on: keep
                        // waiting and continue once it comes back.
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            error = %e,
                            "Waiting for BMC to return after initial reset"
                        );
                        Ok(false)
                    }
                }
            }
            InitialBmcResetPhase::WaitForExplorerRefresh => {
                // Reached only once the refresh flag is cleared, i.e. site
                // explorer re-reads the BMC post-reset.
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Fresh exploration report received after initial BMC reset; running NTP, time-sync, and firmware checks"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_set_ntp_servers(
                        endpoint.address,
                        None,
                        0,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(false)
            }
        }
    }

    /// Handle the `SetNtpServers` preingestion state before initial checks.
    async fn set_ntp_servers(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        set_at: Option<&DateTime<Utc>>,
        attempts: u32,
    ) -> PreingestionManagerResult<bool> {
        if self.ntp_servers.is_empty() || attempts >= SET_NTP_SERVERS_MAX_ATTEMPTS {
            tracing::info!(
                bmc_ip_address = %endpoint.address,
                ntp_server_count = self.ntp_servers.len(),
                attempts,
                max_attempts = SET_NTP_SERVERS_MAX_ATTEMPTS,
                "No NTP servers configured or maximum attempts reached; running initial checks"
            );
            return self.run_initial_checks(db, endpoint).await;
        }

        if let Some(set_at) = set_at {
            let elapsed = Utc::now().signed_duration_since(*set_at);
            if elapsed < SET_NTP_SERVERS_CONVERGENCE_WAIT {
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Waiting for BMC NTP servers to converge before checking time sync"
                );
                return Ok(false);
            }

            tracing::info!(
                bmc_ip_address = %endpoint.address,
                "BMC NTP convergence wait complete; running initial checks"
            );
            return self.run_initial_checks(db, endpoint).await;
        }

        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                return self
                    .set_ntp_servers_retry_or_fail(db, endpoint, attempts, e)
                    .await;
            }
        };

        let ntp_servers: Vec<String> = self
            .ntp_servers
            .iter()
            .map(|addr| addr.to_string())
            .collect();
        if let Err(e) = redfish_client.set_ntp_servers(&ntp_servers).await {
            return self
                .set_ntp_servers_retry_or_fail(
                    db,
                    endpoint,
                    attempts,
                    PreingestionManagerError::RedfishError(e),
                )
                .await;
        }

        tracing::info!(
            bmc_ip_address = %endpoint.address,
            "Set NTP servers; waiting for BMC time to converge"
        );
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_set_ntp_servers(
                endpoint.address,
                Some(Utc::now()),
                attempts,
                txn,
            )
            .boxed()
        })
        .await??;

        Ok(false)
    }

    /// Helper to retry setting NTP servers or fail after a failed NTP Redfish operation.
    async fn set_ntp_servers_retry_or_fail(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        attempts: u32,
        error: PreingestionManagerError,
    ) -> PreingestionManagerResult<bool> {
        if matches!(error, PreingestionManagerError::DatabaseError(_)) {
            return Err(error);
        }

        let next = attempts + 1;
        if next >= SET_NTP_SERVERS_MAX_ATTEMPTS {
            tracing::warn!(
                bmc_ip_address = %endpoint.address,
                attempt = next,
                max_attempts = SET_NTP_SERVERS_MAX_ATTEMPTS,
                error = %error,
                "Failed to set NTP servers; proceeding with initial checks"
            );
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_set_ntp_servers(
                    endpoint.address,
                    None,
                    SET_NTP_SERVERS_MAX_ATTEMPTS,
                    txn,
                )
                .boxed()
            })
            .await??;
            return self.run_initial_checks(db, endpoint).await;
        }

        tracing::warn!(
            bmc_ip_address = %endpoint.address,
            attempt = next,
            max_attempts = SET_NTP_SERVERS_MAX_ATTEMPTS,
            error = %error,
            "Failed to set NTP servers; will retry"
        );
        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_set_ntp_servers(
                endpoint.address,
                None,
                next,
                txn,
            )
            .boxed()
        })
        .await??;
        Ok(false)
    }

    /// Helper: Execute power off and BMC reset sequence
    /// Returns true if successful, false if any step failed
    async fn execute_power_off_and_bmc_reset(
        &self,
        redfish_client: &dyn libredfish::Redfish,
        endpoint: &ExploredEndpoint,
    ) -> bool {
        match instrument_power_op(
            PowerOperation::ForceOff,
            redfish_client.power(SystemPowerControl::ForceOff),
            PowerControlLog::RecoverySequence {
                bmc_ip_address: endpoint.address,
            },
        )
        .await
        {
            Ok(()) => {}
            Err(RedfishError::UnnecessaryOperation) => {}
            Err(_) => return false,
        }

        let status = match redfish_client.get_power_state().await {
            Ok(status) => status,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Could not get power state"
                );
                return false;
            }
        };
        if status != PowerState::Off {
            tracing::warn!(
                bmc_ip_address = %endpoint.address,
                power_state = %status,
                "Host did not turn off when requested"
            );
            return false;
        }
        if instrument_power_op(
            PowerOperation::BmcReset,
            redfish_client.bmc_reset(None),
            PowerControlLog::RecoverySequence {
                bmc_ip_address: endpoint.address,
            },
        )
        .await
        .is_err()
        {
            return false;
        }
        true
    }

    /// Helper: Wait for BMC reset and power on the host
    /// Returns true if successful, false if any step failed
    async fn execute_wait_bmc_and_power_on(
        &self,
        redfish_client: &dyn libredfish::Redfish,
        endpoint: &ExploredEndpoint,
    ) -> bool {
        if let Err(e) = redfish_client.get_tasks().await {
            tracing::info!(
                bmc_ip_address = %endpoint.address,
                error = %e,
                "Waiting for BMC reset to complete"
            );
            return false;
        }

        match instrument_power_op(
            PowerOperation::On,
            redfish_client.power(SystemPowerControl::On),
            PowerControlLog::RecoverySequence {
                bmc_ip_address: endpoint.address,
            },
        )
        .await
        {
            Ok(()) => {}
            Err(RedfishError::UnnecessaryOperation) => {}
            Err(_) => return false,
        }

        let status = match redfish_client.get_power_state().await {
            Ok(status) => status,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Could not get power state"
                );
                return false;
            }
        };
        if status != PowerState::On {
            tracing::warn!(
                bmc_ip_address = %endpoint.address,
                power_state = %status,
                "Host did not turn on when requested"
            );
            return false;
        }
        true
    }

    /// Helper: Check if we should proceed after the boot wait period
    /// Returns true if 20 minutes have elapsed, false otherwise
    fn should_proceed_after_boot_wait(
        &self,
        last_time: Option<&DateTime<Utc>>,
        endpoint: &ExploredEndpoint,
    ) -> bool {
        if Utc::now().signed_duration_since(last_time.unwrap_or(&Utc::now()))
            < chrono::TimeDelta::minutes(20)
        {
            tracing::trace!(
                bmc_ip_address = %endpoint.address,
                "Waiting for host to complete boot sequence"
            );
            return false;
        }
        true
    }

    async fn pre_update_resets(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        phase: Option<&InitialResetPhase>,
        last_time: Option<&DateTime<Utc>>,
    ) -> Result<(), DatabaseError> {
        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Redfish connection failed"
                );
                return Ok(());
            }
        };

        match phase.unwrap_or(&InitialResetPhase::Start) {
            InitialResetPhase::Start => {
                if !self
                    .execute_power_off_and_bmc_reset(redfish_client.as_ref(), endpoint)
                    .await
                {
                    return Ok(());
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Initial reset BMC reset initiated"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_initial_reset(
                        endpoint.address,
                        InitialResetPhase::BMCWasReset,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            InitialResetPhase::BMCWasReset => {
                if !self
                    .execute_wait_bmc_and_power_on(redfish_client.as_ref(), endpoint)
                    .await
                {
                    return Ok(());
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Initial reset BMC reset complete; started host reset"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_initial_reset(
                        endpoint.address,
                        InitialResetPhase::WaitHostBoot,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            InitialResetPhase::WaitHostBoot => {
                if !self.should_proceed_after_boot_wait(last_time, endpoint) {
                    return Ok(());
                }
                // Now we can actually proceed with the upgrade.  Go back to checking firmware so we don't have to store all of that info.
                tracing::info!(bmc_ip_address = %endpoint.address, "Initial reset complete");
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_recheck_versions(endpoint.address, txn)
                        .boxed()
                })
                .await??;
                Ok(())
            }
        }
    }

    async fn time_sync_resets(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        phase: &TimeSyncResetPhase,
        last_time: Option<&DateTime<Utc>>,
        attempt: u32,
    ) -> PreingestionManagerResult<bool> {
        // Number of full reset cycles (power off -> BMC reset -> power on ->
        // 20 min boot wait -> recheck) to attempt before declaring failure. A
        // BMC clock that is out of sync just after a power event is often a
        // transient condition that self-heals once NTP converges, which can
        // take longer than a single boot-wait window; retrying gives it time
        // instead of going terminal on the first miss.
        const MAX_TIME_SYNC_RESET_ATTEMPTS: u32 = 3;

        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Redfish connection failed"
                );
                return Ok(false);
            }
        };

        match phase {
            TimeSyncResetPhase::Start => {
                if let Err(e) = redfish_client.set_utc_timezone().await {
                    tracing::error!(
                        bmc_ip_address = %endpoint.address,
                        error = %e,
                        "Could not set UTC timezone"
                    );
                    return Err(PreingestionManagerError::RedfishError(e));
                }
                if !self
                    .execute_power_off_and_bmc_reset(redfish_client.as_ref(), endpoint)
                    .await
                {
                    return Ok(false);
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Time-sync reset BMC reset initiated"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_time_sync_reset(
                        endpoint.address,
                        TimeSyncResetPhase::BMCWasReset,
                        attempt,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(false)
            }
            TimeSyncResetPhase::BMCWasReset => {
                if !self
                    .execute_wait_bmc_and_power_on(redfish_client.as_ref(), endpoint)
                    .await
                {
                    return Ok(false);
                }
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Time-sync reset BMC reset complete; started host reset"
                );
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_time_sync_reset(
                        endpoint.address,
                        TimeSyncResetPhase::WaitHostBoot,
                        attempt,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(false)
            }
            TimeSyncResetPhase::WaitHostBoot => {
                if !self.should_proceed_after_boot_wait(last_time, endpoint) {
                    return Ok(false);
                }

                // Host has booted, now check time sync again
                tracing::info!(
                    bmc_ip_address = %endpoint.address,
                    "Time-sync reset complete; checking time sync"
                );

                match self.check_bmc_time_sync(db, endpoint).await {
                    Ok(true) => {
                        // Time is now in sync, proceed with firmware check
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "BMC time is now in sync after reset"
                        );
                        let delayed_upgrade = self
                            .check_firmware_versions_below_preingestion(db, endpoint)
                            .await?;
                        Ok(delayed_upgrade)
                    }
                    Ok(false) => {
                        // Time is still not in sync after this reset cycle.
                        // `attempt` counts cycles already completed, so this is
                        // attempt number `attempt + 1`. Retry another full reset
                        // cycle until we exhaust the budget, then fail.
                        let attempts_done = attempt + 1;
                        if attempts_done < MAX_TIME_SYNC_RESET_ATTEMPTS {
                            tracing::warn!(
                                bmc_ip_address = %endpoint.address,
                                attempt = attempts_done,
                                max_attempts = MAX_TIME_SYNC_RESET_ATTEMPTS,
                                "BMC time still out of sync after reset; retrying reset"
                            );
                            db.with_txn(|txn| {
                                db::explored_endpoints::set_preingestion_time_sync_reset(
                                    endpoint.address,
                                    TimeSyncResetPhase::Start,
                                    attempts_done,
                                    txn,
                                )
                                .boxed()
                            })
                            .await??;
                            return Ok(false);
                        }

                        tracing::error!(
                            bmc_ip_address = %endpoint.address,
                            attempt = attempts_done,
                            max_attempts = MAX_TIME_SYNC_RESET_ATTEMPTS,
                            "BMC time is still out of sync after reset attempts; failing preingestion"
                        );
                        db.with_txn(|txn| {
                            db::explored_endpoints::set_preingestion_failed(
                                endpoint.address,
                                format!(
                                    "BMC time synchronization failed after {attempts_done} reset attempts. Time difference exceeds 5 minutes threshold."
                                ),
                                txn,
                            )
                            .boxed()
                        })
                        .await??;
                        Ok(false)
                    }
                    Err(e) => {
                        if let PreingestionManagerError::Internal { message } = e {
                            // Error checking time sync after reset, fail now
                            tracing::error!(
                                bmc_ip_address = %endpoint.address,
                                error = %message,
                                "Internal error checking BMC time sync after reset; failing preingestion"
                            );
                            db.with_txn(|txn| {
                                db::explored_endpoints::set_preingestion_failed(
                                    endpoint.address,
                                    format!("Failed to check BMC time sync after reset: {message}"),
                                    txn,
                                )
                                .boxed()
                            })
                            .await??;
                        } else {
                            tracing::warn!(
                                bmc_ip_address = %endpoint.address,
                                error = %e,
                                "Retryable error checking BMC time sync after reset; will retry later"
                            );
                        }
                        Ok(false)
                    }
                }
            }
        }
    }

    async fn by_script(
        &self,
        db: &PgPool,
        endpoint_address: std::net::IpAddr,
        to_install: &FirmwareEntry,
    ) -> Result<(), DatabaseError> {
        self.upgrade_script_state
            .started(endpoint_address.to_string());

        let address = endpoint_address.to_string();
        let script = to_install.script.clone().unwrap_or("/bin/false".into()); // Should always be Some at this point
        let upgrade_script_state = self.upgrade_script_state.clone();
        let (username, password) = if let Some(credential_reader) = &self.credential_reader {
            // We need to backtrack from the IP address to get the MAC address, which is what the credentials database is keyed on
            let interface = db::machine_interface::find_by_ip(db, endpoint_address).await?;
            let Some(interface) = interface else {
                tracing::warn!(
                    bmc_ip_address = address.as_str(),
                    "Unable to run update script; MAC address not retrievable"
                );
                return Ok(());
            };

            let key = CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: interface.mac_address,
                },
            };
            match credential_reader.get_credentials(&key).await {
                Ok(Some(credentials)) => match credentials {
                    Credentials::UsernamePassword { username, password } => (username, password),
                },
                Ok(None) => {
                    tracing::warn!(
                        bmc_ip_address = address.as_str(),
                        "Unable to run update script; no credentials exist"
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        bmc_ip_address = address.as_str(),
                        error = %e,
                        "Unable to run update script; could not retrieve credentials"
                    );
                    return Ok(());
                }
            }
        } else {
            ("Unknown".to_string(), "Unknown".to_string())
        };
        tokio::spawn(async move {
            let mut cmd = match tokio::process::Command::new(script)
                .env("BMC_IP", address.clone())
                .env("BMC_USERNAME", username)
                .env("BMC_PASSWORD", password)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(cmd) => cmd,
                Err(e) => {
                    tracing::error!(
                        bmc_ip_address = address.as_str(),
                        error = %e,
                        "Upgrade script command creation failed"
                    );
                    upgrade_script_state.completed(address, false);
                    return;
                }
            };

            /// Which capture setup failed, forcing the subprocess cleanup.
            enum CaptureFailure {
                Stdout,
                Stderr,
            }

            impl std::fmt::Display for CaptureFailure {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str(match self {
                        CaptureFailure::Stdout => "STDOUT capture failure",
                        CaptureFailure::Stderr => "STDERR capture failure",
                    })
                }
            }

            // Kill then reap `child`, warning -- but not failing -- if either
            // step errors. `context` names which capture setup failed, rendered
            // via its `Display`.
            async fn kill_and_reap(
                child: &mut tokio::process::Child,
                address: &str,
                context: CaptureFailure,
            ) {
                if let Err(e) = child.kill().await {
                    tracing::warn!(
                        %address,
                        %context,
                        error = %e,
                        "Upgrade script cleanup: failed to kill subprocess"
                    );
                }
                if let Err(e) = child.wait().await {
                    tracing::warn!(
                        %address,
                        %context,
                        error = %e,
                        "Upgrade script cleanup: failed to reap subprocess"
                    );
                }
            }

            let Some(stdout) = cmd.stdout.take() else {
                tracing::error!(
                    bmc_ip_address = address.as_str(),
                    "Upgrade script stdout creation failed"
                );
                kill_and_reap(&mut cmd, &address, CaptureFailure::Stdout).await;
                upgrade_script_state.completed(address, false);
                return;
            };
            let stdout = tokio::io::BufReader::new(stdout);

            let Some(stderr) = cmd.stderr.take() else {
                tracing::error!(
                    bmc_ip_address = address.as_str(),
                    "Upgrade script stderr creation failed"
                );
                kill_and_reap(&mut cmd, &address, CaptureFailure::Stderr).await;
                upgrade_script_state.completed(address, false);
                return;
            };
            let stderr = tokio::io::BufReader::new(stderr);

            // Take the stdout and stderr from the script and write them to a log with a searchable prefix
            let address2 = address.clone();
            let thread = tokio::spawn(async move {
                let mut lines = stderr.lines();
                while let Some(line) = lines.next_line().await.unwrap_or(None) {
                    tracing::info!("Upgrade script {address2} STDERR {line}");
                }
            });
            let mut lines = stdout.lines();
            while let Some(line) = lines.next_line().await.unwrap_or(None) {
                tracing::info!("Upgrade script {address} {line}");
            }
            if let Err(e) = thread.await {
                tracing::warn!(
                    %address,
                    error = %e,
                    "Upgrade script STDERR logging task did not complete cleanly"
                );
            }

            match cmd.wait().await {
                Err(e) => {
                    tracing::info!(
                        bmc_ip_address = address.as_str(),
                        error = %e,
                        "Upgrade script failed while waiting"
                    );
                    upgrade_script_state.completed(address, false);
                }
                Ok(errorcode) => {
                    if errorcode.success() {
                        tracing::info!(
                            bmc_ip_address = address.as_str(),
                            "Upgrade script completed successfully"
                        );
                        upgrade_script_state.completed(address, true);
                    } else {
                        tracing::warn!(
                            bmc_ip_address = address.as_str(),
                            exit_status = %errorcode,
                            "Upgrade script exited unsuccessfully"
                        );
                        upgrade_script_state.completed(address, false);
                    }
                }
            }
        });

        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_script_running(endpoint_address, txn).boxed()
        })
        .await??;
        Ok(())
    }

    async fn waiting_for_script(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> Result<(), DatabaseError> {
        db.with_txn(|txn| async move {
            let address = endpoint.address.to_string();
            let Some(success) = self.upgrade_script_state.state(&address) else {
                // Not yet completed, or we restarted (which specifically needs a manual restart of interrupted scripts)
                return Ok(());
            };

            self.upgrade_script_state.clear(&address);

            if success {
                db::explored_endpoints::set_preingestion_recheck_versions(endpoint.address, txn)
                    .await?;
                Ok(())
            } else {
                db::explored_endpoints::set_preingestion_failed(endpoint.address,format!(
                    "The upgrade script failed.  Search the log for \"Upgrade script {}\" for script output.  Force delete the explored endpoint to retry.",
                    endpoint.address
                ), txn).await?;
                Ok(())
            }
        }.boxed()).await?
    }

    /// check_bmc_time_sync checks if the BMC's system time is synchronized with the local system time.
    /// Returns true if the time difference is within the acceptable NTP drift threshold (5 minutes),
    /// false otherwise.
    async fn check_bmc_time_sync(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> PreingestionManagerResult<bool> {
        tracing::debug!(?endpoint, "Checking BMC time sync");
        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint.address, db)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                return Err(e);
            }
        };

        // Get the manager's DateTime from Redfish
        let bmc_time = match redfish_client
            .get_manager()
            .await
            .map_err(PreingestionManagerError::RedfishError)?
            .date_time
        {
            Some(time) => time,
            None => {
                return Err(PreingestionManagerError::internal(
                    "Failed to get BMC time".to_string(),
                ));
            }
        };

        let system_time = Utc::now();
        let time_diff = (system_time - bmc_time).num_seconds().abs();

        // Reasonable NTP drift threshold: 5 minutes (300 seconds)
        const NTP_DRIFT_THRESHOLD_SECONDS: i64 = 300;

        if time_diff > NTP_DRIFT_THRESHOLD_SECONDS {
            tracing::warn!(
                bmc_ip_address = %endpoint.address,
                %bmc_time,
                %system_time,
                difference_seconds = time_diff,
                "BMC time is out of sync"
            );
            Ok(false)
        } else {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                difference_seconds = time_diff,
                "BMC time is in sync"
            );
            Ok(true)
        }
    }

    /// Returns true if this endpoint's BMC IP already maps to an ingested/paired
    /// fleet machine.
    ///
    /// This reuses the same managed-host predicate site-explorer uses to refuse
    /// remediation on ingested hosts (see
    /// `site_explorer::managed_host::is_endpoint_in_managed_host`); both resolve
    /// to `db::machine_topology::find_machine_id_by_bmc_ip`. We intentionally do
    /// NOT use `endpoint.report.machine_id`, which is derived from DMI during
    /// exploration and is present even for fresh, un-ingested hosts.
    ///
    /// Fails closed: on a lookup error we report the host as ingested so the
    /// caller skips the destructive time-sync remediation rather than risk power
    /// cycling a tenant-assigned node.
    async fn is_ingested_host(&self, db: &PgPool, endpoint: &ExploredEndpoint) -> bool {
        match db::machine_topology::find_machine_id_by_bmc_ip(db, &endpoint.address.to_string())
            .await
        {
            Ok(machine_id) => machine_id.is_some(),
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "Could not determine whether endpoint is an ingested host; treating as ingested to skip time-sync remediation"
                );
                true
            }
        }
    }

    /// Handler for BfbRecoveryNeeded state.
    /// Host preingestion state was already validated by the gRPC handler at trigger time.
    async fn in_bfb_recovery_needed(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        host_bmc_ip: &std::net::IpAddr,
        pre_copy_powercycle: bool,
    ) -> Result<(), DatabaseError> {
        let address = endpoint.address;

        self.bfb_copy_state.clear(&address.to_string());

        if pre_copy_powercycle {
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_bfb_platform_powercycle(
                    address,
                    *host_bmc_ip,
                    model::site_explorer::BfbPlatformPowercyclePhase::PowerOff,
                    false,
                    txn,
                )
                .boxed()
            })
            .await??;
            return Ok(());
        }

        self.start_bfb_copy(db, endpoint, *host_bmc_ip).await
    }

    async fn in_bfb_platform_powercycle(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        host_bmc_ip: &std::net::IpAddr,
        phase: &model::site_explorer::BfbPlatformPowercyclePhase,
        post_install: bool,
    ) -> Result<(), DatabaseError> {
        use model::site_explorer::BfbPlatformPowercyclePhase;

        let address = endpoint.address;

        match phase {
            BfbPlatformPowercyclePhase::PowerOff => {
                let redfish_client = match self
                    .redfish_client_pool
                    .create_client_for_ingested_host(*host_bmc_ip, db)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(
                            dpu_bmc_ip_address = %address,
                            host_bmc_ip_address = %host_bmc_ip,
                            post_install,
                            error = %e,
                            "Failed to create Redfish client for host during BFB power cycle; will retry"
                        );
                        return Ok(());
                    }
                };

                tracing::info!(
                    dpu_bmc_ip_address = %address,
                    host_bmc_ip_address = %host_bmc_ip,
                    post_install,
                    "Powering off host during BFB power cycle"
                );
                if instrument_power_op(
                    PowerOperation::ForceOff,
                    redfish_client.power(SystemPowerControl::ForceOff),
                    PowerControlLog::BfbPlatformPowercycle {
                        dpu_bmc_ip_address: address,
                        host_bmc_ip_address: *host_bmc_ip,
                        post_install,
                    },
                )
                .await
                .is_err()
                {
                    return Ok(());
                }

                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_bfb_platform_powercycle(
                        address,
                        *host_bmc_ip,
                        BfbPlatformPowercyclePhase::PowerOn,
                        post_install,
                        txn,
                    )
                    .boxed()
                })
                .await??;
            }
            BfbPlatformPowercyclePhase::PowerOn => {
                let redfish_client = match self
                    .redfish_client_pool
                    .create_client_for_ingested_host(*host_bmc_ip, db)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(
                            dpu_bmc_ip_address = %address,
                            host_bmc_ip_address = %host_bmc_ip,
                            post_install,
                            error = %e,
                            "Failed to create Redfish client for host during BFB power cycle; will retry"
                        );
                        return Ok(());
                    }
                };

                tracing::info!(
                    dpu_bmc_ip_address = %address,
                    host_bmc_ip_address = %host_bmc_ip,
                    post_install,
                    "Powering on host during BFB power cycle"
                );
                if instrument_power_op(
                    PowerOperation::On,
                    redfish_client.power(SystemPowerControl::On),
                    PowerControlLog::BfbPlatformPowercycle {
                        dpu_bmc_ip_address: address,
                        host_bmc_ip_address: *host_bmc_ip,
                        post_install,
                    },
                )
                .await
                .is_err()
                {
                    return Ok(());
                }

                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_bfb_platform_powercycle(
                        address,
                        *host_bmc_ip,
                        BfbPlatformPowercyclePhase::WaitingForDpuBmc,
                        post_install,
                        txn,
                    )
                    .boxed()
                })
                .await??;
            }
            BfbPlatformPowercyclePhase::WaitingForDpuBmc => {
                let dpu_addr = std::net::SocketAddr::new(address, 443);
                match self
                    .redfish_client_pool
                    .probe_redfish_endpoint(dpu_addr)
                    .await
                {
                    Ok(()) if post_install => {
                        tracing::info!(bmc_ip_address = %address, "DPU BMC online after post-install power-cycle, completing preingestion");
                        db.with_txn(|txn| {
                            async move {
                                db::explored_endpoints::set_preingestion_complete(address, txn)
                                    .await?;
                                db::explored_endpoints::set_waiting_for_explorer_refresh(
                                    address, txn,
                                )
                                .await?;
                                db::explored_endpoints::set_pause_remediation(
                                    *host_bmc_ip,
                                    false,
                                    txn,
                                )
                                .await?;
                                Ok::<(), DatabaseError>(())
                            }
                            .boxed()
                        })
                        .await??;
                    }
                    Ok(()) => {
                        tracing::info!(bmc_ip_address = %address, "DPU BMC online after host power-cycle, starting BFB copy");
                        self.start_bfb_copy(db, endpoint, *host_bmc_ip).await?;
                    }
                    Err(_) => {
                        tracing::debug!(
                            bmc_ip_address = %address,
                            post_install,
                            "DPU BMC not yet reachable after power cycle"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn start_bfb_copy(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        host_bmc_ip: std::net::IpAddr,
    ) -> Result<(), DatabaseError> {
        let address = endpoint.address;

        let Ok(permit) = self.bfb_copy_limiter.clone().try_acquire_owned() else {
            tracing::warn!(bmc_ip_address = %address, "deferring BFB copy, too many copies already active");
            return Ok(());
        };

        let interface = match db::machine_interface::find_by_ip(db, address).await? {
            Some(interface) => interface,
            None => {
                tracing::error!(bmc_ip_address = %address, "no machine interface found for BFB copy, marking as failed");
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_failed(
                        address,
                        "No machine interface found for this DPU. \
                             Re-run `site-explorer copy-bfb-to-dpu-rshim` to retry."
                            .to_string(),
                        txn,
                    )
                    .boxed()
                })
                .await??;
                return Ok(());
            }
        };

        let bmc_credential_key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: interface.mac_address,
            },
        };

        db.with_txn(|txn| {
            db::explored_endpoints::set_preingestion_bfb_copy_in_progress(address, host_bmc_ip, txn)
                .boxed()
        })
        .await??;

        self.bfb_copy_state.started(address.to_string());

        let bfb_copy_state = self.bfb_copy_state.clone();
        let bfb_rshim_copier = self.bfb_rshim_copier.clone();
        let bmc_addr = std::net::SocketAddr::new(address, 22);

        tokio::spawn(async move {
            let _permit = permit;

            tracing::info!(bmc_ip_address = %address, "starting BFB copy to DPU rshim");
            let started = std::time::Instant::now();

            let result = bfb_rshim_copier
                .copy_bfb_to_dpu_rshim(bmc_addr, &bmc_credential_key)
                .await;

            // A result the poll loop no longer tracks was preempted by its
            // timeout: the Timeout observation already counted this copy, so
            // the late result is dropped without a second emit.
            match result {
                Ok(()) => {
                    if bfb_copy_state.completed(&address.to_string(), BfbCopyResult::Success) {
                        emit(BfbCopyFinished::Ok {
                            took: started.elapsed(),
                            address,
                        });
                    }
                }
                Err(e) => {
                    if bfb_copy_state
                        .completed(&address.to_string(), BfbCopyResult::Failed(e.to_string()))
                    {
                        emit(BfbCopyFinished::Error {
                            took: started.elapsed(),
                            address,
                            error: e.to_string(),
                        });
                    }
                }
            }
        });

        Ok(())
    }

    async fn in_bfb_copy_in_progress(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        started_at: &DateTime<Utc>,
        host_bmc_ip: &std::net::IpAddr,
    ) -> Result<(), DatabaseError> {
        let address = endpoint.address.to_string();

        let timeout_mins = BFB_COPY_TIMEOUT_MINS;
        let elapsed = Utc::now().signed_duration_since(*started_at);
        let elapsed_mins = elapsed.num_minutes();

        // One lock-held step decides this pass: a ready result always wins
        // over the deadline (the spawned task already counted it), and a
        // timeout removes the tracker entry under that same lock, so a
        // racing `completed()` finds nothing and drops its late result.
        match self
            .bfb_copy_state
            .resolve_or_timeout(&address, elapsed_mins > timeout_mins)
        {
            BfbResolution::Ready(BfbCopyResult::Success) => {
                tracing::info!(bmc_ip_address = %address, "BFB copy completed, waiting for installation");
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_bfb_installation_wait(
                        endpoint.address,
                        *host_bmc_ip,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            BfbResolution::Ready(BfbCopyResult::Failed(error)) => {
                tracing::error!(bmc_ip_address = %address, error=%error, "BFB copy failed");
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_failed(
                        endpoint.address,
                        format!(
                            "BFB copy failed: {error}. \
                             Re-run `site-explorer copy-bfb-to-dpu-rshim` to retry.",
                        ),
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            BfbResolution::TimedOut => {
                // No result inside the deadline: the task died without
                // reporting, or the copy is still dragging. The entry is
                // already gone (removed under the resolution's lock), so
                // this Timeout stays the copy's only record.
                emit(BfbCopyFinished::Timeout {
                    took: elapsed.to_std().unwrap_or_default(),
                    address: endpoint.address,
                    error: format!(
                        "BFB copy timed out after {elapsed_mins} minutes (limit {timeout_mins} minutes)"
                    ),
                });
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_failed(
                        endpoint.address,
                        format!(
                            "BFB copy timed out after {elapsed_mins} minutes. \
                             Re-run `site-explorer copy-bfb-to-dpu-rshim` to retry.",
                        ),
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            BfbResolution::Untracked => {
                tracing::warn!(bmc_ip_address = %address, "detected orphaned BFB copy state, restarting copy");
                db.with_txn(|txn| {
                    db::explored_endpoints::set_preingestion_bfb_recovery_needed(
                        endpoint.address,
                        "Restarting after orphaned copy state detected".to_string(),
                        *host_bmc_ip,
                        false,
                        txn,
                    )
                    .boxed()
                })
                .await??;
                Ok(())
            }
            BfbResolution::Pending => {
                tracing::debug!(bmc_ip_address = %address, "BFB copy still in progress");
                Ok(())
            }
        }
    }

    async fn in_bfb_installation_wait(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
        started_at: &DateTime<Utc>,
        host_bmc_ip: &std::net::IpAddr,
    ) -> Result<(), DatabaseError> {
        let elapsed_mins = Utc::now().signed_duration_since(*started_at).num_minutes();
        if elapsed_mins > BFB_INSTALLATION_TIMEOUT_MINS {
            tracing::error!(
                bmc_ip_address = %endpoint.address,
                elapsed_minutes = elapsed_mins,
                "BFB installation timed out"
            );
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_failed(
                    endpoint.address,
                    format!(
                        "BFB installation timed out after {elapsed_mins} minutes. \
                         Re-run `site-explorer copy-bfb-to-dpu-rshim` to retry.",
                    ),
                    txn,
                )
                .boxed()
            })
            .await??;
            return Ok(());
        }

        if elapsed_mins < BFB_INSTALLATION_MIN_WAIT_MINS {
            tracing::debug!(
                bmc_ip_address = %endpoint.address,
                elapsed_minutes = elapsed_mins,
                minimum_wait_minutes = BFB_INSTALLATION_MIN_WAIT_MINS,
                "BFB installation in progress, waiting before checking"
            );
            return Ok(());
        }

        if self.check_dpu_console_install_complete(db, endpoint).await {
            tracing::info!(
                bmc_ip_address = %endpoint.address,
                "DPU installation complete, powercycling host"
            );
            db.with_txn(|txn| {
                db::explored_endpoints::set_preingestion_bfb_platform_powercycle(
                    endpoint.address,
                    *host_bmc_ip,
                    model::site_explorer::BfbPlatformPowercyclePhase::PowerOff,
                    true,
                    txn,
                )
                .boxed()
            })
            .await??;
            return Ok(());
        }

        tracing::debug!(
            bmc_ip_address = %endpoint.address,
            elapsed_minutes = elapsed_mins,
            "DPU console login not yet detected, waiting"
        );
        Ok(())
    }

    async fn check_dpu_console_install_complete(
        &self,
        db: &PgPool,
        endpoint: &ExploredEndpoint,
    ) -> bool {
        const MARKERS: &[&str] = &[
            "login:",
            "Running bfb_post_install from bf.cfg",
            "total 100% complete",
        ];

        let address = endpoint.address;
        let bmc_addr = std::net::SocketAddr::new(address, 22);

        let Some(credential_reader) = &self.credential_reader else {
            tracing::debug!(bmc_ip_address = %address, "no credential reader, skipping console check");
            return false;
        };

        let interface = match db::machine_interface::find_by_ip(db, address).await {
            Ok(Some(iface)) => iface,
            _ => {
                tracing::debug!(bmc_ip_address = %address, "no machine interface for console check");
                return false;
            }
        };

        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: interface.mac_address,
            },
        };

        let (username, password) = match credential_reader.get_credentials(&key).await {
            Ok(Some(Credentials::UsernamePassword { username, password })) => (username, password),
            Ok(None) => {
                tracing::debug!(bmc_ip_address = %address, "no credentials found for console check");
                return false;
            }
            Err(e) => {
                tracing::warn!(bmc_ip_address = %address, error=%e, "failed to retrieve credentials for console check");
                return false;
            }
        };

        match forge_ssh::ssh::check_console_for_markers(bmc_addr, username, password, MARKERS).await
        {
            Ok(found) => found,
            Err(e) => {
                tracing::debug!(bmc_ip_address = %address, error=%e, "SSH console check failed");
                false
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum BfbCopyResult {
    Success,
    Failed(String),
}

/// The poll loop's single-lock view of one copy, from
/// [`BfbCopyManager::resolve_or_timeout`].
#[derive(Debug)]
enum BfbResolution {
    /// A recorded result was taken; the entry is gone.
    Ready(BfbCopyResult),
    /// No result inside the deadline; the entry (if any) was removed under
    /// the same lock, so a racing `completed()` finds nothing and its late
    /// result is dropped.
    TimedOut,
    /// Tracked with no result yet, inside the deadline.
    Pending,
    /// Not tracked at all (inside the deadline): an orphaned copy state.
    Untracked,
}

#[derive(Debug, Default)]
struct BfbCopyManager {
    active: std::sync::Mutex<HashMap<String, Option<BfbCopyResult>>>,
}

impl BfbCopyManager {
    fn started(&self, address: String) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.insert(address, None);
    }

    /// Records the copy's result, but only while the address is still
    /// tracked. Returns false when the poll loop already gave up on the copy
    /// (timeout) and removed it -- the late result is dropped so the caller
    /// doesn't count it a second time.
    fn completed(&self, address: &str, result: BfbCopyResult) -> bool {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        match hashmap.get_mut(address) {
            Some(entry) => {
                *entry = Some(result);
                true
            }
            None => false,
        }
    }

    fn clear(&self, address: &str) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.remove(address);
    }

    /// Resolves one poll pass under a single lock, so a `completed()` racing
    /// in between cannot be both consumed and timed out. A present result is
    /// taken (and always wins, deadline or not); otherwise, past the deadline
    /// the entry is removed and the copy reported [`BfbResolution::TimedOut`].
    /// An address that was never tracked also times out past the deadline,
    /// preserving the state machine's `started_at` SLA across restarts.
    fn resolve_or_timeout(&self, address: &str, timed_out: bool) -> BfbResolution {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        let Some(entry) = hashmap.get_mut(address) else {
            return if timed_out {
                BfbResolution::TimedOut
            } else {
                BfbResolution::Untracked
            };
        };
        if let Some(result) = entry.take() {
            hashmap.remove(address);
            return BfbResolution::Ready(result);
        }
        if timed_out {
            hashmap.remove(address);
            return BfbResolution::TimedOut;
        }
        BfbResolution::Pending
    }
}

#[derive(Debug, Default)]
struct UpdateScriptManager {
    active: std::sync::Mutex<HashMap<String, Option<bool>>>,
}

impl UpdateScriptManager {
    fn started(&self, id: String) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.insert(id, None);
    }

    fn completed(&self, id: String, success: bool) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.insert(id, Some(success));
    }

    fn clear(&self, id: &String) {
        let mut hashmap = self.active.lock().expect("lock poisoned");
        hashmap.remove(id);
    }

    fn state(&self, id: &String) -> Option<bool> {
        let hashmap = self.active.lock().expect("lock poisoned");
        *hashmap.get(id).unwrap_or(&None)
    }
}

struct InUpgradeFirmwareWaitArgs<'a> {
    endpoint: &'a ExploredEndpoint,
    task_id: &'a str,
    final_version: &'a str,
    upgrade_type: &'a FirmwareComponentType,
    power_drains_needed: &'a Option<u32>,
    firmware_number: u32,
}

fn select_firmware_for_artifact_continuation<'a>(
    component_info: &'a FirmwareComponent,
    final_version: &str,
) -> Option<&'a FirmwareEntry> {
    component_info
        .known_firmware
        .iter()
        .find(|firmware| firmware.version == final_version)
        .or_else(|| {
            component_info
                .known_firmware
                .iter()
                .find(|firmware| firmware.default)
        })
}

/// need_upgrade determines if the given endpoint needs a firmware upgrade based on the description in fw_info, and if
/// so returns the FirmwareEntry matching the desired upgrade along with the ID that Redfish uses to specify its version.
fn need_upgrade(
    endpoint: &ExploredEndpoint,
    fw_info: &Firmware,
    firmware_type: FirmwareComponentType,
) -> Option<FirmwareEntry> {
    // First, find the BMC version.
    let mut current_version = None;
    for service in endpoint.report.service.iter() {
        if let Some(matching_inventory) = service
            .inventories
            .iter()
            .find(|&x| fw_info.matching_version_id(&x.id, firmware_type))
        {
            current_version = matching_inventory.version.as_ref();
            break;
        };
    }
    let current_version = current_version?.to_owned();

    // Now find the desired version, if it's not the version that is currently installed.
    fw_info
        .components
        .get(&firmware_type)?
        .known_firmware
        .iter()
        .find(|x| (x.default || x.preingestion_exclusive_config) && x.version != current_version)
        .cloned()
}

impl PreingestionManagerStatic {
    /// initiate_update will start a Redfish connection to the given address and start an update
    /// by doing an upload. Returns true once the upload's Redfish task has been stored as the
    /// new wait state. It may be unable to start the update if the firmware has not been
    /// previously downloaded; if that happens it returns Ok(false), but has not modified the
    /// state. On Redfish errors, we also return Ok(false) and leave the state as it was, with
    /// the intention that we will retry on the next go.
    async fn initiate_update(
        &self,
        endpoint_clone: &ExploredEndpoint,
        to_install: &FirmwareEntry,
        firmware_type: &FirmwareComponentType,
        firmware_number: u32,
        db_pool: &PgPool,
    ) -> Result<bool, DatabaseError> {
        let artifact = match resolve_preingestion_firmware_artifact(
            &self.config.firmware_download_cache_directory,
            to_install,
            firmware_number,
        ) {
            Ok(artifact) => artifact,
            Err(error) => {
                tracing::error!(
                    bmc_ip_address = %endpoint_clone.address,
                    %error,
                    "Failed to resolve firmware artifact"
                );
                return Ok(false);
            }
        };

        if !is_bfb_artifact(&artifact.local_path) {
            match &artifact.source {
                ResolvedFirmwareArtifactSource::Remote { url, sha256 } => {
                    if !self.downloader.available(&artifact.local_path, url, sha256) {
                        tracing::debug!(
                            bmc_ip_address = %endpoint_clone.address,
                            path = %artifact.local_path.display(),
                            %url,
                            "Firmware artifact is being downloaded; update deferred"
                        );

                        return Ok(false);
                    }
                }
                ResolvedFirmwareArtifactSource::Local => {
                    if !artifact.local_path.exists() {
                        tracing::error!(
                            bmc_ip_address = %endpoint_clone.address,
                            path = %artifact.local_path.display(),
                            "Firmware artifact is not present"
                        );
                        return Ok(false);
                    }
                }
            }
        }

        // Setup the Redfish connection
        let redfish_client = match self
            .redfish_client_pool
            .create_client_for_ingested_host(endpoint_clone.address, db_pool)
            .await
        {
            Ok(redfish_client) => redfish_client,
            Err(e) => {
                tracing::debug!(
                    bmc_ip_address = %endpoint_clone.address,
                    error = %e,
                    "Failed to open Redfish connection"
                );
                return Ok(false);
            }
        };

        tracing::debug!(
            bmc_ip_address = %endpoint_clone.address,
            "Started firmware upload"
        );

        let redfish_component_type: libredfish::model::update_service::ComponentType =
            match to_install.install_only_specified {
                false => libredfish::model::update_service::ComponentType::Unknown,
                true => firmware_type.into_libredfish(),
            };

        let task = if is_bfb_artifact(&artifact.local_path) {
            redfish_client
                .enable_rshim_bmc()
                .await
                .inspect_err(|e| {
                    tracing::error!(
                        bmc_ip_address = %endpoint_clone.address,
                        error = %e,
                        "Failed to enable RSHIM on BMC"
                    )
                })
                .ok();
            tracing::debug!(
                bmc_ip_address = %endpoint_clone.address,
                image_uri = artifact.bfb_image_uri.as_str(),
                "Using simple update for firmware upload"
            );
            match redfish_client
                .update_firmware_simple_update(
                    artifact.bfb_image_uri.as_str(),
                    vec!["redfish/v1/UpdateService/FirmwareInventory/DPU_OS".to_string()],
                    TransferProtocolType::HTTP,
                )
                .await
            {
                Ok(task) => {
                    emit(FirmwareUploadFinished::Succeeded {
                        method: FirmwareUploadMethod::SimpleUpdate,
                    });
                    task.id
                }
                Err(e) => {
                    emit(FirmwareUploadFinished::SimpleUpdateFailed {
                        bmc_ip_address: endpoint_clone.address,
                        error: e.to_string(),
                    });
                    return Ok(false);
                }
            }
        } else {
            match redfish_client
                .update_firmware_multipart(
                    artifact.local_path.as_path(),
                    true,
                    Duration::from_secs(120),
                    redfish_component_type,
                )
                .await
            {
                Ok(task) => {
                    emit(FirmwareUploadFinished::Succeeded {
                        method: FirmwareUploadMethod::Multipart,
                    });
                    task
                }
                Err(RedfishError::NotSupported(err)) => {
                    emit(MultipartFirmwareUploadUnsupported {
                        bmc_ip_address: endpoint_clone.address,
                        error: err,
                    });
                    let file = match File::open(artifact.local_path.as_path()).await {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::error!(
                                bmc_ip_address = %endpoint_clone.address,
                                error = %e,
                                "Failed to open firmware file"
                            );
                            return Ok(false);
                        }
                    };
                    match redfish_client.update_firmware(file).await {
                        Ok(task) => {
                            emit(FirmwareUploadFinished::Succeeded {
                                method: FirmwareUploadMethod::HttpPush,
                            });
                            task.id
                        }
                        Err(e) => {
                            emit(FirmwareUploadFinished::HttpPushFailed {
                                bmc_ip_address: endpoint_clone.address,
                                error: e.to_string(),
                            });
                            return Ok(false);
                        }
                    }
                }
                Err(e) => {
                    emit(FirmwareUploadFinished::MultipartFailed {
                        bmc_ip_address: endpoint_clone.address,
                        error: e.to_string(),
                    });
                    return Ok(false);
                }
            }
        };

        tracing::debug!(
            bmc_ip_address = %endpoint_clone.address,
            "Completed firmware upload"
        );

        db_pool
            .with_txn(|txn| {
                Box::pin(db::explored_endpoints::set_preingestion_waittask(
                    endpoint_clone.address,
                    task,
                    &to_install.version,
                    firmware_type,
                    to_install.power_drains_needed,
                    firmware_number,
                    txn,
                ))
            })
            .await??;

        Ok(true)
    }
}

struct PreingestionFirmwareArtifact {
    local_path: PathBuf,
    source: ResolvedFirmwareArtifactSource,
    bfb_image_uri: String,
}

fn resolve_preingestion_firmware_artifact(
    firmware_download_cache_directory: &Path,
    to_install: &FirmwareEntry,
    firmware_number: u32,
) -> Result<PreingestionFirmwareArtifact, String> {
    if let Some(artifact) = resolve_files_firmware_artifact(
        firmware_download_cache_directory,
        to_install,
        firmware_number,
    )
    .map_err(|error| error.to_string())?
    {
        let bfb_image_uri = match &artifact.source {
            ResolvedFirmwareArtifactSource::Remote { url, .. } => url.clone(),
            ResolvedFirmwareArtifactSource::Local => {
                legacy_bfb_image_uri(to_install, &artifact.local_path)
            }
        };

        return Ok(PreingestionFirmwareArtifact {
            local_path: artifact.local_path,
            source: artifact.source,
            bfb_image_uri,
        });
    }

    let local_path = to_install.get_filename(firmware_number);
    let bfb_image_uri = legacy_bfb_image_uri(to_install, &local_path);
    let source = match legacy_artifact_remote_url(to_install) {
        Some(url) => ResolvedFirmwareArtifactSource::Remote {
            url: url.to_string(),
            sha256: to_install.get_checksum(),
        },
        None => ResolvedFirmwareArtifactSource::Local,
    };

    Ok(PreingestionFirmwareArtifact {
        local_path,
        source,
        bfb_image_uri,
    })
}

fn legacy_artifact_remote_url(to_install: &FirmwareEntry) -> Option<&str> {
    to_install
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty() && *url != LEGACY_NO_URL_SENTINEL)
}

fn legacy_bfb_image_uri(to_install: &FirmwareEntry, local_path: &Path) -> String {
    format!("{}/{}", to_install.get_url(), local_path.display())
}

fn is_bfb_artifact(path: &Path) -> bool {
    path.ends_with("bfb")
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bfb"))
}

trait CreateClientForIngestedHost {
    async fn create_client_for_ingested_host(
        &self,
        ip: IpAddr,
        db_pool: &PgPool,
    ) -> PreingestionManagerResult<Box<dyn Redfish>>;
}

impl CreateClientForIngestedHost for Arc<dyn RedfishClientPool> {
    async fn create_client_for_ingested_host(
        &self,
        ip: IpAddr,
        db_pool: &PgPool,
    ) -> PreingestionManagerResult<Box<dyn Redfish>> {
        let bmc_access_info =
            db::machine_interface::lookup_bmc_access_info(db_pool, ip, None).await?;

        self.client_by_info(&bmc_access_info)
            .await
            .map_err(|e| match e {
                RedfishClientCreationError::RedfishError(e) => {
                    PreingestionManagerError::RedfishError(e)
                }
                _ => PreingestionManagerError::internal(format!("{e}")),
            })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn resolve_preingestion_firmware_artifact_preserves_legacy_local_entries() {
        for url in [None, Some(LEGACY_NO_URL_SENTINEL.to_string())] {
            let mut firmware =
                FirmwareEntry::standard_filename("1.0.0", "/firmware/local-artifact.bin");
            firmware.url = url;

            let artifact =
                resolve_preingestion_firmware_artifact(Path::new("/cache"), &firmware, 0).unwrap();

            assert_eq!(
                artifact.local_path,
                Path::new("/firmware/local-artifact.bin")
            );
            assert_eq!(artifact.source, ResolvedFirmwareArtifactSource::Local);
        }
    }

    #[test]
    fn resolve_preingestion_firmware_artifact_preserves_legacy_remote_entries() {
        let mut firmware = FirmwareEntry::standard_filename("1.0.0", "/firmware/artifact.bin");
        firmware.url = Some("https://firmware.example.invalid/artifact.bin".to_string());
        firmware.checksum = Some("abc123".to_string());

        let artifact =
            resolve_preingestion_firmware_artifact(Path::new("/cache"), &firmware, 0).unwrap();

        assert_eq!(artifact.local_path, Path::new("/firmware/artifact.bin"));
        assert_eq!(
            artifact.source,
            ResolvedFirmwareArtifactSource::Remote {
                url: "https://firmware.example.invalid/artifact.bin".to_string(),
                sha256: "abc123".to_string(),
            }
        );
    }

    #[test]
    fn is_bfb_artifact_matches_legacy_component_and_bfb_extension() {
        for (path, expected) in [
            ("/firmware/bfb", true),
            ("/firmware/foo.bfb", true),
            ("/firmware/foo.BFB", true),
            ("/firmware/foo.bin", false),
            ("/firmware/bfb.bin", false),
        ] {
            assert_eq!(is_bfb_artifact(Path::new(path)), expected, "{path}");
        }
    }

    /// The poll's single-lock resolution: a ready result always wins over
    /// the deadline, and a timeout removes the entry under that same lock,
    /// so a racing `completed()` finds nothing and reports the late result
    /// dropped.
    #[test]
    fn bfb_copy_manager_resolves_or_times_out_atomically() {
        let manager = BfbCopyManager::default();

        // A recorded result is taken even past the deadline, and taking it
        // untracks the copy.
        manager.started("a".to_string());
        assert!(manager.completed("a", BfbCopyResult::Success));
        assert!(matches!(
            manager.resolve_or_timeout("a", true),
            BfbResolution::Ready(BfbCopyResult::Success)
        ));
        assert!(matches!(
            manager.resolve_or_timeout("a", false),
            BfbResolution::Untracked
        ));

        // A failure result resolves the same way.
        manager.started("b".to_string());
        assert!(manager.completed("b", BfbCopyResult::Failed("ssh died".to_string())));
        assert!(matches!(
            manager.resolve_or_timeout("b", false),
            BfbResolution::Ready(BfbCopyResult::Failed(_))
        ));

        // No result inside the deadline stays tracked and pending; past the
        // deadline the entry is removed, and the task's racing `completed()`
        // finds nothing -- its late result is dropped.
        manager.started("c".to_string());
        assert!(matches!(
            manager.resolve_or_timeout("c", false),
            BfbResolution::Pending
        ));
        assert!(matches!(
            manager.resolve_or_timeout("c", true),
            BfbResolution::TimedOut
        ));
        assert!(!manager.completed("c", BfbCopyResult::Success));

        // A never-tracked address is an orphan inside the deadline and a
        // timeout past it (the started_at SLA holds across restarts).
        assert!(matches!(
            manager.resolve_or_timeout("d", false),
            BfbResolution::Untracked
        ));
        assert!(matches!(
            manager.resolve_or_timeout("d", true),
            BfbResolution::TimedOut
        ));
    }

    #[test]
    fn select_firmware_for_artifact_continuation_selects_requested_or_default() {
        value_scenarios!(run = |requested_version| {
            let component = FirmwareComponent {
                known_firmware: vec![
                    FirmwareEntry::standard_notdefault("6.00.30.00"),
                    FirmwareEntry::standard("6.50.00.00"),
                ],
                ..FirmwareComponent::default()
            };

            select_firmware_for_artifact_continuation(&component, requested_version)
                .unwrap()
                .version
                .clone()
        };
            "final-version preference" {
                "6.00.30.00" => "6.00.30.00".to_string(),
            }

            "default fallback" {
                "7.00.00.00" => "6.50.00.00".to_string(),
            }
        );
    }
}
