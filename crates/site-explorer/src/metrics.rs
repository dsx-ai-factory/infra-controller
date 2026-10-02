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
use std::fmt::{Display, Formatter};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use ::carbide_utils::metrics::SharedMetricsHolder;
use carbide_instrument::{Event, LabelValue};
use carbide_metrics_utils::OtelView;
use carbide_uuid::machine::MachineType;
use mac_address::MacAddress;
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::site_explorer::{EndpointExplorationError, MachineExpectation};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry_sdk::metrics::{Aggregation, InstrumentKind};

use super::config::SiteExplorerConfig;

/// Reasons why a host fails to pair with its DPU(s).
/// These are issues that require manual intervention.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PairingBlockerReason {
    /// Non-Dell host needs power cycle after DPU mode change
    ManualPowerCycleRequired,
    /// Viking CPLDMB_0 version too old, needs data center power cycle
    VikingCpldVersionIssue,
    /// Cannot determine DPU's NIC/DPU mode (likely BMC firmware too old)
    DpuNicModeUnknown,
    /// Cannot retrieve DPU's pf0 MAC address
    DpuPf0MacMissing,
    /// Cannot get system info from host BMC
    HostSystemReportMissing,
    /// Host's boot MAC not found in any discovered DPU
    BootInterfaceMacMismatch,
    /// Host has no DPUs available for management while its effective policy is
    /// `Manage`. We expected managed DPUs but found none -- likely a
    /// misconfiguration or DPU-discovery bug.
    NoDpuReportedByHost,
}

impl Display for PairingBlockerReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::ManualPowerCycleRequired => "manual_power_cycle_required",
            Self::VikingCpldVersionIssue => "viking_cpld_version_issue",
            Self::DpuNicModeUnknown => "dpu_nic_mode_unknown",
            Self::DpuPf0MacMissing => "dpu_pf0_mac_missing",
            Self::HostSystemReportMissing => "host_system_report_missing",
            Self::BootInterfaceMacMismatch => "boot_interface_mac_mismatch",
            Self::NoDpuReportedByHost => "no_dpu_reported_by_host",
        };
        write!(f, "{s}")
    }
}

/// Signals emitted while reconciling a DPU with its resolved target operating mode.
/// The target incorporates the effective host policy and, for `Manage`, product defaults.
/// Each signal marks a step in the resulting flip-and-reset flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DpuMigrationSignal {
    /// Found a DPU whose actual mode differs from the target; will reconfigure.
    ModeMismatchFound,
    /// Issued a `set_nic_mode` flip to a DPU.
    SetNicModeIssued,
    /// Requested a host power-cycle to apply a queued NIC-mode change.
    ResetRequested,
    /// Registered a host with zero managed DPUs because its policy is
    /// `Nic` (distinct from `Ignore`).
    RegisteredZeroDpuForNic,
}

impl Display for DpuMigrationSignal {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::ModeMismatchFound => "mode_mismatch_found",
            Self::SetNicModeIssued => "set_nic_mode_issued",
            Self::ResetRequested => "reset_requested",
            // Keep the established metric label stable across the policy rename.
            Self::RegisteredZeroDpuForNic => "registered_zero_dpu_for_nic_mode",
        };
        write!(f, "{s}")
    }
}

