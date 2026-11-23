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

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::panic::Location;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use carbide_firmware::{FirmwareConfig, FirmwareConfigSnapshot};
use carbide_network::{is_locally_administered_mac, sanitized_mac};
use carbide_redfish::libredfish::conv::IntoModel;
use carbide_secrets::credentials::CredentialManager;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::machine::MachineType;
use carbide_uuid::power_shelf::{PowerShelfIdSource, PowerShelfType};
use chrono::Utc;
use config::SiteExplorerConfig;
use db::{self, DatabaseError, Transaction, machine, power_shelf as db_power_shelf};
use futures_util::stream::FuturesUnordered;
use futures_util::{StreamExt, TryFutureExt};
use itertools::Itertools;
use librms::RmsApi;
use mac_address::MacAddress;
use model::errors::OperatorError;
use model::expected_entity::ExpectedEntity;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::machine::MachineInterfaceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_boot_interface::MachineBootInterface;
use model::machine_interface::InterfaceType;
use model::power_shelf::{NewPowerShelf, PowerShelfConfig};
use model::rack_type::RackProfileConfig;
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    EndpointExplorationError, EndpointExplorationReport, EndpointType, ExploredDpu,
    ExploredEndpoint, ExploredManagedHost, ExploredManagedSwitch, MachineExpectation, NicMode,
    PowerState, PreingestionState, Service, SiteExplorerLastRun, is_bf3_dpu_part_number,
    is_bf3_supernic_part_number, is_bluefield_part_number,
};
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use version_compare::Cmp;
mod endpoint_explorer;
pub use endpoint_explorer::EndpointExplorer;
mod endpoint_lock;
pub use endpoint_lock::{EndpointExplorationGuard, EndpointExplorationLocks};
mod credentials;
mod metrics;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub use metrics::{SiteExplorationMetrics, site_explorer_latency_histogram_view};
mod bmc_endpoint_explorer;
mod redfish;
pub use bmc_endpoint_explorer::BmcEndpointExplorer;
mod boot_order_tracker;
use boot_order_tracker::BootOrderTracker;
mod machine_creator;
pub use machine_creator::MachineCreator;
pub mod explored_endpoint_index;
mod managed_host;
use db::ObjectColumnFilter;
use db::work_lock_manager::WorkLockManagerHandle;
pub use managed_host::is_endpoint_in_managed_host;
use model::DpuModel;
use model::expected_machine::DpuMode;
use model::firmware::FirmwareComponentType;
use model::network_segment::NetworkSegmentType;
mod switch_creator;
use carbide_uuid::rack::RackId;
use model::rack::Rack;
pub use switch_creator::SwitchCreator;
pub mod config;
pub mod errors;
use std::sync::atomic::AtomicBool;

use carbide_ipmi::IPMITool;
use carbide_redfish::libredfish::RedfishClientPool;
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use errors::{SiteExplorerError, SiteExplorerResult};

use self::metrics::{DpuMigrationSignal, PairingBlockerReason, exploration_error_to_metric_label};
use crate::config::SiteExplorerExploreMode;
use crate::explored_endpoint_index::ExploredEndpointIndex;

#[allow(clippy::too_many_arguments)]
pub fn new_bmc_explorer(
    redfish_client_pool: Arc<dyn RedfishClientPool>,
    nv_redfish_client_pool: Arc<NvRedfishClientPool>,
    ipmi_tool: Arc<dyn IPMITool>,
    credential_manager: Arc<dyn CredentialManager>,
    rotate_switch_nvos_credentials: Arc<AtomicBool>,
    mode: SiteExplorerExploreMode,
    explore_drive_firmware: bool,
    database_connection: PgPool,
) -> Arc<BmcEndpointExplorer> {
    BmcEndpointExplorer::new(
        redfish_client_pool,
        nv_redfish_client_pool,
        ipmi_tool,
        credential_manager,
        rotate_switch_nvos_credentials,
        mode,
        Some(database_connection),
    )
    .with_drive_firmware(explore_drive_firmware)
    .into()
}

pub fn enrich_endpoint_exploration_report(
    report: &mut EndpointExplorationReport,
    fw_config_snapshot: &FirmwareConfigSnapshot,
) {
    if !report.is_power_shelf() {
        if let Err(error) = report.generate_machine_id(false) {
            tracing::error!(%error, "Can not generate MachineId for explored endpoint");
        }
        report.model = report.model();
        if let Some(fw_info) = fw_config_snapshot.find_fw_info_for_host_report(report) {
            let components_without_version = report.parse_versions(&fw_info);
            if !components_without_version.is_empty() {
                tracing::debug!(
                    "Can not find firmware version for component(s): {:?}",
                    components_without_version
                );
            }
        } else {
            // It's possible that we knew about this host type before but do not now, so make sure we
            // do not keep stale data.
            report.versions = HashMap::default();
            tracing::debug!(
                "Can not find firmware info for: vendor: {:?}; model: {:?}",
                report.vendor,
                report.model()
            );
        }

        // Go through the chassis entries and get what at least one of them says.
        report.parse_position_info()
    } else {
        tracing::info!("Generating PowerShelfId for power shelf");
        if let Err(error) = report.generate_power_shelf_id() {
            tracing::error!(%error, "Can not generate PowerShelfId for explored power shelf endpoint");
        }
        report.versions = HashMap::default();
    }
}

/// Ensures a rack row exists for the given `rack_id`.
///
/// If the rack already exists, returns it. Otherwise creates a new rack only
/// when a matching expected rack record exists. Returns `None` when no
/// expected rack record is found, allowing callers to proceed without a rack.
pub(crate) async fn ensure_rack_exists(
    txn: &mut sqlx::PgConnection,
    rack_id: &RackId,
) -> SiteExplorerResult<Option<Rack>> {
    match db::rack::find_by(txn, ObjectColumnFilter::One(db::rack::IdColumn, rack_id)).await {
        Ok(mut racks) if !racks.is_empty() => Ok(racks.pop()),
        Ok(_) | Err(DatabaseError::NotFoundError { .. }) => {
            let expected = db::expected_rack::find_by_rack_id(&mut *txn, rack_id).await?;

            let Some(expected) = expected else {
                tracing::warn!(
                    %rack_id,
                    "No expected rack record found; skipping rack creation"
                );
                return Ok(None);
            };

            tracing::info!(%rack_id, "Rack does not exist, creating from expected rack");
            let config = model::rack::RackConfig::default();
            let rack = db::rack::create(
                &mut *txn,
                rack_id,
                Some(&expected.rack_profile_id),
                &config,
                Some(&expected.metadata),
            )
            .await?;

            Ok(Some(rack))
        }
        Err(e) => Err(e.into()),
    }
}

/// Fetches slot_number and tray_index from the RMS for a given rack/node pair.
/// Returns `(None, None)` on any failure, logging a warning with `entity_label`.
pub async fn fetch_slot_and_tray(
    rms_client: &dyn librms::RmsApi,
    request: librms::protos::rack_manager::BatchGetNodeDeviceInfoRequest,
) -> (Option<i32>, Option<i32>) {
    match rms_client.batch_get_node_device_info(request).await {
        Ok(info) => {
            let Some(node_device_details) = info.node_device_details.first() else {
                return (None, None);
            };

            let slot_number = node_device_details
                .slot_number
                .and_then(|value| i32::try_from(value).ok());
            let tray_index = node_device_details
                .tray_index
                .and_then(|value| i32::try_from(value).ok());

            (slot_number, tray_index)
        }
        Err(e) => {
            tracing::warn!(
                %e,
                "Failed to get device info from RMS, slot_number and tray_index will be unset"
            );
            (None, None)
        }
    }
}

pub struct Endpoint<'a> {
    address: IpAddr,
    iface: &'a MachineInterfaceSnapshot,
    last_explored: Option<&'a ExploredEndpoint>,
    pub(crate) expected: Option<&'a ExpectedEntity>,
    pause_ingestion_and_poweron: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct EndpointExplorationStepDurations {
    redfish_explore: Duration,
    failure_context_load: Option<Duration>,
    report_enrich: Option<Duration>,
}

struct EndpointExplorationTaskResult<'a> {
    endpoint: Endpoint<'a>,
    result: Result<EndpointExplorationReport, EndpointExplorationError>,
    exploration_duration: Duration,
    steps: EndpointExplorationStepDurations,
}

impl Display for Endpoint<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.address)
    }
}

impl<'a> Endpoint<'a> {
    fn preingestion_state(&self) -> Cow<'a, PreingestionState> {
        self.last_explored
            .map_or(Cow::Owned(PreingestionState::Initial), |e| {
                Cow::Borrowed(&e.preingestion_state)
            })
    }
}

pub type SiteIdentifiedHosts = Vec<(ExploredManagedHost, EndpointExplorationReport)>;

/// The SiteExplorer periodically runs [modules](machine_update_module::MachineUpdateModule) to initiate upgrades of machine components.
/// On each iteration the SiteExplorer will:
/// 1. collect the number of outstanding updates from all modules.
/// 2. if there are less than the max allowed updates each module will be told to start updates until
///    the number of updates reaches the maximum allowed.
///
/// Config from [CarbideConfig]:
/// * `max_concurrent_machine_updates` the maximum number of updates allowed across all modules
/// * `machine_update_run_interval` how often the manager calls the modules to start updates
pub struct SiteExplorer {
    database_connection: PgPool,
    config: SiteExplorerConfig,
    metric_holder: Arc<metrics::MetricHolder>,
    endpoint_explorer: Arc<dyn EndpointExplorer>,
    firmware_config: Arc<FirmwareConfig>,
    work_lock_manager_handle: WorkLockManagerHandle,
    /// Per-endpoint, in-process exploration locks shared with the API's ad-hoc refresh handler.
    endpoint_exploration_locks: EndpointExplorationLocks,
    machine_creator: MachineCreator,
    switch_creator: SwitchCreator,
    boot_order_tracker: BootOrderTracker,
    // rms_client: Option<Arc<dyn RmsApi>>,
}

