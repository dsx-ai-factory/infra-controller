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
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbide_firmware::FirmwareConfigSnapshot;
use carbide_redfish::libredfish::conv::IntoModel;
use carbide_secrets::credentials::CredentialManager;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::machine::MachineType;
use chrono::Utc;
use component_manager::MachineInfoProvider;
use config::SiteExplorerConfig;
use db::explored_endpoints::EndpointReportNotCurrent;
use db::{self, ConditionalWrite, DatabaseError, Transaction, machine};
use futures_util::stream::FuturesUnordered;
use futures_util::{StreamExt, TryFutureExt};
use itertools::Itertools;
use mac_address::MacAddress;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::errors::OperatorError;
use model::expected_entity::ExpectedEntity;
use model::machine::MachineInterfaceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_interface::InterfaceType;
use model::rack_type::RackProfileConfig;
use model::resource_pool::common::CommonPools;
use model::site_explorer::{
    BlueFieldOperatingMode, EndpointExplorationError, EndpointExplorationReport, EndpointType,
    ExplorationTier, ExploredDpu, ExploredEndpoint, ExploredManagedHost,
    HostPrimaryInterfaceSelection, MachineExpectation, PowerState, PreingestionState,
    SiteExplorerLastRun,
};
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use version_compare::Cmp;
mod attester_inventory;
mod endpoint_explorer;
pub use endpoint_explorer::{AuthenticatedBmc, EndpointExplorer};
mod endpoint_exploration_service;
pub use endpoint_exploration_service::{
    EndpointExplorationService, EndpointExplorationServiceError,
};
mod credentials;
mod endpoint_lock;
mod metrics;
pub mod suppression;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub use metrics::{SiteExplorationMetrics, site_explorer_latency_histogram_view};
mod bmc_endpoint_explorer;
mod redfish;
pub use bmc_endpoint_explorer::{AuthenticatedBmcClient, BmcEndpointExplorer};
pub use redfish::{BmcAccess, EstablishedBmc, ProxiedPools};
mod boot_interfaces;
use boot_interfaces::NicBootInterfaces;
mod host_dpu_matches;
use host_dpu_matches::{HostDpuMatches, MatchedDpu};
mod boot_order_tracker;
use boot_order_tracker::BootOrderTracker;
mod inventory_creator;
pub use inventory_creator::{
    Config as InventoryCreatorConfig, MachineCreator, PowerShelfCreator, SwitchCreator,
};
use inventory_creator::{ConfiguredIngestionCandidate, InventoryCreator};
mod machine_location_reconciler;
pub use machine_location_reconciler::MachineLocationReconciler;
mod endpoint_exploration_report_ext;
mod explored_endpoint_ext;
use explored_endpoint_ext::ExploredEndpointExt;
pub use explored_endpoint_ext::get_sys_image_version;
pub mod explored_endpoint_index;
mod managed_host;
use carbide_uuid::rack::RackId;
use db::ObjectColumnFilter;
use db::work_lock_manager::WorkLockManagerHandle;
pub use managed_host::is_endpoint_in_managed_host;
use model::expected_machine::{
    ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedMachine, ExpectedMachineRequest,
    HostDpuPolicy,
};
use model::firmware::FirmwareComponentType;
use model::network_segment::NetworkSegmentType;
use model::rack::Rack;
pub mod config;
pub mod errors;
use std::sync::atomic::AtomicBool;

use errors::{SiteExplorerError, SiteExplorerResult};

use self::metrics::{
    BmcResetFinished, BmcResetMethod, BmcResetStatus, BmcResetTimestampPersistenceFailed,
    DpuMigrationSignal, PairingBlockerReason, SiteExplorerIterationFinished,
    exploration_error_to_metric_label,
};
use crate::config::SiteExplorerExploreMode;
use crate::explored_endpoint_index::ExploredEndpointIndex;

/// Return whether a HostInband row can be treated as a Redfish endpoint.
///
/// Host data interfaces also DHCP on HostInband, so only BMC rows are normally
/// eligible. The ExpectedMachine BMC identity is the compatibility exception:
/// a historical DPU OS declaration may have left its anonymous row typed
/// `Data`, and successful exploration will reconcile that row back to `Bmc`.
fn should_scan_host_inband_interface_for_redfish(
    interface: &MachineInterfaceSnapshot,
    expected_host_bmc_macs: &HashSet<MacAddress>,
) -> bool {
    interface.interface_type == InterfaceType::Bmc
        || (interface.machine_id.is_none()
            && expected_host_bmc_macs.contains(&interface.mac_address))
}

/// Decide whether an Underlay interface row is a Redfish exploration target:
/// every BMC row and every anonymous row. DHCP types an undeclared BMC lease
/// `Data`, and nothing retypes it before reconciliation or ingestion. Adopted
/// Data rows and MACs declared as DPU OS or NVOS ports stay out.
fn should_scan_underlay_interface_for_redfish(
    interface: &MachineInterfaceSnapshot,
    expected_non_redfish_interface_macs: &HashSet<MacAddress>,
) -> bool {
    !expected_non_redfish_interface_macs.contains(&interface.mac_address)
        && (interface.interface_type == InterfaceType::Bmc || interface.machine_id.is_none())
}

/// Build an endpoint explorer over the authenticated BMC client supplied by
/// the application composition root.
pub fn new_bmc_explorer(
    bmc_client: Arc<AuthenticatedBmcClient>,
    rotate_switch_nvos_credentials: Arc<AtomicBool>,
    mode: SiteExplorerExploreMode,
    database_connection: PgPool,
) -> Arc<BmcEndpointExplorer> {
    BmcEndpointExplorer::new(
        bmc_client,
        rotate_switch_nvos_credentials,
        mode,
        Some(database_connection),
    )
    .into()
}

pub fn enrich_endpoint_exploration_report(
    report: &mut EndpointExplorationReport,
    fw_config_snapshot: &FirmwareConfigSnapshot,
) {
    if !report.is_power_shelf() {
        if !report.is_switch()
            && let Err(error) = report.generate_machine_id(false)
        {
            tracing::error!(%error, "Can not generate MachineId for explored endpoint");
        }
        report.model = report.model();
        if let Some(fw_info) = fw_config_snapshot.find_fw_info_for_host_report(report) {
            let components_without_version = report.parse_versions(&fw_info);
            if !components_without_version.is_empty() {
                tracing::debug!(
                    components = ?components_without_version,
                    "Can not find firmware version for component(s)"
                );
            }
        } else {
            // It's possible that we knew about this host type before but do not now, so make sure we
            // do not keep stale data.
            report.versions = HashMap::default();
            tracing::debug!(
                vendor = ?report.vendor,
                model = ?report.model(),
                "Can not find firmware info"
            );
        }
        // Persist cached position fields so older versions can read reports after rollback.
        // TODO: Remove this compatibility call after release 2.5.
        #[allow(deprecated)]
        report.parse_position_info();
    } else {
        tracing::info!("Generating PowerShelfId for power shelf");
        if let Err(error) = report.generate_power_shelf_id() {
            tracing::error!(%error, "Can not generate PowerShelfId for explored power shelf endpoint");
        }
        report.versions = HashMap::default();
    }
}

fn topology_firmware_versions(report: &EndpointExplorationReport) -> Option<(&str, &str)> {
    let bmc_version = report
        .versions
        .get(&FirmwareComponentType::Bmc)
        .map(String::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .or_else(|| report.observed_host_bmc_version())?;

    let bios_version = report
        .versions
        .get(&FirmwareComponentType::Uefi)
        .map(String::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .or_else(|| report.system_bios_version())?;

    Some((bmc_version, bios_version))
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
            let rack = db::rack::create_from_expected(&mut *txn, &expected, &config).await?;

            Ok(Some(rack))
        }
        Err(e) => Err(e.into()),
    }
}

fn rms_location_value(value: Option<u32>) -> Result<Option<i32>, u32> {
    value
        .map(|value| i32::try_from(value).map_err(|_| value))
        .transpose()
}

pub struct Endpoint<'a> {
    address: IpAddr,
    iface: &'a MachineInterfaceSnapshot,
    last_explored: Option<&'a ExploredEndpoint>,
    pub(crate) expected: Option<&'a ExpectedEntity>,
    pause_ingestion_and_poweron: bool,
}

/// An explored endpoint and the underlay interface it is probed through.
type Candidate<'a> = (IpAddr, &'a MachineInterfaceSnapshot, &'a ExploredEndpoint);

/// What one iteration probes, in probe order. Operator requests sit outside
/// the `explorations_per_run` budget; the other three tiers share it.
#[derive(Debug, Default)]
struct ExplorationPlan<'a> {
    /// `exploration_requested` endpoints, by address.
    priority: Vec<Candidate<'a>>,
    /// Interfaces with no report yet, oldest interface first.
    unexplored: Vec<(IpAddr, &'a MachineInterfaceSnapshot)>,
    /// Endpoints waiting on their next report: expected BMCs whose first few
    /// probes failed and endpoints preingestion parked with
    /// `waiting_for_explorer_refresh`, oldest report first.
    refresh_waits: Vec<Candidate<'a>>,
    /// Everything else, oldest report first.
    routine: Vec<Candidate<'a>>,
    unexplored_candidates: usize,
    refresh_wait_candidates: usize,
    routine_candidates: usize,
}

impl ExplorationPlan<'_> {
    fn selected_total(&self) -> usize {
        self.priority.len() + self.unexplored.len() + self.refresh_waits.len() + self.routine.len()
    }

    /// Candidate and selected counts per tier, as recorded on the metrics.
    fn counts(&self) -> [(&'static str, usize); 9] {
        [
            ("priority_update_candidates", self.priority.len()),
            ("unexplored_candidates", self.unexplored_candidates),
            ("refresh_wait_candidates", self.refresh_wait_candidates),
            ("routine_update_candidates", self.routine_candidates),
            ("selected_priority_updates", self.priority.len()),
            ("selected_unexplored", self.unexplored.len()),
            ("selected_refresh_waits", self.refresh_waits.len()),
            ("selected_routine_updates", self.routine.len()),
            ("selected_total", self.selected_total()),
        ]
    }
}

/// Shares of the `explorations_per_run` budget for the three tiers inside it.
const UNEXPLORED_SHARE_PERCENT: usize = 70;
const REFRESH_WAIT_SHARE_PERCENT: usize = 20;
const ROUTINE_SHARE_PERCENT: usize = 10;
const _: () =
    assert!(UNEXPLORED_SHARE_PERCENT + REFRESH_WAIT_SHARE_PERCENT + ROUTINE_SHARE_PERCENT == 100);

/// Endpoints per tier of the `explorations_per_run` budget: candidates or slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TierCounts {
    unexplored: usize,
    refresh_waits: usize,
    routine: usize,
}

impl TierCounts {
    fn total(&self) -> usize {
        self.unexplored + self.refresh_waits + self.routine
    }
}

/// Sorts the candidates into tiers and spends `budget` on them. A candidate
/// without a successful report is a refresh wait only for its first few
/// failed probes and only when its interface MAC is in `expected_bmc_macs`,
/// the BMC MACs of the expected machines, switches and power shelves; an
/// undeclared one, or one that keeps failing, stays routine. Each tier gets its
/// share of the budget (the `*_SHARE_PERCENT` constants) oldest first:
/// unexplored endpoints by interface creation, the other two by report time.
/// Slots a tier cannot fill go to the other tiers in the same order,
/// unexplored first, so no slot stays unused while any tier has candidates.
/// From a budget of ten up every tier holds its share, so bring-up cannot
/// starve parked BMCs and parked BMCs cannot starve the routine refresh. Below
/// that the smaller shares round to zero and those tiers live on what the
/// earlier tiers leave.
fn plan_explorations<'a>(
    candidates: Vec<Candidate<'a>>,
    mut unexplored: Vec<(IpAddr, &'a MachineInterfaceSnapshot)>,
    budget: usize,
    expected_bmc_macs: &HashSet<MacAddress>,
) -> ExplorationPlan<'a> {
    let mut priority = Vec::new();
    let mut refresh_waits = Vec::new();
    let mut routine = Vec::new();
    for candidate in candidates {
        let (_, iface, endpoint) = candidate;
        match endpoint.exploration_tier(expected_bmc_macs.contains(&iface.mac_address)) {
            ExplorationTier::Priority => priority.push(candidate),
            ExplorationTier::RefreshWait => refresh_waits.push(candidate),
            ExplorationTier::Routine => routine.push(candidate),
        }
    }
    let oldest_report_first =
        |(address, _, endpoint): &Candidate<'a>| (endpoint.report_version.timestamp(), *address);
    priority.sort_by_key(|(address, _, _)| *address);
    unexplored.sort_by_key(|(address, iface)| (iface.created, *address));
    refresh_waits.sort_by_key(oldest_report_first);
    routine.sort_by_key(oldest_report_first);

    let unexplored_candidates = unexplored.len();
    let refresh_wait_candidates = refresh_waits.len();
    let routine_candidates = routine.len();
    let take = allocate(
        budget,
        TierCounts {
            unexplored: unexplored_candidates,
            refresh_waits: refresh_wait_candidates,
            routine: routine_candidates,
        },
    );
    unexplored.truncate(take.unexplored);
    refresh_waits.truncate(take.refresh_waits);
    routine.truncate(take.routine);
    ExplorationPlan {
        priority,
        unexplored,
        refresh_waits,
        routine,
        unexplored_candidates,
        refresh_wait_candidates,
        routine_candidates,
    }
}

/// How many of `budget` each tier gets: its share, capped by its candidates,
/// then the unused rest tier by tier in order. Rounding leftovers go to the
/// first tier, so a budget below ten still reaches the tiers in order.
fn allocate(budget: usize, candidates: TierCounts) -> TierCounts {
    let budget = budget.min(candidates.total());
    let mut quotas = TierCounts {
        unexplored: budget * UNEXPLORED_SHARE_PERCENT / 100,
        refresh_waits: budget * REFRESH_WAIT_SHARE_PERCENT / 100,
        routine: budget * ROUTINE_SHARE_PERCENT / 100,
    };
    quotas.unexplored += budget - quotas.total();
    let mut take = TierCounts {
        unexplored: quotas.unexplored.min(candidates.unexplored),
        refresh_waits: quotas.refresh_waits.min(candidates.refresh_waits),
        routine: quotas.routine.min(candidates.routine),
    };
    let mut spare = budget - take.total();
    for (taken, wanted) in [
        (&mut take.unexplored, candidates.unexplored),
        (&mut take.refresh_waits, candidates.refresh_waits),
        (&mut take.routine, candidates.routine),
    ] {
        let more = (wanted - *taken).min(spare);
        *taken += more;
        spare -= more;
    }
    take
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

/// Selection details needed only between host identification and machine
/// creation. The durable explored host report intentionally remains unchanged;
/// this decision belongs to the desired boot interface record instead.
pub(crate) struct IdentifiedManagedHost {
    /// Host/DPU association that machine creation persists.
    pub(crate) explored_host: ExploredManagedHost,
    /// Discovery report consumed while constructing the machine records.
    pub(crate) report: EndpointExplorationReport,
    /// Report decision to attribute only if its MAC becomes the durable target.
    pub(crate) primary_interface_selection: Option<HostPrimaryInterfaceSelection>,
}

/// A host and its matching decisions, collected before DPU configuration.
struct IngestionCandidate<'a> {
    endpoint: ExploredEndpoint,
    /// `None` when `Ignore` skips DPU matching and configuration.
    dpu_matches: Option<HostDpuMatches<'a>>,
    host_dpu_policy: HostDpuPolicy,
    declared_primary: Option<MacAddress>,
}