/// Metrics that are gathered in one site exploration run
#[derive(Clone, Debug)]
pub struct SiteExplorationMetrics {
    /// When we started recording these metrics
    pub recording_started_at: std::time::Instant,
    /// Total amount of endpoint exploration attempts that has been attempted
    pub endpoint_explorations: usize,
    /// Successful endpoint explorations
    pub endpoint_explorations_success: usize,
    /// Endpoint exploration failures by type
    pub endpoint_explorations_failures_by_type: HashMap<String, usize>,
    /// Total amount of endpoint exploration failures by failure type
    pub endpoint_explorations_failures_overall_count: HashMap<String, usize>,
    /// Total number of machines that have not completed preingestion,
    /// by expected/unexpected and machine type
    pub endpoint_explorations_preingestions_incomplete_overall_count:
        HashMap<(MachineExpectation, MachineType), usize>,
    /// Total amount of expected machines where actual serial doesn't
    /// match expected serial, by machine type.
    pub endpoint_explorations_expected_serial_number_mismatches_overall_count:
        HashMap<MachineType, usize>,
    /// Total number of expected machines that have been explored,
    /// by expected/unexpected and machine type
    pub endpoint_explorations_machines_explored_overall_count:
        HashMap<(MachineExpectation, MachineType), usize>,
    /// Total number of managed hosts have been successfully constructed,
    /// by expected/unexpected.
    pub endpoint_explorations_identified_managed_hosts_overall_count:
        HashMap<MachineExpectation, usize>,
    /// Total number of expected managed hosts that were not successfully constructed.
    pub endpoint_explorations_expected_machines_missing_overall_count: usize,
    /// The time it took to explore endpoints
    pub endpoint_exploration_duration: Vec<Duration>,
    /// Duration of each step inside endpoint exploration attempts.
    pub endpoint_exploration_step_latency: Vec<(&'static str, Duration)>,
    /// Duration of each major Site Explorer iteration phase.
    pub site_explorer_phase_latency: Vec<(&'static str, Duration)>,
    /// Counts from the update_explored_endpoints phase in the last run.
    pub update_explored_endpoints_counts: HashMap<&'static str, usize>,
    /// Total amount of managedhosts that has been identified via Site Exploration
    pub exploration_identified_managed_hosts: usize,
    /// The amount of Machine pairs (Host + DPU) that have been created by Site Explorer
    pub created_machines: usize,
    /// The time it took to create machines
    pub create_machines_latency: Option<Duration>,
    // TODO(chet): Track down Jira created and/or implement Rack metrics
    // also. Currently on Vinod to make a Jira, but leaving this here.
    /// The amount of Power Shelves that have been created by Site Explorer
    pub created_power_shelves_count: usize,
    /// The number of Switches that have been created by Site Explorer
    pub created_switches_count: usize,
    /// The time it took to create power shelves
    pub create_power_shelves_latency: Option<Duration>,
    /// The time it took to create switches
    pub create_switches_latency: Option<Duration>,
    /// Total amount of BMC resets
    pub bmc_reset_count: usize,
    /// Total amount of BMC reboots
    pub bmc_reboot_count: usize,
    /// Total number of expected power shelves that were not successfully identified.
    // TODO(chet): Track down Jira and/or implement similar
    // counter for Switch as well.
    pub endpoint_explorations_expected_power_shelves_missing_overall_count: usize,
    /// Total count of expected machines by SKU ID and device type
    pub expected_machines_sku_count: HashMap<(String, String), usize>, // (sku_id, device_type)
    /// Total count of host+dpu pairing blockers by reason.
    /// These are issues that prevent a host from being paired with its dpu(s)
    /// and require manual intervention.
    pub host_dpu_pairing_blockers: HashMap<String, usize>,
    /// Generic category for the latest whole-run failure. `None` means success.
    pub run_failure_category: Option<String>,
    /// Total count of DPU NIC-mode migration signals by kind. These track the
    /// flip-and-reset flow that reconciles a DPU with its resolved target mode
    /// (mismatch found, `set_nic_mode` issued, reset requested, and zero-DPU
    /// registered for a `Nic` host).
    pub dpu_migration_signals: HashMap<String, usize>,
}

impl Default for SiteExplorationMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteExplorationMetrics {
    pub fn new() -> Self {
        Self {
            recording_started_at: Instant::now(),
            endpoint_explorations: 0,
            endpoint_explorations_success: 0,
            endpoint_explorations_failures_by_type: HashMap::new(),
            endpoint_explorations_failures_overall_count: HashMap::new(),
            endpoint_explorations_preingestions_incomplete_overall_count: HashMap::new(),
            endpoint_explorations_expected_serial_number_mismatches_overall_count: HashMap::new(),
            endpoint_explorations_machines_explored_overall_count: HashMap::new(),
            endpoint_explorations_identified_managed_hosts_overall_count: HashMap::new(),
            endpoint_explorations_expected_machines_missing_overall_count: 0,
            endpoint_exploration_duration: Vec::new(),
            endpoint_exploration_step_latency: Vec::new(),
            site_explorer_phase_latency: Vec::new(),
            update_explored_endpoints_counts: HashMap::new(),
            exploration_identified_managed_hosts: 0,
            created_machines: 0,
            create_machines_latency: None,
            created_power_shelves_count: 0,
            create_power_shelves_latency: None,
            created_switches_count: 0,
            create_switches_latency: None,
            bmc_reset_count: 0,
            bmc_reboot_count: 0,
            endpoint_explorations_expected_power_shelves_missing_overall_count: 0,
            expected_machines_sku_count: HashMap::new(),
            host_dpu_pairing_blockers: HashMap::new(),
            run_failure_category: None,
            dpu_migration_signals: HashMap::new(),
        }
    }

    pub fn increment_endpoint_explorations_failures_overall_count(&mut self, failure_type: String) {
        *self
            .endpoint_explorations_failures_overall_count
            .entry(failure_type)
            .or_default() += 1;
    }

    pub fn increment_endpoint_explorations_preingestions_incomplete_overall_count(
        &mut self,
        expected: MachineExpectation,
        machine_type: MachineType,
    ) {
        *self
            .endpoint_explorations_preingestions_incomplete_overall_count
            .entry((expected, machine_type))
            .or_default() += 1;
    }

    pub fn increment_endpoint_explorations_expected_serial_number_mismatches_overall_count(
        &mut self,
        machine_type: MachineType,
    ) {
        *self
            .endpoint_explorations_expected_serial_number_mismatches_overall_count
            .entry(machine_type)
            .or_default() += 1;
    }

    pub fn increment_endpoint_explorations_machines_explored_overall_count(
        &mut self,
        expected: MachineExpectation,
        machine_type: MachineType,
    ) {
        *self
            .endpoint_explorations_machines_explored_overall_count
            .entry((expected, machine_type))
            .or_default() += 1;
    }

    pub fn increment_endpoint_explorations_identified_managed_hosts_overall_count(
        &mut self,
        expected: MachineExpectation,
    ) {
        *self
            .endpoint_explorations_identified_managed_hosts_overall_count
            .entry(expected)
            .or_default() += 1;
    }

    pub fn increment_expected_machines_sku_count(
        &mut self,
        sku_id: Option<&str>,
        device_type: Option<&str>,
    ) {
        let sku_id_key = sku_id.unwrap_or("unknown").to_string();
        let device_type_key = device_type.unwrap_or("unknown").to_string();
        *self
            .expected_machines_sku_count
            .entry((sku_id_key, device_type_key))
            .or_default() += 1;
    }

    /// Increment the count of host+dpu pairing blockers for a given reason.
    pub fn increment_host_dpu_pairing_blocker(&mut self, reason: PairingBlockerReason) {
        *self
            .host_dpu_pairing_blockers
            .entry(reason.to_string())
            .or_default() += 1;
    }

    pub fn record_phase_latency(&mut self, phase: &'static str, duration: Duration) {
        self.site_explorer_phase_latency.push((phase, duration));
    }

    pub fn record_endpoint_exploration_step_latency(
        &mut self,
        step: &'static str,
        duration: Duration,
    ) {
        self.endpoint_exploration_step_latency
            .push((step, duration));
    }

    pub fn record_update_explored_endpoints_count(&mut self, kind: &'static str, count: usize) {
        self.update_explored_endpoints_counts.insert(kind, count);
    }

    /// Increment the count of DPU NIC-mode migration signals by kind.
    pub fn increment_dpu_migration_signal(&mut self, signal: DpuMigrationSignal) {
        *self
            .dpu_migration_signals
            .entry(signal.to_string())
            .or_default() += 1;
    }
}

/// Histogram bucket boundaries for site explorer duration metrics, in milliseconds.
///
/// Keeps the default OpenTelemetry millisecond buckets through 10 seconds for
/// sub-second endpoint exploration timings, then extends the upper range to one
/// hour so full site explorer iterations are not collapsed into `+Inf`.
const SITE_EXPLORER_DURATION_HISTOGRAM_BOUNDARIES_MS: &[f64] = &[
    0.0,
    5.0,
    10.0,
    25.0,
    50.0,
    75.0,
    100.0,
    250.0,
    500.0,
    750.0,
    1_000.0,
    2_500.0,
    5_000.0,
    7_500.0,
    10_000.0,
    30_000.0,
    60_000.0,
    120_000.0,
    300_000.0,
    600_000.0,
    1_800_000.0,
    3_600_000.0,
];

/// Configures histogram buckets for site explorer latency metrics.
pub fn site_explorer_latency_histogram_view(
    name_filter: &'static str,
) -> carbide_metrics_utils::Result<OtelView> {
    carbide_metrics_utils::new_view(
        name_filter,
        Some(InstrumentKind::Histogram),
        Aggregation::ExplicitBucketHistogram {
            boundaries: SITE_EXPLORER_DURATION_HISTOGRAM_BOUNDARIES_MS.to_vec(),
            record_min_max: true,
        },
    )
}

/// One Site Explorer pass. Both cases sample the duration; only a failure logs
/// -- successes and lock-acquisition skips both stay quiet.
#[derive(Event)]
#[event(
    event_name = "site_explorer_iteration_finished",
    metric_name = "carbide_site_explorer_iteration_latency_milliseconds",
    component = "site-explorer",
    metric = histogram,
    describe = "The time it took to perform one site explorer iteration"
)]
pub(crate) enum SiteExplorerIterationFinished {
    /// A clean pass: sampled, never logged.
    #[event(log = off)]
    Succeeded {
        #[observation]
        latency: Duration,
    },