impl SiteExplorer {
    const ITERATION_WORK_KEY: &'static str = "SiteExplorer::run_single_iteration";
    const SITE_EXPLORER_HEALTH_REPORT_WRITE_BATCH_SIZE: usize = 500;

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        database_connection: sqlx::PgPool,
        explorer_config: SiteExplorerConfig,
        meter: opentelemetry::metrics::Meter,
        endpoint_explorer: Arc<dyn EndpointExplorer>,
        firmware_config: Arc<FirmwareConfig>,
        common_pools: Arc<CommonPools>,
        work_lock_manager_handle: WorkLockManagerHandle,
        endpoint_exploration_locks: EndpointExplorationLocks,
        rack_profiles: RackProfileConfig,
        rms_client: Option<Arc<dyn RmsApi>>,
        credential_manager: Arc<dyn CredentialManager>,
    ) -> Self {
        // We want to hold metrics for longer than the iteration interval, so there is continuity
        // in emitting metrics. However we want to avoid reporting outdated metrics in case
        // reporting gets stuck. Therefore round up the iteration interval by 1min.
        let hold_period = explorer_config
            .run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(
            meter,
            hold_period,
            &explorer_config,
        ));
        let rack_profiles = Arc::new(rack_profiles);

        SiteExplorer {
            machine_creator: MachineCreator::new(
                database_connection.clone(),
                explorer_config.clone(),
                common_pools,
                rack_profiles,
                rms_client.clone(),
                credential_manager,
            ),
            switch_creator: SwitchCreator::new(
                database_connection.clone(),
                explorer_config.clone(),
            ),
            database_connection,
            config: explorer_config,
            metric_holder,
            endpoint_explorer,
            firmware_config,
            work_lock_manager_handle,
            endpoint_exploration_locks,
            boot_order_tracker: BootOrderTracker::default(),
        }
    }

    /// Start the SiteExplorer background task. The task always runs and checks
    /// `config.enabled` each iteration, allowing runtime pause/unpause via the API.
    pub fn start(
        mut self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        join_set
            .build_task()
            .name("site_explorer")
            .spawn(async move { self.run(cancel_token).await })?;

        Ok(())
    }

    async fn firmware_config_snapshot(&self) -> SiteExplorerResult<FirmwareConfigSnapshot> {
        let host_firmware_configs =
            db::host_firmware_config::list_configs(&self.database_connection).await?;

        Ok(self
            .firmware_config
            .create_snapshot_with_overrides(host_firmware_configs))
    }

    async fn run(&mut self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.run_interval);
        loop {
            let tick = timer.tick();

            if self.config.enabled.load(Ordering::Relaxed) {
                match self.run_single_iteration().await {
                    Ok(identified_hosts) => self
                        .boot_order_tracker
                        .track_hosts(Instant::now(), &identified_hosts),
                    Err(e) => {
                        tracing::warn!("SiteExplorer error: {}", e);
                    }
                }
            } else {
                tracing::warn!("SiteExplorer is disabled, skipping iteration");
            }

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("SiteExplorer stop was requested");
                    return;
                }
            }
        }
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    fn txn_begin(&self) -> impl Future<Output = SiteExplorerResult<db::Transaction<'_>>> {
        let loc = Location::caller();
        db::Transaction::begin_with_location(&self.database_connection, loc).map_err(Into::into)
    }

    fn last_run_status(
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        metrics: &SiteExplorationMetrics,
        result: &SiteExplorerResult<SiteIdentifiedHosts>,
    ) -> SiteExplorerLastRun {
        let failure_category = result.as_ref().err().map(Self::run_failure_category);
        SiteExplorerLastRun {
            started_at,
            finished_at,
            success: result.is_ok(),
            error: result.as_ref().err().map(Self::operator_error_message),
            failure_category,
            endpoint_explorations: metrics.endpoint_explorations as i64,
            endpoint_explorations_success: metrics.endpoint_explorations_success as i64,
            endpoint_explorations_failed: metrics
                .endpoint_explorations_failures_by_type
                .values()
                .sum::<usize>() as i64,
            last_successful_finished_at: result.is_ok().then_some(finished_at),
            last_failed_finished_at: result.is_err().then_some(finished_at),
        }
    }

    fn run_failure_category(error: &SiteExplorerError) -> String {
        match error {
            SiteExplorerError::DatabaseError(_) => "database_error",
            SiteExplorerError::ModelError(_) => "model_error",
            SiteExplorerError::AlreadyFoundError { .. } => "already_found",
            SiteExplorerError::NotFoundError { .. } => "not_found",
            SiteExplorerError::InvalidArgument(_) => "invalid_argument",
            SiteExplorerError::EndpointExplorationError { err, .. } => {
                return exploration_error_to_metric_label(err);
            }
            SiteExplorerError::Internal { .. } => "internal",
        }
        .to_string()
    }

    fn operator_error_message(error: &SiteExplorerError) -> String {
        match error {
            SiteExplorerError::EndpointExplorationError {
                err:
                    EndpointExplorationError::MissingCredentials { .. }
                    | EndpointExplorationError::SetCredentials { .. },
                ..
            } => "Site Explorer credentials are missing or invalid".to_string(),
            SiteExplorerError::EndpointExplorationError {
                err: EndpointExplorationError::SecretsEngineError { .. },
                ..
            } => "Site Explorer could not access credentials".to_string(),
            _ => error.to_string(),
        }
    }

    fn record_run_status_metric(
        metrics: &mut SiteExplorationMetrics,
        result: &SiteExplorerResult<SiteIdentifiedHosts>,
    ) {
        metrics.run_failure_category = result.as_ref().err().map(Self::run_failure_category);
    }

    async fn record_last_run(&self, last_run: &SiteExplorerLastRun) -> SiteExplorerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::site_explorer_run_status::upsert(&mut txn, last_run).await?;
        txn.commit().await?;
        Ok(())
    }

    async fn record_last_run_result(
        &self,
        started_at: chrono::DateTime<Utc>,
        metrics: &SiteExplorationMetrics,
        result: &SiteExplorerResult<SiteIdentifiedHosts>,
    ) {
        let last_run = Self::last_run_status(started_at, Utc::now(), metrics, result);
        if let Err(error) = self.record_last_run(&last_run).await {
            tracing::error!(%error, "Failed to record SiteExplorer last run status");
        }
    }

    pub async fn run_single_iteration(&self) -> SiteExplorerResult<SiteIdentifiedHosts> {
        let started_at = Utc::now();
        let mut metrics = SiteExplorationMetrics::new();

        let _work_lock = match self
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(e) => {
                let result = Err(SiteExplorerError::internal(format!(
                    "Failed to acquire connection: {e}"
                )));
                Self::record_run_status_metric(&mut metrics, &result);
                self.record_last_run_result(started_at, &metrics, &result)
                    .await;
                self.metric_holder.update_metrics(metrics);
                return result;
            }
        };

        tracing::trace!(
            lock = SiteExplorer::ITERATION_WORK_KEY,
            "SiteExplorer acquired the lock",
        );

        let span_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));

        let explore_site_span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "explore_site",
            span_id,
            carbide.trace_root = true,
            component = "site-explorer",
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
            created_machines = tracing::field::Empty,
            identified_managed_hosts = tracing::field::Empty,
            endpoint_explorations = tracing::field::Empty,
            endpoint_explorations_success = tracing::field::Empty,
            endpoint_explorations_failures = tracing::field::Empty,
            endpoint_explorations_failures_by_type = tracing::field::Empty,
        );

        let res = self
            .explore_site(&mut metrics)
            .instrument(explore_site_span.clone())
            .await;
        explore_site_span.record(
            "identified_managed_hosts",
            metrics.exploration_identified_managed_hosts,
        );
        explore_site_span.record("created_machines", metrics.created_machines);
        explore_site_span.record("endpoint_explorations", metrics.endpoint_explorations);
        explore_site_span.record(
            "endpoint_explorations_success",
            metrics.endpoint_explorations_success,
        );
        explore_site_span.record(
            "endpoint_explorations_failures",
            metrics
                .endpoint_explorations_failures_by_type
                .values()
                .sum::<usize>(),
        );
        explore_site_span.record(
            "endpoint_explorations_failures_by_type",
            serde_json::to_string(&metrics.endpoint_explorations_failures_by_type)
                .unwrap_or_default(),
        );

        match &res {
            Ok(_) => {
                explore_site_span.record("otel.status_code", "ok");
            }
            Err(e) => {
                tracing::error!("SiteExplorer run failed due to: {:?}", e);
                explore_site_span.record("otel.status_code", "error");
                // Writing this field will set the span status to error
                // Therefore we only write it on errors
                explore_site_span.record("otel.status_message", format!("{e:?}"));
            }
        }

        Self::record_run_status_metric(&mut metrics, &res);
        self.record_last_run_result(started_at, &metrics, &res)
            .await;

        // Cache all other metrics that have been captured in this iteration.
        // Those will be queried by OTEL on demand
        self.metric_holder.update_metrics(metrics);

        res
    }

    /// Audits and collects metrics of _all_ explored results vs. _all_ expected machines, not a single exploration cycle.
    /// Also updates the Site Explorer Health Report for all explored endpoints based on the last exploration data.
    ///
    /// * `metrics`                   - A metrics collector for accumulating and later emitting metrics.
    /// * `matched_expected_machines` - A map of expected machines that have been matched to interfaces, indexed by IP(s).
    async fn audit_exploration_results(
        &self,
        metrics: &mut SiteExplorationMetrics,
        expected_endpoint_index: &ExploredEndpointIndex,
    ) -> SiteExplorerResult<()> {
        let audit_load_start = Instant::now();
        let mut txn = self.txn_begin().await?;

        // Grab them all because we care about everything,
        // not just the subset in the current run.
        let explored_endpoints = db::explored_endpoints::find_all(txn.as_pgconn()).await?;
        let explored_managed_hosts = db::explored_managed_host::find_all(txn.as_pgconn()).await?;

        txn.rollback().await?;
        metrics.record_phase_latency("audit_load", audit_load_start.elapsed());

        let bmc_endpoint_addresses = explored_endpoints
            .iter()
            .filter(|ep| ep.report.endpoint_type == EndpointType::Bmc)
            .map(|ep| ep.address)
            .collect_vec();
        let audit_state_load_start = Instant::now();
        let mut txn = self.txn_begin().await?;
        let machine_audit_states = db::machine::find_site_explorer_machine_audit_states_by_bmc_ips(
            &mut txn,
            &bmc_endpoint_addresses,
        )
        .await?;
        txn.rollback().await?;
        metrics.record_phase_latency("audit_state_load", audit_state_load_start.elapsed());
        let machine_audit_states: HashMap<IpAddr, db::machine::SiteExplorerMachineAuditState> =
            machine_audit_states
                .into_iter()
                .map(|state| (state.bmc_ip, state))
                .collect();
        let mut pending_health_report_updates = Vec::new();
        let audit_compute_start = Instant::now();

        // Go through all the explored endpoints and collect metrics and submit
        // health reports
        for ep in explored_endpoints.into_iter() {
            if ep.report.endpoint_type != EndpointType::Bmc {
                // Skip anything that isn't a BMC.
                continue;
            }

            // We need to find the last health report for the endpoint in order to update it with latest health data
            let machine_audit_state = machine_audit_states.get(&ep.address);
            let machine_id = machine_audit_state.map(|state| state.machine_id);
            let previous_health_report =
                machine_audit_state.and_then(|state| state.site_explorer_health_report.as_ref());
            let mut new_health_report: health_report::HealthReport =
                health_report::HealthReport::empty(
                    health_report::HealthReport::SITE_EXPLORER_SOURCE.to_string(),
                );

            if let Some(ref e) = ep.report.last_exploration_error {
                metrics.increment_endpoint_explorations_failures_overall_count(
                    exploration_error_to_metric_label(e),
                );
                // Despite the last exploration failing, there might still be additional
                // endpoint information available. There might even be an ingested
                // Machine that corresponds to that endpoint.

                // The target allows to distinguish multiple DPUs which might
                // exhibit different alerts
                new_health_report
                    .alerts
                    .push(health_report::HealthProbeAlert {
                        id: "BmcExplorationFailure".parse().unwrap(),
                        target: Some(ep.address.to_string()),
                        in_alert_since: None,
                        message: format!("Endpoint exploration failed: {e}"),
                        tenant_message: None,
                        classifications: vec![
                            health_report::HealthAlertClassification::prevent_allocations(),
                        ],
                    });
            }

            for system in ep.report.systems.iter() {
                if should_alert_power_state(system.power_state) {
                    new_health_report
                        .alerts
                        .push(health_report::HealthProbeAlert {
                            // PoweredOff alert ID covers Off/Paused/Unknown states
                            id: "PoweredOff".parse().unwrap(),
                            target: Some(ep.address.to_string()),
                            in_alert_since: None,
                            message: format!(
                                "System \"{}\" power state is \"{:?}\"",
                                system.id, system.power_state
                            ),
                            tenant_message: None,
                            classifications: vec![
                                health_report::HealthAlertClassification::prevent_allocations(),
                            ],
                        });
                    break;
                }
            }

            let expected_machine = expected_endpoint_index.matched_expected_machine(&ep.address);

            let (machine_type, expected) = match ep.report.is_dpu() {
                true => (MachineType::Dpu, MachineExpectation::NotApplicable),
                false => (MachineType::Host, expected_machine.is_some().into()),
            };

            // Track machines in a preingestion state.
            if ep.preingestion_state != PreingestionState::Complete {
                metrics.increment_endpoint_explorations_preingestions_incomplete_overall_count(
                    expected,
                    machine_type,
                );
            }

            // Increment total exploration counts
            metrics.increment_endpoint_explorations_machines_explored_overall_count(
                expected,
                machine_type,
            );

            if let Some(expected_machine) = expected_machine {
                let expected_sn = &expected_machine.data.serial_number;

                // Check expected vs actual serial number
                // using system serial numbers.
                // If nothing found, try again with chassis
                // serial numbers.
                if !ep
                    .report
                    .systems
                    .iter()
                    .any(|s| s.check_serial_number(expected_sn) || s.check_sku(expected_sn))
                    && !ep.report.chassis.iter().any(|s| match s.serial_number {
                        Some(ref sn) => sn == expected_sn,
                        _ => false,
                    })
                {
                    metrics
                        .increment_endpoint_explorations_expected_serial_number_mismatches_overall_count(
                            machine_type,
                        );

                    new_health_report
                        .alerts
                        .push(health_report::HealthProbeAlert {
                            id: "SerialNumberMismatch".parse().unwrap(),
                            target: Some(ep.address.to_string()),
                            in_alert_since: None,
                            message: format!(
                                "Expected serial number {expected_sn} can not be found"
                            ),
                            tenant_message: None,
                            classifications: vec![
                                health_report::HealthAlertClassification::prevent_allocations(),
                            ],
                        });
                }
            } else if matches!(machine_type, MachineType::Host) && machine_id.is_some() {
                // Orphan: a Managed Host whose BMC MAC is no longer listed in
                // `expected_machines`. Carbide keeps maintaining the host, but
                // it will not be re-ingested if force-deleted. This alert is a warning
                // only and does not block allocations.
                new_health_report
                    .alerts
                    .push(health_report::HealthProbeAlert {
                        id: "OrphanManagedHost".parse().unwrap(),
                        target: None,
                        in_alert_since: None,
                        message: "This managed host is not listed in Expected Machines".to_string(),
                        tenant_message: None,
                        classifications: vec![],
                    });
            }

            new_health_report.update_in_alert_since(previous_health_report);
            if let Some(id) = machine_id
                && site_explorer_health_report_needs_update(
                    previous_health_report,
                    &new_health_report,
                )
            {
                pending_health_report_updates.push((id, new_health_report));
            }
        }
        metrics.record_phase_latency("audit_compute", audit_compute_start.elapsed());

        let audit_write_start = Instant::now();
        for health_report_updates in
            pending_health_report_updates.chunks(Self::SITE_EXPLORER_HEALTH_REPORT_WRITE_BATCH_SIZE)
        {
            let mut txn = self.txn_begin().await?;
            for (id, health_report) in health_report_updates {
                db::machine::update_site_explorer_health_report(txn.as_pgconn(), id, health_report)
                    .await?;
            }
            txn.commit().await?;
        }
        metrics.record_phase_latency("audit_write", audit_write_start.elapsed());

        // Count the total number of explored managed hosts
        for explored_managed_host in explored_managed_hosts {
            metrics.increment_endpoint_explorations_identified_managed_hosts_overall_count(
                expected_endpoint_index
                    .matched_expected_machine(&explored_managed_host.host_bmc_ip)
                    .is_some()
                    .into(),
            );
        }

        Ok(())
    }

    async fn explore_site(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<SiteIdentifiedHosts> {
        self.check_preconditions(metrics).await?;

        let update_explored_endpoints_start = Instant::now();
        let expected_endpoint_index = self.update_explored_endpoints(metrics).await?;
        metrics.record_phase_latency(
            "update_explored_endpoints",
            update_explored_endpoints_start.elapsed(),
        );

        // Create a list of DPUs and hosts that site explorer should try to ingest. Site explorer uses the following criteria to determine whether
        // to ingest a given endpoint (creating a managed host containing the endpoint and adding it to the state machine):
        // 1) Pre-ingestion must have completed for a given endpoint
        // 2a) If the endpoint is for a DPU: make sure that site explorer can retrieve the mac address of the pf0 interface that the DPU exposes to the host.
        // If site explorer is unable to retrieve this mac address, there is no point in creating a managed host: we will not be able to configure the host appropriately.
        // 2b) If the endpoint is for a host: make sure that the host is on and that infinite boot is enabled. Otherwise, we will not be able to provision the DPU appropriately
        // once we create a managed host and add it to the state machine.
        let identify_machines_to_ingest_start = Instant::now();
        let (explored_dpus, explored_hosts) = self.identify_machines_to_ingest(metrics).await?;
        metrics.record_phase_latency(
            "identify_machines_to_ingest",
            identify_machines_to_ingest_start.elapsed(),
        );

        // Note/TODO:
        // Since we generate the managed-host pair in a different transaction than endpoint discovery,
        // the generation of both reports is not necessarily atomic.
        // This is improvable
        // However since host information rarely changes (we never reassign MachineInterfaces),
        // this should be ok. The most noticeable effect is that ManagedHost population might be delayed a bit.
        let identify_managed_hosts_start = Instant::now();
        let mut identified_hosts = self
            .identify_managed_hosts(
                metrics,
                &expected_endpoint_index,
                explored_dpus,
                explored_hosts,
            )
            .await?;
        metrics.record_phase_latency(
            "identify_managed_hosts",
            identify_managed_hosts_start.elapsed(),
        );

        if self.config.create_machines.load(Ordering::Relaxed) {
            let start_create_machines = Instant::now();
            let create_machines_res = self
                .machine_creator
                .create_machines(metrics, &mut identified_hosts, &expected_endpoint_index)
                .await;
            let create_machines_latency = start_create_machines.elapsed();
            metrics.create_machines_latency = Some(create_machines_latency);
            metrics.record_phase_latency("create_machines", create_machines_latency);
            create_machines_res?;
        }

        // Identify and create power shelves
        let identify_power_shelves_to_ingest_start = Instant::now();
        let explored_power_shelves = self.identify_power_shelves_to_ingest().await?;
        metrics.record_phase_latency(
            "identify_power_shelves_to_ingest",
            identify_power_shelves_to_ingest_start.elapsed(),
        );

        if self.config.create_power_shelves.load(Ordering::Relaxed) {
            let start_create_power_shelves = Instant::now();
            let create_power_shelves_res = self
                .create_power_shelves(metrics, explored_power_shelves, &expected_endpoint_index)
                .await;
            let create_power_shelves_latency = start_create_power_shelves.elapsed();
            metrics.create_power_shelves_latency = Some(create_power_shelves_latency);
            metrics.record_phase_latency("create_power_shelves", create_power_shelves_latency);
            create_power_shelves_res?;
        }

        // Identify and create switches
        let identify_switches_to_ingest_start = Instant::now();
        let explored_switches = self.identify_switches_to_ingest().await?;
        metrics.record_phase_latency(
            "identify_switches_to_ingest",
            identify_switches_to_ingest_start.elapsed(),
        );

        if self.config.create_switches.load(Ordering::Relaxed) {
            let start_create_switches = Instant::now();
            let create_switches_res = self
                .switch_creator
                .create_switches(metrics, &explored_switches, &expected_endpoint_index)
                .await;
            let create_switches_latency = start_create_switches.elapsed();
            metrics.create_switches_latency = Some(create_switches_latency);
            metrics.record_phase_latency("create_switches", create_switches_latency);
            create_switches_res?;
        }

        // Audit after everything has been explored, identified, and created.
        let audit_exploration_results_start = Instant::now();
        self.audit_exploration_results(metrics, &expected_endpoint_index)
            .await?;
        metrics.record_phase_latency(
            "audit_exploration_results",
            audit_exploration_results_start.elapsed(),
        );

        // Retained boot interface records that aged out of the configured
        // window are already ignored at read time; sweep them once per pass
        // so MACs that never return don't occupy table rows indefinitely.
        // (A no-op without a window: records wait for their machine.)
        if self.config.retained_boot_interface_window.is_some() {
            let mut txn = self
                .database_connection
                .begin()
                .await
                .map_err(|e| DatabaseError::new("begin retained boot interface sweep", e))?;
            let swept = db::retained_boot_interface::delete_expired(
                &mut txn,
                self.config.retained_boot_interface_window,
            )
            .await?;
            txn.commit()
                .await
                .map_err(|e| DatabaseError::new("end retained boot interface sweep", e))?;
            if swept > 0 {
                tracing::info!(swept, "Removed expired retained boot interface records");
            }
        }

        Ok(identified_hosts)
    }

    async fn create_power_shelves(
        &self,
        metrics: &mut SiteExplorationMetrics,
        explored_power_shelves: Vec<(ExploredEndpoint, EndpointExplorationReport)>,
        expected_endpoint_index: &ExploredEndpointIndex,
    ) -> SiteExplorerResult<()> {
        for (endpoint, _report) in explored_power_shelves {
            let address = endpoint.address;
            let Some(expected_power_shelf) =
                expected_endpoint_index.matched_expected_power_shelf(&endpoint.address)
            else {
                tracing::info!(
                    "No expected power shelf found for endpoint {:#?}",
                    endpoint.address
                );
                continue;
            };

            match self
                .create_power_shelf(endpoint, expected_power_shelf, &self.database_connection)
                .await
            {
                Ok(true) => {
                    metrics.created_power_shelves_count += 1;
                    if metrics.created_power_shelves_count as u64
                        == self.config.power_shelves_created_per_run
                    {
                        break;
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(%error, "Failed to create power shelf {:#?}", address)
                }
            }
        }

        Ok(())
    }

    pub async fn create_power_shelf(
        &self,
        explored_endpoint: ExploredEndpoint,
        expected_shelf: &ExpectedPowerShelf,
        pool: &PgPool,
    ) -> SiteExplorerResult<bool> {
        let mut txn = pool
            .begin()
            .await
            .map_err(|e| DatabaseError::new("begin load create_power_shelf", e))?;

        tracing::info!(
            "creating power shelf for endpoint: {} ",
            explored_endpoint.address
        );

        // Defense against the duplicate-power-shelves bug: if a power shelf
        // already exists in the database for this BMC MAC, don't make another
        // one. This mirrors the dedup check on the switch creation path and
        // catches the case where the input we hash to mint the
        // `PowerShelfId` drifts between exploration cycles.
        if let Some(existing) =
            db_power_shelf::find_by_bmc_mac_address(&mut txn, expected_shelf.bmc_mac_address)
                .await?
        {
            tracing::warn!(
                bmc_mac = %expected_shelf.bmc_mac_address,
                existing_power_shelf_id = %existing.id,
                endpoint = %explored_endpoint.address,
                "Power shelf already exists for this BMC MAC; skipping discovery",
            );
            txn.rollback()
                .await
                .map_err(|e| DatabaseError::new("rollback create_power_shelf", e))?;
            return Ok(false);
        }

        // Check if a power shelf with the same name already exists
        if !expected_shelf.metadata.name.is_empty() {
            let existing_power_shelves = db_power_shelf::find_by(
                &mut txn,
                ObjectColumnFilter::All::<db::power_shelf::NameColumn>,
            )
            .await?;

            // Check if any existing power shelf has the same name
            for existing_ps in &existing_power_shelves {
                if existing_ps.config.name == expected_shelf.metadata.name {
                    tracing::info!(
                        "Power shelf with name '{}' already exists, skipping creation for endpoint {}",
                        &expected_shelf.metadata.name,
                        explored_endpoint.address
                    );
                    txn.rollback()
                        .await
                        .map_err(|e| DatabaseError::new("rollback create_power_shelf", e))?;
                    return Ok(false);
                }
            }
        }

        // Create a new power shelf
        // Generate power_shelf_id similar to machine_id using deterministic hashing.
        // Extract serial / vendor / model from the chassis reported by the
        // explored endpoint. Prefer a chassis whose id identifies it as a
        // power shelf, falling back to the first chassis if none match.
        // Fall back to sensible defaults if the chassis is missing fields so
        // that we can still mint a stable id during exploration.
        let chassis_list = &explored_endpoint.report.chassis;
        let power_shelf_chassis = chassis_list
            .iter()
            .find(|c| c.id.to_lowercase().contains("powershelf"))
            .or_else(|| chassis_list.first());

        if power_shelf_chassis.is_none() {
            tracing::warn!(
                endpoint = %explored_endpoint.address,
                "No chassis reported for power shelf endpoint; falling back to defaults for id generation",
            );
        }

        let power_shelf_serial = power_shelf_chassis
            .and_then(|c| c.serial_number.as_deref())
            .unwrap_or(expected_shelf.metadata.name.as_str());
        let power_shelf_vendor = power_shelf_chassis
            .and_then(|c| c.manufacturer.as_deref())
            .unwrap_or("NVIDIA");
        let power_shelf_model = power_shelf_chassis
            .and_then(|c| c.model.as_deref())
            .unwrap_or("PowerShelf");
        let power_shelf_id = match model::power_shelf::power_shelf_id::from_hardware_info(
            power_shelf_serial,
            power_shelf_vendor,
            power_shelf_model,
            PowerShelfIdSource::ProductBoardChassisSerial,
            PowerShelfType::Rack,
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(%e, "Failed to create power shelf ID");
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "Failed to create power shelf ID: {e}"
                )));
            }
        };

        let config = PowerShelfConfig {
            name: expected_shelf.metadata.name.clone(),
            capacity: Some(100),
            voltage: Some(240),
        };

        let new_power_shelf = NewPowerShelf {
            id: power_shelf_id,
            config,
            bmc_mac_address: Some(expected_shelf.bmc_mac_address),
            metadata: Some(expected_shelf.metadata.clone()),
            rack_id: expected_shelf.rack_id.clone(),
        };

        db_power_shelf::create(&mut txn, &new_power_shelf).await?;

        let mi =
            db::machine_interface::find_by_mac_address(&mut *txn, expected_shelf.bmc_mac_address)
                .await?;
        if let Some(interface) = mi.first() {
            // A power shelf's BMC/PMC interface is its management endpoint, so
            // associate it with the shelf and annotate it as `Bmc` (like
            // host/switch BMC interfaces), demoting it from primary in one
            // statement. This `Bmc` link is what lets the power-shelf load query
            // resolve the shelf's MAC/IP into the `PowerShelf.bmc_info` field.
            db::machine_interface::associate_bmc_interface(
                &interface.id,
                model::machine_interface_address::MachineInterfaceAssociation::PowerShelf(
                    power_shelf_id,
                ),
                &mut txn,
            )
            .await?;
        }

        if let Some(ref rack_id) = expected_shelf.rack_id {
            let _ = crate::ensure_rack_exists(txn.as_mut(), rack_id).await?;
        }
        // No need to update the power shelf name again; it was already set in config above.
        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("end create_power_shelf", e))?;

        tracing::info!(
            "Created power shelf {} for endpoint {}",
            power_shelf_id,
            explored_endpoint.address
        );

        Ok(true)
    }

    /// identify_machines_to_ingest returns two maps.
    /// The first map returned identifies all of the DPUs that site explorer will try to ingest.
    /// The latter identifies all of the hosts the the site explorer will try to ingest.
    /// Both map from machine BMC IP address to the corresponding explored endpoint.
    async fn identify_machines_to_ingest(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<(
        HashMap<IpAddr, ExploredEndpoint>,
        HashMap<IpAddr, ExploredEndpoint>,
    )> {
        let mut txn = self.txn_begin().await?;

        // TODO: We reload the endpoint list even though we just regenerated it
        // Could optimize this by keeping it in memory. But since the manipulations
        // are quite complicated in the previous step, this makes things much easier
        let explored_endpoints =
            db::explored_endpoints::find_all_preingestion_complete(&mut txn).await?;

        txn.commit().await?;

        let mut explored_dpus = HashMap::new();
        let mut explored_hosts = HashMap::new();
        for ep in explored_endpoints.into_iter() {
            if ep.report.endpoint_type != EndpointType::Bmc {
                continue;
            }

            if ep.report.is_power_shelf() {
                continue;
            }

            if ep.report.is_switch() {
                continue;
            }

            if ep.report.is_dpu() {
                if self.can_ingest_dpu_endpoint(metrics, &ep).await? {
                    explored_dpus.insert(ep.address, ep);
                }
            } else if self.can_ingest_host_endpoint(metrics, &ep).await? {
                explored_hosts.insert(ep.address, ep);
            }
        }

        Ok((explored_dpus, explored_hosts))
    }

    async fn identify_managed_hosts(
        &self,
        metrics: &mut SiteExplorationMetrics,
        expected_explored_endpoint_index: &ExploredEndpointIndex,
        explored_dpus: HashMap<IpAddr, ExploredEndpoint>,
        explored_hosts: HashMap<IpAddr, ExploredEndpoint>,
    ) -> SiteExplorerResult<Vec<(ExploredManagedHost, EndpointExplorationReport)>> {
        // Per-host DPU-mode resolution. Precedence:
        //   1. Per-host `ExpectedMachine.dpu_mode` (NicMode / NoDpu wins).
        //   2. Site-wide `SiteExplorerConfig.dpu_mode` setting.
        //   3. Otherwise: `DpuMode::DpuMode` (the absolute default).
        let site_dpu_mode = self.config.dpu_mode;
        let effective_mode = |host_bmc_ip: &IpAddr| -> DpuMode {
            let declared = expected_explored_endpoint_index
                .matched_expected_machine(host_bmc_ip)
                .map(|em| em.data.dpu_mode);
            DpuMode::resolve(declared, site_dpu_mode)
        };
        // Match HOST and DPU using the serial Redfish reports for the same
        // physical card. BF4 does not expose that serial on the DPU system
        // object, so this uses `EndpointExplorationReport::dpu_pairing_serial_number`
        // rather than reading `systems[0].serial_number` directly.
        let mut dpu_sn_to_endpoint = HashMap::new();
        for (_, ep) in explored_dpus {
            if let Some(sn) = ep.report.dpu_pairing_serial_number() {
                dpu_sn_to_endpoint.insert(sn.to_string(), ep);
            }
        }

        let mut managed_hosts = Vec::new();
        let mut boot_interfaces: Vec<(IpAddr, MachineBootInterface)> = Vec::new();
        // Each host NIC's full boot interface (MAC + Redfish id), to record on
        // its machine_interfaces row so the primary row holds the complete pair.
        let mut nic_boot_interfaces: Vec<MachineBootInterface> = Vec::new();

        for (_, ep) in explored_hosts {
            // Record every host NIC's boot interface (MAC + Redfish id) on its
            // machine_interfaces row (matched by MAC), so the primary-flagged
            // row holds the full pair -- whatever the NIC type (integrated NICs,
            // SuperNICs, DPU host-PFs, DPUs in NIC mode). This sits before the
            // zero-DPU/NoDpu and unmatched-host `continue`s below, so every
            // explored host is covered -- including a zero-DPU host whose primary
            // boots from a plain NIC. The UPDATE no-ops for MACs with no row
            // (e.g. a never-cabled NIC). Last-known-good: only NICs that resolve
            // a full pair in this report are recorded, so a wiped MAC keeps its
            // prior id.
            nic_boot_interfaces.extend(ep.report.complete_boot_interfaces());

            // Surface partial records -- a host NIC reporting only one of (MAC,
            // interface id). `complete_boot_interfaces` skips these, so log here
            // to make it visible that we saw, and ignored, an incomplete NIC.
            for iface in ep
                .report
                .systems
                .iter()
                .flat_map(|s| s.ethernet_interfaces.iter())
            {
                let has_mac = iface.mac_address.is_some();
                let has_id = iface.id.as_deref().is_some_and(|s| !s.is_empty());
                if has_mac != has_id {
                    tracing::info!(
                        address = %ep.address,
                        mac = ?iface.mac_address,
                        interface_id = ?iface.id,
                        "site-explorer: host NIC reported with only one of (MAC, interface id) -- not recording its boot interface",
                    );
                }
            }

            // Resolve the operator-declared DPU mode for this host once;
            // it drives both auto-correction (`check_and_configure_dpu_mode`
            // below -- operator override wins over BF3 part-number heuristics)
            // and the post-match attach decision (NicMode/NoDpu hosts emit
            // a bare managed host regardless of what matched).
            let host_dpu_mode = effective_mode(&ep.address);

            // If an operator has declared this host `dpu_mode::NoDpu`,
            // treat it as zero-DPU, regardless of what BMC hardware
            // enumeration says about attached DPUs. Without this check,
            // we can't ingest hosts which may have >= DPUs, but aren't
            // actively using them. For instance, a machine may have DPUs
            // that aren't actually cabled up, and we're instead using a
            // basic NIC. Since they aren't cabled, we'll never be able to
            // discover + link them; just ignore them entirely.
            if matches!(host_dpu_mode, DpuMode::NoDpu) {
                managed_hosts.push((
                    ExploredManagedHost {
                        host_bmc_ip: ep.address,
                        dpus: Vec::new(),
                    },
                    ep.report,
                ));
                metrics.exploration_identified_managed_hosts += 1;
                continue;
            }

            // Record the host's DPU devices against the discovered DPU BMCs.
            // A DPU can appear as a PCIe device under a system or as a chassis
            // network adapter (vendor-dependent), so we scan the PCIe inventory first
            // and fall back to chassis adapters only if it turned up nothing. The
            // per-device logic -- counting, `set_nic_mode` auto-correction, NIC-mode
            // stripping -- lives once in `record_host_dpu_device` / `classify_matched_dpu`.
            let mut dpu_exploration = DpuExplorationState::new();
            let mut seen_bluefield_serials = HashSet::new();
            for system in ep.report.systems.iter() {
                for pcie_device in system.pcie_devices.iter() {
                    if let Some(serial_number) = duplicate_bluefield_serial(
                        pcie_device.part_number.as_deref(),
                        pcie_device.serial_number.as_deref(),
                        &mut seen_bluefield_serials,
                    ) {
                        tracing::warn!(
                            host_bmc_ip = %ep.address,
                            %serial_number,
                            pcie_device_id = ?pcie_device.id,
                            "duplicate BlueField serial in host PCIe inventory; skipping duplicate record",
                        );
                        continue;
                    }
                    self.record_host_dpu_device(
                        pcie_device.part_number.as_deref(),
                        pcie_device.serial_number.as_deref(),
                        &dpu_sn_to_endpoint,
                        host_dpu_mode,
                        &ep,
                        &mut dpu_exploration,
                        metrics,
                    )
                    .await;
                }
            }

            // A DPU can show up as a chassis instead of a PCIe device on some
            // BMCs; fall back to the chassis inventory only if the PCIe scan
            // found none.
            if dpu_exploration.expected_managed_total() == 0 {
                for chassis in ep.report.chassis.iter() {
                    // Some BMCs (e.g. the AMI/Lenovo GB300 host BMC) report the
                    // BlueField as the chassis object itself -- model, part_number
                    // and serial_number live on the chassis, while its nested
                    // network adapter carries an empty serial. Match on the
                    // chassis identity in that case; otherwise fall back to the
                    // chassis's network adapters (other vendors put the DPU
                    // serial there). Matching only one of the two per chassis
                    // keeps a single DPU from being counted twice.
                    let chassis_is_bluefield = chassis
                        .part_number
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(is_bluefield_part_number);
                    let chassis_has_serial = chassis
                        .serial_number
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|serial| !serial.is_empty());
                    if chassis_is_bluefield && chassis_has_serial {
                        self.record_host_dpu_device(
                            chassis.part_number.as_deref(),
                            chassis.serial_number.as_deref(),
                            &dpu_sn_to_endpoint,
                            host_dpu_mode,
                            &ep,
                            &mut dpu_exploration,
                            metrics,
                        )
                        .await;
                    } else {
                        for network_adapter in chassis.network_adapters.iter() {
                            self.record_host_dpu_device(
                                network_adapter.part_number.as_deref(),
                                network_adapter.serial_number.as_deref(),
                                &dpu_sn_to_endpoint,
                                host_dpu_mode,
                                &ep,
                                &mut dpu_exploration,
                                metrics,
                            )
                            .await;
                        }
                    }
                }
            }

            // Bring the accumulated counts into variables that the rest
            // of this function uses.
            let DpuExplorationState {
                reported_total: host_reported_dpus_total,
                running_as_nic_total: mut host_reported_dpus_nic_mode_total,
                all_configured: mut all_dpus_configured_properly_in_host,
                running_as_dpu: mut dpus_explored_for_host,
            } = dpu_exploration;

            if dpus_explored_for_host.is_empty()
                || dpus_explored_for_host.len()
                    != host_reported_dpus_total.saturating_sub(host_reported_dpus_nic_mode_total)
            {
                // Check if there are dpu serial(s) specified in expected_machine table for this host
                // Lets assume for now that if a DPU is specific in the expected machine table for the host
                // it has been configured properly (DPU vs NIC mode).
                let mut dpu_added = false;
                if let Some(expected_machine) =
                    expected_explored_endpoint_index.matched_expected_machine(&ep.address)
                {
                    for dpu_sn in &expected_machine.data.fallback_dpu_serial_numbers {
                        if let Some(dpu_ep) = dpu_sn_to_endpoint.remove(dpu_sn.as_str()) {
                            // Fallback matching has only the expected serial and the
                            // discovered DPU BMC report; pass the DPU's own part number
                            // for the legacy BF3 heuristic.
                            let mode_check = Some(
                                self.check_and_configure_dpu_mode(
                                    &dpu_ep,
                                    dpu_ep.report.dpu_part_number(),
                                    host_dpu_mode,
                                    metrics,
                                )
                                .await,
                            );

                            match classify_matched_dpu(&dpu_ep, &ep, mode_check) {
                                DiscoveredDpu::RunningAsDpu(dpu) => {
                                    // The expected-machine fallback list is the source of
                                    // truth here, so discard whatever the PCIe scan found
                                    // on the first confirmed match.
                                    if !dpu_added {
                                        dpus_explored_for_host.clear();
                                    }
                                    dpu_added = true;
                                    dpus_explored_for_host.push(dpu);
                                }
                                DiscoveredDpu::RunningAsNic => {
                                    host_reported_dpus_nic_mode_total += 1;
                                }
                                DiscoveredDpu::NeedsReconfig => {
                                    // `set_nic_mode` was just issued; the host needs a
                                    // reset before this DPU re-reports in the new mode, so
                                    // mark it not-yet-configured and let the reset path
                                    // below run.
                                    all_dpus_configured_properly_in_host = false;
                                }
                                DiscoveredDpu::ModeCheckFailed(err) => {
                                    tracing::warn!(
                                        dpu = %dpu_ep.address,
                                        dpu_sn = %dpu_sn,
                                        error = %err,
                                        "failed to check fallback-matched DPU mode; skipping this device this pass",
                                    );
                                }
                            }
                        }
                    }
                }

                // The site explorer should only create a managed host after exploring all of the DPUs attached to the host.
                // If a host reports that it has two DPUs, the site explorer must wait until **both** DPUs have made the DHCP request.
                // If only one of the two DPUs have made the DHCP request, the site explorer must wait until it has explored the latter DPU's BMC
                // (ensuring that the second DPU has also made the DHCP request).
                if !dpu_added {
                    // Net DPUs still expected to pair: reported DPU minus those
                    // confirmed to be running as plain NICs.
                    let expected_managed_dpus_total =
                        host_reported_dpus_total.saturating_sub(host_reported_dpus_nic_mode_total);
                    // Enter the reset/wait path when DPUs are still expected to pair, or
                    // when a `set_nic_mode` was just issued -- a fallback-serial match can
                    // queue a flip even on a host whose BMC reports no DPU over PCIe
                    // (`expected_managed_dpus_total == 0`), which is the usual reason we are
                    // on the fallback path at all.
                    if expected_managed_dpus_total > 0 || !all_dpus_configured_properly_in_host {
                        if expected_managed_dpus_total > 0 {
                            tracing::warn!(
                                address = %ep.address,
                                exploration_report = ?ep,
                                "cannot identify managed host because the site explorer has only discovered {} out of the {} attached DPUs (all_dpus_configured_properly_in_host={all_dpus_configured_properly_in_host}):\n{:#?}",
                                dpus_explored_for_host.len(), expected_managed_dpus_total, dpus_explored_for_host
                            );
                        }

                        if !all_dpus_configured_properly_in_host {
                            // A queued `set_nic_mode` only takes effect after a host
                            // power cycle, so drive one for every vendor --
                            // `redfish_powercycle` issues `PowerCycle` and falls back
                            // to a cold `ACPowercycle` for vendors that refuse it --
                            // throttled by `reset_rate_limit`. A BMC that refuses both
                            // surfaces the host as needing a manual power cycle via
                            // the pairing-blocker metric.
                            let time_since_redfish_powercycle = Utc::now().signed_duration_since(
                                ep.last_redfish_powercycle.unwrap_or_default(),
                            );
                            if time_since_redfish_powercycle > self.config.reset_rate_limit {
                                tracing::warn!(
                                    "power cycling host {} to apply nic mode change for its incorrectly configured DPUs; time since last powercycle: {time_since_redfish_powercycle}",
                                    ep.address,
                                );
                                metrics.increment_dpu_migration_signal(
                                    DpuMigrationSignal::ResetRequested,
                                );

                                if let Err(err) = self.redfish_powercycle(ep.address).await {
                                    tracing::warn!(
                                        "site explorer failed to power cycle host {} to apply DPU mode changes: {err}; a manual power cycle may be required",
                                        ep.address
                                    );
                                    metrics.increment_host_dpu_pairing_blocker(
                                        PairingBlockerReason::ManualPowerCycleRequired,
                                    );
                                }
                            } else {
                                // We power-cycled within the rate limit and the
                                // DPUs still aren't in the declared mode -- either
                                // the change is mid-flight (the host is booting, a
                                // pass or two of normal convergence) or this
                                // vendor's `PowerCycle` is a warm reset that never
                                // actually drops power. Keep the pairing-blocker
                                // signal standing so a host stuck in the warm-reset
                                // loop stays visible to operators instead of
                                // rebooting hourly in silence.
                                //
                                // The reset above already escalates a refused
                                // `PowerCycle` to a cold `ACPowercycle`
                                // (`redfish_powercycle`), so a host still unflipped
                                // here is mid-flight or genuinely stuck -- either way
                                // it stays visible via the metric.
                                metrics.increment_host_dpu_pairing_blocker(
                                    PairingBlockerReason::ManualPowerCycleRequired,
                                );
                            }
                        }

                        continue;
                    } else if matches!(host_dpu_mode, DpuMode::DpuMode) {
                        // Host has no DPU PCIe devices reported by its
                        // BMC, and the effective `dpu_mode` is the
                        // default (`DpuMode`) -- i.e. neither per-host
                        // on `ExpectedMachine.dpu_mode` nor site-wide on
                        // `[site_explorer] dpu_mode` declared this host
                        // as zero-DPU. We expect DPUs but found none --
                        // probably a misconfiguration or a DPU-discovery
                        // bug. Skip ingestion this cycle; site-explorer
                        // will retry on the next iteration, giving the
                        // operator a chance to either fix the host or
                        // declare it as `NoDpu`.
                        //
                        // (`NoDpu` hosts are handled by the fast-path
                        // earlier in the loop; `NicMode` hosts fall
                        // through to the push below with an empty `dpus`
                        // vector -- the operator already declared
                        // "treat as zero-DPU.")
                        tracing::warn!(
                            address = %ep.address,
                            exploration_report = ?ep,
                            ?host_dpu_mode,
                            "cannot identify managed host: site explorer sees no DPUs on this host and it isn't declared as `NoDpu`; declare `dpu_mode = \"no_dpu\"` to ingest as zero-DPU",
                        );
                        metrics.increment_host_dpu_pairing_blocker(
                            PairingBlockerReason::NoDpuReportedByHost,
                        );
                        continue;
                    }
                }
            }

            // If we know the booting interface of the host, we should use this for deciding
            // primary interface.
            let mut is_sorted = false;
            // A declared `ExpectedHostNic.primary` (when the matched expected
            // machine sets one) wins over the automatic DPU-PF pick, so the
            // explored default names the same NIC the managed store will.
            let declared_primary = expected_explored_endpoint_index
                .matched_expected_machine(&ep.address)
                .and_then(|expected| expected.data.declared_primary_mac());
            if let Some(mac_address) = ep
                .report
                .fetch_host_primary_interface_mac(&dpus_explored_for_host, declared_primary)
            {
                // Capture the boot interface's [stable] Redfish interface id
                // alongside its MAC. Only persist when both resolve from the
                // current report: if the MAC has no matching interface id in
                // this report, keep the last-known-good stored boot interface
                // rather than clobbering it with a partial record.
                if let Some(interface_id) = ep.report.find_interface_id_for_mac(mac_address) {
                    boot_interfaces.push((
                        ep.address,
                        MachineBootInterface {
                            mac_address,
                            interface_id: interface_id.to_string(),
                        },
                    ));
                } else {
                    tracing::debug!(
                        address = %ep.address,
                        %mac_address,
                        "boot interface MAC has no matching Redfish interface id in the report; keeping last-known-good stored boot interface",
                    );
                }

                let primary_dpu_position = dpus_explored_for_host
                    .iter()
                    .position(|x| x.host_pf_mac_address.unwrap_or_default() == mac_address);

                if let Some(primary_dpu_position) = primary_dpu_position {
                    if primary_dpu_position != 0 {
                        let dpu = dpus_explored_for_host.remove(primary_dpu_position);
                        dpus_explored_for_host.insert(0, dpu);
                    }
                    is_sorted = true;
                } else if !dpus_explored_for_host.is_empty() {
                    let all_mac = dpus_explored_for_host
                        .iter()
                        .map(|x| {
                            x.host_pf_mac_address
                                .map(|x| x.to_string())
                                .unwrap_or_default()
                        })
                        .collect_vec()
                        .join(",");

                    tracing::error!(
                        "Could not find mac_address {mac_address} in discovered DPU's list {all_mac}, host bmc: {}.",
                        ep.address
                    );
                    metrics.increment_host_dpu_pairing_blocker(
                        PairingBlockerReason::BootInterfaceMacMismatch,
                    );
                    continue;
                }
            }

            if !is_sorted {
                // Sort using usual way.
                dpus_explored_for_host.sort_by_key(|d| {
                    d.report.systems[0]
                        .serial_number
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                });
            }

            // For NicMode hosts, don't attach DPUs even if matching
            // discovered some: the operator has declared "treat this host
            // as zero-DPU". Any DPU hardware has already had `set_nic_mode`
            // issued by the check-and-configure step above if it was in
            // DPU mode; this cycle we just emit a bare host.
            // For NoDpu hosts, we should have already returned/continued
            // earlier on after detecting the host_dpu_mode as such, so
            // this shouldn't fire.
            let dpus = match host_dpu_mode {
                DpuMode::NicMode => {
                    metrics.increment_dpu_migration_signal(
                        DpuMigrationSignal::RegisteredZeroDpuForNicMode,
                    );
                    Vec::new()
                }
                DpuMode::DpuMode => dpus_explored_for_host,
                // Now that we continue/return early for NoDpu hosts,
                // we shouldn't actually get here. Probably could be
                // lazy and just leave it as Vec::new(), but I think
                // this firing would also surface a bug, which we
                // probably want.
                DpuMode::NoDpu => unreachable!("NoDpu hosts should have already returned early"),
            };

            managed_hosts.push((
                ExploredManagedHost {
                    host_bmc_ip: ep.address,
                    dpus,
                },
                ep.report,
            ));
            metrics.exploration_identified_managed_hosts += 1;
        }

        let mut txn = self.txn_begin().await?;

        db::explored_managed_host::update(
            &mut txn,
            managed_hosts
                .iter()
                .map(|(h, _)| h)
                .collect::<Vec<_>>()
                .as_slice(),
        )
        .await?;

        // Persist boot interface MACs for host endpoints
        for (address, boot_interface) in &boot_interfaces {
            db::explored_endpoints::set_boot_interface(*address, boot_interface, &mut txn).await?;
        }

        // Record each host NIC's Redfish id on its machine_interfaces row so the
        // primary-flagged row is the host's complete boot interface (MAC + id).
        // Pending predicted interfaces get the same refresh, so a prediction
        // minted before the report resolved the id stays as current as the
        // live report until DHCP promotes it.
        for boot_interface in &nic_boot_interfaces {
            db::machine_interface::set_boot_interface_id(
                boot_interface.mac_address,
                &boot_interface.interface_id,
                &mut txn,
            )
            .await?;
            db::predicted_machine_interface::set_boot_interface_id(
                &mut txn,
                boot_interface.mac_address,
                &boot_interface.interface_id,
            )
            .await?;
        }

        txn.commit().await?;

        Ok(managed_hosts)
    }

    /// Record a single host-reported device (a PCIe device or a chassis network
    /// adapter) into `exploration`, against the discovered DPU BMCs.
    ///
    /// The one piece of IO -- `check_and_configure_dpu_mode`, which may issue a
    /// `set_nic_mode` to auto-correct a mismatch -- happens here; the actual
    /// classification of its result lives in [`classify_matched_dpu`], which is
    /// unit-tested directly. Both the PCIe loop and the chassis fallback call this.
    #[allow(clippy::too_many_arguments)]
    async fn record_host_dpu_device(
        &self,
        part_number: Option<&str>,
        serial_number: Option<&str>,
        dpu_sn_to_endpoint: &HashMap<String, ExploredEndpoint>,
        host_dpu_mode: DpuMode,
        host_ep: &ExploredEndpoint,
        exploration: &mut DpuExplorationState,
        metrics: &mut SiteExplorationMetrics,
    ) {
        // Count every DPU the host reports, independent of whether we've
        // discovered its BMC yet.
        if part_number
            .map(str::trim)
            .is_some_and(is_bluefield_part_number)
        {
            exploration.reported_total += 1;
        }

        // Only a device whose serial matches a *discovered* DPU BMC is ours to
        // classify; anything else is some other device, or a DPU whose BMC
        // we haven't explored yet.
        let Some(dpu_ep) = serial_number
            .map(str::trim)
            .and_then(|sn| dpu_sn_to_endpoint.get(sn))
        else {
            return;
        };

        // Resolve the DPU's mode against what the host declared. This is the only
        // I/O, and may issue a `set_nic_mode` (in which case it returns `Ok(false)`).
        let mode_check = Some(
            self.check_and_configure_dpu_mode(dpu_ep, part_number, host_dpu_mode, metrics)
                .await,
        );

        match classify_matched_dpu(dpu_ep, host_ep, mode_check) {
            DiscoveredDpu::RunningAsDpu(dpu) => exploration.running_as_dpu.push(dpu),
            DiscoveredDpu::RunningAsNic => exploration.running_as_nic_total += 1,
            DiscoveredDpu::NeedsReconfig => exploration.all_configured = false,
            DiscoveredDpu::ModeCheckFailed(err) => {
                tracing::warn!(
                    dpu = %dpu_ep.address,
                    error = %err,
                    "failed to check DPU mode; skipping this device",
                );
            }
        }
    }

    async fn identify_power_shelves_to_ingest(
        &self,
    ) -> SiteExplorerResult<Vec<(ExploredEndpoint, EndpointExplorationReport)>> {
        let mut txn = self
            .database_connection
            .begin()
            .await
            .map_err(|e| DatabaseError::new("load find_all_preingestion_complete data", e))?;

        let explored_endpoints =
            db::explored_endpoints::find_all_preingestion_complete(&mut txn).await?;

        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("end find_all_preingestion_complete data", e))?;

        let mut explored_power_shelves = Vec::new();
        for ep in explored_endpoints.into_iter() {
            if ep.report.endpoint_type != EndpointType::Bmc {
                continue;
            }
            if ep.report.is_power_shelf() {
                explored_power_shelves.push((ep.clone(), ep.report.clone()));
            }
            //ignore other endpoints
        }

        Ok(explored_power_shelves)
    }

    async fn identify_switches_to_ingest(&self) -> SiteExplorerResult<Vec<ExploredManagedSwitch>> {
        let mut txn = self
            .database_connection
            .begin()
            .await
            .map_err(|e| DatabaseError::new("load find_all_preingestion_complete data", e))?;

        let explored_endpoints =
            db::explored_endpoints::find_all_preingestion_complete(&mut txn).await?;

        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("end find_all_preingestion_complete data", e))?;
        let managed_switches = explored_endpoints
            .iter()
            .filter(|ep| ep.report.endpoint_type == EndpointType::Bmc && ep.report.is_switch())
            .map(|ep| ExploredManagedSwitch {
                bmc_ip: ep.address,
                nv_os_mac_addresses: ep.report.all_mac_addresses(),
                report: ep.report.clone(),
            })
            .collect::<Vec<_>>();

        Ok(managed_switches)
    }

    /// Checks if all data that a site exploration run requires is actually configured
    ///
    /// Doing this upfront avoids the risk of trying to log into BMCs without
    /// the necessary credentials - which could trigger a lockout.
    async fn check_preconditions(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<()> {
        self.endpoint_explorer
            .check_preconditions(metrics)
            .await
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "check_preconditions",
                err,
            })
    }

    async fn update_explored_endpoints(
        &self,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<ExploredEndpointIndex> {
        let load_start = Instant::now();
        let mut txn = self.txn_begin().await?;

        let underlay_segments =
            db::network_segment::list_segment_ids(&mut txn, Some(NetworkSegmentType::Underlay))
                .await?;
        let explored_endpoints = db::explored_endpoints::find_all(txn.as_pgconn()).await?;
        let expected_switches = db::expected_switch::find_all(&mut txn).await?;
        let expected_machines = db::expected_machine::find_all(&mut txn).await?;
        let expected_power_shelves = db::expected_power_shelf::find_all(&mut txn).await?;

        let explore_power_shelves_from_static_ip = self
            .config
            .explore_power_shelves_from_static_ip
            .load(Ordering::Relaxed);

        // Load SKU information for expected machines to record metrics
        let sku_ids: Vec<&str> = expected_machines
            .iter()
            .filter_map(|em| em.data.sku_id.as_deref())
            .collect();
        let skus = db::sku::find(&mut txn, &sku_ids).await?;

        txn.commit().await?;
        metrics.record_phase_latency("update_explored_endpoints_load", load_start.elapsed());

        let explored_endpoint_count = explored_endpoints.len();
        let expected_switch_count = expected_switches.len();
        let expected_machine_count = expected_machines.len();
        let expected_power_shelf_count = expected_power_shelves.len();
        metrics.record_update_explored_endpoints_count(
            "explored_endpoints_loaded",
            explored_endpoint_count,
        );
        metrics.record_update_explored_endpoints_count("expected_switches", expected_switch_count);
        metrics.record_update_explored_endpoints_count("expected_machines", expected_machine_count);
        metrics.record_update_explored_endpoints_count(
            "expected_power_shelves",
            expected_power_shelf_count,
        );

        // Create a map of sku_id -> device_type for quick lookup
        let sku_device_types: HashMap<String, Option<String>> = skus
            .into_iter()
            .map(|sku| (sku.id, sku.device_type))
            .collect();

        // Record expected machine metrics and reconcile any configured static-IP reservations
        // (bmc_ip_address, host_nics[].fixed_ip) into machine_interfaces. Idempotent on the
        // api-db side -- steady-state runs are no-ops at the row level. This is the canonical
        // path that materializes static reservations for IPs that don't reach
        // `discover_dhcp` (devices on the static-assignments segment, devices not yet powered
        // on, etc.), and a belt-and-suspenders for the in-network case too.
        let preallocate_start = Instant::now();
        for expected_machine in &expected_machines {
            let device_type = expected_machine
                .data
                .sku_id
                .as_ref()
                .and_then(|sku_id| sku_device_types.get(sku_id))
                .and_then(|dt| dt.as_deref());
            metrics.increment_expected_machines_sku_count(
                expected_machine.data.sku_id.as_deref(),
                device_type,
            );

            if let Some(bmc_ip) = expected_machine.data.bmc_ip_address {
                try_preallocate_one(
                    &self.database_connection,
                    expected_machine.bmc_mac_address,
                    bmc_ip,
                    InterfaceType::Bmc,
                    "expected_machine BMC",
                    self.config.retained_boot_interface_window,
                )
                .await;
            }
            for nic in &expected_machine.data.host_nics {
                let Some(ip) = nic.fixed_ip else {
                    continue;
                };
                try_preallocate_one(
                    &self.database_connection,
                    nic.mac_address,
                    ip,
                    InterfaceType::Data,
                    "expected_machine host NIC",
                    self.config.retained_boot_interface_window,
                )
                .await;
            }
        }

        for expected_switch in &expected_switches {
            if let Some(bmc_ip) = expected_switch.bmc_ip_address {
                try_preallocate_one(
                    &self.database_connection,
                    expected_switch.bmc_mac_address,
                    bmc_ip,
                    InterfaceType::Bmc,
                    "expected_switch BMC",
                    self.config.retained_boot_interface_window,
                )
                .await;
            }
            // NVOS static IP: handler-side validation pairs `nvos_ip_address` with
            // exactly one `nvos_mac_addresses` entry (the single wired NVOS port).
            // ...but re-check here just incase, with the failure case being a
            // log and skip for this pass.
            if let Some(nvos_ip) = expected_switch.nvos_ip_address {
                match expected_switch.nvos_mac_addresses.as_slice() {
                    [nvos_mac] => {
                        try_preallocate_one(
                            &self.database_connection,
                            *nvos_mac,
                            nvos_ip,
                            InterfaceType::Data,
                            "expected_switch NVOS",
                            self.config.retained_boot_interface_window,
                        )
                        .await;
                    }
                    macs => {
                        tracing::warn!(
                            bmc_mac = %expected_switch.bmc_mac_address,
                            %nvos_ip,
                            nvos_mac_count = macs.len(),
                            "Skipping NVOS preallocation: nvos_ip_address requires exactly one nvos_mac_addresses entry"
                        );
                    }
                }
            }
        }

        for expected_power_shelf in &expected_power_shelves {
            if let Some(bmc_ip) = expected_power_shelf.bmc_ip_address {
                try_preallocate_one(
                    &self.database_connection,
                    expected_power_shelf.bmc_mac_address,
                    bmc_ip,
                    InterfaceType::Bmc,
                    "expected_power_shelf BMC",
                    self.config.retained_boot_interface_window,
                )
                .await;
            }
        }
        metrics.record_phase_latency(
            "update_explored_endpoints_preallocate",
            preallocate_start.elapsed(),
        );

        let expected_count = expected_machines.len();

        // We don't have to scan anything that is on the Tenant or Admin Segments,
        // since we know what those Segments are used for (Forge allocated the IPs on the segments
        // for a specific machine).
        // We also can skip scanning IPs which are knowingly used as DPU OOB interfaces,
        // since those will not speak redfish.
        // Note: As a side effect of this, OOB interfaces might for a short time be scanned,
        // until the machine is ingested. At that point in time this filter will remove them
        // from the to-be-scanned list.
        // Get all underlay interfaces from the database, which includes interfaces
        // which have come from both DHCP and/or static assignments. Fetched here, after the
        // preallocate loops above, so we see any freshly preallocated rows from this iteration.
        let interface_load_start = Instant::now();
        let mut txn = self.txn_begin().await?;
        let interfaces = db::machine_interface::find_all(&mut txn).await?;
        txn.commit().await?;
        metrics.record_phase_latency(
            "update_explored_endpoints_interface_load",
            interface_load_start.elapsed(),
        );

        let build_index_start = Instant::now();
        let underlay_interfaces: Vec<MachineInterfaceSnapshot> = interfaces
            .into_iter()
            .filter(|iface| {
                underlay_segments.contains(&iface.segment_id)
                    && (iface.machine_id.is_none() || iface.interface_type == InterfaceType::Bmc)
            })
            .collect();
        let underlay_interface_count = underlay_interfaces.len();
        metrics.record_update_explored_endpoints_count(
            "underlay_interfaces",
            underlay_interface_count,
        );

        // Start an index of all underlay interfaces, expected machines, expected power shelves, and expected switches.
        let index = ExploredEndpointIndex::builder(explored_endpoints, underlay_interfaces)
            .with_expected_machines(expected_machines)
            .with_expected_switches(expected_switches)
            .with_expected_power_shelves(expected_power_shelves)
            .build();
        metrics.record_phase_latency(
            "update_explored_endpoints_build_index",
            build_index_start.elapsed(),
        );

        // If a previously explored endpoint is not part of `MachineInterfaces` anymore,
        // we can delete knowledge about it. Otherwise we might try to refresh the
        // information about the endpoint
        let plan_start = Instant::now();
        let mut delete_endpoints = Vec::new();
        let mut priority_update_endpoints = Vec::new();
        let mut update_endpoints = Vec::with_capacity(index.explored_endpoints().len());
        for (address, endpoint) in index.explored_endpoints() {
            match index.underlay_interface(address) {
                Some(iface) => {
                    if endpoint.exploration_requested {
                        priority_update_endpoints.push((*address, iface, endpoint));
                    } else {
                        update_endpoints.push((*address, iface, endpoint));
                    }
                }
                None => {
                    if endpoint.report.is_power_shelf() && explore_power_shelves_from_static_ip {
                        tracing::info!(%address, "Not deleting power shelf endpoint from database, as we are sourcing power shelves from static IP's")
                    } else {
                        delete_endpoints.push(*address)
                    }
                }
            }
        }
        metrics.record_update_explored_endpoints_count(
            "priority_update_candidates",
            priority_update_endpoints.len(),
        );
        metrics.record_update_explored_endpoints_count(
            "routine_update_candidates",
            update_endpoints.len(),
        );
        metrics.record_update_explored_endpoints_count(
            "stale_delete_candidates",
            delete_endpoints.len(),
        );

        // The unknown endpoints can quickly be cleaned up
        let delete_stale_start = Instant::now();
        if !delete_endpoints.is_empty() {
            let mut txn = self.txn_begin().await?;
            db::explored_endpoints::delete_many(&mut txn, &delete_endpoints).await?;
            txn.commit().await?;
        }
        metrics.record_phase_latency(
            "update_explored_endpoints_delete_stale",
            delete_stale_start.elapsed(),
        );

        // If there is a MachineInterface and no previously discovered information,
        // we need to detect it. This includes both regular machines, PowerShelves
        // and Switches.
        let unexplored_endpoints = index.get_unexplored_endpoints();
        metrics.record_update_explored_endpoints_count(
            "unexplored_candidates",
            unexplored_endpoints.len(),
        );

        // Now that we gathered the candidates for exploration, let's decide what
        // we are actually going to explore. The config limits the amount of explorations
        // per iteration.
        let num_explore_endpoints = (self.config.explorations_per_run as usize)
            .min(unexplored_endpoints.len() + update_endpoints.len());

        let mut explore_endpoint_data =
            Vec::with_capacity(priority_update_endpoints.len() + num_explore_endpoints);

        // Existing endpoints with `exploration_requested` are enqueued
        // unconditionally and sit outside the per-iteration count budget.
        // Operators set this flag to request a guaranteed next-tick attempt, so
        // we must not let the routine refresh budget delay them. Concurrency is
        // still bounded by the `concurrent_explorations` semaphore below.
        for (address, iface, endpoint) in priority_update_endpoints {
            explore_endpoint_data.push(Endpoint {
                address,
                iface,
                last_explored: Some(endpoint),
                pause_ingestion_and_poweron: endpoint.pause_ingestion_and_poweron,
                expected: index.matched_expected(&address),
            });
        }

        let priority_selected_count = explore_endpoint_data.len();
        metrics.record_update_explored_endpoints_count(
            "selected_priority_updates",
            priority_selected_count,
        );

        // Next priority are all endpoints that we've never looked at
        let remaining_explore_endpoints = num_explore_endpoints;
        for (address, iface) in unexplored_endpoints
            .iter()
            .take(remaining_explore_endpoints)
        {
            let pause_ingestion_and_poweron =
                pause_ingestion_and_poweron(index.expected(), &iface.mac_address);
            explore_endpoint_data.push(Endpoint {
                address: *address,
                iface,
                last_explored: None,
                pause_ingestion_and_poweron,
                expected: index.matched_expected(address),
            });
        }
        let selected_unexplored = explore_endpoint_data.len() - priority_selected_count;
        metrics.record_update_explored_endpoints_count("selected_unexplored", selected_unexplored);

        // If we have any capacity available, we update knowledge about endpoints we looked at earlier on
        let remaining_explore_endpoints =
            num_explore_endpoints - (explore_endpoint_data.len() - priority_selected_count);
        if remaining_explore_endpoints != 0 {
            // Sort endpoints so that we will replace the oldest report first
            update_endpoints.sort_by_key(|(_address, _machine_interface, endpoint)| {
                endpoint.report_version.timestamp()
            });
            for (address, iface, endpoint) in update_endpoints
                .into_iter()
                .take(remaining_explore_endpoints)
            {
                explore_endpoint_data.push(Endpoint {
                    address,
                    iface,
                    last_explored: Some(endpoint),
                    pause_ingestion_and_poweron: endpoint.pause_ingestion_and_poweron,
                    expected: index.matched_expected(&address),
                });
            }
        }
        metrics.record_update_explored_endpoints_count(
            "selected_routine_updates",
            explore_endpoint_data.len() - priority_selected_count - selected_unexplored,
        );
        metrics
            .record_update_explored_endpoints_count("selected_total", explore_endpoint_data.len());
        metrics.record_phase_latency("update_explored_endpoints_plan", plan_start.elapsed());

        let task_set = FuturesUnordered::new();
        let concurrency_limiter = Arc::new(tokio::sync::Semaphore::new(
            self.config.concurrent_explorations as usize,
        ));

        // Record the difference between the total expected machine count and
        // the number of expected machines we've actually "seen."
        metrics.endpoint_explorations_expected_machines_missing_overall_count =
            expected_count - index.all_matched_expected_machines().len();
        let fw_config_snapshot = Arc::new(self.firmware_config_snapshot().await?);

        let probe_start = Instant::now();
        for endpoint in explore_endpoint_data.into_iter() {
            let endpoint_explorer = self.endpoint_explorer.clone();
            let endpoint_exploration_locks = self.endpoint_exploration_locks.clone();
            let concurrency_limiter = concurrency_limiter.clone();

            let bmc_target_port = self.config.override_target_port.unwrap_or(443);
            let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
            let fw_config_snapshot = fw_config_snapshot.clone();
            let database_connection = self.database_connection.clone();

            task_set.push(
                async move {
                    let start = Instant::now();
                    let mut steps = EndpointExplorationStepDurations::default();

                    // Acquire a permit which will block more than `concurrent_explorations`
                    // tasks from running.
                    // Note that assigning the permit to a named variable is necessary
                    // to make it live until the end of the scope. Using `_` would
                    // immediately dispose the permit.
                    let _permit = concurrency_limiter
                        .acquire()
                        .await
                        .expect("Semaphore can't be closed");

                    // If an ad-hoc refresh or another periodic task is already exploring this
                    // endpoint, skip it for this iteration.
                    let _endpoint_guard =
                        match endpoint_exploration_locks.try_claim(endpoint.address) {
                            Some(guard) => guard,
                            None => {
                                tracing::info!(
                                    address = %endpoint.address,
                                    "Skipping periodic endpoint exploration; endpoint already in progress"
                                );
                                return Ok(None);
                            }
                        };

                    let redfish_explore_start = Instant::now();
                    let mut result = endpoint_explorer
                        .explore_endpoint(
                            bmc_target_addr,
                            endpoint.iface,
                            endpoint.expected,
                            endpoint
                                .last_explored
                                .and_then(|e| e.report.last_exploration_error.as_ref()),
                            endpoint.last_explored.and_then(|e| e.boot_interface_mac),
                        )
                        .await;
                    steps.redfish_explore = redfish_explore_start.elapsed();

                    if let Err(error) = &result {
                        // For logging purposes
                        let failure_context_load_start = Instant::now();
                        let machine_state = match get_machine_state_by_bmc_ip(
                            &database_connection,
                            &endpoint.address.to_string(),
                        )
                        .await
                        {
                            Ok(state) if !state.is_empty() => Some(state),
                            _ => None,
                        };
                        steps.failure_context_load = Some(failure_context_load_start.elapsed());
                        let schema = error.operator_error_schema();
                        if let Some(machine_state) = machine_state.as_deref() {
                            tracing::info!(
                                endpoint = %bmc_target_addr,
                                error = %error,
                                error_code = %schema.error_code,
                                mitigation = %schema.mitigation_for_log(),
                                text = %schema.text,
                                machine_state,
                                "Failed to explore endpoint"
                            );
                        } else {
                            tracing::info!(
                                endpoint = %bmc_target_addr,
                                error = %error,
                                error_code = %schema.error_code,
                                mitigation = %schema.mitigation_for_log(),
                                text = %schema.text,
                                "Failed to explore endpoint"
                            );
                        }
                    }

                    if let Ok(report) = &mut result {
                        let report_enrich_start = Instant::now();
                        enrich_endpoint_exploration_report(report, &fw_config_snapshot);
                        steps.report_enrich = Some(report_enrich_start.elapsed());
                    }

                    Ok(Some(EndpointExplorationTaskResult {
                        endpoint,
                        result,
                        exploration_duration: start.elapsed(),
                        steps,
                    }))
                }
                .in_current_span(),
            );
        }

        // We want for all tasks to run to completion here and therefore can't
        // return early until the `TaskSet` is fully consumed.
        // If we would return early then some tasks might still work on an object
        // even thought the next controller iteration already started.
        // Therefore we drain the `task_set` here completely and record all errors
        // before returning.
        let exploration_results = task_set
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<SiteExplorerResult<Vec<_>>>()?;
        metrics.record_phase_latency("update_explored_endpoints_probe", probe_start.elapsed());
        for EndpointExplorationTaskResult { steps, .. } in exploration_results.iter().flatten() {
            metrics
                .record_endpoint_exploration_step_latency("redfish_explore", steps.redfish_explore);
            if let Some(duration) = steps.failure_context_load {
                metrics.record_endpoint_exploration_step_latency("failure_context_load", duration);
            }
            if let Some(duration) = steps.report_enrich {
                metrics.record_endpoint_exploration_step_latency("report_enrich", duration);
            }
        }

        // All subtasks finished. We now update the database
        let persist_start = Instant::now();
        metrics.record_update_explored_endpoints_count("insert_endpoint_attempts", 0);
        metrics.record_update_explored_endpoints_count("endpoint_report_update_attempts", 0);
        metrics.record_update_explored_endpoints_count("endpoint_error_update_attempts", 0);
        metrics.record_update_explored_endpoints_count("firmware_version_update_attempts", 0);
        metrics.record_update_explored_endpoints_count("redfish_remediation_candidates", 0);
        let mut txn = self.txn_begin().await?;

        let mut redfish_errors = Vec::new();
        let mut insert_endpoint_attempts = 0;
        let mut endpoint_report_update_attempts = 0;
        let mut endpoint_error_update_attempts = 0;
        let mut firmware_version_update_attempts = 0;

        for EndpointExplorationTaskResult {
            endpoint,
            result,
            exploration_duration,
            ..
        } in exploration_results.into_iter().flatten()
        {
            let address = endpoint.address;
            let mut redfish_error = None;

            metrics.endpoint_explorations += 1;
            metrics
                .endpoint_exploration_duration
                .push(exploration_duration);
            match &result {
                Ok(report) => {
                    metrics.endpoint_explorations_success += 1;
                    if let Some(e) = &report.remediation_error {
                        redfish_error = Some(e.clone());
                    }
                }
                Err(e) => {
                    *metrics
                        .endpoint_explorations_failures_by_type
                        .entry(exploration_error_to_metric_label(e))
                        .or_default() += 1;

                    if e.is_redfish() {
                        redfish_error = Some(e.clone());
                    }
                }
            }

            // Update possible stale machine versions
            if let Ok(report) = &result
                && let Some(bmc_version) = report.versions.get(&FirmwareComponentType::Bmc)
                && let Some(uefi_version) = report.versions.get(&FirmwareComponentType::Uefi)
            {
                let machine_id = match report.machine_id.as_ref().copied() {
                    Some(machine_id) => Some(machine_id),
                    None => db::machine::find_id_by_bmc_ip(&mut txn, &address).await?,
                };

                if let Some(machine_id) = machine_id {
                    db::machine_topology::update_firmware_version_by_machine_id(
                        &mut txn,
                        &machine_id,
                        bmc_version,
                        uefi_version,
                    )
                    .await?;
                    firmware_version_update_attempts += 1;
                }
            }

            match endpoint.last_explored {
                Some(explored) => {
                    let old_version = explored.report_version;
                    let old_report = &explored.report;
                    match result {
                        Ok(mut report) => {
                            report.last_exploration_latency = Some(exploration_duration);
                            if old_report.endpoint_type == EndpointType::Unknown {
                                tracing::info!(
                                    address = %address,
                                    exploration_report = ?report,
                                    "Initial exploration of endpoint"
                                );
                            }
                            db::explored_endpoints::try_update(
                                address,
                                old_version,
                                &report,
                                false,
                                &mut txn,
                            )
                            .await?;
                            endpoint_report_update_attempts += 1;
                        }
                        Err(e) => {
                            // If an endpoint can not be explored we don't delete the known information, since it's
                            // still helpful. The failure might just be intermittent.
                            db::explored_endpoints::try_update_last_exploration_error(
                                address,
                                old_version,
                                &e,
                                exploration_duration,
                                &mut txn,
                            )
                            .await?;
                            endpoint_error_update_attempts += 1;
                        }
                    }
                }
                None => {
                    let should_pause_ingestion_and_poweron =
                        pause_ingestion_and_poweron(index.expected(), &endpoint.iface.mac_address);
                    match result {
                        Ok(mut report) => {
                            report.last_exploration_latency = Some(exploration_duration);
                            tracing::info!(
                                address = %address,
                                exploration_report = ?report,
                                "Initial exploration of endpoint"
                            );
                            db::explored_endpoints::insert(
                                address,
                                &report,
                                should_pause_ingestion_and_poweron,
                                &mut txn,
                            )
                            .await?;
                            insert_endpoint_attempts += 1;
                        }
                        Err(e) => {
                            // If an endpoint exploration failed we still track the result in the database
                            // That will avoid immmediatly retrying the exploration in the next run
                            let mut report = EndpointExplorationReport::new_with_error(e);
                            report.last_exploration_latency = Some(exploration_duration);
                            db::explored_endpoints::insert(
                                address,
                                &report,
                                should_pause_ingestion_and_poweron,
                                &mut txn,
                            )
                            .await?;
                            insert_endpoint_attempts += 1;
                        }
                    }

                    let power_shelf_manual_ingestion = endpoint
                        .expected
                        .is_some_and(|v| matches!(v, ExpectedEntity::PowerShelf(_)))
                        && explore_power_shelves_from_static_ip;

                    if !self.config.create_machines.load(Ordering::Relaxed)
                        || power_shelf_manual_ingestion
                    {
                        // We're using manual ingestion, making preingestion updates risky.  Go ahead and skip them.
                        db::explored_endpoints::set_preingestion_complete(address, &mut txn).await?
                    }
                }
            }

            // We wait until the end to add it to redfish_errors so we can move endpoint safely
            if let Some(e) = redfish_error {
                redfish_errors.push((e, endpoint));
            }
        }

        txn.commit().await?;
        metrics.record_phase_latency("update_explored_endpoints_persist", persist_start.elapsed());
        metrics.record_update_explored_endpoints_count(
            "insert_endpoint_attempts",
            insert_endpoint_attempts,
        );
        metrics.record_update_explored_endpoints_count(
            "endpoint_report_update_attempts",
            endpoint_report_update_attempts,
        );
        metrics.record_update_explored_endpoints_count(
            "endpoint_error_update_attempts",
            endpoint_error_update_attempts,
        );
        metrics.record_update_explored_endpoints_count(
            "firmware_version_update_attempts",
            firmware_version_update_attempts,
        );
        metrics.record_update_explored_endpoints_count(
            "redfish_remediation_candidates",
            redfish_errors.len(),
        );

        // We handle redfish errors after committing the transaction, to avoid holding the
        // transaction while issuing expensive redfish calls.
        let remediate_start = Instant::now();
        for (e, endpoint) in redfish_errors {
            self.handle_redfish_error(&endpoint, metrics, &e).await;
        }
        metrics.record_phase_latency(
            "update_explored_endpoints_remediate",
            remediate_start.elapsed(),
        );

        Ok(index)
    }

    pub async fn handle_redfish_error(
        &self,
        endpoint: &Endpoint<'_>,
        metrics: &mut SiteExplorationMetrics,
        error: &EndpointExplorationError,
    ) {
        // Check if remediation is paused for this endpoint first.
        // New endpoints haven't been explored yet, so pause_remediation defaults to false
        if endpoint.last_explored.is_some_and(|e| e.pause_remediation) {
            tracing::info!(
                "Site explorer will not remediate error for {endpoint} because remediation is paused for this endpoint: {error}"
            );
            return;
        }

        // If site explorer can't log in, there's nothing we can do.
        if !self
            .endpoint_explorer
            .have_credentials(endpoint.iface)
            .await
        {
            return;
        }

        if !matches!(
            *endpoint.preingestion_state(),
            PreingestionState::Initial | PreingestionState::Complete
        ) {
            tracing::info!(
                "Site explorer will not remediate error for {endpoint} because endpoint is in preingestion state {:?}: {error}",
                endpoint.preingestion_state(),
            );
            return;
        }

        match self
            .is_managed_host_created_for_endpoint(endpoint.address)
            .await
        {
            Ok(managed_host_exists) => {
                if managed_host_exists {
                    tracing::info!(
                        "Site explorer will not remediate error for {endpoint} because a managed host has already been created for this endpoint: {error}"
                    );
                    return;
                }
            }
            Err(e) => {
                tracing::error!(%e, "failed to retrieve whether managed host was created for endpoint: {endpoint}");
                return;
            }
        };

        // Power on machine endpoints in the initial preingestion state automatically unless ingestion was explicitly paused.
        if matches!(*endpoint.preingestion_state(), PreingestionState::Initial)
            && matches!(endpoint.expected, Some(ExpectedEntity::Machine(_)))
            && !endpoint.pause_ingestion_and_poweron
            && let Ok(power_state) = self.redfish_get_power_state(endpoint).await
            && !matches!(power_state, PowerState::On)
        {
            tracing::warn!(
                "Site Explorer found a host (bmc_ip_address: {}) that isnt on. Turning it on now.",
                endpoint.address,
            );

            match self
                .redfish_power(endpoint, libredfish::SystemPowerControl::On)
                .await
            {
                Ok(()) => return,
                Err(err) => {
                    tracing::error!(%err, "Site Explorer failed to power on host through Redfish");
                }
            }
        }

        // Dont let site explorer issue either a force-restart or bmc-reset more than the rate limit.
        let reset_rate_limit = self.config.reset_rate_limit;
        let min_time_since_last_action_mins = 20;
        let start = Utc::now();
        let time_since_redfish_reboot = start.signed_duration_since(
            endpoint
                .last_explored
                .and_then(|e| e.last_redfish_reboot)
                .unwrap_or_default(),
        );
        let time_since_redfish_bmc_reset = start.signed_duration_since(
            endpoint
                .last_explored
                .and_then(|e| e.last_redfish_bmc_reset)
                .unwrap_or_default(),
        );
        let time_since_ipmitool_bmc_reset = start.signed_duration_since(
            endpoint
                .last_explored
                .and_then(|e| e.last_ipmitool_bmc_reset)
                .unwrap_or_default(),
        );

        if time_since_redfish_reboot.num_minutes() < min_time_since_last_action_mins
            || time_since_redfish_bmc_reset.num_minutes() < min_time_since_last_action_mins
            || time_since_ipmitool_bmc_reset.num_minutes() < min_time_since_last_action_mins
        {
            tracing::info!(
                "waiting to remediate error {error} for {endpoint}; time_since_redfish_reboot: {time_since_redfish_reboot}; time_since_redfish_bmc_reset: {time_since_redfish_bmc_reset}; time_since_ipmitool_bmc_reset: {time_since_ipmitool_bmc_reset}"
            );
            return;
        }

        tracing::info!(
            "Site explorer captured an error for {endpoint}: {error};\n time_since_redfish_reboot: {time_since_redfish_reboot}; time_since_redfish_bmc_reset: {time_since_redfish_bmc_reset}; time_since_ipmitool_bmc_reset: {time_since_ipmitool_bmc_reset}'"
        );

        // If the endpoint is a DPU, and the error is that the BIOS attributes are coming up as empty for this DPU,
        // reboot the DPU as our first course of action. This is the official workaround from the DPU redfish team to mitigate empty UEFI attributes
        // until https://redmine.mellanox.com/issues/3746477 is fixed.
        //
        // If this fails, and we continue seeing the BIOS attributes come up as empty after twenty minutes (providing plenty of time)
        // for the DPU to come back up after the reboot, lets try resetting the BMC to see if it helps.

        if (error.is_dpu_redfish_bios_response_invalid())
            && time_since_redfish_reboot > reset_rate_limit
            && self
                .redfish_power(endpoint, libredfish::SystemPowerControl::ForceRestart)
                .await
                .map_err(|err| {
                    tracing::error!(
                        "Site Explorer failed to reboot {}: {}",
                        endpoint.address,
                        err
                    )
                })
                .is_ok()
        {
            metrics.bmc_reboot_count += 1;
            return;
        }

        if self.is_viking_bmc(endpoint).await && time_since_redfish_reboot > reset_rate_limit {
            match self.clear_nvram(endpoint).await {
                Ok(_) => {
                    metrics.bmc_reboot_count += 1;
                    return;
                }
                Err(e) => {
                    tracing::error!(
                        "Site Explorer failed to clear nvram {}: {}",
                        endpoint.address,
                        e
                    )
                }
            }
        }

        if time_since_redfish_bmc_reset > reset_rate_limit
            && self
                .redfish_reset_bmc(endpoint)
                .await
                .map_err(|err| {
                    tracing::error!(
                        "Site Explorer failed to reset BMC {} through redfish: {}",
                        endpoint.address,
                        err
                    )
                })
                .is_ok()
        {
            metrics.bmc_reset_count += 1;
            return;
        }

        if time_since_ipmitool_bmc_reset > reset_rate_limit {
            self.ipmitool_reset_bmc(endpoint)
                .await
                .map_err(|err| {
                    tracing::error!(
                        "Site Explorer failed to reset BMC {} through ipmitool: {}",
                        endpoint.address,
                        err
                    )
                })
                .ok();
            metrics.bmc_reset_count += 1;
        }
    }

    pub async fn ipmitool_reset_bmc(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            "SiteExplorer is initiating a cold BMC reset through IPMI to IP {}",
            endpoint.address
        );

        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
        match self
            .endpoint_explorer
            .ipmitool_reset_bmc(bmc_target_addr, endpoint.iface)
            .await
        {
            Ok(_) => {
                let mut txn = self.txn_begin().await?;

                db::explored_endpoints::set_last_ipmitool_bmc_reset(endpoint.address, &mut txn)
                    .await?;

                txn.commit().await?;

                Ok(())
            }
            Err(e) => Err(SiteExplorerError::internal(format!(
                "site-explorer failed to cold reset bmc through ipmitool {}: {:#?}",
                endpoint.address, e
            ))),
        }
    }

    pub async fn redfish_reset_bmc(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            "SiteExplorer is initiating a BMC reset through Redfish to IP {}",
            endpoint.address
        );
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
        match self
            .endpoint_explorer
            .redfish_reset_bmc(bmc_target_addr, endpoint.iface)
            .await
        {
            Ok(_) => {
                let mut txn = self.txn_begin().await?;

                db::explored_endpoints::set_last_redfish_bmc_reset(endpoint.address, &mut txn)
                    .await?;

                txn.commit().await?;

                Ok(())
            }
            Err(e) => Err(SiteExplorerError::internal(format!(
                "site-explorer failed to reset bmc through redfish {}: {:#?}",
                endpoint.address, e
            ))),
        }
    }

    async fn redfish_get_power_state(
        &self,
        endpoint: &Endpoint<'_>,
    ) -> SiteExplorerResult<PowerState> {
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);

        self.endpoint_explorer
            .redfish_get_power_state(bmc_target_addr, endpoint.iface)
            .await
            .map(IntoModel::into_model)
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "redfish_get_power_state",
                err,
            })
    }

    async fn redfish_power(
        &self,
        endpoint: &Endpoint<'_>,
        action: libredfish::SystemPowerControl,
    ) -> SiteExplorerResult<()> {
        let is_reboot = matches!(&action, libredfish::SystemPowerControl::ForceRestart);
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);

        self.endpoint_explorer
            .redfish_power_control(bmc_target_addr, endpoint.iface, action)
            .await
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "redfish_power",
                err,
            })?;

        if is_reboot {
            let mut txn = self.txn_begin().await?;
            db::explored_endpoints::set_last_redfish_reboot(endpoint.address, &mut txn).await?;
            txn.commit().await?;
        }

        Ok(())
    }

    pub async fn is_viking_bmc(&self, endpoint: &Endpoint<'_>) -> bool {
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
        match self
            .endpoint_explorer
            .is_viking(bmc_target_addr, endpoint.iface)
            .await
        {
            Ok(is_viking) => is_viking,
            Err(e) => {
                tracing::warn!("could not retrieve vendor for {}: {e}", endpoint.address);
                false
            }
        }
    }
    pub async fn clear_nvram(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            "SiteExplorer is issuing a clean_nvram through Redfish to IP {}",
            endpoint.address
        );
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);

        self.endpoint_explorer
            .clear_nvram(bmc_target_addr, endpoint.iface)
            .await
            .map_err(|err| {
                SiteExplorerError::internal(format!(
                    "site-explorer failed to clear nvram {}: {:#?}",
                    endpoint.address, err
                ))
            })?;

        self.redfish_power(endpoint, libredfish::SystemPowerControl::ForceRestart)
            .await
    }

    async fn is_managed_host_created_for_endpoint(
        &self,
        bmc_ip_address: IpAddr,
    ) -> SiteExplorerResult<bool> {
        let mut txn = self.txn_begin().await?;

        let is_endpoint_in_managed_host =
            is_endpoint_in_managed_host(bmc_ip_address, txn.as_pgconn()).await?;

        txn.commit().await?;

        Ok(is_endpoint_in_managed_host)
    }

    /// can_ingest_dpu_endpoint returns a boolean indicating whether the site explorer should continue ingesting a DPU endpoint.
    /// it will always return true for a DPU that has already been ingested.
    async fn can_ingest_dpu_endpoint(
        &self,
        metrics: &mut SiteExplorationMetrics,
        dpu_endpoint: &ExploredEndpoint,
    ) -> SiteExplorerResult<bool> {
        let is_managed_host_created_for_endpoint = match self
            .is_managed_host_created_for_endpoint(dpu_endpoint.address)
            .await
        {
            Ok(managed_host_exists) => managed_host_exists,
            Err(e) => {
                tracing::error!(%e, "failed to retrieve whether managed host was created for DPU endpoint: {dpu_endpoint}");
                // return true by default
                true
            }
        };

        if is_managed_host_created_for_endpoint {
            // this dpu has already been ingested
            return Ok(true);
        }

        match dpu_endpoint.report.nic_mode() {
            Some(NicMode::Nic) => {
                // DPU's in NIC mode do not have full redfish functionality,
                // for example, we will not be able to retrieve the base GUID
                // from the redfish response. Skip the next check because the DPUs
                // in NIC mode will not expose a pf0 interface to the host.
                tracing::info!(
                    "Site explorer found an uningested DPU (bmc ip: {}) in NIC mode",
                    dpu_endpoint.address
                );
                return Ok(true);
            }
            Some(NicMode::Dpu) => {}
            None if dpu_endpoint.report.dpu_pairing_serial_number().is_some() => {
                tracing::warn!(
                    "Site explorer found an uningested DPU (bmc ip: {}) without a Redfish DPU/NIC mode; continuing because it has a host-pairing serial",
                    dpu_endpoint.address
                );
            }
            None => {
                tracing::error!(
                    "Site explorer found an uningested DPU (bmc ip: {}) without being able to determine if it is in NIC mode",
                    dpu_endpoint.address
                );
                metrics.increment_host_dpu_pairing_blocker(PairingBlockerReason::DpuNicModeUnknown);
                return Ok(false);
            }
        }

        // This is a BlueField that should be pairable as a managed DPU. BF4 may
        // not report mode, so host pairing and the PF MAC check decide whether
        // it can continue.
        match find_host_pf_mac_address(dpu_endpoint) {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::error!(%error, "Site explorer found an uningested DPU (bmc ip: {}): failed to find the MAC address of the pf0 interface that the DPU exposes to the host", dpu_endpoint.address);
                metrics.increment_host_dpu_pairing_blocker(PairingBlockerReason::DpuPf0MacMissing);
                Ok(false)
            }
        }
    }

    async fn set_nic_mode(
        &self,
        dpu_endpoint: &ExploredEndpoint,
        mode: NicMode,
    ) -> SiteExplorerResult<()> {
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(dpu_endpoint.address, bmc_target_port);

        let interface = self
            .find_machine_interface_for_ip(dpu_endpoint.address)
            .await?;

        self.endpoint_explorer
            .set_nic_mode(bmc_target_addr, &interface, mode)
            .await
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "set_nic_mode",
                err,
            })
    }

    async fn redfish_power_control(
        &self,
        bmc_ip_address: IpAddr,
        action: libredfish::SystemPowerControl,
    ) -> SiteExplorerResult<()> {
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(bmc_ip_address, bmc_target_port);

        let interface = self.find_machine_interface_for_ip(bmc_ip_address).await?;

        self.endpoint_explorer
            .redfish_power_control(bmc_target_addr, &interface, action)
            .await
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "redfish_power_control",
                err,
            })
    }

    /// Drive a power cycle to apply a queued BlueField NIC-mode change.
    ///
    /// `PowerCycle` (Redfish `ComputerSystem.Reset`) is implemented only by Dell
    /// and the DPU BMCs; other host vendors -- and Vikings -- refuse it. Fall
    /// back to `ACPowercycle`, the cold AC cycle the HPE/Lenovo/Supermicro/GBx00
    /// wrappers implement, so the queued change still applies without an
    /// operator. If both are refused the error propagates and the caller
    /// surfaces `ManualPowerCycleRequired`.
    async fn redfish_powercycle(&self, bmc_ip_address: IpAddr) -> SiteExplorerResult<()> {
        if let Err(power_cycle_err) = self
            .redfish_power_control(bmc_ip_address, libredfish::SystemPowerControl::PowerCycle)
            .await
        {
            tracing::warn!(
                %bmc_ip_address,
                error = %power_cycle_err,
                "PowerCycle failed; falling back to ACPowercycle to apply the queued NIC mode change",
            );
            self.redfish_power_control(
                bmc_ip_address,
                libredfish::SystemPowerControl::ACPowercycle,
            )
            .await?;
        }

        let mut txn = self.txn_begin().await?;

        db::explored_endpoints::set_last_redfish_powercycle(bmc_ip_address, &mut txn).await?;

        Ok(txn.commit().await?)
    }

    async fn find_machine_interface_for_ip(
        &self,
        ip_address: IpAddr,
    ) -> SiteExplorerResult<MachineInterfaceSnapshot> {
        let mut txn = self.txn_begin().await?;

        let machine_interface = db::machine_interface::find_by_ip(&mut txn, ip_address).await?;

        txn.commit().await?;

        match machine_interface {
            Some(interface) => Ok(interface),
            None => Err(SiteExplorerError::NotFoundError {
                kind: "machine_interface",
                id: format!("remote_ip={ip_address:?}"),
            }),
        }
    }

    //// can_ingest_host_endpoint will return true if the site explorer should proceed with ingesting a given host endpoint.
    /// It will always return true for a host that has already been ingested.
    ///
    /// If the host has not been ingested, and is not on, the function will try to turn the host on and return false.
    /// If the host has not been ingested, is a Lenovo,  and infinite boot is disabled, the function will try to enable
    /// infinite boot and return false.
    /// Otherwise, the function will return true.
    async fn can_ingest_host_endpoint(
        &self,
        metrics: &mut SiteExplorationMetrics,
        host_endpoint: &ExploredEndpoint,
    ) -> SiteExplorerResult<bool> {
        let is_managed_host_created_for_endpoint = match self
            .is_managed_host_created_for_endpoint(host_endpoint.address)
            .await
        {
            Ok(managed_host_exists) => managed_host_exists,
            Err(e) => {
                tracing::error!(%e, "failed to retrieve whether managed host was created for Host endpoint: {host_endpoint}");
                // return true by default
                true
            }
        };

        if is_managed_host_created_for_endpoint {
            // this host has already been ingested
            return Ok(true);
        }

        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(host_endpoint.address, bmc_target_port);
        let Some(system) = host_endpoint.report.systems.first() else {
            tracing::warn!(
                "Site Explorer could not find the system report for a host (bmc_ip_address: {})",
                host_endpoint.address,
            );
            metrics
                .increment_host_dpu_pairing_blocker(PairingBlockerReason::HostSystemReportMissing);
            return Ok(false);
        };

        // if we are explicitly forbidden from powering on in the expected_machines,
        // then don't do it
        if host_endpoint.pause_ingestion_and_poweron {
            tracing::warn!(
                "Host with bmc_ip_address: {} is configured to pause on ingestion",
                host_endpoint.address
            );
            return Ok(false);
        }

        let mut ingest_host = true;
        let interface = match self
            .find_machine_interface_for_ip(host_endpoint.address)
            .await
        {
            Ok(interface) => Some(interface),
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %host_endpoint.address,
                    error = %e,
                    "Site Explorer could not find machine interface for host endpoint",
                );
                None
            }
        };

        // The cached `systems[].power_state` may be stale when this endpoint was
        // not refreshed in the current iteration, so prefer a live Redfish power
        // state check for uningested hosts. The exceptions are auth/lockout and
        // unreachable failures, where another live read is either unsafe or very
        // unlikely to help. `None` means we have no trustworthy reading; we fall
        // back to the cached state for remediation decisions only and defer
        // ingestion to a later run.
        let fresh_power_state: Option<PowerState> =
            match host_endpoint.report.last_exploration_error.as_ref() {
                Some(err) if err.is_unauthorized() || err.is_unreachable() => None,
                _ => match interface.as_ref() {
                    Some(interface) => self
                        .endpoint_explorer
                        .redfish_get_power_state(bmc_target_addr, interface)
                        .await
                        .ok()
                        .map(IntoModel::into_model),
                    None => None,
                },
            };

        let effective_power_state = fresh_power_state.unwrap_or(system.power_state);

        if fresh_power_state.is_none() {
            ingest_host = false;
        }

        if !matches!(effective_power_state, PowerState::On) {
            ingest_host = false;

            if host_endpoint.pause_remediation {
                tracing::info!(
                    "Site Explorer found an uningested host (bmc_ip_address: {}) that is off, but remediation is paused — skipping power-on",
                    host_endpoint.address,
                );
            } else if fresh_power_state.is_some() {
                tracing::warn!(
                    "Site Explorer found an uningested host (bmc_ip_address: {}) that isn't on: {:#?}",
                    host_endpoint.address,
                    effective_power_state
                );

                if let Some(interface) = interface.as_ref() {
                    self.endpoint_explorer
                        .redfish_power_control(
                            bmc_target_addr,
                            interface,
                            libredfish::SystemPowerControl::On,
                        )
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                "Site Explorer failed to turn on host (bmc_ip_address: {}) through redfish: {}",
                                host_endpoint.address,
                                err
                            )
                        })
                        .ok();
                }
            }
        }

        if host_endpoint.report.vendor.unwrap_or_default().is_nvidia() {
            let Some(manager) = host_endpoint.report.managers.first() else {
                tracing::warn!(
                    "Site Explorer could not find the system report for a Nvidia host (bmc_ip_address: {})",
                    host_endpoint.address,
                );

                return Ok(false);
            };

            // Viking
            if system.id == "DGX" && manager.id == "BMC" {
                for service in host_endpoint.report.service.iter() {
                    if let Some(cpldmb_0_inventory) =
                        service.inventories.iter().find(|&x| x.id == "CPLDMB_0")
                    {
                        let current_cpldmb_0_version =
                            cpldmb_0_inventory.version.clone().unwrap_or_default();
                        let expected_cpldmb_0_version = "0.2.1.9";
                        match version_compare::compare_to(
                            &current_cpldmb_0_version,
                            expected_cpldmb_0_version,
                            Cmp::Eq,
                        ) {
                            Ok(is_cpldmb_version_at_expected) => {
                                if !is_cpldmb_version_at_expected {
                                    tracing::warn!(
                                        "Site Explorer found a Viking (bmc_ip_address: {}) with a CPLDMB_0 version of {current_cpldmb_0_version}, which is less than the expected version of {expected_cpldmb_0_version}. A DC Power Cycle may be needed",
                                        host_endpoint.address,
                                    );
                                    metrics.increment_host_dpu_pairing_blocker(
                                        PairingBlockerReason::VikingCpldVersionIssue,
                                    );
                                    return Ok(false);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Site Explorer found a Viking (bmc_ip_address: {}) with a CPLDMB_0 version of {current_cpldmb_0_version} and could not compare it to the current CPLDMB_0 version of {expected_cpldmb_0_version}: {e:#?}",
                                    host_endpoint.address,
                                );
                                metrics.increment_host_dpu_pairing_blocker(
                                    PairingBlockerReason::VikingCpldVersionIssue,
                                );
                                return Ok(false);
                            }
                        }
                    } else {
                        tracing::warn!(
                            "Site Explorer could not find the CPLDMB_0 inventory for a Viking (bmc_ip_address: {})",
                            host_endpoint.address,
                        );
                        metrics.increment_host_dpu_pairing_blocker(
                            PairingBlockerReason::VikingCpldVersionIssue,
                        );
                        return Ok(false);
                    };
                }
            }
        }

        if host_endpoint.report.vendor.unwrap_or_default().is_lenovo()
            && system
                .attributes
                .is_infinite_boot_enabled
                .is_some_and(|status| !status)
        {
            tracing::warn!(
                "Site Explorer found an uningested Lenovo (bmc_ip_address: {}) without infinite boot enabled; System Report: {:#?}",
                host_endpoint.address,
                system.attributes
            );

            let interface = self
                .find_machine_interface_for_ip(bmc_target_addr.ip())
                .await?;

            self.endpoint_explorer
                .machine_setup(bmc_target_addr, &interface, None)
                .await
                .inspect_err(|err| {
                    tracing::error!(
                        "Site Explorer failed to call machine_setup against Lenovo (bmc_ip_address: {}): {}",
                        host_endpoint.address,
                        err
                    )
                }).ok();

            self.endpoint_explorer
                .redfish_power_control(
                    bmc_target_addr,
                    &interface,
                    libredfish::SystemPowerControl::ForceRestart,
                )
                .await
                .inspect_err(|err| {
                    tracing::error!(
                        "Site Explorer failed to restart Lenovo (bmc_ip_address: {}) after calling machine_setup: {}",
                        host_endpoint.address,
                        err
                    )
                }).ok();

            ingest_host = false;
        }

        Ok(ingest_host)
    }

    /// Returns `true` when the DPU's hardware NIC mode already matches the
    /// desired target; `false` when the function has issued a `set_nic_mode`
    /// to fix a mismatch (in which case the caller should skip this host
    /// for the current site-explorer cycle -- the next cycle will pick up
    /// the corrected mode).
    ///
    /// The target is resolved in priority order:
    /// 1. If the operator explicitly declared `DpuMode::NicMode` on the
    ///    `ExpectedMachine`, target NIC mode (per-host override).
    /// 2. If the operator declared `DpuMode::NoDpu`, bail out -- the
    ///    `MachineValidation` state handler is where "hardware reports a
    ///    DPU but operator said no DPU" gets surfaced as a health alert;
    ///    we don't try to reconfigure in that case.
    /// 3. Otherwise (operator default `DpuMode::DpuMode`), fall back to
    ///    the existing BF3 SuperNIC / BF3 DPU part-number heuristic for
    ///    backward compat: BF3 SuperNIC → NIC mode, BF3 DPU → DPU mode,
    ///    BF2 / unknown → no-op.
    async fn check_and_configure_dpu_mode(
        &self,
        dpu_ep: &ExploredEndpoint,
        dpu_part_number: Option<&str>,
        host_dpu_mode: DpuMode,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<bool> {
        // Compute the target NIC mode. `None` means "no opinion -- don't
        // attempt to reconfigure" (e.g., BF2 where the heuristic doesn't
        // apply, or NoDpu where we defer to the health-check path).
        let target_nic_mode: Option<NicMode> = match host_dpu_mode {
            DpuMode::NicMode => Some(NicMode::Nic),
            DpuMode::NoDpu => None,
            DpuMode::DpuMode => {
                // Preserve existing BF3 part-number heuristics when the operator
                // hasn't explicitly chosen a mode. Missing part numbers only
                // disable this heuristic fallback; explicit modes above do not
                // require a part number. BF4 does not currently
                // expose a reliable DPU/NIC mode signal over Redfish, so the
                // default path does not infer or reconfigure BF4 mode,
                dpu_part_number.and_then(|dpu_part_number| {
                    if is_bf3_supernic_part_number(dpu_part_number) {
                        Some(NicMode::Nic)
                    } else if is_bf3_dpu_part_number(dpu_part_number) {
                        Some(NicMode::Dpu)
                    } else {
                        None
                    }
                })
            }
        };

        let Some(target_nic_mode) = target_nic_mode else {
            return Ok(true);
        };

        match dpu_ep.report.nic_mode() {
            Some(observed) if observed == target_nic_mode => Ok(true),
            Some(observed) => {
                tracing::warn!(
                    address = %dpu_ep.address,
                    part_number = ?dpu_part_number,
                    %observed,
                    ?target_nic_mode,
                    ?host_dpu_mode,
                    "site explorer found a DPU with a mode that does not match the target; will try to reconfigure"
                );
                metrics.increment_dpu_migration_signal(DpuMigrationSignal::ModeMismatchFound);
                self.set_nic_mode(dpu_ep, target_nic_mode).await?;
                metrics.increment_dpu_migration_signal(DpuMigrationSignal::SetNicModeIssued);
                Ok(false)
            }
            None => {
                tracing::warn!(
                    "Site explorer cannot determine this DPU's mode {}: {:#?}",
                    dpu_ep.address,
                    dpu_ep.report
                );
                Ok(true)
            }
        }
    }
}