/// When Site Explorer last physically reset a BMC, tracked per reset method.
/// Backs [`RecentBmcResets`].
#[derive(Clone, Copy, Default)]
struct BmcResetTimes {
    redfish: Option<chrono::DateTime<Utc>>,
    ipmitool: Option<chrono::DateTime<Utc>>,
}

/// In-memory record of recent physical BMC resets, keyed by endpoint address.
///
/// The reset rate limit is normally driven by the persisted
/// `last_redfish_bmc_reset` / `last_ipmitool_bmc_reset` timestamps. A physical
/// BMC reset can succeed even when writing its timestamp to the database fails;
/// relying on the persisted value alone, the next Site Explorer iteration would
/// read a stale timestamp and re-issue the reset well inside the rate-limit
/// window. Recording the reset here lets the throttle use the more recent of the
/// persisted and in-memory times, keeping it honest for the life of the process
/// -- across a restart the persisted timestamp (or a fresh start) takes over.
///
/// Bounded by the endpoint set: each address holds a single entry that is
/// overwritten in place on each reset.
#[derive(Clone, Default)]
struct RecentBmcResets {
    inner: Arc<Mutex<HashMap<IpAddr, BmcResetTimes>>>,
}

impl RecentBmcResets {
    fn guard(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, BmcResetTimes>> {
        // A poisoned lock only means another thread panicked while holding it;
        // the map is still consistent, so recover the data instead of panicking.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records that the BMC at `address` was physically reset via Redfish at `at`.
    fn record_redfish(&self, address: IpAddr, at: chrono::DateTime<Utc>) {
        self.guard().entry(address).or_default().redfish = Some(at);
    }

    /// Records that the BMC at `address` was physically reset via IPMI at `at`.
    fn record_ipmitool(&self, address: IpAddr, at: chrono::DateTime<Utc>) {
        self.guard().entry(address).or_default().ipmitool = Some(at);
    }

    /// The in-memory reset times recorded for `address`, if any.
    fn get(&self, address: IpAddr) -> BmcResetTimes {
        self.guard().get(&address).copied().unwrap_or_default()
    }
}

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
    /// Authenticated BMC client for remediation (reset, power, NIC mode, etc.),
    /// supplied independently so BMC operations don't route through the explorer.
    bmc_client: Arc<dyn AuthenticatedBmc>,
    endpoint_exploration_service: Arc<EndpointExplorationService>,
    work_lock_manager_handle: WorkLockManagerHandle,
    inventory_creator: InventoryCreator,
    machine_location_reconciler: MachineLocationReconciler,
    boot_order_tracker: BootOrderTracker,
    /// Backstops the persisted BMC-reset timestamps for the reset rate limit,
    /// so a reset whose timestamp write failed still throttles the next reset.
    recent_bmc_resets: RecentBmcResets,
}

/// State captured once and applied throughout a Site Explorer iteration.
struct SiteExplorerRunContext {
    suppressed_bmc_macs: HashSet<MacAddress>,
    dpf_provisioning_bmc_macs: HashSet<MacAddress>,
}

impl SiteExplorer {
    const ITERATION_WORK_KEY: &'static str = "SiteExplorer::run_single_iteration";
    const SITE_EXPLORER_HEALTH_REPORT_WRITE_BATCH_SIZE: usize = 500;

    #[allow(clippy::too_many_arguments)]
    /// Creates a site explorer.
    ///
    /// When `dpf_enabled_at_site` is true, eligible hosts are marked for DPF-managed ingestion.
    /// Otherwise, hosts use the non-DPF ingestion path.
    pub fn new(
        database_connection: sqlx::PgPool,
        explorer_config: SiteExplorerConfig,
        meter: opentelemetry::metrics::Meter,
        endpoint_exploration_service: Arc<EndpointExplorationService>,
        bmc_client: Arc<dyn AuthenticatedBmc>,
        common_pools: Arc<CommonPools>,
        work_lock_manager_handle: WorkLockManagerHandle,
        rack_profiles: RackProfileConfig,
        machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
        credential_manager: Arc<dyn CredentialManager>,
        dpf_enabled_at_site: bool,
    ) -> Self {
        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, &explorer_config));
        let rack_profiles = Arc::new(rack_profiles);
        let endpoint_explorer = endpoint_exploration_service.endpoint_explorer();

        SiteExplorer {
            machine_location_reconciler: MachineLocationReconciler::new(
                database_connection.clone(),
                rack_profiles.clone(),
                machine_info_provider.clone(),
                credential_manager,
            ),
            inventory_creator: InventoryCreator::new(
                database_connection.clone(),
                (&explorer_config).into(),
                common_pools,
                rack_profiles,
                machine_info_provider,
                dpf_enabled_at_site,
            ),
            database_connection,
            config: explorer_config,
            metric_holder,
            endpoint_explorer,
            bmc_client,
            endpoint_exploration_service,
            work_lock_manager_handle,
            boot_order_tracker: BootOrderTracker::default(),
            recent_bmc_resets: RecentBmcResets::default(),
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
                        tracing::warn!(error = %e, "SiteExplorer error");
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
            SiteExplorerError::InvalidHostMachineId { .. } => "invalid_host_machine_id",
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

    fn emit_iteration_finished(
        metrics: &SiteExplorationMetrics,
        error: Option<&SiteExplorerError>,
    ) {
        let latency = metrics.recording_started_at.elapsed();
        carbide_instrument::emit(match error {
            None => SiteExplorerIterationFinished::Succeeded { latency },
            Some(error) => SiteExplorerIterationFinished::Failed {
                latency,
                error: format!("{error:?}"),
            },
        });
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
                // `run` already records this returned error at `WARN`. Passing
                // `None` keeps `SiteExplorerIterationFinished` metric-only, so
                // expected replica contention does not also produce an
                // `ERROR`.
                Self::emit_iteration_finished(&metrics, None);
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
                explore_site_span.record("otel.status_code", "error");
                // Writing this field will set the span status to error
                // Therefore we only write it on errors
                explore_site_span.record("otel.status_message", format!("{e:?}"));
            }
        }

        Self::record_run_status_metric(&mut metrics, &res);
        self.record_last_run_result(started_at, &metrics, &res)
            .await;
        Self::emit_iteration_finished(&metrics, res.as_ref().err());

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

            new_health_report
                .alerts
                .extend(ep.report.power_state_alert(ep.address));

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
                // using the primary system serial number.
                // If nothing found, try again with chassis
                // serial numbers.
                if !ep
                    .report
                    .primary_system()
                    .is_some_and(|s| s.check_serial_number(expected_sn) || s.check_sku(expected_sn))
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

        // Match other multi-machine health-report writers' row-lock order to
        // prevent PostgreSQL deadlocks when their batches overlap.
        let pending_health_report_updates =
            db::machine::MachineRowLockOrderIter::new(pending_health_report_updates);
        let audit_write_start = Instant::now();
        for health_report_updates in pending_health_report_updates
            .as_slice()
            .chunks(Self::SITE_EXPLORER_HEALTH_REPORT_WRITE_BATCH_SIZE)
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
        let run_context = self.acknowledge_site_explorer_suppressions().await?;
        self.check_preconditions(metrics).await?;

        let update_explored_endpoints_start = Instant::now();
        let (expected_endpoint_index, explored_this_run) = self
            .update_explored_endpoints(metrics, &run_context)
            .await?;
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
        let (explored_dpus, explored_hosts, already_ingested_bmc_ips) = self
            .identify_machines_to_ingest(metrics, &expected_endpoint_index, &run_context)
            .await?;
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
        let nic_boot_interfaces = NicBootInterfaces::new(&explored_hosts);
        // Per-host DPU policy resolution. Precedence:
        //   1. Per-host `ExpectedMachine.dpu_policy` (`Nic` / `Ignore` wins).
        //   2. Site-wide `SiteExplorerConfig.dpu_policy` setting.
        //   3. Otherwise: `HostDpuPolicy::Manage` (the absolute default).
        let site_dpu_policy = self.config.dpu_policy;
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

        let mut candidates = Vec::with_capacity(explored_hosts.len());
        for (_, ep) in explored_hosts {
            let expected_machine = expected_endpoint_index.matched_expected_machine(&ep.address);

            // Resolve the operator-declared DPU policy for this host once;
            // it drives both auto-correction (`update_dpu_mode`
            // below -- operator override wins over BF3 part-number heuristics)
            // and the post-match attach decision (`Nic` / `Ignore` policies
            // emit a bare managed host regardless of what matched).
            let host_dpu_policy = HostDpuPolicy::resolve(
                expected_machine.map(|expected| expected.data.dpu_policy),
                site_dpu_policy,
            );

            // A declared `ExpectedInterface.primary` (when the matched expected
            // machine sets one) wins over the automatic DPU-PF pick, so the
            // explored default names the same NIC the managed store will.
            let declared_primary =
                expected_machine.and_then(|expected| expected.data.declared_primary_mac());

            let dpu_matches = match host_dpu_policy {
                HostDpuPolicy::Ignore => None,
                HostDpuPolicy::Manage | HostDpuPolicy::Nic => Some(HostDpuMatches::new(
                    &ep,
                    &dpu_sn_to_endpoint,
                    expected_machine,
                    host_dpu_policy,
                )),
            };
            candidates.push(IngestionCandidate {
                endpoint: ep,
                dpu_matches,
                host_dpu_policy,
                declared_primary,
            });
        }