    #[event(log = error, message = "SiteExplorer run failed")]
    Failed {
        #[observation]
        latency: Duration,
        #[context]
        error: String,
    },
}

/// Selection mechanisms Site Explorer currently uses for managed hosts with
/// multiple DPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum BootInterfaceSelectionMechanism {
    ExpectedMachine,
    RedfishUefiPci,
    /// Natural ordering of the host BMC `Chassis.id` values selected the first DPU.
    RedfishChassisId,
    RedfishSerialNumber,
}

/// One successful boot interface selection during a Site Explorer pass for a
/// host with more than one explored DPU and effective DPU policy `Manage`.
#[derive(Event)]
#[event(
    event_name = "boot_interface_selected",
    metric_name = "carbide_site_explorer_boot_interface_selections_total",
    component = "site-explorer",
    log = off,
    metric = counter,
    describe = "Number of successful Site Explorer boot interface selections for hosts with more than one explored DPU and effective DPU policy Manage, by selection mechanism.",
    labels(mechanism: BootInterfaceSelectionMechanism),
)]
pub(crate) enum BootInterfaceSelected {
    #[event(labels(mechanism = ExpectedMachine))]
    ExpectedMachine {},

    #[event(labels(mechanism = RedfishUefiPci))]
    RedfishUefiPci {},

    #[event(labels(mechanism = RedfishChassisId))]
    RedfishChassisId {},

    #[event(
        labels(mechanism = RedfishSerialNumber),
        log = debug,
        message = "Selected boot interface using Redfish serial ordering"
    )]
    RedfishSerialNumber {
        #[context]
        host_bmc_ip_address: IpAddr,
        #[context]
        dpu_count: usize,
        #[context]
        selected_mac_address: String,
        #[context]
        selected_dpu_serial_number: Option<String>,
    },
}

impl BootInterfaceSelected {
    /// Builds an Event only for mechanisms handled by this Site Explorer flow.
    pub(crate) fn from_selection_source(
        source: BootInterfaceSelectionSource,
        host_bmc_ip_address: IpAddr,
        dpu_count: usize,
        selected_mac_address: MacAddress,
        selected_dpu_serial_number: Option<String>,
    ) -> Option<Self> {
        match source {
            BootInterfaceSelectionSource::ExpectedMachine => Some(Self::ExpectedMachine {}),
            BootInterfaceSelectionSource::RedfishUefiPci => Some(Self::RedfishUefiPci {}),
            BootInterfaceSelectionSource::RedfishChassisId => Some(Self::RedfishChassisId {}),
            BootInterfaceSelectionSource::RedfishSerialNumber => Some(Self::RedfishSerialNumber {
                host_bmc_ip_address,
                dpu_count,
                selected_mac_address: selected_mac_address.to_string(),
                selected_dpu_serial_number,
            }),
            BootInterfaceSelectionSource::Operator
            | BootInterfaceSelectionSource::LegacyUnknown
            | BootInterfaceSelectionSource::ScoutReportPci => None,
        }
    }
}

/// The step that left a machine's RMS location data incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum SiteExplorerMachineSlotTrayFailureStage {
    RmsRequest,
    ResponseMissing,
    DatabaseUpdate,
}

// `LabelValue` exports the full variant names, including the shared suffix, as
// the established `failure_stage` values.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum SiteExplorerMachineSlotTrayValueFailureStage {
    SlotNumberOutOfRange,
    TrayIndexOutOfRange,
}

/// `SiteExplorerMachineSlotTrayFetchFailed` records an RMS request failure
/// that leaves both machine location fields unset.
#[derive(Event)]
#[event(
    event_name = "site_explorer_machine_slot_tray_fetch_failed",
    metric_name = "carbide_site_explorer_machine_slot_tray_enrichment_failures_total",
    component = "site-explorer",
    log = warn,
    metric = counter,
    message = "Failed to get device info from RMS, slot_number and tray_index will be unset",
    describe = "Number of Site Explorer machine slot and tray enrichment failures, by failure stage."
)]
pub(crate) struct SiteExplorerMachineSlotTrayFetchFailed {
    #[label]
    failure_stage: SiteExplorerMachineSlotTrayFailureStage,
    #[context]
    error: String,
}

impl SiteExplorerMachineSlotTrayFetchFailed {
    pub(crate) fn new(error: String) -> Self {
        Self {
            failure_stage: SiteExplorerMachineSlotTrayFailureStage::RmsRequest,
            error,
        }
    }
}

/// `SiteExplorerMachineSlotTrayResponseMissing` records a successful RMS call
/// whose response did not include device details.
#[derive(Event)]
#[event(
    event_name = "site_explorer_machine_slot_tray_response_missing",
    metric_name = "carbide_site_explorer_machine_slot_tray_enrichment_failures_total",
    component = "site-explorer",
    log = warn,
    metric = counter,
    message = "RMS returned no device info, slot_number and tray_index will be unset",
    describe = "Number of Site Explorer machine slot and tray enrichment failures, by failure stage."
)]
pub(crate) struct SiteExplorerMachineSlotTrayResponseMissing {
    #[label]
    failure_stage: SiteExplorerMachineSlotTrayFailureStage,
}

impl SiteExplorerMachineSlotTrayResponseMissing {
    pub(crate) fn new() -> Self {
        Self {
            failure_stage: SiteExplorerMachineSlotTrayFailureStage::ResponseMissing,
        }
    }
}

/// RMS returned a slot or tray value Site Explorer cannot use. Each variant is
/// the field that was out of range.
#[derive(Event)]
#[event(
    event_name = "site_explorer_machine_slot_tray_value_invalid",
    metric_name = "carbide_site_explorer_machine_slot_tray_enrichment_failures_total",
    component = "site-explorer",
    metric = counter,
    describe = "Number of Site Explorer machine slot and tray enrichment failures, by failure stage.",
    labels(failure_stage: SiteExplorerMachineSlotTrayValueFailureStage),
)]
pub(crate) enum SiteExplorerMachineSlotTrayValueInvalid {
    #[event(
        labels(failure_stage = SiteExplorerMachineSlotTrayValueFailureStage::SlotNumberOutOfRange),
        log = warn,
        message = "RMS returned slot_number outside the supported range, slot_number will be unset"
    )]
    SlotNumber {
        #[context]
        value: u32,
    },

    #[event(
        labels(failure_stage = SiteExplorerMachineSlotTrayValueFailureStage::TrayIndexOutOfRange),
        log = warn,
        message = "RMS returned tray_index outside the supported range, tray_index will be unset"
    )]
    TrayIndex {
        #[context]
        value: u32,
    },
}
/// `SiteExplorerMachineSlotTrayPersistenceFailed` records a database update
/// failure after the best-effort RMS lookup.
#[derive(Event)]
#[event(
    event_name = "site_explorer_machine_slot_tray_persistence_failed",
    metric_name = "carbide_site_explorer_machine_slot_tray_enrichment_failures_total",
    component = "site-explorer",
    log = warn,
    metric = counter,
    message = "Failed to update slot_number and tray_index for machine",
    describe = "Number of Site Explorer machine slot and tray enrichment failures, by failure stage."
)]
pub(crate) struct SiteExplorerMachineSlotTrayPersistenceFailed {
    #[label]
    failure_stage: SiteExplorerMachineSlotTrayFailureStage,
    #[context]
    error: String,
    #[context]
    host_machine_id: String,
}