/// Reconcile a single static-IP reservation into `machine_interfaces` in its
/// own transaction.
///
/// Called once per configured static IP during the `update_explored_endpoints`
/// walk over `expected_machine` / `expected_switch` / `expected_power_shelf`.
/// Idempotent on the api-db side -- steady-state runs are noops. Per-entry
/// errors are logged as warnings, and doesn't stop the wider iteration.
///
/// This is `pub` so tests can drive a single (mac, ip, interface_type)
/// preallocation directly without needing to create a full `SiteExplorer`.
pub async fn try_preallocate_one(
    pool: &PgPool,
    mac: MacAddress,
    ip: IpAddr,
    interface_type: InterfaceType,
    kind: &'static str,
    retained_window: Option<chrono::Duration>,
) {
    let mut txn = match db::Transaction::begin(pool).await {
        Ok(t) => t,
        Err(error) => {
            tracing::warn!(
                %error, %mac, %ip, kind,
                "Site-explorer preallocation: txn_begin failed"
            );
            return;
        }
    };
    let result = match interface_type {
        InterfaceType::Bmc => {
            db::machine_interface::preallocate_bmc_machine_interface(
                txn.as_pgconn(),
                mac,
                ip,
                retained_window,
            )
            .await
        }
        InterfaceType::Data => {
            db::machine_interface::preallocate_machine_interface(
                txn.as_pgconn(),
                mac,
                ip,
                retained_window,
            )
            .await
        }
    };
    match result {
        Ok(()) => {
            if let Err(error) = txn.commit().await {
                tracing::warn!(
                    %error, %mac, %ip, kind,
                    "Site-explorer preallocation: commit failed"
                );
            }
        }
        Err(error) => {
            tracing::warn!(%error, %mac, %ip, kind, "Site-explorer preallocation skipped");
        }
    }
}