        let mut configured_candidates = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let dpus = match candidate.dpu_matches {
                Some(dpu_matches) => {
                    let Some(dpus) = self
                        .configure_host_dpus(
                            &candidate.endpoint,
                            dpu_matches,
                            candidate.host_dpu_policy,
                            metrics,
                        )
                        .await
                    else {
                        continue;
                    };
                    dpus
                }
                // Ignore permits hosts whose installed DPUs cannot be discovered.
                None => Vec::new(),
            };
            configured_candidates.push(ConfiguredIngestionCandidate {
                endpoint: candidate.endpoint,
                dpus,
                host_dpu_policy: candidate.host_dpu_policy,
                declared_primary: candidate.declared_primary,
            });
        }

        let (mut identified_hosts, boot_interfaces) =
            MachineCreator::identify_managed_hosts(configured_candidates, metrics);

        let mut txn = self.txn_begin().await?;
        Self::persist_identified_managed_hosts(&mut txn, &identified_hosts).await?;
        boot_interfaces.persist(&mut txn).await?;
        nic_boot_interfaces.persist(&mut txn).await?;
        txn.commit().await?;

        metrics.record_phase_latency(
            "identify_managed_hosts",
            identify_managed_hosts_start.elapsed(),
        );

        self.inventory_creator
            .create_inventory(
                metrics,
                &mut identified_hosts,
                &expected_endpoint_index,
                &already_ingested_bmc_ips,
                &explored_this_run,
                &run_context.suppressed_bmc_macs,
            )
            .await?;

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
                tracing::info!(
                    removed_retained_boot_interface_count = swept,
                    "Removed expired retained boot interface records"
                );
            }
        }

        let reconcile_machine_locations_start = Instant::now();
        let host_bmc_ips = identified_hosts
            .iter()
            .map(|identified| identified.explored_host.host_bmc_ip)
            .collect::<Vec<_>>();
        if let Err(error) = self
            .machine_location_reconciler
            .reconcile_machine_locations(&host_bmc_ips)
            .await
        {
            tracing::warn!(
                %error,
                "Machine RMS location reconciliation failed; a later Site Explorer run will retry"
            );
        }
        metrics.record_phase_latency(
            "reconcile_machine_locations",
            reconcile_machine_locations_start.elapsed(),
        );

        Ok(identified_hosts
            .into_iter()
            .map(|identified| (identified.explored_host, identified.report))
            .collect())
    }

    /// identify_machines_to_ingest returns two maps and a set.
    /// The first map returned identifies all of the DPUs that site explorer will try to ingest.
    /// The latter identifies all of the hosts the the site explorer will try to ingest.
    /// Both map from machine BMC IP address to the corresponding explored endpoint.
    /// The set holds the BMC IPs that already belong to an ingested machine, so
    /// `create_machines` can skip existing hosts without reading it again.
    async fn identify_machines_to_ingest(
        &self,
        metrics: &mut SiteExplorationMetrics,
        expected_endpoint_index: &ExploredEndpointIndex,
        run_context: &SiteExplorerRunContext,
    ) -> SiteExplorerResult<(
        HashMap<IpAddr, ExploredEndpoint>,
        HashMap<IpAddr, ExploredEndpoint>,
        HashSet<IpAddr>,
    )> {
        let mut txn = self.txn_begin().await?;

        // TODO: We reload the endpoint list even though we just regenerated it
        // Could optimize this by keeping it in memory. But since the manipulations
        // are quite complicated in the previous step, this makes things much easier
        let explored_endpoints =
            db::explored_endpoints::find_all_preingestion_complete(&mut txn).await?;

        // Ingested BMC IPs are read once for the whole loop and for `create_machines`
        // rather than per endpoint. The iteration work lock makes site-explorer the
        // only writer that ingests machines, so the snapshot stays exact until
        // `create_machines` starts creating.
        let already_ingested_bmc_ips =
            db::machine_topology::find_all_ingested_bmc_ips(&mut txn).await?;

        txn.commit().await?;

        let mut explored_dpus = HashMap::new();
        let mut explored_hosts = HashMap::new();
        for ep in explored_endpoints.into_iter() {
            if let Some(bmc_interface) = expected_endpoint_index.underlay_interface(&ep.address)
                && run_context
                    .suppressed_bmc_macs
                    .contains(&bmc_interface.mac_address)
            {
                tracing::info!(
                    bmc_ip_address = %ep.address,
                    bmc_mac_address = %bmc_interface.mac_address,
                    "Skipping ingestion of acknowledged suppressed BMC",
                );
                continue;
            }

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
                if self.can_ingest_dpu_endpoint(metrics, &ep, &already_ingested_bmc_ips)? {
                    explored_dpus.insert(ep.address, ep);
                }
            } else if self
                .can_ingest_host_endpoint(metrics, &ep, &already_ingested_bmc_ips)
                .await?
            {
                explored_hosts.insert(ep.address, ep);
            }
        }

        Ok((explored_dpus, explored_hosts, already_ingested_bmc_ips))
    }

    /// Applies the DPU policy and checks whether host ingestion can proceed.
    ///
    /// Returns `None` on the reset/wait paths, handling rate-limited host resets.
    /// A host ready for ingestion may have an empty DPU list.
    async fn configure_host_dpus(
        &self,
        ep: &ExploredEndpoint,
        matches: HostDpuMatches<'_>,
        host_dpu_policy: HostDpuPolicy,
        metrics: &mut SiteExplorationMetrics,
    ) -> Option<Vec<ExploredDpu>> {
        let HostDpuMatches {
            inventory: dpu_inventory,
            inventory_matches,
            fallback_matches,
            expected_fallback_count,
        } = matches;
        let needs_expected_serial_fallback =
            inventory_matches.is_empty() || dpu_inventory.has_unmatched_reported_dpu;
        let matched_total = inventory_matches.len();

        let inventory_cfg_status = self
            .configure_matched_dpus(ep, inventory_matches, metrics)
            .await;
        let fallback_cfg_status = self
            .configure_matched_dpus(ep, fallback_matches, metrics)
            .await;

        let has_unready_matches = matched_total
            > inventory_cfg_status.running_as_dpu.len() + inventory_cfg_status.running_as_nic_total;

        let host_reported_dpus_total = dpu_inventory.reported_total;

        let incomplete_dpu_exploration = needs_expected_serial_fallback
            || has_unready_matches
            || inventory_cfg_status.running_as_dpu.is_empty()
            || inventory_cfg_status.running_as_dpu.len()
                != host_reported_dpus_total
                    .saturating_sub(inventory_cfg_status.running_as_nic_total);

        let host_reported_dpus_nic_mode_total =
            inventory_cfg_status.running_as_nic_total + fallback_cfg_status.running_as_nic_total;
        let configuration_requests = inventory_cfg_status.configuration_requests
            + fallback_cfg_status.configuration_requests;

        if configuration_requests > 0 {
            // A queued `set_nic_mode` only takes effect after a host
            // power cycle, so drive one for every vendor --
            // `redfish_powercycle` issues `PowerCycle` and falls back
            // to a cold `ACPowercycle` for vendors that refuse it --
            // throttled by `reset_rate_limit`. A BMC that refuses both
            // surfaces the host as needing a manual power cycle via
            // the pairing-blocker metric.
            let time_since_redfish_powercycle =
                Utc::now().signed_duration_since(ep.last_redfish_powercycle.unwrap_or_default());
            if time_since_redfish_powercycle > self.config.reset_rate_limit {
                tracing::warn!(
                    bmc_ip_address = %ep.address,
                    %time_since_redfish_powercycle,
                    "power cycling host to apply nic mode change for its incorrectly configured DPUs"
                );
                metrics.increment_dpu_migration_signal(DpuMigrationSignal::ResetRequested);

                if let Err(err) = self.redfish_powercycle(ep.address).await {
                    tracing::warn!(
                        bmc_ip_address = %ep.address,
                        error = %err,
                        "site explorer failed to power cycle host to apply DPU mode changes; a manual power cycle may be required"
                    );
                    metrics.increment_host_dpu_pairing_blocker(
                        PairingBlockerReason::ManualPowerCycleRequired,
                    );
                }
            } else {
                // We power-cycled within the rate limit and the
                // DPUs still aren't in the target operating
                // mode -- either the change is mid-flight (the
                // host is booting, a pass or two of normal
                // convergence) or this
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
            return None;
        }

        // Every selected fallback serial must be discovered and ready, whether
        // the DPU is managed or intentionally runs as a NIC.
        if fallback_cfg_status.running_as_dpu.len() + fallback_cfg_status.running_as_nic_total
            != expected_fallback_count
        {
            return None;
        }

        // The site explorer should only create a managed host after exploring all of the DPUs attached to the host.
        // If a host reports that it has two DPUs, the site explorer must wait until **both** DPUs have made the DHCP request.
        // If only one of the two DPUs have made the DHCP request, the site explorer must wait until it has explored the latter DPU's BMC
        // (ensuring that the second DPU has also made the DHCP request).
        if incomplete_dpu_exploration && fallback_cfg_status.running_as_dpu.is_empty() {
            // Net DPUs still expected to pair: reported DPU minus those
            // confirmed to be running as plain NICs.
            let expected_managed_dpus_total =
                host_reported_dpus_total.saturating_sub(host_reported_dpus_nic_mode_total);
            if expected_managed_dpus_total > 0 || has_unready_matches {
                if expected_managed_dpus_total > 0 {
                    tracing::warn!(
                        bmc_ip_address = %ep.address,
                        discovered_dpu_count = inventory_cfg_status.running_as_dpu.len(),
                        expected_managed_dpu_count = expected_managed_dpus_total,
                        all_dpus_configured_properly_in_host = configuration_requests == 0,
                        "cannot identify managed host because the site explorer has not discovered all attached DPUs"
                    );
                }

                return None;
            } else if host_dpu_policy.expects_managed_dpus() {
                // No DPUs remain available for management, and the
                // effective policy is `Manage` --
                // i.e. neither per-host `ExpectedMachine.dpu_policy`
                // nor site-wide `[site_explorer] dpu_policy` declared
                // this host as zero-DPU. We expect DPUs but found none --
                // probably a misconfiguration or a DPU-discovery
                // bug. Skip ingestion this cycle; site-explorer
                // will retry on the next iteration, giving the
                // operator a chance to either fix the host, select
                // `HostDpuPolicy::Nic` for installed BlueFields
                // that should run as NICs, or select
                // `HostDpuPolicy::Ignore` for a host with no usable DPU
                // hardware.
                //
                // `Ignore` hosts are handled before this method is called;
                // `Nic` hosts can return an empty DPU list.
                tracing::warn!(
                    bmc_ip_address = %ep.address,
                    exploration_report = ?ep,
                    ?host_dpu_policy,
                    "cannot identify managed host: site explorer found no DPUs available for management while the effective DPU policy is `Manage`; use `dpu_policy = \"nic\"` for installed BlueFields that should run as NICs, or `dpu_policy = \"ignore\"` for a host with no usable DPU hardware",
                );
                metrics
                    .increment_host_dpu_pairing_blocker(PairingBlockerReason::NoDpuReportedByHost);
                return None;
            }
        }

        // Expected serials replace inventory DPUs only after a ready DPU match.
        // An empty or all-NIC fallback keeps the inventory DPU list.
        if fallback_cfg_status.running_as_dpu.is_empty() {
            Some(inventory_cfg_status.running_as_dpu)
        } else {
            Some(fallback_cfg_status.running_as_dpu)
        }
    }

    /// Applies the selected target modes and records each matched DPU's outcome.
    async fn configure_matched_dpus(
        &self,
        host_ep: &ExploredEndpoint,
        matched_dpus: Vec<MatchedDpu<'_>>,
        metrics: &mut SiteExplorationMetrics,
    ) -> DpuConfigStatus {
        let mut cfg_status = DpuConfigStatus::new();
        for matched_dpu in matched_dpus {
            // Only a successful mode-change request marks the host for reset.
            let mode_check = Some(match matched_dpu.target_operating_mode {
                Some(target) => self.update_dpu_mode(&matched_dpu, target, metrics).await,
                None => Ok(true),
            });

            cfg_status.record_matched_dpu(&matched_dpu, host_ep, mode_check);
        }

        cfg_status
    }

    async fn persist_identified_managed_hosts(
        txn: &mut Transaction<'_>,
        managed_hosts: &[IdentifiedManagedHost],
    ) -> SiteExplorerResult<()> {
        db::explored_managed_host::update(
            txn,
            managed_hosts
                .iter()
                .map(|identified| &identified.explored_host)
                .collect::<Vec<_>>()
                .as_slice(),
        )
        .await?;

        Ok(())
    }

    /// Sends a mode-setting request when the DPU reports a different current mode.
    ///
    /// Returns `false` after a successful request, `true` when no request is
    /// sent (including an unknown current mode), or an error if the request fails.
    async fn update_dpu_mode(
        &self,
        matched_dpu: &MatchedDpu<'_>,
        target_operating_mode: BlueFieldOperatingMode,
        metrics: &mut SiteExplorationMetrics,
    ) -> SiteExplorerResult<bool> {
        let dpu_ep = matched_dpu.endpoint;
        let observed_mode = dpu_ep.report.bluefield_operating_mode();
        let Some(observed) = observed_mode.filter(|mode| *mode != target_operating_mode) else {
            if observed_mode.is_none() {
                tracing::warn!(
                    bmc_ip_address = %dpu_ep.address,
                    dpu_report = ?dpu_ep.report,
                    "Site explorer cannot determine this DPU's mode"
                );
            }
            return Ok(true);
        };

        tracing::warn!(
            bmc_ip_address = %dpu_ep.address,
            %observed,
            ?target_operating_mode,
            "site explorer found a DPU with a mode that does not match the target; will try to reconfigure"
        );
        metrics.increment_dpu_migration_signal(DpuMigrationSignal::ModeMismatchFound);
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(dpu_ep.address, bmc_target_port);

        let interface = self.find_machine_interface_for_ip(dpu_ep.address).await?;

        self.bmc_client
            .set_nic_mode(bmc_target_addr, &interface, target_operating_mode)
            .await
            .map_err(|err| SiteExplorerError::EndpointExplorationError {
                action: "set_nic_mode",
                err,
            })?;
        metrics.increment_dpu_migration_signal(DpuMigrationSignal::SetNicModeIssued);
        Ok(false)
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

    /// Loads the run's suppressed and DPF-provisioning BMC sets, acknowledging
    /// pending suppressions that require endpoint guards. DPF provisioning does
    /// not wait for acknowledgement.
    async fn acknowledge_site_explorer_suppressions(
        &self,
    ) -> SiteExplorerResult<SiteExplorerRunContext> {
        let suppressions = db::bmc_suppression::find_all_by_subsystem(
            &self.database_connection,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?;
        let suppressed_bmc_macs = suppressions
            .iter()
            .map(|suppression| suppression.bmc_mac_address)
            .collect();

        let dpf_provisioning_bmc_macs = suppressions
            .iter()
            .filter(|s| s.source == model::bmc_suppression::BmcSuppressionSource::DpfProvisioning)
            .map(|s| s.bmc_mac_address)
            .collect();
        let mut guards = Vec::new();
        let mut acknowledgements = Vec::new();
        for suppression in suppressions
            .iter()
            .filter(|suppression| {
                suppression.acknowledged_at.is_none()
                    && suppression.source
                        != model::bmc_suppression::BmcSuppressionSource::DpfProvisioning
            })
            .unique_by(|suppression| suppression.bmc_mac_address)
        {
            let bmc_ips = db::machine_interface::lookup_bmc_ip_by_mac_address(
                &self.database_connection,
                suppression.bmc_mac_address,
            )
            .await?;
            let mut suppression_guards = Vec::new();
            let mut all_endpoints_available = true;
            for bmc_ip in bmc_ips {
                let Some(guard) = self.endpoint_exploration_service.try_claim_endpoint(bmc_ip)
                else {
                    all_endpoints_available = false;
                    break;
                };
                suppression_guards.push(guard);
            }
            if all_endpoints_available {
                guards.extend(suppression_guards);
                acknowledgements.push(suppression.bmc_mac_address);
            }
        }

        if !acknowledgements.is_empty() {
            let mut txn = self.txn_begin().await?;
            db::bmc_suppression::acknowledge_unacknowledged(
                &mut txn,
                &acknowledgements,
                BmcSuppressionSubsystem::SiteExplorer,
            )
            .await?;
            txn.commit().await?;
        }
        drop(guards);

        Ok(SiteExplorerRunContext {
            suppressed_bmc_macs,
            dpf_provisioning_bmc_macs,
        })
    }

    /// Returns the endpoint index and the addresses explored in this run.
    async fn update_explored_endpoints(
        &self,
        metrics: &mut SiteExplorationMetrics,
        run_context: &SiteExplorerRunContext,
    ) -> SiteExplorerResult<(ExploredEndpointIndex, HashSet<IpAddr>)> {
        let load_start = Instant::now();
        let mut txn = self.txn_begin().await?;

        let underlay_segments =
            db::network_segment::list_segment_ids(&mut txn, Some(NetworkSegmentType::Underlay))
                .await?;
        // A BMC that shares the host network is preallocated on a HostInband segment so scan
        // those too, but only for BMC interfaces. The host in-band NIC also DHCPs onto a
        // HostInband segment with no machine_id and must never be treated as a Redfish endpoint.
        let host_inband_segments =
            db::network_segment::list_segment_ids(&mut txn, Some(NetworkSegmentType::HostInband))
                .await?;
        let explored_endpoints = db::explored_endpoints::find_all(txn.as_pgconn()).await?;
        let expected_switches = db::expected_switch::find_all(&mut txn).await?;
        let expected_machines = db::expected_machine::find_all(&mut txn).await?;
        let expected_power_shelves = db::expected_power_shelf::find_all(&mut txn).await?;

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

        // Record Expected Machine metrics and create initial Fixed
        // reservations. DHCP inserts Retained allocations as `Static`; later
        // inventory passes leave existing allocation types unchanged.
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

            if run_context
                .suppressed_bmc_macs
                .contains(&expected_machine.bmc_mac_address)
            {
                tracing::info!(
                    bmc_mac_address = %expected_machine.bmc_mac_address,
                    "Skipping network reservation preallocation for suppressed expected machine",
                );
                continue;
            }

            // Compatibility columns may be the only Host BMC configuration on
            // rows created before the nested role existed. Resolve that one
            // declaration in memory, then skip any stored entry at the BMC
            // identity so Site Explorer applies its policy exactly once.
            let host_bmc = expected_machine.effective_host_bmc();
            try_apply_expected_interface(
                &self.database_connection,
                expected_machine,
                &host_bmc,
                self.config.retained_boot_interface_window,
            )
            .await;
            for nic in
                expected_machine.data.interfaces.iter().filter(|interface| {
                    interface.mac_address != Some(expected_machine.bmc_mac_address)
                })
            {
                try_apply_expected_interface(
                    &self.database_connection,
                    expected_machine,
                    nic,
                    self.config.retained_boot_interface_window,
                )
                .await;
            }
        }

        for expected_switch in &expected_switches {
            if run_context
                .suppressed_bmc_macs
                .contains(&expected_switch.bmc_mac_address)
            {
                tracing::info!(
                    bmc_mac_address = %expected_switch.bmc_mac_address,
                    "Skipping network reservation preallocation for suppressed expected switch",
                );
                continue;
            }

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
            // exactly one `nvos_interfaces` entry (the single wired NVOS port).
            // Re-check here, with the failure case being a
            // log and skip for this pass.
            if let Some(nvos_ip) = expected_switch.nvos_ip_address {
                match expected_switch.nvos_mac_addresses().as_slice() {
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
                            bmc_mac_address = %expected_switch.bmc_mac_address,
                            nvos_ip_address = %nvos_ip,
                            nvos_mac_address_count = macs.len(),
                            "Skipping NVOS preallocation: nvos_ip_address requires exactly one nvos_interfaces entry"
                        );
                    }
                }
            }
        }

        for expected_power_shelf in &expected_power_shelves {
            if run_context
                .suppressed_bmc_macs
                .contains(&expected_power_shelf.bmc_mac_address)
            {
                tracing::info!(
                    bmc_mac_address = %expected_power_shelf.bmc_mac_address,
                    "Skipping network reservation preallocation for suppressed expected power shelf",
                );
                continue;
            }

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
        let expected_host_bmc_macs = expected_machines
            .iter()
            .map(|machine| machine.bmc_mac_address)
            .collect::<HashSet<_>>();
        let expected_bmc_macs = expected_machines
            .iter()
            .map(|machine| machine.bmc_mac_address)
            // A DpuBmc interface is the Redfish endpoint of a DPU declared on
            // its host.
            .chain(
                expected_machines
                    .iter()
                    .flat_map(|machine| &machine.data.interfaces)
                    .filter(|interface| {
                        interface.role == model::expected_machine::ExpectedInterfaceRole::DpuBmc
                    })
                    .filter_map(|interface| interface.mac_address),
            )
            .chain(
                expected_switches
                    .iter()
                    .map(|switch| switch.bmc_mac_address),
            )
            .chain(
                expected_power_shelves
                    .iter()
                    .map(|power_shelf| power_shelf.bmc_mac_address),
            )
            .collect::<HashSet<_>>();
        let expected_non_redfish_interface_macs = expected_machines
            .iter()
            .flat_map(|machine| &machine.data.interfaces)
            .filter(|interface| {
                // A DpuOs interface terminates on the DPU operating system,
                // not its management controller. Redfish is exposed through
                // the separate DpuBmc interface.
                interface.role == model::expected_machine::ExpectedInterfaceRole::DpuOs
            })
            .filter_map(|interface| interface.mac_address)
            .chain(
                expected_switches
                    .iter()
                    .flat_map(|switch| switch.nvos_mac_addresses()),
            )
            // An explicit BMC identity wins if legacy data assigns the same
            // MAC address to both a BMC and an OS interface.
            .filter(|mac_address| !expected_bmc_macs.contains(mac_address))
            .collect::<HashSet<_>>();

        // Tenant and Admin segments are never Redfish discovery networks. The
        // Underlay may contain DPU OS and switch NVOS data interfaces, so keep
        // their explicit MACs out of the scan unless a MAC is also an expected
        // BMC identity. Host remains the compatibility default for legacy
        // ExpectedMachine entries, and DPU BMC interfaces remain eligible.
        //
        // Load interfaces after allocation reconciliation so this iteration
        // also sees newly-created fixed reservations.
        let interface_load_start = Instant::now();
        let mut txn = self.txn_begin().await?;
        let interfaces = db::machine_interface::find_all(&mut txn).await?;
        txn.commit().await?;
        metrics.record_phase_latency(
            "update_explored_endpoints_interface_load",
            interface_load_start.elapsed(),
        );

        let build_index_start = Instant::now();
        let scannable_interfaces: Vec<MachineInterfaceSnapshot> = interfaces
            .into_iter()
            .filter(|iface| {
                let underlay = underlay_segments.contains(&iface.segment_id)
                    && should_scan_underlay_interface_for_redfish(
                        iface,
                        &expected_non_redfish_interface_macs,
                    );
                // Host data interfaces also DHCP on HostInband. Only scan BMC
                // rows plus an anonymous row at an ExpectedMachine BMC identity,
                // which covers historical rows that were left typed as Data.
                let host_inband = host_inband_segments.contains(&iface.segment_id)
                    && should_scan_host_inband_interface_for_redfish(
                        iface,
                        &expected_host_bmc_macs,
                    );
                underlay || host_inband
            })
            .collect();
        let scannable_interface_count = scannable_interfaces.len();
        metrics.record_update_explored_endpoints_count(
            "scannable_interfaces",
            scannable_interface_count,
        );

        // Start an index of all scannable interfaces, expected machines, expected power shelves, and expected switches.
        let index = ExploredEndpointIndex::builder(explored_endpoints, scannable_interfaces)
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
        let mut candidates: Vec<Candidate<'_>> =
            Vec::with_capacity(index.explored_endpoints().len());
        for (address, endpoint) in index.explored_endpoints() {
            match index.underlay_interface(address) {
                Some(iface) => {
                    if run_context.suppressed_bmc_macs.contains(&iface.mac_address) {
                        if run_context
                            .dpf_provisioning_bmc_macs
                            .contains(&iface.mac_address)
                        {
                            suppression::record_deferred(
                                *address,
                                suppression::ExplorationRequest::Periodic,
                            );
                        }
                        tracing::info!(bmc_ip_address = %address, bmc_mac_address = %iface.mac_address, "Skipping exploration of suppressed BMC");
                        continue;
                    }
                    candidates.push((*address, iface, endpoint));
                }
                None => {
                    if endpoint.report.is_power_shelf() {
                        tracing::info!(bmc_ip_address = %address, "Retaining power shelf endpoint with no underlay interface; power shelves are sourced from their expected static IP");
                    } else {
                        delete_endpoints.push(*address)
                    }
                }
            }
        }
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
        let unexplored_endpoints = index
            .get_unexplored_endpoints()
            .into_iter()
            .filter(|(address, iface)| {
                if run_context
                    .suppressed_bmc_macs
                    .contains(&iface.mac_address)
                {
                    if run_context.dpf_provisioning_bmc_macs.contains(&iface.mac_address) {
                        suppression::record_deferred(*address, suppression::ExplorationRequest::Periodic);
                    }
                    tracing::info!(bmc_ip_address = %address, bmc_mac_address = %iface.mac_address, "Skipping exploration of suppressed BMC");
                    return false;
                }
                true
            })
            .collect::<Vec<_>>();
        let plan = plan_explorations(
            candidates,
            unexplored_endpoints,
            self.config.explorations_per_run as usize,
            &expected_bmc_macs,
        );
        for (kind, count) in plan.counts() {
            metrics.record_update_explored_endpoints_count(kind, count);
        }

        let mut explore_endpoint_data = Vec::with_capacity(plan.selected_total());
        let ExplorationPlan {
            priority,
            unexplored,
            refresh_waits,
            routine,
            ..
        } = plan;
        let explored = |(address, iface, endpoint)| Endpoint {
            address,
            iface,
            last_explored: Some(endpoint),
            pause_ingestion_and_poweron: endpoint.pause_ingestion_and_poweron,
            expected: index.matched_expected(&address),
        };
        explore_endpoint_data.extend(priority.into_iter().map(explored));
        for (address, iface) in unexplored {
            let pause_ingestion_and_poweron =
                pause_ingestion_and_poweron(index.expected(), &iface.mac_address);
            explore_endpoint_data.push(Endpoint {
                address,
                iface,
                last_explored: None,
                pause_ingestion_and_poweron,
                expected: index.matched_expected(&address),
            });
        }
        explore_endpoint_data.extend(refresh_waits.into_iter().map(explored));
        explore_endpoint_data.extend(routine.into_iter().map(explored));
        metrics.record_phase_latency("update_explored_endpoints_plan", plan_start.elapsed());

        let task_set = FuturesUnordered::new();
        let concurrency_limiter = Arc::new(tokio::sync::Semaphore::new(
            self.config.concurrent_explorations as usize,
        ));

        // Record the difference between the total expected machine count and
        // the number of expected machines we've actually "seen."
        metrics.endpoint_explorations_expected_machines_missing_overall_count =
            expected_count - index.all_matched_expected_machines().len();
        let fw_config_snapshot = Arc::new(
            self.endpoint_exploration_service
                .firmware_config_snapshot()
                .await?,
        );

        let probe_start = Instant::now();
        for endpoint in explore_endpoint_data.into_iter() {
            let endpoint_exploration_service = self.endpoint_exploration_service.clone();
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

                    let Some(probe) = endpoint_exploration_service
                        .try_explore_endpoint(
                            bmc_target_addr,
                            endpoint.iface,
                            endpoint.expected,
                            endpoint
                                .last_explored
                                .and_then(|e| e.report.last_exploration_error.as_ref()),
                            endpoint
                                .last_explored
                                .and_then(ExploredEndpoint::boot_interface_target)
                                .map(Into::into),
                        )
                        .await
                    else {
                        tracing::info!(
                            bmc_ip_address = %endpoint.address,
                            "Skipping periodic endpoint exploration; endpoint already in progress"
                        );
                        return Ok(None);
                    };
                    steps.redfish_explore = probe.redfish_explore_duration;
                    let mut result = probe.result;

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
                                machine_state,
                                "Failed to explore endpoint"
                            );
                        } else {
                            tracing::info!(
                                endpoint = %bmc_target_addr,
                                error = %error,
                                error_code = %schema.error_code,
                                mitigation = %schema.mitigation_for_log(),
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
        let mut exploration_results = task_set
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<SiteExplorerResult<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        // Any transaction touching multiple explored_endpoints needs to sort them the same way to
        // avoid deadlocks: sort by IP.
        exploration_results.sort_by_key(|result| result.endpoint.address);
        let explored_this_run: HashSet<IpAddr> = exploration_results
            .iter()
            .map(|result| result.endpoint.address)
            .collect();
        metrics.record_phase_latency("update_explored_endpoints_probe", probe_start.elapsed());
        for EndpointExplorationTaskResult { steps, .. } in &exploration_results {
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
        // Commit the whole batch before dispatching remediation. A later write
        // failure must roll back earlier reports and request clearing, since it
        // also discards the remediation collected for them.
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
        } in exploration_results
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

            // Keep topology writes ahead of endpoint writes to match machine deletion's
            // lock order. A savepoint lets a rejected report undo only its own topology.
            let mut txn = db::Transaction::begin_inner(txn.as_pgconn()).await?;

            // Update possible stale machine versions
            // Configured firmware versions remain the preferred source. Hosts
            // without firmware-management configuration, such as Lenovo GB300
            // RMS systems, can use versions observed directly from Redfish.
            if let Ok(report) = &result
                && let Some((bmc_version, bios_version)) = topology_firmware_versions(report)
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
                        bios_version,
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
                                    bmc_ip_address = %address,
                                    exploration_report = ?report,
                                    "Initial exploration of endpoint"
                                );
                            }
                            let report_write = db::explored_endpoints::try_update(
                                address,
                                old_version,
                                &report,
                                false,
                                &mut txn,
                            )
                            .await?;
                            endpoint_report_update_attempts += 1;
                            match report_write {
                                ConditionalWrite::Applied(()) => {
                                    attester_inventory::record(&report, &mut txn).await?;
                                }
                                ConditionalWrite::NotApplied(EndpointReportNotCurrent) => {
                                    // Skip transient remediation: it would use
                                    // the rejected report's stale endpoint snapshot.
                                    txn.rollback().await?;
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            // If an endpoint can not be explored we don't delete the known information, since it's
                            // still helpful. The failure might just be intermittent.
                            let error_write =
                                db::explored_endpoints::try_update_last_exploration_error(
                                    address,
                                    old_version,
                                    &e,
                                    exploration_duration,
                                    &mut txn,
                                )
                                .await?;
                            match error_write {
                                ConditionalWrite::Applied(()) => {}
                                ConditionalWrite::NotApplied(EndpointReportNotCurrent) => {
                                    // The endpoint disappeared or its report changed
                                    // while we were probing. Don't remediate an error
                                    // the database didn't accept.
                                    redfish_error = None;
                                }
                            }
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
                                bmc_ip_address = %address,
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
                            attester_inventory::record(&report, &mut txn).await?;
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
                        .is_some_and(|v| matches!(v, ExpectedEntity::PowerShelf(_)));

                    if !self.config.create_machines.load(Ordering::Relaxed)
                        || power_shelf_manual_ingestion
                    {
                        // We're using manual ingestion, making preingestion updates risky.  Go ahead and skip them.
                        db::explored_endpoints::set_preingestion_complete(address, &mut txn).await?
                    }
                }
            }

            txn.commit().await?;

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

        Ok((index, explored_this_run))
    }

    /// Records one terminal BMC-reset attempt. `bmc_reset_count` remains the
    /// latest run's successful-reset gauge, while `BmcResetFinished` counts
    /// every attempt and owns its terminal log record. Returns whether the
    /// reset succeeded so Redfish success can stop the IPMI fallback.
    fn record_bmc_reset_outcome(
        result: SiteExplorerResult<()>,
        method: BmcResetMethod,
        address: IpAddr,
        metrics: &mut SiteExplorationMetrics,
    ) -> bool {
        let (status, error, succeeded) = match result {
            Ok(()) => {
                metrics.bmc_reset_count += 1;
                (BmcResetStatus::Succeeded, String::new(), true)
            }
            Err(err) => (BmcResetStatus::Failed, err.to_string(), false),
        };

        carbide_instrument::emit(match status {
            BmcResetStatus::Succeeded => BmcResetFinished::Succeeded {
                method,
                address,
                error,
            },
            BmcResetStatus::Failed => BmcResetFinished::Failed {
                method,
                address,
                error,
            },
        });
        succeeded
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
                %endpoint,
                %error,
                "Site explorer will not remediate error because remediation is paused for this endpoint"
            );
            return;
        }

        // If site explorer can't log in, there's nothing we can do.
        if !self.bmc_client.have_credentials(endpoint.iface).await {
            return;
        }

        if !matches!(
            *endpoint.preingestion_state(),
            PreingestionState::Initial | PreingestionState::Complete
        ) {
            tracing::info!(
                %endpoint,
                preingestion_state = ?endpoint.preingestion_state(),
                %error,
                "Site explorer will not remediate error because endpoint is in preingestion state",
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
                        %endpoint,
                        %error,
                        "Site explorer will not remediate error because a managed host has already been created for this endpoint"
                    );
                    return;
                }
            }
            Err(e) => {
                tracing::error!(
                    %endpoint,
                    error = %e,
                    "Failed to determine whether managed host was created"
                );
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
                bmc_ip_address = %endpoint.address,
                "Site Explorer found a host that isn't on. Turning it on now.",
            );

            match self
                .redfish_power(endpoint, libredfish::SystemPowerControl::On)
                .await
            {
                Ok(()) => return,
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "Site Explorer failed to power on host through Redfish"
                    );
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
        // Throttle on the more recent of the persisted reset timestamp and any
        // in-memory marker. A physical reset whose timestamp write failed still
        // left an in-memory time, and it must keep the reset throttled rather
        // than letting a stale persisted timestamp trigger a too-soon re-reset.
        let recent_bmc_resets = self.recent_bmc_resets.get(endpoint.address);
        let time_since_redfish_bmc_reset = start.signed_duration_since(
            endpoint
                .last_explored
                .and_then(|e| e.last_redfish_bmc_reset)
                .max(recent_bmc_resets.redfish)
                .unwrap_or_default(),
        );
        let time_since_ipmitool_bmc_reset = start.signed_duration_since(
            endpoint
                .last_explored
                .and_then(|e| e.last_ipmitool_bmc_reset)
                .max(recent_bmc_resets.ipmitool)
                .unwrap_or_default(),
        );

        if time_since_redfish_reboot.num_minutes() < min_time_since_last_action_mins
            || time_since_redfish_bmc_reset.num_minutes() < min_time_since_last_action_mins
            || time_since_ipmitool_bmc_reset.num_minutes() < min_time_since_last_action_mins
        {
            tracing::info!(
                %endpoint,
                %error,
                %time_since_redfish_reboot,
                %time_since_redfish_bmc_reset,
                %time_since_ipmitool_bmc_reset,
                "waiting to remediate error"
            );
            return;
        }

        tracing::info!(
            %endpoint,
            %error,
            %time_since_redfish_reboot,
            %time_since_redfish_bmc_reset,
            %time_since_ipmitool_bmc_reset,
            "Site explorer captured an error"
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
                        bmc_ip_address = %endpoint.address,
                        error = %err,
                        "Site Explorer failed to reboot"
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
                        bmc_ip_address = %endpoint.address,
                        error = %e,
                        "Site Explorer failed to clear nvram"
                    )
                }
            }
        }

        if time_since_redfish_bmc_reset > reset_rate_limit
            && Self::record_bmc_reset_outcome(
                self.redfish_reset_bmc(endpoint).await,
                BmcResetMethod::Redfish,
                endpoint.address,
                metrics,
            )
        {
            return;
        }

        if time_since_ipmitool_bmc_reset > reset_rate_limit {
            Self::record_bmc_reset_outcome(
                self.ipmitool_reset_bmc(endpoint).await,
                BmcResetMethod::Ipmitool,
                endpoint.address,
                metrics,
            );
        }
    }

    /// Records the rate-limit timestamp for the most recent IPMI BMC reset.
    ///
    /// Kept separate from `ipmitool_reset_bmc` so that a failure to persist this
    /// bookkeeping timestamp can be handled without being mistaken for a failure
    /// of the physical reset itself.
    async fn persist_last_ipmitool_bmc_reset(&self, address: IpAddr) -> SiteExplorerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::explored_endpoints::set_last_ipmitool_bmc_reset(address, &mut txn).await?;
        txn.commit().await?;
        Ok(())
    }

    pub async fn ipmitool_reset_bmc(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            bmc_ip_address = %endpoint.address,
            "SiteExplorer is initiating a cold BMC reset through IPMI"
        );

        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
        match self
            .bmc_client
            .ipmitool_reset_bmc(bmc_target_addr, endpoint.iface)
            .await
        {
            Ok(_) => {
                // The BMC was physically reset. Record it in memory first so the
                // reset rate limit honors it even if persisting the timestamp
                // below fails -- otherwise the next iteration would read a stale
                // persisted timestamp and re-issue the reset. Persisting is
                // bookkeeping: a failure there must not be reported as a reset
                // failure, or the successful reset would be miscounted.
                self.recent_bmc_resets
                    .record_ipmitool(endpoint.address, Utc::now());
                if let Err(e) = self.persist_last_ipmitool_bmc_reset(endpoint.address).await {
                    carbide_instrument::emit(BmcResetTimestampPersistenceFailed {
                        method: BmcResetMethod::Ipmitool,
                        bmc_ip_address: endpoint.address,
                        error: e.to_string(),
                    });
                }
                Ok(())
            }
            Err(e) => Err(SiteExplorerError::internal(format!(
                "site-explorer failed to cold reset bmc through ipmitool {}: {:#?}",
                endpoint.address, e
            ))),
        }
    }

    /// Records the rate-limit timestamp for the most recent Redfish BMC reset.
    ///
    /// Kept separate from `redfish_reset_bmc` so that a failure to persist this
    /// bookkeeping timestamp can be handled without being mistaken for a failure
    /// of the physical reset itself.
    async fn persist_last_redfish_bmc_reset(&self, address: IpAddr) -> SiteExplorerResult<()> {
        let mut txn = self.txn_begin().await?;
        db::explored_endpoints::set_last_redfish_bmc_reset(address, &mut txn).await?;
        txn.commit().await?;
        Ok(())
    }

    pub async fn redfish_reset_bmc(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            bmc_ip_address = %endpoint.address,
            "SiteExplorer is initiating a BMC reset through Redfish"
        );
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);
        match self
            .bmc_client
            .redfish_reset_bmc(bmc_target_addr, endpoint.iface, None)
            .await
        {
            Ok(_) => {
                // The BMC was physically reset. Record it in memory first so the
                // reset rate limit honors it even if persisting the timestamp
                // below fails -- otherwise the next iteration would read a stale
                // persisted timestamp and re-issue the reset. Persisting is
                // bookkeeping: a failure there must not be reported as a reset
                // failure, or the successful reset would be miscounted and the
                // dispatch would fall through to a redundant second reset via IPMI.
                self.recent_bmc_resets
                    .record_redfish(endpoint.address, Utc::now());
                if let Err(e) = self.persist_last_redfish_bmc_reset(endpoint.address).await {
                    carbide_instrument::emit(BmcResetTimestampPersistenceFailed {
                        method: BmcResetMethod::Redfish,
                        bmc_ip_address: endpoint.address,
                        error: e.to_string(),
                    });
                }
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

        self.bmc_client
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

        self.bmc_client
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
            .bmc_client
            .is_viking(bmc_target_addr, endpoint.iface)
            .await
        {
            Ok(is_viking) => is_viking,
            Err(e) => {
                tracing::warn!(
                    bmc_ip_address = %endpoint.address,
                    error = %e,
                    "could not retrieve vendor"
                );
                false
            }
        }
    }
    pub async fn clear_nvram(&self, endpoint: &Endpoint<'_>) -> SiteExplorerResult<()> {
        tracing::info!(
            bmc_ip_address = %endpoint.address,
            "Site explorer is clearing NVRAM through Redfish"
        );
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(endpoint.address, bmc_target_port);

        self.bmc_client
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
    ///
    /// `already_ingested_bmc_ips` is the caller's snapshot of ingested BMC IPs, so
    /// this decision costs no database round trip per endpoint.
    fn can_ingest_dpu_endpoint(
        &self,
        metrics: &mut SiteExplorationMetrics,
        dpu_endpoint: &ExploredEndpoint,
        already_ingested_bmc_ips: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<bool> {
        if already_ingested_bmc_ips.contains(&dpu_endpoint.address) {
            // this dpu has already been ingested
            return Ok(true);
        }

        match dpu_endpoint.report.bluefield_operating_mode() {
            Some(BlueFieldOperatingMode::Nic) => {
                // DPU's in NIC mode do not have full redfish functionality,
                // for example, we will not be able to retrieve the base GUID
                // from the redfish response. Skip the next check because the DPUs
                // in NIC mode will not expose a pf0 interface to the host.
                tracing::info!(
                    bmc_ip_address = %dpu_endpoint.address,
                    "Site explorer found an uningested DPU in NIC mode"
                );
                return Ok(true);
            }
            Some(BlueFieldOperatingMode::Dpu) => {}
            None if dpu_endpoint.report.dpu_pairing_serial_number().is_some() => {
                tracing::warn!(
                    bmc_ip_address = %dpu_endpoint.address,
                    "Site explorer found an uningested DPU without a Redfish DPU/NIC mode; continuing because it has a host-pairing serial"
                );
            }
            None => {
                tracing::error!(
                    bmc_ip_address = %dpu_endpoint.address,
                    "Site explorer found an uningested DPU without being able to determine if it is in NIC mode"
                );
                metrics.increment_host_dpu_pairing_blocker(PairingBlockerReason::DpuNicModeUnknown);
                return Ok(false);
            }
        }

        // This is a BlueField that should be pairable as a managed DPU. BF4 may
        // not report mode, so host pairing and the PF MAC check decide whether
        // it can continue.
        match dpu_endpoint.find_host_pf_mac_address() {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::error!(
                    %error,
                    bmc_ip_address = %dpu_endpoint.address,
                    "Site explorer found an uningested DPU: failed to find the MAC address of the pf0 interface that the DPU exposes to the host"
                );
                metrics.increment_host_dpu_pairing_blocker(PairingBlockerReason::DpuPf0MacMissing);
                Ok(false)
            }
        }
    }

    async fn redfish_power_control(
        &self,
        bmc_ip_address: IpAddr,
        action: libredfish::SystemPowerControl,
    ) -> SiteExplorerResult<()> {
        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(bmc_ip_address, bmc_target_port);

        let interface = self.find_machine_interface_for_ip(bmc_ip_address).await?;

        self.bmc_client
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
    ///
    /// `already_ingested_bmc_ips` is the caller's snapshot of ingested BMC IPs, so
    /// this decision costs no database round trip per endpoint.
    async fn can_ingest_host_endpoint(
        &self,
        metrics: &mut SiteExplorationMetrics,
        host_endpoint: &ExploredEndpoint,
        already_ingested_bmc_ips: &HashSet<IpAddr>,
    ) -> SiteExplorerResult<bool> {
        if already_ingested_bmc_ips.contains(&host_endpoint.address) {
            // this host has already been ingested
            return Ok(true);
        }

        let bmc_target_port = self.config.override_target_port.unwrap_or(443);
        let bmc_target_addr = SocketAddr::new(host_endpoint.address, bmc_target_port);
        let Some(system) = host_endpoint.report.systems.first() else {
            tracing::warn!(
                bmc_ip_address = %host_endpoint.address,
                "Site Explorer could not find the system report for a host",
            );
            metrics
                .increment_host_dpu_pairing_blocker(PairingBlockerReason::HostSystemReportMissing);
            return Ok(false);
        };

        // if we are explicitly forbidden from powering on in the expected_machines,
        // then don't do it
        if host_endpoint.pause_ingestion_and_poweron {
            tracing::warn!(
                bmc_ip_address = %host_endpoint.address,
                "Host is configured to pause on ingestion"
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
                        .bmc_client
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
                    bmc_ip_address = %host_endpoint.address,
                    "Site Explorer found an uningested host that is off, but remediation is paused — skipping power-on",
                );
            } else if fresh_power_state.is_some() {
                tracing::warn!(
                    bmc_ip_address = %host_endpoint.address,
                    power_state = ?effective_power_state,
                    "Site Explorer found an uningested host that isn't on"
                );

                if let Some(interface) = interface.as_ref() {
                    self.bmc_client
                        .redfish_power_control(
                            bmc_target_addr,
                            interface,
                            libredfish::SystemPowerControl::On,
                        )
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                bmc_ip_address = %host_endpoint.address,
                                error = %err,
                                "Site Explorer failed to turn on host through redfish"
                            )
                        })
                        .ok();
                }
            }
        }

        if host_endpoint.report.vendor.unwrap_or_default().is_nvidia() {
            let Some(manager) = host_endpoint.report.managers.first() else {
                tracing::warn!(
                    bmc_ip_address = %host_endpoint.address,
                    "Site Explorer could not find the manager report for an NVIDIA host",
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
                                        bmc_ip_address = %host_endpoint.address,
                                        %current_cpldmb_0_version,
                                        %expected_cpldmb_0_version,
                                        "Site Explorer found a Viking whose CPLDMB_0 version does not match the expected version. A DC power cycle may be needed",
                                    );
                                    metrics.increment_host_dpu_pairing_blocker(
                                        PairingBlockerReason::VikingCpldVersionIssue,
                                    );
                                    return Ok(false);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    bmc_ip_address = %host_endpoint.address,
                                    %current_cpldmb_0_version,
                                    %expected_cpldmb_0_version,
                                    error = ?e,
                                    "Site Explorer found a Viking with a CPLDMB_0 version and could not compare it to the current CPLDMB_0 version",
                                );
                                metrics.increment_host_dpu_pairing_blocker(
                                    PairingBlockerReason::VikingCpldVersionIssue,
                                );
                                return Ok(false);
                            }
                        }
                    } else {
                        tracing::warn!(
                            bmc_ip_address = %host_endpoint.address,
                            "Site Explorer could not find the CPLDMB_0 inventory for a Viking",
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
                bmc_ip_address = %host_endpoint.address,
                system_report = ?system.attributes,
                "Site Explorer found an uningested Lenovo without infinite boot enabled"
            );

            let interface = self
                .find_machine_interface_for_ip(bmc_target_addr.ip())
                .await?;

            self.bmc_client
                .machine_setup(bmc_target_addr, &interface, None)
                .await
                .inspect_err(|err| {
                    tracing::error!(
                        bmc_ip_address = %host_endpoint.address,
                        error = %err,
                        "Site explorer failed to run machine setup against Lenovo"
                    )
                })
                .ok();

            self.bmc_client
                .redfish_power_control(
                    bmc_target_addr,
                    &interface,
                    libredfish::SystemPowerControl::ForceRestart,
                )
                .await
                .inspect_err(|err| {
                    tracing::error!(
                        bmc_ip_address = %host_endpoint.address,
                        error = %err,
                        "Site explorer failed to restart Lenovo after running machine setup"
                    )
                })
                .ok();

            ingest_host = false;
        }

        Ok(ingest_host)
    }
}