impl SiteExplorerMachineSlotTrayPersistenceFailed {
    pub(crate) fn new(error: String, host_machine_id: String) -> Self {
        Self {
            failure_stage: SiteExplorerMachineSlotTrayFailureStage::DatabaseUpdate,
            error,
            host_machine_id,
        }
    }
}

/// The transport used for a BMC reset. Keeping this as an enum bounds the
/// `method` label to the two reset paths Site Explorer can dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum BmcResetMethod {
    Ipmitool,
    Redfish,
}

impl Display for BmcResetMethod {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ipmitool => "ipmitool",
            Self::Redfish => "redfish",
        })
    }
}

/// Whether a dispatched BMC reset finished successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum BmcResetStatus {
    Succeeded,
    Failed,
}

/// One Site Explorer BMC reset attempt. Each variant is the result, and keeps
/// the level and wording that result already had.
#[derive(Event)]
#[event(
    event_name = "site_explorer_bmc_reset_finished",
    metric_name = "carbide_site_explorer_bmc_reset_attempts_total",
    component = "site-explorer",
    metric = counter,
    describe = "Number of Site Explorer BMC reset attempts, by method and status.",
    labels(status: BmcResetStatus, method: BmcResetMethod),
)]
pub(crate) enum BmcResetFinished {
    #[event(
        labels(status = BmcResetStatus::Succeeded),
        log = info,
        message = "Site Explorer reset BMC"
    )]
    Succeeded {
        #[label]
        method: BmcResetMethod,
        #[context]
        address: IpAddr,
        #[context]
        error: String,
    },

    #[event(
        labels(status = BmcResetStatus::Failed),
        log = error,
        message = "Site Explorer failed to reset BMC"
    )]
    Failed {
        #[label]
        method: BmcResetMethod,
        #[context]
        address: IpAddr,
        #[context]
        error: String,
    },
}
/// A physical BMC reset succeeded, but its rate-limit timestamp could not be
/// persisted. This is a separate Event because the bookkeeping failure must
/// not turn the completed reset into a failed attempt.
#[derive(Event)]
#[event(
    event_name = "site_explorer_bmc_reset_timestamp_persistence_failed",
    metric_name = "carbide_site_explorer_bmc_reset_timestamp_persistence_failures_total",
    component = "site-explorer",
    log = warn,
    metric = counter,
    message = "BMC reset succeeded but recording its rate-limit timestamp failed",
    describe = "Number of Site Explorer BMC reset timestamp persistence failures, by method."
)]
pub(crate) struct BmcResetTimestampPersistenceFailed {
    #[label]
    pub(crate) method: BmcResetMethod,
    #[context]
    pub(crate) bmc_ip_address: IpAddr,
    #[context]
    pub(crate) error: String,
}

/// Instruments that are used by the Site Explorer
struct SiteExplorerInstruments {
    endpoint_exploration_duration: Histogram<f64>,
    endpoint_exploration_step_latency: Histogram<f64>,
    site_explorer_phase_latency: Histogram<f64>,
    site_explorer_create_machines_latency: Histogram<f64>,
    site_explorer_create_power_shelves_latency: Histogram<f64>,
    site_explorer_create_switches_latency: Histogram<f64>,
}