pub fn get_sys_image_version(services: &[Service]) -> Result<&String, String> {
    let Some(service) = services.iter().find(|s| s.id == "FirmwareInventory") else {
        return Err("Missing FirmwareInventory".to_string());
    };

    let Some(image) = service
        .inventories
        .iter()
        .find(|inv| inv.id == "DPU_SYS_IMAGE")
    else {
        return Err("Missing DPU_SYS_IMAGE".to_string());
    };

    image
        .version
        .as_ref()
        .ok_or("Missing DPU_SYS_IMAGE version".to_string())
}

/// get_base_mac_from_sys_image_version returns a base MAC address
/// for a given sys image version/ See comments below about how the
/// DPU derives a MAC from a DPU_SYS_IMAGE, but ultimately, a
/// DPU_SYS_IMAGE of a088:c203:0046:0c68 means you just take out
/// chars 6-10, and you get a MAC of a0:88:c2:46:0c:68.
fn get_base_mac_from_sys_image_version(sys_image_version: &String) -> Result<String, String> {
    // The DPU_SYS_IMAGE is always 19 characters long. Well, until
    // it isn't, but for now, the DPU_SYS_IMAGE is 19 characters
    // long.
    if sys_image_version.len() != 19 {
        return Err(format!(
            "Invalid sys_image_version length: {} ({})",
            sys_image_version.len(),
            sys_image_version,
        ));
    }

    // First, strip out the colons, and make sure we're
    // left with 16 [what should be hex-friendly] characters.
    let mut base_mac = sys_image_version.replace(':', "");
    if base_mac.len() != 16 {
        return Err(format!(
            "Invalid base_mac length from sys_image_version after removing ':': {}",
            base_mac.len()
        ));
    }

    // And now drop range 6-10, leaving us with what
    // should be the 12 characters for the MAC address.
    base_mac.replace_range(6..10, "");

    Ok(base_mac)
}