/// `try_preallocate_one` reconciles one generic fixed-address reservation in
/// its own transaction.
///
/// Site Explorer uses this for switch NVOS and switch/power-shelf BMC
/// configuration. ExpectedMachine interfaces use
/// [`try_apply_expected_interface`] because their role and segment guard also
/// need to be applied. Database operations are idempotent, and a failure is
/// logged without stopping the wider reconciliation pass.
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
                %error,
                mac_address = %mac,
                ip_address = %ip,
                kind,
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
                    %error,
                    mac_address = %mac,
                    ip_address = %ip,
                    kind,
                    "Site-explorer preallocation: commit failed"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                mac_address = %mac,
                ip_address = %ip,
                kind,
                "Site-explorer preallocation skipped"
            );
        }
    }
}

/// `try_apply_expected_interface` applies the allocation policy for one
/// configured or compatibility-derived expected interface.
///
/// Fixed reservations apply only before that family's first stateful
/// allocation. Dynamic and Retained allocations are created by DHCP, so
/// neither requires reconciliation here.
///
/// Each interface gets its own transaction so one invalid reservation cannot
/// stop Site Explorer from processing the remaining expected inventory.
///
/// The captured `expected_machine` must identify a stored row. Its declaration
/// is revalidated under a lock held through the address write and commit.
/// Deleted or changed declarations are skipped; the next inventory pass reads
/// the new configuration.
pub async fn try_apply_expected_interface(
    pool: &PgPool,
    expected_machine: &ExpectedMachine,
    expected_interface: &ExpectedInterface,
    retained_window: Option<chrono::Duration>,
) {
    // Cerebro-only declarations are stored until name-to-MAC resolution is implemented.
    let Some(mac_address) = expected_interface.mac_address else {
        return;
    };
    if expected_interface.resolved_ip_allocation() != ExpectedInterfaceIpAllocation::Fixed {
        return;
    }
    let mut txn = match db::Transaction::begin(pool).await {
        Ok(txn) => txn,
        Err(error) => {
            tracing::warn!(
                %error,
                mac_address = %mac_address,
                "Site-explorer expected-interface allocation: txn_begin failed"
            );
            return;
        }
    };

    // The inventory snapshot was read in an earlier transaction. Keep this
    // lock through allocation so an edit or deletion cannot commit between
    // validating the declaration and writing a `Static` address.
    let current = match db::expected_machine::find_for_update(
        txn.as_pgconn(),
        &ExpectedMachineRequest {
            id: expected_machine.id,
            bmc_mac_address: None,
        },
    )
    .await
    {
        Ok(Some(current)) => current,
        Ok(None) => {
            txn.rollback_or_log("expected machine deleted before allocation")
                .await;
            return;
        }
        Err(error) => {
            tracing::warn!(
                %error,
                expected_machine_id = ?expected_machine.id,
                mac_address = %mac_address,
                "Site-explorer expected-interface allocation: configuration lookup failed"
            );
            txn.rollback_or_log("expected interface allocation lookup failed")
                .await;
            return;
        }
    };

    let declaration_matches = if mac_address == current.bmc_mac_address {
        current.effective_host_bmc() == *expected_interface
    } else {
        current.data.interfaces.contains(expected_interface)
    };
    // Replace-all can reuse an ID for a different BMC MAC. That must not
    // authorize a declaration captured for the previous owner.
    if current.bmc_mac_address != expected_machine.bmc_mac_address || !declaration_matches {
        tracing::debug!(
            expected_machine_id = ?expected_machine.id,
            mac_address = %mac_address,
            "Site-explorer expected-interface allocation: owner or declaration changed, skipping"
        );
        txn.rollback_or_log("expected owner or interface declaration changed before allocation")
            .await;
        return;
    }

    let result = db::machine_interface::preallocate_expected_machine_interface(
        txn.as_pgconn(),
        expected_interface,
        retained_window,
    )
    .await;

    match result {
        Ok(()) => {
            if let Err(error) = txn.commit().await {
                tracing::warn!(
                    %error,
                    mac_address = %mac_address,
                    "Site-explorer expected-interface allocation: commit failed"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                mac_address = %mac_address,
                "Site-explorer expected-interface allocation skipped"
            );
        }
    }
}

/// Whether a discovered DPU BMC is reporting that it's running as a plain NIC.
fn is_dpu_in_nic_mode(dpu_ep: &ExploredEndpoint, host_ep: &ExploredEndpoint) -> bool {
    let nic_mode = dpu_ep
        .report
        .bluefield_operating_mode()
        .is_some_and(|m| m == BlueFieldOperatingMode::Nic);
    if nic_mode {
        tracing::info!(
            dpu_bmc_ip_address = %dpu_ep.address,
            host_bmc_ip_address = %host_ep.address,
            "discovered bluefield in NIC mode attached to host"
        );
    }
    nic_mode
}

/// Status of matched DPUs after applying target operating modes.
#[derive(Debug)]
struct DpuConfigStatus {
    /// Matched DPUs confirmed running as a plain NIC, excluded from management.
    running_as_nic_total: usize,
    /// Successful mode-change requests that require a host power cycle.
    configuration_requests: usize,
    /// DPUs running in DPU mode and configured correctly, attached to the host.
    running_as_dpu: Vec<ExploredDpu>,
}

impl DpuConfigStatus {
    fn new() -> Self {
        Self {
            running_as_nic_total: 0,
            configuration_requests: 0,
            running_as_dpu: Vec::new(),
        }
    }

    /// Records a matched DPU after its mode check or configuration attempt.
    fn record_matched_dpu(
        &mut self,
        matched_dpu: &MatchedDpu<'_>,
        host_ep: &ExploredEndpoint,
        mode_check: Option<SiteExplorerResult<bool>>,
    ) {
        match classify_matched_dpu(matched_dpu, host_ep, mode_check) {
            DiscoveredDpu::RunningAsDpu(dpu) => self.running_as_dpu.push(dpu),
            DiscoveredDpu::RunningAsNic => self.running_as_nic_total += 1,
            DiscoveredDpu::NeedsReconfig => self.configuration_requests += 1,
            DiscoveredDpu::ModeCheckFailed(err) => {
                matched_dpu.warn_mode_check_failed(&err);
            }
        }
    }
}

/// Status of a discovered DPU (one whose serial matched an explored DPU BMC)
/// relative to a host, as determined by [`classify_matched_dpu`].
enum DiscoveredDpu {
    /// Running in DPU mode and configured correctly -- the caller attaches it.
    RunningAsDpu(ExploredDpu),
    /// A DPU running as a plain NIC -- counted, but not a managed DPU.
    RunningAsNic,
    /// Mode didn't match the target; `set_nic_mode` was issued successfully.
    /// The host needs a power cycle (handled downstream) before
    /// this DPU re-reports in the corrected mode, so we can't pair it this cycle.
    NeedsReconfig,
    /// The DPU's mode couldn't be checked (Redfish error); skip it this cycle.
    ModeCheckFailed(SiteExplorerError),
}

/// Classify a discovered DPU against a host.
///
/// The only IO (`set_nic_mode`) happens in the caller, which passes its result in as
/// `mode_check` (`None` when the caller deliberately skipped the mode check).
/// Keeping the decision here makes it unit-testable without a Redfish mock.
fn classify_matched_dpu(
    matched_dpu: &MatchedDpu<'_>,
    host_ep: &ExploredEndpoint,
    mode_check: Option<SiteExplorerResult<bool>>,
) -> DiscoveredDpu {
    match mode_check {
        Some(Ok(false)) => return DiscoveredDpu::NeedsReconfig,
        Some(Err(err)) => return DiscoveredDpu::ModeCheckFailed(err),
        // Mode already correct, or the caller skipped the mode check.
        Some(Ok(true)) | None => {}
    }

    let dpu_ep = matched_dpu.endpoint;
    // We do not want to attach DPUs running as NICs as "managed" DPUs.
    if is_dpu_in_nic_mode(dpu_ep, host_ep) {
        return DiscoveredDpu::RunningAsNic;
    }

    DiscoveredDpu::RunningAsDpu(ExploredDpu {
        bmc_ip: dpu_ep.address,
        host_pf_mac_address: dpu_ep.get_host_pf_mac_address(),
        host_chassis_id: None,
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
    use carbide_test_support::{Check, check_values, value_scenarios};
    use config_version::ConfigVersion;
    use model::site_explorer::{
        ComputerSystem, InitialBmcResetPhase, Inventory, PreingestionState, Service,
    };

    use super::*;
    use crate::host_dpu_matches::matched_pcie_dpu;
    use crate::test_support::{bf2_dpu, explored_endpoint};

    #[test]
    fn enrichment_serializes_rack_position_for_older_versions() {
        let mut report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "HGX_Baseboard_0".into(),
                processors: Some(vec![model::site_explorer::Processor {
                    id: "GPU_0".into(),
                    model: None,
                    physical_slot_number: Some(26),
                    compute_tray_index: Some(16),
                }]),
                ..Default::default()
            }],
            chassis: vec![model::site_explorer::Chassis {
                topology_id: Some(7),
                revision_id: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        };
        let firmware = carbide_firmware::FirmwareConfig::default().create_snapshot();
        enrich_endpoint_exploration_report(&mut report, &firmware);

        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["PhysicalSlotNumber"], 26);
        assert_eq!(json["ComputeTrayIndex"], 16);
        assert_eq!(json["TopologyId"], 7);
        assert_eq!(json["RevisionId"], 2);
    }

    fn observed_firmware_report(
        bmc_version: Option<&str>,
        bios_version: Option<&str>,
    ) -> EndpointExplorationReport {
        EndpointExplorationReport {
            service: vec![Service {
                id: "FirmwareInventory".to_string(),
                inventories: bmc_version
                    .map(|version| Inventory {
                        id: "BMC".to_string(),
                        version: Some(version.to_string()),
                        ..Default::default()
                    })
                    .into_iter()
                    .collect(),
            }],
            systems: vec![ComputerSystem {
                id: "System_0".to_string(),
                bios_version: bios_version.map(str::to_string),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn topology_firmware_versions_uses_observed_values_without_configuration() {
        let report = observed_firmware_report(Some("1.0.0"), Some("GBHC01A_01.05.0"));

        assert_eq!(
            topology_firmware_versions(&report),
            Some(("1.0.0", "GBHC01A_01.05.0")),
            "topology persistence should use directly observed BMC and BIOS versions"
        );
        assert!(
            report.versions.is_empty(),
            "topology fallback must not populate the config-shaped versions map"
        );
    }

    #[test]
    fn topology_firmware_versions_prefers_configured_values() {
        let mut report = observed_firmware_report(Some("observed-bmc"), Some("observed-bios"));
        report
            .versions
            .insert(FirmwareComponentType::Bmc, "configured-bmc".to_string());
        report
            .versions
            .insert(FirmwareComponentType::Uefi, "configured-uefi".to_string());

        assert_eq!(
            topology_firmware_versions(&report),
            Some(("configured-bmc", "configured-uefi")),
            "configured firmware versions must remain preferred"
        );
    }

    #[test]
    fn topology_firmware_versions_prefers_configured_values_per_component() {
        let mut report = observed_firmware_report(Some("observed-bmc"), Some("observed-bios"));
        report
            .versions
            .insert(FirmwareComponentType::Bmc, "configured-bmc".to_string());
        assert_eq!(
            topology_firmware_versions(&report),
            Some(("configured-bmc", "observed-bios")),
            "a configured BMC version should be combined with the observed BIOS version"
        );

        let mut report = observed_firmware_report(Some("observed-bmc"), Some("observed-bios"));
        report
            .versions
            .insert(FirmwareComponentType::Uefi, "configured-uefi".to_string());
        assert_eq!(
            topology_firmware_versions(&report),
            Some(("observed-bmc", "configured-uefi")),
            "the observed BMC version should be combined with a configured UEFI version"
        );
    }

    #[test]
    fn topology_firmware_versions_rejects_blank_configured_values() {
        let mut report = observed_firmware_report(Some("observed-bmc"), Some("observed-bios"));
        report
            .versions
            .insert(FirmwareComponentType::Bmc, " \t ".to_string());
        report
            .versions
            .insert(FirmwareComponentType::Uefi, String::new());

        assert_eq!(
            topology_firmware_versions(&report),
            Some(("observed-bmc", "observed-bios")),
            "blank configured values should fall back to observed firmware versions"
        );
    }

    #[test]
    fn topology_firmware_versions_requires_both_components() {
        assert_eq!(
            topology_firmware_versions(&observed_firmware_report(None, Some("GBHC01A_01.05.0"))),
            None,
            "a BIOS version without a BMC version is incomplete"
        );
        assert_eq!(
            topology_firmware_versions(&observed_firmware_report(Some("1.0.0"), None)),
            None,
            "a BMC version without a BIOS version is incomplete"
        );
    }

    #[test]
    fn rms_location_value_preserves_valid_and_absent_values_and_returns_out_of_range_input() {
        check_values(
            [
                Check {
                    scenario: "valid value",
                    input: Some(42),
                    expect: Ok(Some(42)),
                },
                Check {
                    scenario: "absent value",
                    input: None,
                    expect: Ok(None),
                },
                Check {
                    scenario: "first value outside i32",
                    input: Some(i32::MAX as u32 + 1),
                    expect: Err(i32::MAX as u32 + 1),
                },
                Check {
                    scenario: "largest value outside i32",
                    input: Some(u32::MAX),
                    expect: Err(u32::MAX),
                },
            ],
            rms_location_value,
        );
    }

    /// An interface row of `interface_type` at `mac_address`, adopted by a
    /// host machine when `associated`.
    fn scan_candidate(
        interface_type: InterfaceType,
        mac_address: MacAddress,
        associated: bool,
    ) -> MachineInterfaceSnapshot {
        let mut interface = MachineInterfaceSnapshot::mock_with_mac(mac_address);
        interface.interface_type = interface_type;
        interface.machine_id = associated.then(|| {
            carbide_uuid::machine::MachineId::new(
                carbide_uuid::machine::MachineIdSource::Tpm,
                [0; 32],
                MachineType::Host,
            )
        });
        interface
    }

    /// HostInband only scans BMC rows, except for the anonymous compatibility
    /// row at an ExpectedMachine's top-level BMC identity.
    #[test]
    fn host_inband_redfish_scan_allows_historical_bmc_identity() {
        let host_bmc_mac_address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let other_mac_address = "AA:BB:CC:DD:EE:FE".parse().unwrap();
        let expected_host_bmc_macs = HashSet::from([host_bmc_mac_address]);

        check_values(
            [
                Check {
                    scenario: "BMC row",
                    input: (InterfaceType::Bmc, other_mac_address, false),
                    expect: true,
                },
                Check {
                    scenario: "anonymous historical Data row at the Host BMC identity",
                    input: (InterfaceType::Data, host_bmc_mac_address, false),
                    expect: true,
                },
                Check {
                    scenario: "anonymous host Data row",
                    input: (InterfaceType::Data, other_mac_address, false),
                    expect: false,
                },
                Check {
                    scenario: "associated Data row at the Host BMC identity",
                    input: (InterfaceType::Data, host_bmc_mac_address, true),
                    expect: false,
                },
            ],
            |(interface_type, mac_address, associated)| {
                should_scan_host_inband_interface_for_redfish(
                    &scan_candidate(interface_type, mac_address, associated),
                    &expected_host_bmc_macs,
                )
            },
        );
    }

    /// Underlay scans every BMC row and every anonymous row, since an
    /// undeclared BMC lease is an anonymous Data row until its first report.
    /// Adopted Data rows and declared DPU OS or NVOS MACs stay out.
    #[test]
    fn underlay_redfish_scan_keeps_anonymous_rows_and_drops_declared_non_redfish_macs() {
        let nvos_mac_address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let other_mac_address = "AA:BB:CC:DD:EE:FE".parse().unwrap();
        let expected_non_redfish_interface_macs = HashSet::from([nvos_mac_address]);

        check_values(
            [
                Check {
                    scenario: "adopted BMC row",
                    input: (InterfaceType::Bmc, other_mac_address, true),
                    expect: true,
                },
                Check {
                    scenario: "anonymous BMC row",
                    input: (InterfaceType::Bmc, other_mac_address, false),
                    expect: true,
                },
                Check {
                    scenario: "anonymous Data row, an undeclared BMC lease",
                    input: (InterfaceType::Data, other_mac_address, false),
                    expect: true,
                },
                Check {
                    scenario: "adopted Data row",
                    input: (InterfaceType::Data, other_mac_address, true),
                    expect: false,
                },
                Check {
                    scenario: "anonymous Data row at a declared NVOS port",
                    input: (InterfaceType::Data, nvos_mac_address, false),
                    expect: false,
                },
                Check {
                    scenario: "BMC row at a declared NVOS port",
                    input: (InterfaceType::Bmc, nvos_mac_address, false),
                    expect: false,
                },
            ],
            |(interface_type, mac_address, associated)| {
                should_scan_underlay_interface_for_redfish(
                    &scan_candidate(interface_type, mac_address, associated),
                    &expected_non_redfish_interface_macs,
                )
            },
        );
    }

    #[test]
    fn in_memory_marker_keeps_bmc_reset_throttled_when_persist_fails() {
        // Reproduces the reset-throttle computation in `handle_redfish_error`:
        // a BMC was physically reset but persisting its timestamp failed, so the
        // persisted value stays absent/stale. The in-memory marker must make the
        // effective "time since last reset" small enough to keep the reset inside
        // the one-hour rate limit, preventing a redundant re-reset next iteration.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let resets = RecentBmcResets::default();

        // Persist failed (None) and no marker yet: the effective last-reset falls
        // back to the epoch default, so the throttle would wrongly allow an
        // immediate re-reset.
        let persisted: Option<chrono::DateTime<Utc>> = None;
        let without_marker = persisted.max(resets.get(addr).redfish).unwrap_or_default();
        assert!(
            Utc::now().signed_duration_since(without_marker) > chrono::Duration::hours(1),
            "without a marker a stale persisted timestamp should look far in the past"
        );

        // Record the physical reset in memory (as the reset method now does even
        // when the persist fails).
        resets.record_redfish(addr, Utc::now());

        // Now the effective last-reset is the recent in-memory time, so the
        // throttle keeps the window closed.
        let with_marker = persisted.max(resets.get(addr).redfish).unwrap_or_default();
        assert!(
            Utc::now().signed_duration_since(with_marker) < chrono::Duration::hours(1),
            "the in-memory marker should keep the reset throttled"
        );

        // Isolation: the IPMI method and unrelated endpoints are untouched.
        assert_eq!(resets.get(addr).ipmitool, None);
        let other: IpAddr = "10.9.9.9".parse().unwrap();
        assert_eq!(resets.get(other).redfish, None);
    }

    #[test]
    fn effective_bmc_reset_time_takes_the_more_recent_source() {
        // The throttle uses `max()` of the persisted and in-memory times, so
        // whichever reset happened more recently wins.
        let addr: IpAddr = "10.1.2.3".parse().unwrap();
        let two_hours_ago = Utc::now() - chrono::Duration::hours(2);
        let recent = Utc::now();

        // In-memory newer than persisted -> in-memory wins.
        let resets = RecentBmcResets::default();
        resets.record_ipmitool(addr, recent);
        assert_eq!(
            Some(two_hours_ago).max(resets.get(addr).ipmitool),
            Some(recent)
        );

        // Persisted newer than in-memory -> persisted wins.
        let resets = RecentBmcResets::default();
        resets.record_ipmitool(addr, two_hours_ago);
        assert_eq!(Some(recent).max(resets.get(addr).ipmitool), Some(recent));
    }

    /// Each BMC reset result updates the cumulative Event counter and writes a
    /// terminal record. The existing `bmc_reset_count` gauge backing remains
    /// success-only, so a failed attempt cannot change the latest-run value.
    #[test]
    fn bmc_reset_results_update_legacy_gauge_event_and_log() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        struct ResetCase {
            method: BmcResetMethod,
            error: Option<&'static str>,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            method: Option<String>,
            status: Option<String>,
            address: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            succeeded: bool,
            latest_run_success_count: usize,
            attempt_count_delta: f64,
            log_count: usize,
            log: Option<LogObservation>,
        }

        const METRIC_NAME: &str = "carbide_site_explorer_bmc_reset_attempts_total";
        let address: IpAddr = "127.0.0.1".parse().unwrap();

        value_scenarios!(
            run = |ResetCase { method, error }| {
                let method_label = method.to_string();
                let status_label = if error.is_some() { "failed" } else { "succeeded" };
                let result = error.map_or(Ok(()), |error| {
                    Err(SiteExplorerError::internal(error.to_string()))
                });
                let event_metrics = MetricsCapture::start();
                let mut run_metrics = SiteExplorationMetrics::new();
                let mut succeeded = false;
                let logs = capture_logs(|| {
                    succeeded = SiteExplorer::record_bmc_reset_outcome(
                        result,
                        method,
                        address,
                        &mut run_metrics,
                    );
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    method: log.field("method").map(str::to_string),
                    status: log.field("status").map(str::to_string),
                    address: log.field("address").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    succeeded,
                    latest_run_success_count: run_metrics.bmc_reset_count,
                    attempt_count_delta: event_metrics.counter_delta(
                        METRIC_NAME,
                        &[("method", method_label.as_str()), ("status", status_label)],
                    ),
                    log_count: logs.len(),
                    log,
                }
            };
            "IPMI reset" {
                ResetCase {
                    method: BmcResetMethod::Ipmitool,
                    error: None,
                } => Observation {
                    succeeded: true,
                    latest_run_success_count: 1,
                    attempt_count_delta: 1.0,
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::INFO,
                        metadata_name: "site_explorer_bmc_reset_finished".to_string(),
                        message: "Site Explorer reset BMC".to_string(),
                        event_name: Some("site_explorer_bmc_reset_finished".to_string()),
                        metric_name: Some(METRIC_NAME.to_string()),
                        method: Some("ipmitool".to_string()),
                        status: Some("succeeded".to_string()),
                        address: Some("127.0.0.1".to_string()),
                        error: Some(String::new()),
                    }),
                },
                ResetCase {
                    method: BmcResetMethod::Ipmitool,
                    error: Some("simulated ipmitool failure"),
                } => Observation {
                    succeeded: false,
                    latest_run_success_count: 0,
                    attempt_count_delta: 1.0,
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::ERROR,
                        metadata_name: "site_explorer_bmc_reset_finished".to_string(),
                        message: "Site Explorer failed to reset BMC".to_string(),
                        event_name: Some("site_explorer_bmc_reset_finished".to_string()),
                        metric_name: Some(METRIC_NAME.to_string()),
                        method: Some("ipmitool".to_string()),
                        status: Some("failed".to_string()),
                        address: Some("127.0.0.1".to_string()),
                        error: Some("internal error: simulated ipmitool failure".to_string()),
                    }),
                },
            }
            "Redfish reset" {
                ResetCase {
                    method: BmcResetMethod::Redfish,
                    error: None,
                } => Observation {
                    succeeded: true,
                    latest_run_success_count: 1,
                    attempt_count_delta: 1.0,
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::INFO,
                        metadata_name: "site_explorer_bmc_reset_finished".to_string(),
                        message: "Site Explorer reset BMC".to_string(),
                        event_name: Some("site_explorer_bmc_reset_finished".to_string()),
                        metric_name: Some(METRIC_NAME.to_string()),
                        method: Some("redfish".to_string()),
                        status: Some("succeeded".to_string()),
                        address: Some("127.0.0.1".to_string()),
                        error: Some(String::new()),
                    }),
                },
                ResetCase {
                    method: BmcResetMethod::Redfish,
                    error: Some("simulated redfish failure"),
                } => Observation {
                    succeeded: false,
                    latest_run_success_count: 0,
                    attempt_count_delta: 1.0,
                    log_count: 1,
                    log: Some(LogObservation {
                        level: tracing::Level::ERROR,
                        metadata_name: "site_explorer_bmc_reset_finished".to_string(),
                        message: "Site Explorer failed to reset BMC".to_string(),
                        event_name: Some("site_explorer_bmc_reset_finished".to_string()),
                        metric_name: Some(METRIC_NAME.to_string()),
                        method: Some("redfish".to_string()),
                        status: Some("failed".to_string()),
                        address: Some("127.0.0.1".to_string()),
                        error: Some("internal error: simulated redfish failure".to_string()),
                    }),
                },
            }
        );
    }

    /// A timestamp write failure gets its own warning and method-only counter.
    /// The terminal reset Event remains successful because the physical reset
    /// finished before this bookkeeping operation ran.
    #[test]
    fn bmc_reset_timestamp_persistence_failures_are_instrumented_separately() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        #[derive(Debug, PartialEq)]
        struct Observation {
            counter_delta: f64,
            log_count: usize,
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            method: Option<String>,
            bmc_ip_address: Option<String>,
            error: Option<String>,
        }

        const METRIC_NAME: &str =
            "carbide_site_explorer_bmc_reset_timestamp_persistence_failures_total";
        let bmc_ip_address: IpAddr = "127.0.0.1".parse().unwrap();

        value_scenarios!(
            run = |method: BmcResetMethod| {
                let method_label = method.to_string();
                let event_metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    carbide_instrument::emit(BmcResetTimestampPersistenceFailed {
                        method,
                        bmc_ip_address,
                        error: "simulated timestamp write failure".to_string(),
                    });
                });
                let log = logs.first().expect("the persistence Event must log");

                Observation {
                    counter_delta: event_metrics.counter_delta(
                        METRIC_NAME,
                        &[("method", method_label.as_str())],
                    ),
                    log_count: logs.len(),
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    method: log.field("method").map(str::to_string),
                    bmc_ip_address: log.field("bmc_ip_address").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                }
            };
            "timestamp persistence warning" {
                BmcResetMethod::Ipmitool => Observation {
                    counter_delta: 1.0,
                    log_count: 1,
                    level: tracing::Level::WARN,
                    metadata_name:
                        "site_explorer_bmc_reset_timestamp_persistence_failed".to_string(),
                    message:
                        "BMC reset succeeded but recording its rate-limit timestamp failed"
                            .to_string(),
                    event_name: Some(
                        "site_explorer_bmc_reset_timestamp_persistence_failed".to_string(),
                    ),
                    metric_name: Some(METRIC_NAME.to_string()),
                    method: Some("ipmitool".to_string()),
                    bmc_ip_address: Some("127.0.0.1".to_string()),
                    error: Some("simulated timestamp write failure".to_string()),
                },
                BmcResetMethod::Redfish => Observation {
                    counter_delta: 1.0,
                    log_count: 1,
                    level: tracing::Level::WARN,
                    metadata_name:
                        "site_explorer_bmc_reset_timestamp_persistence_failed".to_string(),
                    message:
                        "BMC reset succeeded but recording its rate-limit timestamp failed"
                            .to_string(),
                    event_name: Some(
                        "site_explorer_bmc_reset_timestamp_persistence_failed".to_string(),
                    ),
                    metric_name: Some(METRIC_NAME.to_string()),
                    method: Some("redfish".to_string()),
                    bmc_ip_address: Some("127.0.0.1".to_string()),
                    error: Some("simulated timestamp write failure".to_string()),
                },
            }
        );
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

    /// A candidate for `plan_explorations`: a successfully explored endpoint at
    /// `address` whose report carries `version`, with the interface it is
    /// probed through.
    fn planned(
        address: &str,
        state: PreingestionState,
        waiting: bool,
        requested: bool,
        version: ConfigVersion,
    ) -> (ExploredEndpoint, MachineInterfaceSnapshot) {
        let mut endpoint = explored_endpoint(EndpointExplorationReport::default());
        endpoint.report.endpoint_type = EndpointType::Bmc;
        endpoint.address = address.parse().unwrap();
        endpoint.preingestion_state = state;
        endpoint.waiting_for_explorer_refresh = waiting;
        endpoint.exploration_requested = requested;
        endpoint.report_version = version;
        let iface = MachineInterfaceSnapshot::mock_with_mac(MacAddress::new([2, 0, 0, 0, 0, 1]));
        (endpoint, iface)
    }

    /// A `planned` candidate whose only report is a failed probe, reached
    /// through an interface whose MAC ends in `octet`.
    fn failed_probe(
        address: &str,
        state: PreingestionState,
        waiting: bool,
        octet: u8,
        version: ConfigVersion,
    ) -> (ExploredEndpoint, MachineInterfaceSnapshot) {
        let (mut endpoint, mut iface) = planned(address, state, waiting, false, version);
        endpoint.report =
            EndpointExplorationReport::new_with_error(EndpointExplorationError::Unreachable {
                details: None,
            });
        iface.mac_address = MacAddress::new([2, 0, 0, 0, 0, octet]);
        (endpoint, iface)
    }

    /// A report version whose timestamp is `micros` after the epoch.
    fn version_at(micros: i64) -> ConfigVersion {
        format!("V1-T{micros}").parse().unwrap()
    }

    fn addresses<T>(selected: &[T], address: impl Fn(&T) -> IpAddr) -> Vec<IpAddr> {
        selected.iter().map(address).collect()
    }

    #[test]
    fn allocate_gives_each_tier_its_share_and_passes_unused_slots_on() {
        let counts = |unexplored, refresh_waits, routine| TierCounts {
            unexplored,
            refresh_waits,
            routine,
        };
        // input: (budget, candidates per tier); expect: slots per tier
        check_values(
            [
                Check {
                    scenario: "every tier full: 70/20/10",
                    input: (10, counts(20, 20, 20)),
                    expect: counts(7, 2, 1),
                },
                Check {
                    scenario: "no unexplored: its share flows to refresh waits, then routine",
                    input: (10, counts(0, 5, 20)),
                    expect: counts(0, 5, 5),
                },
                Check {
                    scenario: "no candidates for the later tiers: unexplored takes everything",
                    input: (10, counts(20, 0, 0)),
                    expect: counts(10, 0, 0),
                },
                Check {
                    scenario: "the rounding leftover of a small budget goes to unexplored",
                    input: (1, counts(1, 1, 1)),
                    expect: counts(1, 0, 0),
                },
                Check {
                    scenario: "fewer candidates than budget: all of them",
                    input: (10, counts(1, 1, 1)),
                    expect: counts(1, 1, 1),
                },
            ],
            |(budget, candidates)| allocate(budget, candidates),
        );
    }

    #[test]
    fn plan_explorations_spends_the_budget_by_tier() {
        let parked = || PreingestionState::InitialBMCReset {
            phase: InitialBmcResetPhase::WaitForExplorerRefresh,
        };
        struct Case {
            name: &'static str,
            // (address, state, waiting, requested, report timestamp in micros)
            explored: Vec<(&'static str, PreingestionState, bool, bool, i64)>,
            // (address, interface creation time in seconds)
            unexplored: Vec<(&'static str, i64)>,
            budget: usize,
            priority: Vec<&'static str>,
            selected_unexplored: Vec<&'static str>,
            refresh_waits: Vec<&'static str>,
            routine: Vec<&'static str>,
        }
        let cases = [
            Case {
                name: "an operator request is served outside the budget even when parked",
                explored: vec![
                    ("10.0.0.1", parked(), true, true, 10),
                    ("10.0.0.2", PreingestionState::Complete, false, false, 20),
                ],
                unexplored: vec![],
                budget: 0,
                priority: vec!["10.0.0.1"],
                selected_unexplored: vec![],
                refresh_waits: vec![],
                routine: vec![],
            },
            Case {
                name: "during bring-up unexplored endpoints get 70 percent, oldest interface first, and the other tiers keep theirs",
                explored: vec![
                    ("10.0.0.1", parked(), true, false, 10),
                    ("10.0.0.2", parked(), true, false, 11),
                    ("10.0.0.3", parked(), true, false, 12),
                    ("10.0.0.4", PreingestionState::Complete, false, false, 13),
                    ("10.0.0.5", PreingestionState::Complete, false, false, 14),
                ],
                unexplored: vec![
                    ("10.0.1.1", 300),
                    ("10.0.1.2", 100),
                    ("10.0.1.3", 200),
                    ("10.0.1.4", 400),
                    ("10.0.1.5", 500),
                    ("10.0.1.6", 600),
                    ("10.0.1.7", 700),
                    ("10.0.1.8", 800),
                    ("10.0.1.9", 900),
                ],
                budget: 10,
                priority: vec![],
                selected_unexplored: vec![
                    "10.0.1.2", "10.0.1.3", "10.0.1.1", "10.0.1.4", "10.0.1.5", "10.0.1.6",
                    "10.0.1.7",
                ],
                refresh_waits: vec!["10.0.0.1", "10.0.0.2"],
                routine: vec!["10.0.0.4"],
            },
            Case {
                name: "slots unexplored endpoints cannot use flow to refresh waits, then routine",
                explored: vec![
                    ("10.0.0.1", parked(), true, false, 10),
                    ("10.0.0.2", parked(), true, false, 11),
                    ("10.0.0.3", parked(), true, false, 12),
                    ("10.0.0.4", PreingestionState::Complete, false, false, 13),
                    ("10.0.0.5", PreingestionState::Complete, false, false, 14),
                ],
                unexplored: vec![("10.0.1.1", 100)],
                budget: 5,
                priority: vec![],
                selected_unexplored: vec!["10.0.1.1"],
                refresh_waits: vec!["10.0.0.1", "10.0.0.2", "10.0.0.3"],
                routine: vec!["10.0.0.4"],
            },
            Case {
                name: "a refresh wait is served before an older routine report when the budget is one",
                explored: vec![
                    ("10.0.0.1", PreingestionState::Complete, false, false, 10),
                    ("10.0.0.2", parked(), true, false, 20),
                ],
                unexplored: vec![],
                budget: 1,
                priority: vec![],
                selected_unexplored: vec![],
                refresh_waits: vec!["10.0.0.2"],
                routine: vec![],
            },
            Case {
                name: "refresh waits are served oldest report first, so a failed probe rotates to the back",
                explored: vec![
                    ("10.0.0.1", parked(), true, false, 30),
                    ("10.0.0.2", parked(), true, false, 10),
                    ("10.0.0.3", parked(), true, false, 20),
                ],
                unexplored: vec![],
                budget: 2,
                priority: vec![],
                selected_unexplored: vec![],
                refresh_waits: vec!["10.0.0.2", "10.0.0.3"],
                routine: vec![],
            },
            Case {
                name: "a wave of parked BMCs leaves the routine refresh its 10 percent",
                explored: vec![
                    ("10.0.0.1", parked(), true, false, 10),
                    ("10.0.0.2", parked(), true, false, 11),
                    ("10.0.0.3", parked(), true, false, 12),
                    ("10.0.0.4", parked(), true, false, 13),
                    ("10.0.0.5", parked(), true, false, 14),
                    ("10.0.0.6", parked(), true, false, 15),
                    ("10.0.0.7", parked(), true, false, 16),
                    ("10.0.0.8", parked(), true, false, 17),
                    ("10.0.0.9", parked(), true, false, 18),
                    ("10.0.0.10", parked(), true, false, 19),
                    ("10.0.0.11", PreingestionState::Complete, false, false, 20),
                    ("10.0.0.12", PreingestionState::Complete, false, false, 21),
                ],
                unexplored: vec![],
                budget: 10,
                priority: vec![],
                selected_unexplored: vec![],
                refresh_waits: vec![
                    "10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4", "10.0.0.5", "10.0.0.6",
                    "10.0.0.7", "10.0.0.8", "10.0.0.9",
                ],
                routine: vec!["10.0.0.11"],
            },
            Case {
                name: "only the states that read the next report are refresh waits",
                explored: vec![
                    (
                        "10.0.0.1",
                        PreingestionState::RecheckVersions,
                        true,
                        false,
                        10,
                    ),
                    (
                        "10.0.0.2",
                        PreingestionState::NewFirmwareReportedWait {
                            final_version: "1.0".to_string(),
                            upgrade_type: FirmwareComponentType::Bmc,
                            previous_reset_time: None,
                        },
                        true,
                        false,
                        11,
                    ),
                    (
                        "10.0.0.3",
                        PreingestionState::RecheckVersionsAfterFailure {
                            reason: "job failed".to_string(),
                        },
                        true,
                        false,
                        12,
                    ),
                    (
                        "10.0.0.4",
                        PreingestionState::InitialBMCReset {
                            phase: InitialBmcResetPhase::WaitForBmc,
                        },
                        true,
                        false,
                        13,
                    ),
                    ("10.0.0.5", PreingestionState::Initial, true, false, 14),
                    ("10.0.0.6", PreingestionState::Complete, true, false, 15),
                    (
                        "10.0.0.7",
                        PreingestionState::Failed {
                            reason: "bmc never answered".to_string(),
                        },
                        true,
                        false,
                        16,
                    ),
                ],
                unexplored: vec![],
                budget: 7,
                priority: vec![],
                selected_unexplored: vec![],
                refresh_waits: vec!["10.0.0.1", "10.0.0.2", "10.0.0.3"],
                routine: vec!["10.0.0.4", "10.0.0.5", "10.0.0.6", "10.0.0.7"],
            },
        ];
        for case in cases {
            let explored: Vec<_> = case
                .explored
                .iter()
                .map(|(address, state, waiting, requested, micros)| {
                    planned(
                        address,
                        state.clone(),
                        *waiting,
                        *requested,
                        version_at(*micros),
                    )
                })
                .collect();
            let unexplored_ifaces: Vec<(IpAddr, MachineInterfaceSnapshot)> = case
                .unexplored
                .iter()
                .map(|(address, created)| {
                    let mut iface = MachineInterfaceSnapshot::mock_with_mac(MacAddress::new([
                        2, 0, 0, 0, 0, 2,
                    ]));
                    iface.created = chrono::DateTime::from_timestamp(*created, 0).unwrap();
                    (address.parse().unwrap(), iface)
                })
                .collect();
            let plan = plan_explorations(
                explored
                    .iter()
                    .map(|(endpoint, iface)| (endpoint.address, iface, endpoint))
                    .collect(),
                unexplored_ifaces
                    .iter()
                    .map(|(address, iface)| (*address, iface))
                    .collect(),
                case.budget,
                &HashSet::new(),
            );
            let ips = |list: &[&str]| -> Vec<IpAddr> {
                list.iter().map(|a| a.parse().unwrap()).collect()
            };
            assert_eq!(
                addresses(&plan.priority, |c| c.0),
                ips(&case.priority),
                "{}: priority",
                case.name
            );
            assert_eq!(
                addresses(&plan.unexplored, |c| c.0),
                ips(&case.selected_unexplored),
                "{}: unexplored",
                case.name
            );
            assert_eq!(
                addresses(&plan.refresh_waits, |c| c.0),
                ips(&case.refresh_waits),
                "{}: refresh waits",
                case.name
            );
            assert_eq!(
                addresses(&plan.routine, |c| c.0),
                ips(&case.routine),
                "{}: routine",
                case.name
            );
            assert_eq!(
                plan.selected_total(),
                case.priority.len()
                    + case.selected_unexplored.len()
                    + case.refresh_waits.len()
                    + case.routine.len(),
                "{}: total",
                case.name
            );
        }
    }

    /// An expected endpoint without a successful report is a refresh wait in
    /// any state. It takes its place among the parked BMCs by report time and
    /// ahead of every routine refresh, so a declared BMC whose first probe
    /// failed does not queue behind older complete rows.
    #[test]
    fn plan_explorations_serves_expected_never_explored_endpoints_as_refresh_waits() {
        let complete = planned(
            "10.0.0.1",
            PreingestionState::Complete,
            false,
            false,
            version_at(10),
        );
        let initial = failed_probe(
            "10.0.0.2",
            PreingestionState::Initial,
            true,
            2,
            version_at(30),
        );
        let manual = failed_probe(
            "10.0.0.3",
            PreingestionState::Complete,
            false,
            3,
            version_at(20),
        );
        let parked = planned(
            "10.0.0.4",
            PreingestionState::InitialBMCReset {
                phase: InitialBmcResetPhase::WaitForExplorerRefresh,
            },
            true,
            false,
            version_at(25),
        );
        // Declared, but every quick retry failed too: back to routine.
        let exhausted = failed_probe(
            "10.0.0.5",
            PreingestionState::Initial,
            true,
            5,
            "V4-T5".parse().unwrap(),
        );
        let explored = [complete, initial, manual, parked, exhausted];
        let expected = HashSet::from([
            MacAddress::new([2, 0, 0, 0, 0, 2]),
            MacAddress::new([2, 0, 0, 0, 0, 3]),
            MacAddress::new([2, 0, 0, 0, 0, 5]),
        ]);

        let plan = plan_explorations(
            explored
                .iter()
                .map(|(endpoint, iface)| (endpoint.address, iface, endpoint))
                .collect(),
            Vec::new(),
            3,
            &expected,
        );

        let ips =
            |list: [&str; 3]| -> Vec<IpAddr> { list.iter().map(|a| a.parse().unwrap()).collect() };
        assert_eq!(
            addresses(&plan.refresh_waits, |c| c.0),
            ips(["10.0.0.3", "10.0.0.4", "10.0.0.2"])
        );
        assert!(plan.routine.is_empty());
        assert_eq!(
            (plan.refresh_wait_candidates, plan.routine_candidates),
            (3, 2)
        );
    }

    /// A never-explored row nobody declared is routine, so it neither takes a
    /// refresh-wait slot from the parked BMCs during bring-up nor the spare
    /// budget from the complete reports of a steady fleet, and it is retried
    /// in its turn by report time. A declared one competes with the parked
    /// BMCs.
    #[test]
    fn plan_explorations_keeps_undeclared_never_explored_endpoints_routine() {
        let complete = |address: &str, micros: i64| {
            planned(
                address,
                PreingestionState::Complete,
                false,
                false,
                version_at(micros),
            )
        };
        let parked = |address: &str, micros: i64| {
            planned(
                address,
                PreingestionState::InitialBMCReset {
                    phase: InitialBmcResetPhase::WaitForExplorerRefresh,
                },
                true,
                false,
                version_at(micros),
            )
        };
        let failed = |address: &str, octet: u8, micros: i64| {
            failed_probe(
                address,
                PreingestionState::Initial,
                true,
                octet,
                version_at(micros),
            )
        };
        let explored = [
            complete("10.0.0.1", 20),
            complete("10.0.0.2", 21),
            complete("10.0.0.3", 22),
            complete("10.0.0.4", 23),
            complete("10.0.0.5", 24),
            complete("10.0.0.6", 25),
            parked("10.0.0.7", 40),
            parked("10.0.0.8", 41),
            failed("10.0.0.9", 9, 45),
            failed("10.0.0.10", 10, 30),
            failed("10.0.0.11", 11, 31),
            failed("10.0.0.12", 12, 32),
        ];
        let expected = HashSet::from([MacAddress::new([2, 0, 0, 0, 0, 9])]);
        let unexplored_ifaces: Vec<(IpAddr, MachineInterfaceSnapshot)> = (1..=7)
            .map(|octet| {
                (
                    format!("10.0.1.{octet}").parse().unwrap(),
                    MachineInterfaceSnapshot::mock_with_mac(MacAddress::new([
                        2, 0, 0, 0, 1, octet,
                    ])),
                )
            })
            .collect();
        struct Case {
            name: &'static str,
            unexplored: usize,
            refresh_waits: Vec<&'static str>,
            routine: Vec<&'static str>,
        }
        let cases = [
            Case {
                name: "bring-up: the parked BMCs keep the refresh-wait share",
                unexplored: 7,
                refresh_waits: vec!["10.0.0.7", "10.0.0.8"],
                routine: vec!["10.0.0.1"],
            },
            Case {
                name: "steady state: the spare budget refreshes complete reports before the undeclared row",
                unexplored: 0,
                refresh_waits: vec!["10.0.0.7", "10.0.0.8", "10.0.0.9"],
                routine: vec![
                    "10.0.0.1",
                    "10.0.0.2",
                    "10.0.0.3",
                    "10.0.0.4",
                    "10.0.0.5",
                    "10.0.0.6",
                    "10.0.0.10",
                ],
            },
        ];
        let ips =
            |list: &[&str]| -> Vec<IpAddr> { list.iter().map(|a| a.parse().unwrap()).collect() };
        for case in cases {
            let plan = plan_explorations(
                explored
                    .iter()
                    .map(|(endpoint, iface)| (endpoint.address, iface, endpoint))
                    .collect(),
                unexplored_ifaces[..case.unexplored]
                    .iter()
                    .map(|(address, iface)| (*address, iface))
                    .collect(),
                10,
                &expected,
            );
            assert_eq!(
                plan.unexplored.len(),
                case.unexplored,
                "{}: unexplored",
                case.name
            );
            assert_eq!(
                addresses(&plan.refresh_waits, |c| c.0),
                ips(&case.refresh_waits),
                "{}: refresh waits",
                case.name
            );
            assert_eq!(
                addresses(&plan.routine, |c| c.0),
                ips(&case.routine),
                "{}: routine",
                case.name
            );
            assert_eq!(
                (plan.refresh_wait_candidates, plan.routine_candidates),
                (3, 9),
                "{}: candidates",
                case.name
            );
        }
    }

    #[test]
    fn classify_running_as_dpu_when_in_dpu_mode() {
        let dpu = bf2_dpu(Some(BlueFieldOperatingMode::Dpu));
        let host = explored_endpoint(load_dell_ep_report());
        // Mode already correct (`Ok(true)`) -> attach as a managed DPU.
        assert!(matches!(
            classify_matched_dpu(&matched_pcie_dpu(&dpu), &host, Some(Ok(true))),
            DiscoveredDpu::RunningAsDpu(_)
        ));
        // A skipped mode check (`None`) behaves the same.
        assert!(matches!(
            classify_matched_dpu(&matched_pcie_dpu(&dpu), &host, None),
            DiscoveredDpu::RunningAsDpu(_)
        ));
    }

    #[test]
    fn classify_running_as_nic_when_dpu_reports_nic_mode() {
        let dpu = bf2_dpu(Some(BlueFieldOperatingMode::Nic));
        let host = explored_endpoint(load_dell_ep_report());
        assert!(matches!(
            classify_matched_dpu(&matched_pcie_dpu(&dpu), &host, Some(Ok(true))),
            DiscoveredDpu::RunningAsNic
        ));
    }

    #[test]
    fn classify_needs_reconfig_when_set_nic_mode_was_issued() {
        // `Ok(false)` means `set_nic_mode` was issued successfully.
        let dpu = bf2_dpu(Some(BlueFieldOperatingMode::Nic));
        let host = explored_endpoint(load_dell_ep_report());
        assert!(matches!(
            classify_matched_dpu(&matched_pcie_dpu(&dpu), &host, Some(Ok(false))),
            DiscoveredDpu::NeedsReconfig
        ));
    }

    #[test]
    fn classify_mode_check_failed_on_error() {
        let dpu = bf2_dpu(Some(BlueFieldOperatingMode::Dpu));
        let host = explored_endpoint(load_dell_ep_report());
        let err = SiteExplorerError::InvalidArgument("boom".to_string());
        assert!(matches!(
            classify_matched_dpu(&matched_pcie_dpu(&dpu), &host, Some(Err(err))),
            DiscoveredDpu::ModeCheckFailed(_)
        ));
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