impl SiteExplorerInstruments {
    fn new(
        meter: Meter,
        shared_metrics: SharedMetricsHolder<SiteExplorationMetrics>,
        config: &SiteExplorerConfig,
    ) -> Self {
        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_endpoint_explorations_count")
                .with_description("Number of attempted endpoint explorations")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.endpoint_explorations as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_last_run_status")
                .with_description("The status of the latest Site Explorer run")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let (status, failure_category) = metrics
                            .run_failure_category
                            .as_deref()
                            .map_or(("success", "none"), |category| ("failed", category));
                        observer.observe(
                            1,
                            &[
                                attrs,
                                &[
                                    KeyValue::new("status", status),
                                    KeyValue::new("failure_category", failure_category.to_string()),
                                ],
                            ]
                            .concat(),
                        );
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_endpoint_exploration_success_count")
                .with_description("Number of successful endpoint explorations")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.endpoint_explorations_success as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_endpoint_exploration_failures_count")
                .with_description("Number of failed endpoint explorations, by error")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (error, &count) in metrics.endpoint_explorations_failures_by_type.iter()
                        {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("failure", error.to_string())]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_endpoint_exploration_failures_overall_count")
                .with_description("Number of failed endpoint explorations, by error")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (error, &count) in
                            metrics.endpoint_explorations_failures_overall_count.iter()
                        {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("failure", error.to_string())]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_preingestions_incomplete_overall_count",
                )
                .with_description(
                    "Number of machines in a preingestion state by expectation and machine type",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((expected, machine_type), &count) in metrics
                            .endpoint_explorations_preingestions_incomplete_overall_count
                            .iter()
                        {
                            observer.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("expectation", expected.to_string()),
                                        KeyValue::new("machine_type", machine_type.metrics_value()),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_endpoint_exploration_expected_serial_number_mismatches_overall_count")
                .with_description("Number of found expected machines by machine type where the observed and expected serial numbers do not match")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (machine_type, &count) in metrics
                            .endpoint_explorations_expected_serial_number_mismatches_overall_count
                            .iter()
                        {
                            observer.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[KeyValue::new("machine_type", machine_type.metrics_value())],
                                ].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_machines_explored_overall_count",
                )
                .with_description("Number of machines explored, by expectation and machine type")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((expected, machine_type), &count) in metrics
                            .endpoint_explorations_machines_explored_overall_count
                            .iter()
                        {
                            observer.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("expectation", expected.to_string()),
                                        KeyValue::new("machine_type", machine_type.metrics_value()),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_identified_managed_hosts_overall_count",
                )
                .with_description("Number of managed hosts identified by expectation")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (expected, &count) in metrics
                            .endpoint_explorations_identified_managed_hosts_overall_count
                            .iter()
                        {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("expectation", expected.to_string())]]
                                    .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_expected_machines_missing_overall_count",
                )
                .with_description("Number of machines expected but not identified")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics.endpoint_explorations_expected_machines_missing_overall_count
                                as u64,
                            attrs,
                        );
                    })
                })
                .build();
        }

        let endpoint_exploration_duration = meter
            .f64_histogram("carbide_endpoint_exploration_duration")
            .with_description("The time it took to explore an endpoint")
            .with_unit("ms")
            .build();

        let endpoint_exploration_step_latency = meter
            .f64_histogram("carbide_endpoint_exploration_step_latency")
            .with_description("The time it took to perform one endpoint exploration step")
            .with_unit("ms")
            .build();

        let site_explorer_phase_latency = meter
            .f64_histogram("carbide_site_explorer_phase_latency")
            .with_description("The time it took to perform one site explorer iteration phase")
            .with_unit("ms")
            .build();

        let site_explorer_create_machines_latency = meter
            .f64_histogram("carbide_site_explorer_create_machines_latency")
            .with_description("The time it took to perform create_machines inside site-explorer")
            .with_unit("ms")
            .build();

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_exploration_identified_managed_hosts_count")
                .with_description(
                    "Number of Host+DPU pairs identified in the last SiteExplorer run",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer
                            .observe(metrics.exploration_identified_managed_hosts as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_update_explored_endpoints_count")
                .with_description("Counts from the last update_explored_endpoints phase by kind")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (kind, &count) in metrics.update_explored_endpoints_counts.iter() {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("kind", *kind)]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_created_machines_count")
                .with_description(
                    "Number of machine pairs created by Site Explorer after identification",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.created_machines as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_bmc_reset_count")
                .with_description("Number of successful BMC resets in the last SiteExplorer run")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.bmc_reset_count as u64, attrs);
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_expected_power_shelves_missing_overall_count",
                )
                .with_description("Number of power shelves expected but not identified")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics
                                .endpoint_explorations_expected_power_shelves_missing_overall_count
                                as u64,
                            attrs,
                        );
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_exploration_expected_machines_sku_count")
                .with_description("Number of expected machines by SKU ID and device type")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for ((sku_id, device_type), &count) in
                            metrics.expected_machines_sku_count.iter()
                        {
                            observer.observe(
                                count as u64,
                                &[
                                    attrs,
                                    &[
                                        KeyValue::new("sku_id", sku_id.clone()),
                                        KeyValue::new("device_type", device_type.clone()),
                                    ],
                                ]
                                .concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_host_dpu_pairing_blockers_count")
                .with_description(
                    "Number of host+DPU pairing blockers by reason. These are issues that prevent \
                     a host from being paired with its DPU(s) and require manual intervention.",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (reason, &count) in metrics.host_dpu_pairing_blockers.iter() {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("reason", reason.clone())]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_dpu_migration_signals_count")
                .with_description(
                    "Number of DPU NIC-mode migration signals by signal type -- mode-mismatch found, \
                     set_nic_mode issued, reset requested, and zero-DPU registered for a host \
                     whose DPU policy is nic.",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (signal, &count) in metrics.dpu_migration_signals.iter() {
                            observer.observe(
                                count as u64,
                                &[attrs, &[KeyValue::new("signal", signal.clone())]].concat(),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge(
                    "carbide_endpoint_exploration_expected_power_shelves_missing_overall_count",
                )
                .with_description("Number of power shelves expected but not identified")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(
                            metrics
                                .endpoint_explorations_expected_power_shelves_missing_overall_count
                                as u64,
                            attrs,
                        );
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics;
            meter
                .u64_observable_gauge("carbide_site_explorer_created_power_shelves_count")
                .with_description(
                    "Number of power shelves created by Site Explorer after identification",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        observer.observe(metrics.created_power_shelves_count as u64, attrs);
                    })
                })
                .build();
        }

        let site_explorer_create_power_shelves_latency = meter
            .f64_histogram("site_explorer_create_power_shelves_latency")
            .with_description("Duration of power shelf creation")
            .with_unit("s")
            .build();

        let site_explorer_create_switches_latency = meter
            .f64_histogram("site_explorer_create_switches_latency")
            .with_description("Duration of switch creation")
            .with_unit("s")
            .build();

        {
            let enabled = config.enabled.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_enabled")
                .with_description("Whether site-explorer is enabled (1) or paused (0)")
                .with_callback(move |observer| {
                    let val = u64::from(enabled.load(std::sync::atomic::Ordering::Relaxed));
                    observer.observe(val, &[]);
                })
                .build();
        }

        {
            let create_machines = config.create_machines.clone();
            meter
                .u64_observable_gauge("carbide_site_explorer_create_machines")
                .with_description(
                    "Whether site-explorer machine creation is enabled (1) or disabled (0)",
                )
                .with_callback(move |observer| {
                    let val = u64::from(create_machines.load(std::sync::atomic::Ordering::Relaxed));
                    observer.observe(val, &[]);
                })
                .build();
        }

        SiteExplorerInstruments {
            endpoint_exploration_duration,
            endpoint_exploration_step_latency,
            site_explorer_phase_latency,
            site_explorer_create_machines_latency,
            site_explorer_create_power_shelves_latency,
            site_explorer_create_switches_latency,
        }
    }

    /// Emits the latency metrics that are captured during a single site explorer
    /// iteration. Those are emitted immediately as histograms, whereas the
    /// amount of objects in states is emitted as gauges.
    fn emit_latency_metrics(&self, metrics: &SiteExplorationMetrics) {
        if let Some(latency) = metrics.create_machines_latency {
            self.site_explorer_create_machines_latency
                .record(1000.0 * latency.as_secs_f64(), &[]);
        }

        if let Some(latency) = metrics.create_power_shelves_latency {
            self.site_explorer_create_power_shelves_latency
                .record(1000.0 * latency.as_secs_f64(), &[]);
        }

        if let Some(latency) = metrics.create_switches_latency {
            self.site_explorer_create_switches_latency
                .record(1000.0 * latency.as_secs_f64(), &[]);
        }

        for duration in metrics.endpoint_exploration_duration.iter() {
            self.endpoint_exploration_duration
                .record(duration.as_secs_f64() * 1000.0, &[]);
        }

        for (step, duration) in metrics.endpoint_exploration_step_latency.iter() {
            self.endpoint_exploration_step_latency.record(
                duration.as_secs_f64() * 1000.0,
                &[KeyValue::new("step", *step)],
            );
        }

        for (phase, duration) in metrics.site_explorer_phase_latency.iter() {
            self.site_explorer_phase_latency.record(
                duration.as_secs_f64() * 1000.0,
                &[KeyValue::new("phase", *phase)],
            );
        }
    }
}