/// Identifies the MAC address that is used by the pf0 interface that
/// the DPU exposes to the host.
///
/// According "MAC and GUID allocation and assignment" document
///
/// Ethernet only require allocation of MAC address. Similarly,
/// IB only requires GUID allocation. Yet, since Mellanox devices support RoCE,
/// NIC cards require allocation of GUID addresses. Similarly, since IB supports
/// IP traffic HCA cards require allocation of MAC addresses.
/// As both MAC addresses and GUID addresses are allocated together, there is a
/// correlation between these 2 values. Unfortunately the translation from MAC
/// address to GUID and vice-versa is inconsistent between different platforms and operating systems.
/// To assure that this will not cause future issues, it is required that future
/// devices will not rely on any conversion formulas between MAC and GUID values,
/// and that these values will be explicitly stored in the device's nonvolatile memory.
///
/// Assumption:
/// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE(Version)
/// is identical to
/// flint -d /dev/mst/mt*_pciconf0 q full (BASE GUID)
///
/// Details:
/// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE
/// is taken from /sys/class/infiniband/mlx*_<port>/sys_image_guid
///
/// Example:
/// DPU_SYS_IMAGE: a088:c203:0046:0c68
/// Base GUID: a088c20300460c68
/// Base MAC:  a088c2    460c68
/// Note: 0300 in the middle looks as a constant for dpu
///
/// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE
/// "Version": "a088:c203:0046:0c68"
///
/// ibdev2netdev -v
/// 0000:31:00.0 mlx5_0 (MT41692 - 900-9D3B6-00CV-AA0) BlueField-3 P-Series DPU 200GbE/NDR200 dual-port QSFP112,
/// PCIe Gen5.0 x16 FHHL, Crypto Enabled, 32GB DDR5, BMC, Tall Bracket  fw 32.37.1306 port 1 (DOWN  ) ==> ens3np0 (Down)
///
/// cat /sys/class/infiniband/mlx5_0/sys_image_guid
/// a088:c203:0046:0c68
///
/// ip link show ens3np0
/// 6: ens3np0: <BROADCAST,MULTICAST> mtu 1500 qdisc noop state DOWN mode DEFAULT group default qlen 1000
/// link/ether a0:88:c2:46:0c:68 brd ff:ff:ff:ff:ff:ff
///
/// The method should be migrated to the DPU directly providing the
/// MAC address: https://redmine.mellanox.com/issues/3749837
fn find_host_pf_mac_address(dpu_ep: &ExploredEndpoint) -> Result<MacAddress, String> {
    // Base-MAC derivation has three paths, tried in order of trust:
    //   1. Primary  : the explored ComputerSystem base_mac (OEM Redfish BaseMAC).
    //   2. Legacy    : derived from UpdateService/FirmwareInventory/DPU_SYS_IMAGE.Version.
    //   3. BMC offset: derived from the BMC manager eth0 MAC minus a per-platform
    //                  offset (fallback-only; see derive_base_mac_from_bmc_eth0).
    // We only hard-fail if all three paths fail.

    // Path 1: explored computer-system base_mac.
    if let Some(system_mac) = dpu_ep.report.systems.first().and_then(|s| s.base_mac) {
        return Ok(system_mac.to_mac());
    }

    // Path 2: legacy DPU_SYS_IMAGE derivation. Soft-fail so we can try path 3.
    tracing::warn!("ComputerSystem doesn't have base_mac, falling back to DPU_SYS_IMAGE method");
    let legacy_err = match get_sys_image_version(dpu_ep.report.service.as_ref())
        .and_then(get_base_mac_from_sys_image_version)
        .and_then(|legacy_mac| {
            sanitized_mac(&legacy_mac).map_err(|e| {
                format!("Failed to build sanitized MAC from legacy/service MAC: {e} (source_mac: {legacy_mac})")
            })
        }) {
        Ok(mac) => return Ok(mac),
        Err(e) => {
            tracing::warn!("DPU_SYS_IMAGE derivation failed, falling back to BMC eth0 offset: {e}");
            e
        }
    };

    // Path 3: BMC manager eth0 MAC minus a per-platform offset. If this path is
    // also unavailable, surface the legacy error so we still fail the old way.
    derive_base_mac_from_bmc_eth0(&dpu_ep.report).ok_or(legacy_err)
}

// The PF0 base MAC sits a fixed offset below the DPU BMC's eth0 MAC, within the
// contiguous MAC block allocated to the card. Per the BlueField-3 DPU Controller
// User Manual (§10.1, "DPU Controller Board Label"):
//   * host high-speed ports are `base + port_index`
//   * `DPU_BMC_MAC = OOB_MAC + 1`
// so the offset decomposes as `(OOB - base) + 1`.
//
// Measured on a real BF3 DPU (offset = 0x25 = 37):
//   DPU BMC eth0                              : 5c:25:73:9e:ac:eb
//   base (DPU_SYS_IMAGE 5c25:7303:009e:acc6)  : 5c:25:73:9e:ac:c6
// which implies OOB = bmc - 1 = ...ea and a host-reservation gap of
// (OOB - base) = 0x24 = 36, consistent with the manual's `BMC = OOB + 1`.
//
// The host-reservation gap is not published and could differ on other SKUs
// (e.g. 1- vs 2-port); revisit if a card of a different SKU mis-derives.
const BF3_ETH0_TO_BASE_MAC_OFFSET: u64 = 0x25; // measured: BlueField-3, see above

/// The per-platform offset to subtract from the BMC manager eth0 MAC to obtain
/// the DPU PF0 base MAC, or `None` for platforms we can't classify (we never guess).
fn bmc_eth0_to_base_mac_offset(report: &EndpointExplorationReport) -> Option<u64> {
    match report.identify_dpu()? {
        DpuModel::BlueField3 => Some(BF3_ETH0_TO_BASE_MAC_OFFSET),
        // BlueField-2 is not supported by the BMC eth0 offset fallback.
        DpuModel::BlueField2 | DpuModel::Unknown => None,
    }
}