/// Converts an endpoint exploration error into a concise label for metrics
///
/// Since we want to keep the amount of dimensions in metrics down, only the top
/// level error information is copied and details are omitted.
pub(super) fn exploration_error_to_metric_label(error: &EndpointExplorationError) -> String {
    match error {
        EndpointExplorationError::ConnectionRefused { .. } => "connection_refused",
        EndpointExplorationError::ConnectionTimeout { .. } => "connection_timeout",
        EndpointExplorationError::Unreachable { .. } => "unreachable",
        EndpointExplorationError::UnsupportedVendor { .. } => "unsupported_vendor",
        EndpointExplorationError::RedfishError { .. } => "redfish_error",
        EndpointExplorationError::Unauthorized { .. } => "unauthorized",
        EndpointExplorationError::MissingCredentials { .. } => "missing_credentials",
        EndpointExplorationError::SetCredentials { .. } => "set_credentials",
        EndpointExplorationError::MissingRedfish { .. } => "missing_redfish",
        // Distinguish a BMC that reported no vendor at all (commonly transient,
        // still initializing) from one that reported a vendor we don't support,
        // so the two failure modes can be alerted on separately. See NVBug 6036327.
        EndpointExplorationError::MissingVendor { observed: None } => "vendor_not_reported",
        EndpointExplorationError::MissingVendor { observed: Some(_) } => "vendor_unrecognized",
        EndpointExplorationError::AvoidLockout => "avoid_lockout",
        EndpointExplorationError::Other { .. } => "other",
        EndpointExplorationError::VikingFWInventoryForbiddenError { .. } => {
            "viking_fw_inventory_forbidden"
        }
        EndpointExplorationError::InvalidDpuRedfishBiosResponse { .. } => {
            "invalid_dpu_redfish_bios_response"
        }
        EndpointExplorationError::SecretsEngineError { .. } => "secrets_engine",
        EndpointExplorationError::IntermittentUnauthorized { .. } => "intermittent_unauthorized",
    }
    .to_string()
}

/// Stores Metric data shared between SiteExplorer and the OpenTelemetry background task
pub(super) struct MetricHolder {
    instruments: SiteExplorerInstruments,
    last_iteration_metrics: SharedMetricsHolder<SiteExplorationMetrics>,
}