/// Fallback-only (item #2 of issue #1076): derive the DPU PF0 base MAC from the
/// BMC manager eth0 MAC minus a platform-specific offset. Returns `None` if the
/// eth0 interface MAC is missing, locally-administered (pre-sync), the platform
/// is unknown, or the subtraction would underflow.
fn derive_base_mac_from_bmc_eth0(report: &EndpointExplorationReport) -> Option<MacAddress> {
    let offset = bmc_eth0_to_base_mac_offset(report)?;

    // Pick the eth0 interface specifically -- the OOB interface also lives in
    // the manager's ethernet_interfaces list.
    let bmc_eth0 = report
        .managers
        .iter()
        .flat_map(|m| m.ethernet_interfaces.iter())
        .find(|e| {
            e.id.as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case("eth0"))
        })
        .and_then(|e| e.mac_address)?;

    // A real NVIDIA BMC MAC is globally unique. A locally-administered MAC means
    // the BMC hasn't synced its burned-in address yet (transient post-boot
    // state) -- refuse to derive a base MAC from it rather than hand back a
    // plausible-but-wrong value.
    if is_locally_administered_mac(bmc_eth0) {
        tracing::warn!(
            bmc_eth0 = %bmc_eth0,
            "BMC eth0 MAC is locally-administered (pre-sync?); skipping offset derivation",
        );
        return None;
    }

    let derived = mac_to_u64(bmc_eth0).checked_sub(offset)?;
    let mac = u64_to_mac(derived);
    tracing::warn!(
        bmc_eth0 = %bmc_eth0,
        derived = %mac,
        "derived DPU base MAC from BMC eth0 via offset fallback",
    );
    Some(mac)
}

/// MAC address as a 48-bit big-endian integer (top two bytes of the u64 are zero).
fn mac_to_u64(mac: MacAddress) -> u64 {
    mac.bytes()
        .iter()
        .fold(0u64, |acc, &byte| (acc << 8) | u64::from(byte))
}

/// Inverse of [`mac_to_u64`]; the high 16 bits are discarded.
fn u64_to_mac(value: u64) -> MacAddress {
    let b = value.to_be_bytes();
    MacAddress::new([b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Whether a discovered DPU BMC is reporting that it's running as a plain NIC.
fn is_dpu_in_nic_mode(dpu_ep: &ExploredEndpoint, host_ep: &ExploredEndpoint) -> bool {
    let nic_mode = dpu_ep.report.nic_mode().is_some_and(|m| m == NicMode::Nic);
    if nic_mode {
        tracing::info!(
            address = %dpu_ep.address,
            "discovered bluefield in NIC mode attached to host {}",
            host_ep.address
        );
    }
    nic_mode
}

/// The host-facing PF MAC of a discovered DPU, or `None` if it can't be determined.
fn get_host_pf_mac_address(dpu_ep: &ExploredEndpoint) -> Option<MacAddress> {
    match find_host_pf_mac_address(dpu_ep) {
        Ok(m) => Some(m),
        Err(error) => {
            tracing::error!(%error, dpu_ip = %dpu_ep.address, "Failed to find base mac address for DPU");
            None
        }
    }
}

/// Returns the normalized serial when a host PCIe record repeats a BlueField
/// serial already seen during this host's ingestion pass.
fn duplicate_bluefield_serial<'a>(
    part_number: Option<&str>,
    serial_number: Option<&'a str>,
    seen: &mut HashSet<&'a str>,
) -> Option<&'a str> {
    if !part_number
        .map(str::trim)
        .is_some_and(is_bluefield_part_number)
    {
        return None;
    }

    let serial_number = serial_number.map(str::trim).filter(|s| !s.is_empty())?;
    (!seen.insert(serial_number)).then_some(serial_number)
}

/// State from exploring a host's DPUs and pairing them with DPU BMCs.
///
/// The two counts are only ever incremented (monotonic), so the
/// bookkeeping can never underflow; DPUs we still expect to manage is
/// the derived difference ([`DpuExplorationState::expected_managed_total`]).
#[derive(Debug)]
struct DpuExplorationState {
    /// DPUs the host's BMC reports (matched on `part_number`).
    reported_total: usize,
    /// Of those, the ones confirmed running as a plain NIC -- not managed DPUs.
    running_as_nic_total: usize,
    /// `false` once any discovered DPU's mode didn't match the target (a
    /// `set_nic_mode` was issued); drives the downstream host power-cycle.
    all_configured: bool,
    /// DPUs running in DPU mode (configured correctly) -- attached to the host.
    running_as_dpu: Vec<ExploredDpu>,
}

impl DpuExplorationState {
    fn new() -> Self {
        Self {
            reported_total: 0,
            running_as_nic_total: 0,
            all_configured: true,
            running_as_dpu: Vec::new(),
        }
    }

    /// DPUs we still expect to manage = reported DPUs minus those running as NICs.
    fn expected_managed_total(&self) -> usize {
        self.reported_total
            .saturating_sub(self.running_as_nic_total)
    }
}

/// Status of a discovered DPU (one whose serial matched an explored DPU BMC)
/// relative to a host, as determined by [`classify_matched_dpu`].
enum DiscoveredDpu {
    /// Running in DPU mode and configured correctly -- the caller attaches it.
    RunningAsDpu(ExploredDpu),
    /// A DPU running as a plain NIC -- counted, but not a managed DPU.
    RunningAsNic,
    /// Mode didn't match the target; `check_and_configure_dpu_mode` just issued a
    /// `set_nic_mode`. The host needs a power cycle (handled downstream) before
    /// this DPU re-reports in the corrected mode, so we can't pair it this cycle.
    NeedsReconfig,
    /// The DPU's mode couldn't be checked (Redfish error); skip it this cycle.
    ModeCheckFailed(SiteExplorerError),
}

/// Classify a discovered DPU against a host.
///
/// The only IO (`check_and_configure_dpu_mode`, which may issue a
/// `set_nic_mode`) happens in the caller, which passes its result in as
/// `mode_check` (`None` when the caller deliberately skipped the mode check).
/// Keeping the decision here makes it unit-testable without a Redfish mock.
fn classify_matched_dpu(
    dpu_ep: &ExploredEndpoint,
    host_ep: &ExploredEndpoint,
    mode_check: Option<SiteExplorerResult<bool>>,
) -> DiscoveredDpu {
    match mode_check {
        Some(Ok(false)) => return DiscoveredDpu::NeedsReconfig,
        Some(Err(err)) => return DiscoveredDpu::ModeCheckFailed(err),
        // Mode already correct, or the caller skipped the mode check.
        Some(Ok(true)) | None => {}
    }

    // We do not want to attach DPUs running as NICs as "managed" DPUs.
    if is_dpu_in_nic_mode(dpu_ep, host_ep) {
        return DiscoveredDpu::RunningAsNic;
    }

    DiscoveredDpu::RunningAsDpu(ExploredDpu {
        bmc_ip: dpu_ep.address,
        host_pf_mac_address: get_host_pf_mac_address(dpu_ep),
        report: dpu_ep.report.clone().into(),
    })
}

pub async fn get_machine_state_by_bmc_ip(
    database_connection: &PgPool,
    bmc_ip: &str,
) -> Result<String, DatabaseError> {
    let mut txn = Transaction::begin(database_connection).await?;

    let state = match db::machine_topology::find_machine_id_by_bmc_ip(txn.as_pgconn(), bmc_ip)
        .await?
    {
        Some(machine_id) => {
            match machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default()).await? {
                Some(machine) => machine.current_state().to_string(),
                None => String::new(),
            }
        }
        None => String::new(),
    };

    txn.commit().await?;

    Ok(state)
}