impl MetricHolder {
    pub(super) fn new(
        meter: Meter,
        hold_period: std::time::Duration,
        config: &SiteExplorerConfig,
    ) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        let instruments =
            SiteExplorerInstruments::new(meter, last_iteration_metrics.clone(), config);
        Self {
            instruments,
            last_iteration_metrics,
        }
    }

    /// Updates the most recent metrics
    pub(super) fn update_metrics(&self, mut metrics: SiteExplorationMetrics) {
        // Emit the last recent latency metrics
        self.instruments.emit_latency_metrics(&metrics);
        // We don't need to store the latency metrics anymore
        metrics.endpoint_exploration_duration.clear();
        metrics.endpoint_exploration_step_latency.clear();
        metrics.site_explorer_phase_latency.clear();
        // And store the remaining metrics
        self.last_iteration_metrics.update(metrics);
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use prometheus::{Encoder, TextEncoder};

    use super::*;

    struct LatencyHistogramTestMeter {
        meter_provider: SdkMeterProvider,
        registry: prometheus::Registry,
    }

    impl LatencyHistogramTestMeter {
        fn new(name_filter: &'static str) -> Self {
            let registry = prometheus::Registry::new();
            let metrics_exporter = opentelemetry_prometheus::exporter()
                .with_registry(registry.clone())
                .without_scope_info()
                .without_target_info()
                .build()
                .unwrap();

            let meter_provider = SdkMeterProvider::builder()
                .with_reader(metrics_exporter)
                .with_view(site_explorer_latency_histogram_view(name_filter).unwrap())
                .build();

            Self {
                meter_provider,
                registry,
            }
        }

        fn export_metrics(&self) -> String {
            let mut buffer = vec![];
            let encoder = TextEncoder::new();
            let metric_families = self.registry.gather();
            encoder.encode(&metric_families, &mut buffer).unwrap();
            String::from_utf8(buffer).unwrap()
        }
    }

    #[test]
    fn dpu_migration_signal_labels_stay_stable() {
        value_scenarios!(
            run = |signal: DpuMigrationSignal| signal.to_string();
            "mode mismatch" {
                DpuMigrationSignal::ModeMismatchFound => "mode_mismatch_found".to_string(),
            }
            "NIC-mode change issued" {
                DpuMigrationSignal::SetNicModeIssued => "set_nic_mode_issued".to_string(),
            }
            "reset requested" {
                DpuMigrationSignal::ResetRequested => "reset_requested".to_string(),
            }
            "NIC policy registration keeps the legacy label" {
                DpuMigrationSignal::RegisteredZeroDpuForNic =>
                    "registered_zero_dpu_for_nic_mode".to_string(),
            }
        );
    }

    #[derive(Clone, Copy)]
    struct BootInterfaceSelectionCase {
        source: BootInterfaceSelectionSource,
        mechanism: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct BootInterfaceSelectionObservation {
        log_count: usize,
        level: Option<tracing::Level>,
        message: Option<String>,
        event_name: Option<String>,
        metric_name: Option<String>,
        host_bmc_ip_address: Option<String>,
        dpu_count: Option<String>,
        selected_mac_address: Option<String>,
        selected_dpu_serial_number: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn boot_interface_selection_mechanisms_have_bounded_labels_and_expected_logs() {
        const METRIC: &str = "carbide_site_explorer_boot_interface_selections_total";

        value_scenarios!(
            run = |BootInterfaceSelectionCase { source, mechanism }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(
                        BootInterfaceSelected::from_selection_source(
                            source,
                            "192.0.2.10".parse().unwrap(),
                            2,
                            "02:00:00:00:00:01".parse().unwrap(),
                            Some("DPU-SERIAL-1".to_string()),
                        )
                        .expect("supported Site Explorer selection source"),
                    );
                });
                let log = logs.first();

                BootInterfaceSelectionObservation {
                    log_count: logs.len(),
                    level: log.map(|log| log.level),
                    message: log.map(|log| log.message.clone()),
                    event_name: log
                        .and_then(|log| log.field("event_name"))
                        .map(str::to_string),
                    metric_name: log
                        .and_then(|log| log.field("metric_name"))
                        .map(str::to_string),
                    host_bmc_ip_address: log
                        .and_then(|log| log.field("host_bmc_ip_address"))
                        .map(str::to_string),
                    dpu_count: log
                        .and_then(|log| log.field("dpu_count"))
                        .map(str::to_string),
                    selected_mac_address: log
                        .and_then(|log| log.field("selected_mac_address"))
                        .map(str::to_string),
                    selected_dpu_serial_number: log
                        .and_then(|log| log.field("selected_dpu_serial_number"))
                        .map(str::to_string),
                    counter_delta: metrics.counter_delta(METRIC, &[("mechanism", mechanism)]),
                }
            };
            "ExpectedMachine selection is metric-only" {
                BootInterfaceSelectionCase {
                    source: BootInterfaceSelectionSource::ExpectedMachine,
                    mechanism: "expected_machine",
                } => BootInterfaceSelectionObservation {
                    log_count: 0,
                    level: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    host_bmc_ip_address: None,
                    dpu_count: None,
                    selected_mac_address: None,
                    selected_dpu_serial_number: None,
                    counter_delta: 1.0,
                },
            }
            "RedfishUefiPci selection is metric-only" {
                BootInterfaceSelectionCase {
                    source: BootInterfaceSelectionSource::RedfishUefiPci,
                    mechanism: "redfish_uefi_pci",
                } => BootInterfaceSelectionObservation {
                    log_count: 0,
                    level: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    host_bmc_ip_address: None,
                    dpu_count: None,
                    selected_mac_address: None,
                    selected_dpu_serial_number: None,
                    counter_delta: 1.0,
                },
            }
            "RedfishChassisId selection is metric-only" {
                BootInterfaceSelectionCase {
                    source: BootInterfaceSelectionSource::RedfishChassisId,
                    mechanism: "redfish_chassis_id",
                } => BootInterfaceSelectionObservation {
                    log_count: 0,
                    level: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    host_bmc_ip_address: None,
                    dpu_count: None,
                    selected_mac_address: None,
                    selected_dpu_serial_number: None,
                    counter_delta: 1.0,
                },
            }
            "RedfishSerialNumber selection retains its DEBUG diagnostic" {
                BootInterfaceSelectionCase {
                    source: BootInterfaceSelectionSource::RedfishSerialNumber,
                    mechanism: "redfish_serial_number",
                } => BootInterfaceSelectionObservation {
                    log_count: 1,
                    level: Some(tracing::Level::DEBUG),
                    message: Some(
                        "Selected boot interface using Redfish serial ordering".to_string(),
                    ),
                    event_name: Some("boot_interface_selected".to_string()),
                    metric_name: Some(METRIC.to_string()),
                    host_bmc_ip_address: Some("192.0.2.10".to_string()),
                    dpu_count: Some("2".to_string()),
                    selected_mac_address: Some("02:00:00:00:00:01".to_string()),
                    selected_dpu_serial_number: Some("DPU-SERIAL-1".to_string()),
                    counter_delta: 1.0,
                },
            }
        );
    }

    #[test]
    fn unsupported_boot_interface_selection_sources_do_not_construct_an_event() {
        value_scenarios!(
            run = |source| BootInterfaceSelected::from_selection_source(
                source,
                "192.0.2.10".parse().unwrap(),
                2,
                "02:00:00:00:00:01".parse().unwrap(),
                Some("DPU-SERIAL-1".to_string()),
            )
            .is_none();
            "operator selection" {
                BootInterfaceSelectionSource::Operator => true,
            }
            "legacy unknown selection" {
                BootInterfaceSelectionSource::LegacyUnknown => true,
            }
            "Scout report PCI selection" {
                BootInterfaceSelectionSource::ScoutReportPci => true,
            }
        );
    }

    #[derive(Clone, Copy)]
    enum SlotTrayFailureCase {
        RmsRequest,
        ResponseMissing,
        SlotNumberOutOfRange,
        TrayIndexOutOfRange,
        DatabaseUpdate,
    }

    #[derive(Debug, PartialEq)]
    struct SlotTrayFailureObservation {
        log_count: usize,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        failure_stage: Option<String>,
        error: Option<String>,
        value: Option<String>,
        host_machine_id: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn machine_slot_tray_failures_keep_their_logs_and_count_by_stage() {
        const ERROR: &str = "simulated failure";
        const HOST_MACHINE_ID: &str = "machine-1";
        const METRIC: &str = "carbide_site_explorer_machine_slot_tray_enrichment_failures_total";
        const OUT_OF_RANGE: u32 = i32::MAX as u32 + 1;

        check_values(
            [
                Check {
                    scenario: "RMS request",
                    input: SlotTrayFailureCase::RmsRequest,
                    expect: SlotTrayFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        message: "Failed to get device info from RMS, slot_number and tray_index will be unset".to_string(),
                        event_name: Some(
                            "site_explorer_machine_slot_tray_fetch_failed".to_string(),
                        ),
                        metric_name: Some(METRIC.to_string()),
                        failure_stage: Some("rms_request".to_string()),
                        error: Some(ERROR.to_string()),
                        value: None,
                        host_machine_id: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "missing response",
                    input: SlotTrayFailureCase::ResponseMissing,
                    expect: SlotTrayFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        message:
                            "RMS returned no device info, slot_number and tray_index will be unset"
                                .to_string(),
                        event_name: Some(
                            "site_explorer_machine_slot_tray_response_missing".to_string(),
                        ),
                        metric_name: Some(METRIC.to_string()),
                        failure_stage: Some("response_missing".to_string()),
                        error: None,
                        value: None,
                        host_machine_id: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "slot number outside i32",
                    input: SlotTrayFailureCase::SlotNumberOutOfRange,
                    expect: SlotTrayFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        message: "RMS returned slot_number outside the supported range, slot_number will be unset".to_string(),
                        event_name: Some(
                            "site_explorer_machine_slot_tray_value_invalid".to_string(),
                        ),
                        metric_name: Some(METRIC.to_string()),
                        failure_stage: Some("slot_number_out_of_range".to_string()),
                        error: None,
                        value: Some(OUT_OF_RANGE.to_string()),
                        host_machine_id: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "tray index outside i32",
                    input: SlotTrayFailureCase::TrayIndexOutOfRange,
                    expect: SlotTrayFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        message: "RMS returned tray_index outside the supported range, tray_index will be unset".to_string(),
                        event_name: Some(
                            "site_explorer_machine_slot_tray_value_invalid".to_string(),
                        ),
                        metric_name: Some(METRIC.to_string()),
                        failure_stage: Some("tray_index_out_of_range".to_string()),
                        error: None,
                        value: Some(OUT_OF_RANGE.to_string()),
                        host_machine_id: None,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "database update",
                    input: SlotTrayFailureCase::DatabaseUpdate,
                    expect: SlotTrayFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        message:
                            "Failed to update slot_number and tray_index for machine".to_string(),
                        event_name: Some(
                            "site_explorer_machine_slot_tray_persistence_failed".to_string(),
                        ),
                        metric_name: Some(METRIC.to_string()),
                        failure_stage: Some("database_update".to_string()),
                        error: Some(ERROR.to_string()),
                        value: None,
                        host_machine_id: Some(HOST_MACHINE_ID.to_string()),
                        counter_delta: 1.0,
                    },
                },
            ],
            |case| {
                let failure_stage_label = match case {
                    SlotTrayFailureCase::RmsRequest => "rms_request",
                    SlotTrayFailureCase::ResponseMissing => "response_missing",
                    SlotTrayFailureCase::SlotNumberOutOfRange => {
                        "slot_number_out_of_range"
                    }
                    SlotTrayFailureCase::TrayIndexOutOfRange => {
                        "tray_index_out_of_range"
                    }
                    SlotTrayFailureCase::DatabaseUpdate => "database_update",
                };
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match case {
                    SlotTrayFailureCase::RmsRequest => {
                        emit(SiteExplorerMachineSlotTrayFetchFailed::new(
                            ERROR.to_string(),
                        ));
                    }
                    SlotTrayFailureCase::ResponseMissing => {
                        emit(SiteExplorerMachineSlotTrayResponseMissing::new());
                    }
                    SlotTrayFailureCase::SlotNumberOutOfRange => {
                        emit(SiteExplorerMachineSlotTrayValueInvalid::SlotNumber { value: OUT_OF_RANGE });
                    }
                    SlotTrayFailureCase::TrayIndexOutOfRange => {
                        emit(SiteExplorerMachineSlotTrayValueInvalid::TrayIndex { value: OUT_OF_RANGE });
                    }
                    SlotTrayFailureCase::DatabaseUpdate => {
                        emit(SiteExplorerMachineSlotTrayPersistenceFailed::new(
                            ERROR.to_string(),
                            HOST_MACHINE_ID.to_string(),
                        ));
                    }
                });
                let log = logs.first().expect("failure Event logged");

                SlotTrayFailureObservation {
                    log_count: logs.len(),
                    level: log.level,
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    failure_stage: log.field("failure_stage").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    value: log.field("value").map(str::to_string),
                    host_machine_id: log.field("host_machine_id").map(str::to_string),
                    counter_delta: metrics
                        .counter_delta(METRIC, &[("failure_stage", failure_stage_label)]),
                }
            },
        );
    }

    /// One `SiteExplorerIterationFinished` emission always moves the
    /// label-free histogram. A non-empty `error` also emits the existing
    /// `ERROR` record; an empty value stays silent.
    #[test]
    fn site_explorer_iteration_outcomes_pair_latency_with_failure_log() {
        const EXPOSED_METRIC: &str = "carbide_site_explorer_iteration_latency_milliseconds";

        struct IterationCase {
            latency: Duration,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        let failure = r#"Internal { message: "simulated iteration failure" }"#;
        check_values(
            [
                Check {
                    scenario: "successful iteration",
                    input: IterationCase {
                        latency: Duration::from_millis(125),
                        error: "",
                    },
                    expect: Observation {
                        log_count: 0,
                        log: None,
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(125.0),
                    },
                },
                Check {
                    scenario: "failed iteration",
                    input: IterationCase {
                        latency: Duration::from_millis(375),
                        error: failure,
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            level: tracing::Level::ERROR,
                            metadata_name: "site_explorer_iteration_finished".to_string(),
                            message: "SiteExplorer run failed".to_string(),
                            event_name: Some("site_explorer_iteration_finished".to_string()),
                            metric_name: Some(EXPOSED_METRIC.to_string()),
                            error: Some(failure.to_string()),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(375.0),
                    },
                },
            ],
            |IterationCase { latency, error }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(if error.is_empty() {
                        SiteExplorerIterationFinished::Succeeded { latency }
                    } else {
                        SiteExplorerIterationFinished::Failed {
                            latency,
                            error: error.to_string(),
                        }
                    });
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics.histogram_count_delta(EXPOSED_METRIC, &[]),
                    histogram_sum_delta: metrics.histogram_sum_delta(EXPOSED_METRIC, &[]),
                }
            },
        );
    }

    /// Replacing the manual iteration histogram must not change its exposed
    /// contract. This test pins the family name, description, unit suffix, and
    /// label-free series shape.
    #[test]
    fn site_explorer_iteration_histogram_exposition_stays_stable() {
        const EXPOSED_METRIC: &str = "carbide_site_explorer_iteration_latency_milliseconds";

        let metrics = MetricsCapture::start();
        emit(SiteExplorerIterationFinished::Succeeded {
            latency: Duration::from_millis(125),
        });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {EXPOSED_METRIC} The time it took to perform one site explorer iteration\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {EXPOSED_METRIC} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            !encoded.contains("carbide_site_explorer_iteration_latency_milliseconds_milliseconds"),
            "the unit suffix must be applied exactly once:\n{encoded}"
        );
        for suffix in ["count", "sum"] {
            let prefix = format!("{EXPOSED_METRIC}_{suffix} ");
            let sample = encoded
                .lines()
                .find(|line| line.starts_with(&prefix))
                .unwrap_or_else(|| panic!("missing {prefix} sample:\n{encoded}"));
            assert!(
                !sample.contains('{'),
                "iteration latency must remain label-free: {sample}"
            );
        }
    }

    #[test]
    fn site_explorer_latency_histogram_views_build() {
        scenarios!(
            run = |name_filter: &'static str| {
                site_explorer_latency_histogram_view(name_filter)
                    .map(|_| ())
                    .map_err(drop)
            };
            "iteration latency" {
                "carbide_site_explorer_iteration_latency" => Yields(()),
            }

            "carbide site explorer latency glob" {
                "carbide_site_explorer_*_latency" => Yields(()),
            }

            "endpoint exploration duration" {
                "carbide_endpoint_exploration_duration" => Yields(()),
            }
        );
    }

    #[test]
    fn site_explorer_latency_histogram_redistributes_observations_above_ten_seconds() {
        let test_meter = LatencyHistogramTestMeter::new("carbide_site_explorer_iteration_latency");
        let meter = test_meter.meter_provider.meter("site-explorer-test");
        let histogram = meter
            .f64_histogram("carbide_site_explorer_iteration_latency")
            .with_unit("ms")
            .build();

        histogram.record(30_000.0, &[]);

        let encoded = test_meter.export_metrics();
        assert!(
            encoded.contains(
                r#"carbide_site_explorer_iteration_latency_milliseconds_bucket{le="30000"} 1"#
            ),
            "expected 30s observation in the 30000ms bucket, got:\n{encoded}"
        );
        assert!(
            !encoded.contains(
                r#"carbide_site_explorer_iteration_latency_milliseconds_bucket{le="10000"} 1"#
            ),
            "30s observation should not land in the 10000ms bucket:\n{encoded}"
        );
    }
}