fn pause_ingestion_and_poweron(
    expected_machines_by_mac: &HashMap<MacAddress, ExpectedEntity>,
    mac_address: &mac_address::MacAddress,
) -> bool {
    if let Some(ExpectedEntity::Machine(expected_machine)) =
        expected_machines_by_mac.get(mac_address)
    {
        return expected_machine
            .data
            .default_pause_ingestion_and_poweron
            .unwrap_or(false);
    }

    false
}

/// Returns true if the power state should trigger a PoweredOff health alert.
///
/// We alert on `Off`, `Paused`, and `Unknown` states, but NOT on transitional
/// states (`PoweringOn`, `PoweringOff`) because the BMC is still responding
/// during graceful power reset (warm reboot)
fn should_alert_power_state(power_state: PowerState) -> bool {
    !matches!(
        power_state,
        PowerState::On | PowerState::PoweringOn | PowerState::PoweringOff
    )
}

fn site_explorer_health_report_needs_update(
    previous_health_report: Option<&health_report::HealthReport>,
    new_health_report: &health_report::HealthReport,
) -> bool {
    match previous_health_report {
        None => !new_health_report.alerts.is_empty(),
        Some(_) if new_health_report.alerts.is_empty() => true,
        Some(previous_health_report) => {
            !health_reports_equal_ignoring_observed_at(previous_health_report, new_health_report)
        }
    }
}

fn health_reports_equal_ignoring_observed_at(
    left: &health_report::HealthReport,
    right: &health_report::HealthReport,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.observed_at = None;
    right.observed_at = None;
    left == right
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use config_version::ConfigVersion;
    use model::site_explorer::PreingestionState;

    use super::*;

    #[test]
    fn mac_u64_roundtrip() {
        let mac: MacAddress = "a0:88:c2:46:0c:68".parse().unwrap();
        assert_eq!(mac_to_u64(mac), 0x0000_a088_c246_0c68);
        assert_eq!(u64_to_mac(mac_to_u64(mac)), mac);
    }

    #[test]
    fn u64_to_mac_discards_high_bits() {
        // High 16 bits set must not leak into the MAC bytes.
        assert_eq!(
            u64_to_mac(0xffff_a088_c246_0c68),
            "a0:88:c2:46:0c:68".parse().unwrap()
        );
    }

    #[test]
    fn bf3_offset_derives_measured_base_mac() {
        // Real BF3 DPU measurement (see BF3_ETH0_TO_BASE_MAC_OFFSET):
        // BMC eth0 - offset must yield the DPU_SYS_IMAGE-derived base MAC.
        let bmc_eth0: MacAddress = "5c:25:73:9e:ac:eb".parse().unwrap();
        let base: MacAddress = "5c:25:73:9e:ac:c6".parse().unwrap();
        let derived = u64_to_mac(mac_to_u64(bmc_eth0) - BF3_ETH0_TO_BASE_MAC_OFFSET);
        assert_eq!(derived, base);
        // Cross-check the documented BMC = OOB + 1 relationship.
        let oob = u64_to_mac(mac_to_u64(bmc_eth0) - 1);
        assert_eq!(oob, "5c:25:73:9e:ac:ea".parse().unwrap());
    }

    const BF3_MODEL_VARIANTS: [&str; 2] = ["NVIDIA BlueField 3 DPU", "BlueField-3 DPU"];

    // Minimal BlueField-3 report with a single manager eth0 interface carrying
    // `eth0_mac`. Classifies as BF3 (system id "Bluefield" + Card1 BF3 chassis).
    fn bf3_report_with_eth0(model: &str, eth0_mac: &str) -> EndpointExplorationReport {
        use model::site_explorer::{Chassis, ComputerSystem, EthernetInterface, Manager};
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "Card1".to_string(),
                model: Some(model.to_string()),
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "Bluefield_BMC".to_string(),
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("eth0".to_string()),
                    mac_address: Some(eth0_mac.parse().unwrap()),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn bmc_eth0_offset_skips_locally_administered_mac() {
        check_values(
            [
                Check {
                    scenario: "space-separated BF3 model with pre-sync eth0",
                    input: (BF3_MODEL_VARIANTS[0], "9a:72:d5:07:ae:7e"),
                    expect: None,
                },
                Check {
                    scenario: "hyphenated BF3 model with pre-sync eth0",
                    input: (BF3_MODEL_VARIANTS[1], "9a:72:d5:07:ae:7e"),
                    expect: None,
                },
                Check {
                    scenario: "space-separated BF3 model with synced eth0",
                    input: (BF3_MODEL_VARIANTS[0], "5c:25:73:9e:ac:eb"),
                    expect: Some("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Check {
                    scenario: "hyphenated BF3 model with synced eth0",
                    input: (BF3_MODEL_VARIANTS[1], "5c:25:73:9e:ac:eb"),
                    expect: Some("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
            ],
            |(model, eth0_mac)| {
                let report = bf3_report_with_eth0(model, eth0_mac);
                assert_eq!(bmc_eth0_to_base_mac_offset(&report), Some(0x25));
                derive_base_mac_from_bmc_eth0(&report)
            },
        );
    }

    #[test]
    fn bmc_eth0_offset_fallback_unsupported_for_bf2() {
        // BlueField-2 is intentionally not supported by the BMC eth0 offset
        // fallback, so derivation must return None even when an eth0 MAC exists.
        let mut report = load_bf2_ep_report();
        for s in report.systems.iter_mut() {
            s.base_mac = None;
        }
        assert!(bmc_eth0_to_base_mac_offset(&report).is_none());
        assert!(derive_base_mac_from_bmc_eth0(&report).is_none());
    }

    fn load_bf2_ep_report() -> EndpointExplorationReport {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/test_data/bf2_report.json");
        let report: EndpointExplorationReport =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(!report.systems.is_empty());
        assert!(!report.managers.is_empty());
        assert!(!report.chassis.is_empty());
        assert!(!report.service.is_empty());
        report
    }

    fn load_dell_ep_report() -> EndpointExplorationReport {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/test_data/dell_report.json"
        );
        let report: EndpointExplorationReport =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(!report.systems.is_empty());
        assert!(!report.managers.is_empty());
        assert!(!report.chassis.is_empty());
        assert!(report.service.is_empty());
        report
    }

    #[test]
    fn test_load_dell_report() {
        let _ = load_dell_ep_report();
    }

    fn explored_endpoint(report: EndpointExplorationReport) -> ExploredEndpoint {
        ExploredEndpoint {
            address: "10.0.0.1".parse().unwrap(),
            report,
            report_version: ConfigVersion::initial(),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        }
    }

    /// A BF2 DPU endpoint with its reported NIC mode forced to `nic_mode`.
    fn bf2_dpu(nic_mode: Option<NicMode>) -> ExploredEndpoint {
        let mut report = load_bf2_ep_report();
        report
            .systems
            .first_mut()
            .expect("bf2 report has a system")
            .attributes
            .nic_mode = nic_mode;
        explored_endpoint(report)
    }

    #[test]
    fn classify_running_as_dpu_when_in_dpu_mode() {
        let dpu = bf2_dpu(Some(NicMode::Dpu));
        let host = explored_endpoint(load_dell_ep_report());
        // Mode already correct (`Ok(true)`) -> attach as a managed DPU.
        assert!(matches!(
            classify_matched_dpu(&dpu, &host, Some(Ok(true))),
            DiscoveredDpu::RunningAsDpu(_)
        ));
        // A skipped mode check (`None`) behaves the same.
        assert!(matches!(
            classify_matched_dpu(&dpu, &host, None),
            DiscoveredDpu::RunningAsDpu(_)
        ));
    }

    #[test]
    fn classify_running_as_nic_when_dpu_reports_nic_mode() {
        let dpu = bf2_dpu(Some(NicMode::Nic));
        let host = explored_endpoint(load_dell_ep_report());
        assert!(matches!(
            classify_matched_dpu(&dpu, &host, Some(Ok(true))),
            DiscoveredDpu::RunningAsNic
        ));
    }

    #[test]
    fn classify_needs_reconfig_when_set_nic_mode_was_issued() {
        // `Ok(false)` means `check_and_configure_dpu_mode` just issued a `set_nic_mode`.
        let dpu = bf2_dpu(Some(NicMode::Nic));
        let host = explored_endpoint(load_dell_ep_report());
        assert!(matches!(
            classify_matched_dpu(&dpu, &host, Some(Ok(false))),
            DiscoveredDpu::NeedsReconfig
        ));
    }

    #[test]
    fn classify_mode_check_failed_on_error() {
        let dpu = bf2_dpu(Some(NicMode::Dpu));
        let host = explored_endpoint(load_dell_ep_report());
        let err = SiteExplorerError::InvalidArgument("boom".to_string());
        assert!(matches!(
            classify_matched_dpu(&dpu, &host, Some(Err(err))),
            DiscoveredDpu::ModeCheckFailed(_)
        ));
    }

    #[test]
    fn dpu_exploration_expected_managed_total_saturates() {
        let mut exploration = DpuExplorationState::new();
        // More NIC-mode than reported (the partial-data case that used to
        // underflow `-= 1`): the derived total saturates to 0 instead of panicking.
        exploration.reported_total = 1;
        exploration.running_as_nic_total = 3;
        assert_eq!(exploration.expected_managed_total(), 0);
        // Normal case: reported DPUs minus those running as NICs.
        exploration.reported_total = 5;
        exploration.running_as_nic_total = 2;
        assert_eq!(exploration.expected_managed_total(), 3);
    }

    #[test]
    fn duplicate_bluefield_serial_only_flags_repeated_bluefield_serials() {
        struct Case {
            name: &'static str,
            devices: &'static [(Option<&'static str>, Option<&'static str>)],
            expected: &'static [bool],
        }

        let cases = [
            Case {
                name: "duplicate BlueField serial",
                devices: &[
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-1")),
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-1")),
                ],
                expected: &[false, true],
            },
            Case {
                name: "trimmed duplicate BlueField serial",
                devices: &[
                    (Some("900-9D3B6-00SV-AA0"), Some(" DPU-SERIAL-1 ")),
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-1")),
                ],
                expected: &[false, true],
            },
            Case {
                name: "distinct BlueField serials",
                devices: &[
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-1")),
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-2")),
                ],
                expected: &[false, false],
            },
            Case {
                name: "non-BlueField does not reserve serial",
                devices: &[
                    (Some("0JKJDC"), Some("DPU-SERIAL-1")),
                    (Some("900-9D3B6-00SV-AA0"), Some("DPU-SERIAL-1")),
                ],
                expected: &[false, false],
            },
            Case {
                name: "missing and empty serials are not duplicates",
                devices: &[
                    (Some("900-9D3B6-00SV-AA0"), None),
                    (Some("900-9D3B6-00SV-AA0"), Some("")),
                    (Some("900-9D3B6-00SV-AA0"), Some("   ")),
                ],
                expected: &[false, false, false],
            },
        ];

        for case in cases {
            let mut seen = HashSet::new();
            let actual: Vec<bool> = case
                .devices
                .iter()
                .map(|(part_number, serial_number)| {
                    duplicate_bluefield_serial(*part_number, *serial_number, &mut seen).is_some()
                })
                .collect();
            assert_eq!(actual, case.expected, "{}", case.name);
        }
    }

    #[test]
    fn test_find_host_pf_mac_address() {
        // A freshly-loaded BF2 endpoint; each case starts from one of these and
        // perturbs the firmware inventory the legacy MAC lookup reads from.
        let endpoint = || ExploredEndpoint {
            address: "10.217.132.202".parse().unwrap(),
            report: load_bf2_ep_report(),
            report_version: ConfigVersion::initial(),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        };

        // Override the `DPU_SYS_IMAGE` firmware version the legacy path parses.
        let with_sys_image = |version: &str| {
            let mut ep = endpoint();
            let inv = ep
                .report
                .service
                .iter_mut()
                .find(|s| s.id == "FirmwareInventory")
                .unwrap()
                .inventories
                .iter_mut()
                .find(|inv| inv.id == "DPU_SYS_IMAGE")
                .unwrap();
            inv.version = Some(version.to_string());
            ep
        };

        let bf3_with_empty_sys_image = |model| {
            let mut ep = explored_endpoint(bf3_report_with_eth0(model, "5c:25:73:9e:ac:eb"));
            ep.report.service = with_sys_image("").report.service;
            ep
        };

        // Drop the firmware-inventory entry whose `id` matches `inventory_id`.
        let without_inventory = |inventory_id: &str| {
            let mut ep = endpoint();
            ep.report
                .service
                .iter_mut()
                .find(|s| s.id == "FirmwareInventory")
                .unwrap()
                .inventories
                .retain(|inv| inv.id != inventory_id);
            ep
        };

        // Drop the whole `FirmwareInventory` service.
        let without_firmware_inventory = || {
            let mut ep = endpoint();
            ep.report.service.retain(|s| s.id != "FirmwareInventory");
            ep
        };

        check_cases(
            [
                Case {
                    scenario: "legacy sys-image MAC, sanitized",
                    input: endpoint(),
                    expect: Yields("B8:3F:D2:90:95:F4".parse().unwrap()),
                },
                Case {
                    scenario: "space-separated BF3 model falls back to BMC eth0",
                    input: bf3_with_empty_sys_image(BF3_MODEL_VARIANTS[0]),
                    expect: Yields("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Case {
                    scenario: "hyphenated BF3 model falls back to BMC eth0",
                    input: bf3_with_empty_sys_image(BF3_MODEL_VARIANTS[1]),
                    expect: Yields("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Case {
                    scenario: "legacy sys-image MAC fails sanitization",
                    input: with_sys_image("b83f:d203:0090:95fz"),
                    expect: FailsWith("Failed to build sanitized MAC from legacy/service MAC: Invalid stripped MAC length: 11 (input: b83fd29095fz, output: b83fd29095f) (source_mac: b83fd29095fz)".to_string()),
                },
                Case {
                    scenario: "legacy sys-image is too short",
                    input: with_sys_image("abc"),
                    expect: FailsWith("Invalid sys_image_version length: 3 (abc)".to_string()),
                },
                Case {
                    scenario: "no DPU_SYS_IMAGE inventory",
                    input: without_inventory("DPU_SYS_IMAGE"),
                    expect: FailsWith("Missing DPU_SYS_IMAGE".to_string()),
                },
                Case {
                    scenario: "no FirmwareInventory service",
                    input: without_firmware_inventory(),
                    expect: FailsWith("Missing FirmwareInventory".to_string()),
                },
            ],
            |ep| find_host_pf_mac_address(&ep),
        );
    }

    #[test]
    fn test_should_alert_power_state() {
        // Should NOT alert on On or transitional states (PoweringOn/PoweringOff)
        // because the BMC is still responding during graceful power reset
        assert!(!should_alert_power_state(PowerState::On));
        assert!(!should_alert_power_state(PowerState::PoweringOn));
        assert!(!should_alert_power_state(PowerState::PoweringOff));

        // Should alert on Off, Paused, and Unknown states
        assert!(should_alert_power_state(PowerState::Off));
        assert!(should_alert_power_state(PowerState::Paused));
        assert!(should_alert_power_state(PowerState::Unknown));
    }

    #[test]
    fn test_site_explorer_health_report_needs_update() {
        fn empty_report() -> health_report::HealthReport {
            health_report::HealthReport::empty(
                health_report::HealthReport::SITE_EXPLORER_SOURCE.to_string(),
            )
        }

        fn report_with_alert(
            message: &str,
            in_alert_since: Option<chrono::DateTime<chrono::Utc>>,
        ) -> health_report::HealthReport {
            let mut report = empty_report();
            report.alerts.push(health_report::HealthProbeAlert {
                id: "BmcExplorationFailure".parse().unwrap(),
                target: Some("192.0.2.10".to_string()),
                in_alert_since,
                message: message.to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations(),
                ],
            });
            report
        }

        let empty = empty_report();
        assert!(!site_explorer_health_report_needs_update(None, &empty));

        let alert_started_at = chrono::Utc::now();
        let new_alert = report_with_alert("Endpoint exploration failed", Some(alert_started_at));
        assert!(site_explorer_health_report_needs_update(None, &new_alert));

        let mut previous_alert = new_alert.clone();
        previous_alert.observed_at = Some(alert_started_at);
        let mut same_alert = new_alert;
        same_alert.observed_at = None;
        assert!(!site_explorer_health_report_needs_update(
            Some(&previous_alert),
            &same_alert,
        ));

        let mut timestamp_changed = same_alert;
        timestamp_changed.alerts[0].in_alert_since =
            Some(alert_started_at + chrono::Duration::seconds(1));
        assert!(site_explorer_health_report_needs_update(
            Some(&previous_alert),
            &timestamp_changed,
        ));

        let changed_alert =
            report_with_alert("Endpoint exploration still failed", Some(alert_started_at));
        assert!(site_explorer_health_report_needs_update(
            Some(&previous_alert),
            &changed_alert,
        ));

        assert!(site_explorer_health_report_needs_update(
            Some(&previous_alert),
            &empty,
        ));
    }
}
