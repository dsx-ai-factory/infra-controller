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
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::{
    DpuMachineId, HostMachineId, InvalidMachineType, MachineId, MachineInterfaceId,
};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Duration, Utc};
use config_version::{ConfigVersion, Versioned};
use duration_str::deserialize_duration_chrono;
use health_report::HealthReport;
use json::MachineSnapshotPgJson;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize, Serializer};
use sqlx::postgres::PgRow;
use sqlx::{Column, FromRow, Row};
use strum_macros::EnumIter;

use self::network::{MachineNetworkStatusObservation, ManagedHostNetworkConfig};
use super::StateSla;
use super::instance::snapshot::InstanceSnapshot;
use super::instance::status::extension_service::InstanceExtensionServiceStatusObservationByType;
use super::instance::status::network::InstanceNetworkStatusObservation;
use super::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};
use super::metadata::Metadata;
use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::dpa_interface::{DpaInterface, DpaInterfaceType};
use crate::errors::{ModelError, ModelResult};
use crate::expected_machine::ExpectedMachineData;
use crate::firmware::FirmwareComponentType;
use crate::instance::config::network::DeviceLocator;
use crate::instance::snapshot::InstanceSnapshotPgJson;
use crate::machine::capabilities::MachineCapabilitiesSet;
use crate::machine::health_override::HealthReportSources;
use crate::machine_interface::InterfaceType;
use crate::machine_interface_address::InterfaceAssociationType;
use crate::network_segment::NetworkSegmentType;
use crate::predicted_machine_interface::PredictedMachineInterface;
use crate::state_history::StateHistoryRecord;

pub mod slas;

pub mod capabilities;
pub mod config;
pub mod health_override;
pub mod infiniband;
pub mod json;
pub mod machine_id;
pub mod machine_search_config;
pub mod network;
pub mod nvlink;
pub mod spx;
pub mod status;
pub mod topology;
pub mod upgrade_policy;

pub use self::config::MachineConfig;
pub use self::status::MachineStatus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpuOsOperationalState {
    pub state_detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpuRepresentorStatus {
    pub name: String,
    pub carrier_up: Option<bool>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpuInfoStatusObservation {
    pub os_operational_state: Option<DpuOsOperationalState>,
    pub firmware_version: Option<String>,
    pub representors: Vec<DpuRepresentorStatus>,
    pub last_heartbeat: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DpuInfo {
    pub id: String,
    pub loopback_ip: String,
    pub observed_status: Option<DpuInfoStatusObservation>,
}

type DpuDeviceMappings = (
    HashMap<DpuMachineId, String>,
    HashMap<String, Vec<DpuMachineId>>,
);

pub fn get_display_ids(machines: &[Machine]) -> String {
    machines
        .iter()
        .map(|x| x.id.to_string())
        .collect::<Vec<String>>()
        .join("/")
}

fn default_true() -> bool {
    true
}

// This should be updated on each new model introduction
pub const CURRENT_STATE_MODEL_VERSION: i16 = 3;

fn pending_boot_interface_config_version(
    desired_version: Option<ConfigVersion>,
    verified_version: Option<ConfigVersion>,
) -> Option<ConfigVersion> {
    desired_version.filter(|desired_version| Some(*desired_version) != verified_version)
}

/// Represents the current state of `Machine`
#[derive(Debug, Clone)]
pub struct ManagedHostStateSnapshot {
    pub host_snapshot: Machine,
    pub dpu_snapshots: Vec<Machine>,
    pub dpa_interface_snapshots: Vec<DpaInterface>,
    /// If there is an instance provisioned on top of the machine, this holds
    /// its state
    pub instance: Option<InstanceSnapshot>,
    pub managed_state: ManagedHostState,
    /// Aggregated health. This is calculated based on the health of Hosts and DPUs
    pub aggregate_health: health_report::HealthReport,
    /// Health overrides inherited from the rack this host belongs to (if any).
    /// Populated at read time; not stored on the machines table.
    pub rack_health_overrides: Option<HealthReportSources>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ManagedHostStateSnapshot {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        #[derive(Deserialize)]
        struct RackHealthOverrides {
            json: HealthReportSources,
        }

        let host_snapshot: sqlx::types::Json<MachineSnapshotPgJson> =
            row.try_get("host_snapshot")?;
        let dpu_snapshots: sqlx::types::Json<Vec<Option<MachineSnapshotPgJson>>> =
            row.try_get("dpu_snapshots")?;
        let rack_health_overrides: sqlx::types::Json<Vec<Option<RackHealthOverrides>>> =
            row.try_get("rack_health_overrides")?;
        // We are setting dpa_interface_snapshots to an emtpy vector here.
        // This will be filled by load_object_state later.
        let dpa_interface_snapshots: Vec<DpaInterface> = Vec::new();

        let mut instance: Option<InstanceSnapshot> =
            if let Some(column) = row.columns().iter().find(|c| c.name() == "instance") {
                let json: sqlx::types::Json<Option<InstanceSnapshotPgJson>> =
                    row.try_get(column.ordinal())?;
                json.0.map(TryInto::try_into).transpose()?
            } else {
                None
            };

        let host_snapshot: Machine = host_snapshot.0.try_into()?;

        let dpu_snapshots: Vec<Machine> = dpu_snapshots
            .0
            .into_iter()
            .flatten()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let rack_health_overrides = rack_health_overrides
            .0
            .into_iter()
            .next()
            .flatten()
            .and_then(|overrides| {
                let overrides = overrides.json;
                if overrides.replace.is_some() || !overrides.merges.is_empty() {
                    Some(overrides)
                } else {
                    None
                }
            });

        // Instance network observation is fetched from dpu_snapshots.
        if let Some(instance) = &mut instance {
            instance.observations.network =
                InstanceNetworkStatusObservation::aggregate_instance_observation(&dpu_snapshots);
            instance.observations.extension_services =
                InstanceExtensionServiceStatusObservationByType::aggregate_instance_observation(
                    &dpu_snapshots,
                );
        }

        // TODO: consider dropping this field from ManagedHostStateSnapshot
        let managed_state = host_snapshot.state.value.clone();

        let mut result = Self {
            host_snapshot,
            dpu_snapshots,
            dpa_interface_snapshots,
            managed_state,
            instance,
            rack_health_overrides,
            // This will need to be modified by callers, as its value depends on a
            // HardwareHealthReportsConfig being specified.
            aggregate_health: health_report::HealthReport::empty("".to_string()),
        };

        result.sort_dpu_snapshots()?;

        Ok(result)
    }
}

/// Reasons why a Machine is not allocatable
#[derive(thiserror::Error, Clone, PartialEq, Eq, Debug)]
pub enum NotAllocatableReason {
    #[error("the machine is in a state other than `ready`: {0:?}")]
    InvalidState(Box<ManagedHostState>),
    #[error(
        "the machine has a pending instance creation request, that has not yet been processed by the state handler"
    )]
    PendingInstanceCreation,
    #[error("the machine has a pending boot configuration")]
    PendingBootConfiguration,
    #[error("there are no dpu_snapshots, but associated_dpu_machine_ids is non-empty")]
    NoDpuSnapshots,
    #[error("the machine is in maintenance mode")]
    MaintenanceMode,
    #[error("A health alert prevents the machine from being allocated: {0:?}")]
    HealthAlert(Box<health_report::HealthProbeAlert>),
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedHostStateSnapshotError {
    #[error(transparent)]
    InvalidMachineType(#[from] InvalidMachineType),

    #[error("missing primary interface. machine id: {0}")]
    PrimaryInterfaceMissing(HostMachineId),

    #[error("missing dpu with primary dpu id. machine id: {0}, DPU ID: {1}")]
    MissingPrimaryDpu(HostMachineId, DpuMachineId),
}

impl From<ManagedHostStateSnapshotError> for sqlx::Error {
    fn from(value: ManagedHostStateSnapshotError) -> Self {
        Self::Decode(Box::new(value))
    }
}

/// Pick the MAC address the host should boot from, given its interfaces.
///
/// See `ManagedHostStateSnapshot::boot_interface_mac` for the caller-facing
/// details. I split this out as a function so it can be unit-tested directly
/// without constructing a full `ManagedHostStateSnapshot`.
///
/// Ordering:
/// 1. Any interface with `primary_interface == true` wins. This is the
///    path operators can drive explicitly via `ExpectedInterface.primary`,
///    in the case of zero DPU hosts, and is also how hosts with DPU(s)
///    end up with a boot MAC automatically during site-explorer ingestion.
/// 2. If there's no primary interface, the interface outside the management
///    segment with the "smallest" MAC address wins -- mainly so we can
///    have some sense of a stable MAC for zero-DPU hosts where no operator
///    explicitly declared a primary NIC (and ingestion didn't assign one either).
/// 3. `None` -- This is the "I don't have any candidate interfaces yet" case,
///    so it's on the caller to figure out. What this usually means is the
///    caller passes `boot_interface_mac: None` to machine_setup, and then
///    subsequent logic flows from there (e.g. ::NoDpu handling).
///
/// Public because admin boot-interface resolution (api-core) applies the same
/// selection to a machine's interface rows when targeting a host by BMC
/// endpoint.
pub fn pick_boot_interface(
    interfaces: &[MachineInterfaceSnapshot],
) -> Option<&MachineInterfaceSnapshot> {
    // The primary wins!
    if let Some(primary) = interfaces.iter().find(|x| x.primary_interface) {
        return Some(primary);
    }
    // ..no primary, so lets try to find *some* interface.
    pick_default_boot_interface(interfaces)
}

/// What [`pick_boot_interface`] falls back to when no row is flagged primary:
/// the lowest-MAC non-underlay interface (ordering rationale in its docs).
///
/// Public so the admin boot-interface view can report this pick alongside the
/// effective one: comparing the two shows whether a primary designation is
/// overriding what the automation would have chosen, or merely confirming it.
pub fn pick_default_boot_interface(
    interfaces: &[MachineInterfaceSnapshot],
) -> Option<&MachineInterfaceSnapshot> {
    interfaces
        .iter()
        .filter(|x| x.network_segment_type != Some(NetworkSegmentType::Underlay))
        .min_by_key(|x| x.mac_address)
}

fn pick_boot_interface_mac(
    interfaces: &[MachineInterfaceSnapshot],
) -> Option<mac_address::MacAddress> {
    pick_boot_interface(interfaces).map(|x| x.mac_address)
}

/// Resolves the boot interface to the picked interface's own
/// [`MachineBootInterface`]. Split out like `pick_boot_interface_mac` so it's
/// unit-testable without a full snapshot.
fn pick_boot_interface_pair(
    interfaces: &[MachineInterfaceSnapshot],
) -> Option<MachineBootInterface> {
    pick_boot_interface(interfaces).and_then(MachineInterfaceSnapshot::boot_interface)
}

/// Pick the predicted interface a host should boot from in the window before
/// its first DHCP lease creates a real `machine_interfaces` row. Mirrors
/// `pick_boot_interface`'s precedence, one step down -- predictions, not rows:
///
/// 1. A prediction flagged `primary_interface` wins -- the declared
///    `ExpectedInterface.primary`, recorded onto the prediction at minting.
/// 2. Otherwise the sole non-underlay prediction. With several and none
///    declared primary the boot NIC is unknowable (e.g. a host whose report
///    lists SuperNICs alongside the boot NIC), so this returns `None` rather
///    than guess against whichever NIC happens to sort first.
///
/// Public because both the machine-controller and admin boot-interface
/// resolution apply it for the pre-first-lease window, when the real rows
/// offer no candidate yet. Callers map the chosen prediction to a boot target
/// via `PredictedMachineInterface::boot_interface()`.
pub fn pick_boot_prediction(
    predictions: &[PredictedMachineInterface],
) -> Option<&PredictedMachineInterface> {
    // The declared primary wins, exactly as in `pick_boot_interface`.
    if let Some(primary) = predictions.iter().find(|p| p.primary_interface) {
        return Some(primary);
    }
    // Otherwise only a sole non-underlay prediction is unambiguous.
    let mut non_underlay = predictions
        .iter()
        .filter(|p| p.expected_network_segment_type != NetworkSegmentType::Underlay);
    match (non_underlay.next(), non_underlay.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

impl ManagedHostStateSnapshot {
    /// Returns `true` if this managed host has at least one DPU snapshot
    /// attached -- i.e. a DPU we actively manage as a `Machine`.
    ///
    /// A `false` return ("no managed DPUs") covers the two effective policies
    /// that intentionally attach no DPU snapshots: `HostDpuPolicy::Ignore` and
    /// `HostDpuPolicy::Nic`. A `Nic` host may have DPU hardware, but
    /// site-explorer puts it into NIC mode at ingestion, so no DPU snapshot is
    /// ever attached.
    ///
    /// Some callers combine this w/ `associated_dpu_machine_ids().is_empty()`
    /// to distinguish between truly no managed DPUs vs. DPU expected per
    /// topology (but something happened, like the snapshot failed to load).
    /// Those sites intentionally inspect both sides of this, so simply relying
    /// on this might not be what they'd want (at least for now).
    ///
    /// NOTE(chet): State-controller handlers normally reject a topology with
    /// no loaded DPU snapshots before dispatch, so `false` means no managed DPUs.
    /// `ManagedHostState::BootConfiguring` is the exception: it must dispatch so
    /// `ReadyBootConfigState::LockHost` can restore lockdown and
    /// `ReadyBootConfigState::Prepare` can wait for a
    /// `MachineNetworkStatusObservation` from every DPU in the host topology.
    ///
    /// Now, callers OUTSIDE the state-controller path DON'T get that upstream
    /// guard; if you need the stronger guarantee there, you'll need to
    /// check both:
    /// `self.dpu_snapshots.is_empty()` and
    /// `self.host_snapshot.associated_dpu_machine_ids().is_empty()`.
    pub fn has_managed_dpus(&self) -> bool {
        !self.dpu_snapshots.is_empty()
    }

    /// Returns `true` if this managed host is currently operating on the
    /// admin network (rather than a tenant overlay).
    ///
    /// Zero-DPU hosts always return `true`, because there's no DPU to
    /// handle tenant overlay networking. This allows consumers like the
    /// IB and NVLink partition monitors to treat the host as admin-only
    /// and detach.
    pub fn use_admin_network(&self) -> bool {
        self.host_snapshot
            .network_config
            .use_admin_network
            .unwrap_or(true)
    }

    /// Returns the MAC address the host should boot from, if one can be
    /// determined from this snapshot.
    ///
    /// For hosts with DPUs, this is the DPU-facing "primary" `machine_interface`,
    /// flagged as `primary_interface: true` during site-explorer ingestion.
    ///
    /// For zero-DPU hosts, the `primary_interface` flag is not (yet) set at
    /// ingestion time, so this method "falls back" to the first non-underlay
    /// `machine_interface` (i.e. not the BMC) sorted deterministically by MAC.
    ///
    /// Returns `None` if the host has no non-underlay interfaces yet -- e.g.
    /// only the BMC has been discovered, or the host's primary NIC hasn't
    /// DHCP'd yet.
    ///
    /// This helper exists to centralize the boot MAC selection logic that used
    /// to be duplicated at every state controller callsite needing to pass a MAC
    /// into things like machine_setup, is_bios_setup, etc.
    pub fn boot_interface_mac(&self) -> Option<mac_address::MacAddress> {
        pick_boot_interface_mac(&self.host_snapshot.status.interfaces)
    }

    /// Returns the host's boot interface as a fully-populated
    /// [`MachineBootInterface`] (MAC + Redfish interface id), derived from the
    /// same primary `machine_interface` row that [`Self::boot_interface_mac`]
    /// selects.
    ///
    /// Returns `None` when that row hasn't captured a Redfish interface id yet
    /// (e.g. not yet explored, or a zero-DPU host) -- callers then target the MAC
    /// alone. Because the MAC and id come from one row, the pair can never name a
    /// different interface than `boot_interface_mac`.
    pub fn boot_interface(&self) -> Option<MachineBootInterface> {
        pick_boot_interface_pair(&self.host_snapshot.status.interfaces)
    }

    // We are examining the dpa_interface_snapshots of the MH to see if has
    // any NICs of type Astra.
    pub fn has_astra_nics(&self) -> bool {
        self.dpa_interface_snapshots
            .iter()
            .any(|nic| matches!(nic.interface_type, DpaInterfaceType::Astra))
    }

    // Returns the Astra NICs found in the MH's dpa_interface_snapshots. Only
    // interfaces whose interface_type is Astra are returned.
    pub fn astra_nics(&self) -> Vec<&DpaInterface> {
        self.dpa_interface_snapshots
            .iter()
            .filter(|nic| matches!(nic.interface_type, DpaInterfaceType::Astra))
            .collect()
    }

    /// Returns `true` if override report is hw_health, `false` otherwise.
    fn merge_override_report_with_hw_health(
        output: &mut HealthReport,
        source: &str,
        report: &mut HealthReport,
        hardware_health_config: HardwareHealthReportsConfig,
    ) -> bool {
        if HealthReportSources::is_hardware_health_override_source(source) {
            match hardware_health_config {
                HardwareHealthReportsConfig::Disabled => {}
                HardwareHealthReportsConfig::MonitorOnly => {
                    for alert in &mut report.alerts {
                        alert.classifications.clear();
                    }
                    output.merge(report)
                }
                HardwareHealthReportsConfig::Enabled => output.merge(report),
            }
            true
        } else {
            output.merge(report);
            false
        }
    }

    /// Returns `Ok` if the Host can be used as an instance
    ///
    /// This requires
    /// - the Machine to be in `Ready` state
    /// - the Machine has not yet been target of an instance creation request
    /// - no health alerts which classification `PreventAllocations` to be set
    /// - the machine not to be in Maintenance Mode
    /// - the desired boot-interface generation to have a matching observation
    pub fn is_usable_as_instance(&self, allow_unhealthy: bool) -> Result<(), NotAllocatableReason> {
        // TODO: allow other states than Ready when allow_unhealthy=true. Will require changes to state machine (see Matthias).
        if !matches!(self.managed_state, ManagedHostState::Ready) {
            return Err(NotAllocatableReason::InvalidState(Box::new(
                self.managed_state.clone(),
            )));
        }

        // A new instance can be created only in Ready state.
        // This is possible that a instance is created by user, but still not picked by state machine.
        // To avoid that race condition, need to check if db has any entry with given machine id.
        if self.instance.is_some() {
            return Err(NotAllocatableReason::PendingInstanceCreation);
        }

        // A desired boot-interface update and instance allocation can race
        // before machine-controller has persisted BootConfiguring. Keep the
        // host unavailable as soon as the desired version lacks a matching
        // convergence status.
        if self
            .host_snapshot
            .pending_boot_interface_config_version()
            .is_some()
        {
            return Err(NotAllocatableReason::PendingBootConfiguration);
        }

        if self.dpu_snapshots.is_empty()
            && !self.host_snapshot.associated_dpu_machine_ids().is_empty()
        {
            return Err(NotAllocatableReason::NoDpuSnapshots);
        }

        if !allow_unhealthy
            && let Some(alert) = self.aggregate_health.find_alert_by_classification(
                &health_report::HealthAlertClassification::prevent_allocations(),
            )
        {
            return Err(NotAllocatableReason::HealthAlert(Box::new(alert.clone())));
        }

        Ok(())
    }

    /// Derives the aggregate health of the Managed Host based on individual
    /// health reports
    pub fn derive_aggregate_health(&mut self, host_health_config: HostHealthConfig) {
        let source = "aggregate-host-health".to_string();
        let observed_at = Some(chrono::Utc::now());

        // If there is an [`HealthReportApplyMode::Replace`] health report override on
        // the host, then use that. A host-level Replace takes full precedence,
        // including over any rack-level overrides.
        if let Some(mut over) = self.host_snapshot.health_reports.replace.clone() {
            over.source = source;
            over.observed_at = observed_at;
            self.aggregate_health = over;
            return;
        }

        let mut output = health_report::HealthReport::empty("".to_string());

        let merge_or_timeout =
            |output: &mut HealthReport, input: &Option<HealthReport>, target: String| {
                if let Some(input) = input {
                    output.merge(input);
                } else {
                    output.merge(&HealthReport::heartbeat_timeout(
                        "".to_string(),
                        target,
                        "".to_string(),
                        true,
                        false,
                    ));
                }
            };

        // Merge DPU's alerts.  If DPU alerts should be suppressed, than remove the classification from the
        // alert so that metrics won't show a critical issue.
        let suppress_dpu_alerts = self.managed_state.suppress_dpu_alerts();
        for snapshot in self.dpu_snapshots.iter_mut() {
            if let Some(over) = snapshot.health_reports.replace.as_mut() {
                let source = over.source.clone();
                Self::merge_override_report_with_hw_health(
                    &mut output,
                    &source,
                    over,
                    host_health_config.hardware_health_reports,
                );
                continue;
            }

            let health_report = if suppress_dpu_alerts {
                let mut health_report = snapshot.dpu_agent_health_report().cloned();

                if let Some(health_report) = &mut health_report {
                    for alert in &mut health_report.alerts {
                        alert.classifications.clear();
                    }
                }
                health_report
            } else {
                snapshot.dpu_agent_health_report().cloned()
            };

            if let Some(network_status_observation) = snapshot.network_status_observation.as_ref()
                && let Some(health_report) = network_status_observation
                    .expired_version_health_report(
                        host_health_config.dpu_agent_version_staleness_threshold,
                        host_health_config.prevent_allocations_on_stale_dpu_agent_version,
                    )
            {
                output.merge(&health_report);
            }

            merge_or_timeout(&mut output, &health_report, "forge-dpu-agent".to_string());

            for (source, over) in snapshot
                .health_reports
                .merges
                .iter_mut()
                .filter(|(source, _)| source.as_str() != HealthReport::DPU_AGENT_SOURCE)
            {
                Self::merge_override_report_with_hw_health(
                    &mut output,
                    source,
                    over,
                    host_health_config.hardware_health_reports,
                );
            }
        }

        let mut has_host_hardware_health = false;
        for (source, over) in self.host_snapshot.health_reports.merges.iter_mut() {
            let merged_hardware = Self::merge_override_report_with_hw_health(
                &mut output,
                source,
                over,
                host_health_config.hardware_health_reports,
            );
            has_host_hardware_health |= merged_hardware;
        }

        if host_health_config.hardware_health_reports == HardwareHealthReportsConfig::Enabled
            && !has_host_hardware_health
        {
            merge_or_timeout(&mut output, &None, "hardware-health".to_string());
        }

        if let Some(rack_overrides) = &self.rack_health_overrides {
            if let Some(rack_replace) = &rack_overrides.replace {
                output.merge(rack_replace);
            }
            for rack_merge in rack_overrides.merges.values() {
                output.merge(rack_merge);
            }
        }

        output.source = source;
        output.observed_at = observed_at;
        self.aggregate_health = output;
    }

    /// Returns true when every DPU in the host topology has applied the current
    /// host network configuration.
    ///
    /// Each topology DPU must have a `MachineNetworkStatusObservation` whose
    /// `network_config_version` matches `host_snapshot.network_config.version`.
    /// Matching by machine ID prevents an empty or partial `dpu_snapshots` load
    /// from appearing current. A host with no topology DPUs is already current.
    pub fn managed_host_network_config_version_synced(&self) -> bool {
        let host_version = self.host_snapshot.network_config.version;

        self.host_snapshot
            .associated_dpu_machine_ids()
            .into_iter()
            .all(|dpu_machine_id| {
                self.dpu_snapshots
                    .iter()
                    .find(|dpu_snapshot| dpu_snapshot.id == dpu_machine_id.into())
                    .is_some_and(|dpu_snapshot| {
                        dpu_snapshot.managed_host_network_config_version_synced(host_version)
                    })
            })
    }

    /// Sort the DPUs by pci address and then make sure the primary DPU is the first.
    pub fn sort_dpu_snapshots(&mut self) -> Result<(), ManagedHostStateSnapshotError> {
        let mac_pci_map: HashMap<MacAddress, Option<&str>> = self
            .host_snapshot
            .status
            .hardware_info
            .iter()
            .flat_map(|hi| &hi.network_interfaces)
            .map(|interface| {
                (
                    interface.mac_address,
                    interface
                        .pci_properties
                        .as_ref()
                        .and_then(|pci| pci.slot.as_deref()),
                )
            })
            .collect();

        self.dpu_snapshots.sort_by(|lhs, rhs| {
            let Some(lhs_dpu_mac) = lhs
                .status
                .hardware_info
                .as_ref()
                .and_then(|hi| hi.dpu_info.as_ref())
                .and_then(|di| di.factory_mac_address.parse().ok())
            else {
                return Ordering::Greater;
            };

            let Some(rhs_dpu_mac) = rhs
                .status
                .hardware_info
                .as_ref()
                .and_then(|hi| hi.dpu_info.as_ref())
                .and_then(|di| di.factory_mac_address.parse().ok())
            else {
                return Ordering::Less;
            };

            let lhs_pci_slot = mac_pci_map.get(&lhs_dpu_mac).unwrap_or(&None);
            let rhs_pci_slot = mac_pci_map.get(&rhs_dpu_mac).unwrap_or(&None);

            match (lhs_pci_slot, rhs_pci_slot) {
                (Some(lhs_pci_slot), Some(rhs_pci_slot)) => lhs_pci_slot.cmp(rhs_pci_slot),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        });

        let primary_interface = self
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.primary_interface);
        let primary_dpu_id =
            primary_interface.and_then(|interface| interface.attached_dpu_machine_id);

        if let Some(primary_dpu_id) = primary_dpu_id {
            let index = self
                .dpu_snapshots
                .iter()
                .position(|x| x.id == primary_dpu_id.into())
                .ok_or({
                    ManagedHostStateSnapshotError::MissingPrimaryDpu(
                        self.host_snapshot.host_machine_id()?,
                        primary_dpu_id,
                    )
                })?;

            if index != 0 {
                let snapshot = self.dpu_snapshots.remove(index);
                self.dpu_snapshots.insert(0, snapshot);
            }
        } else if primary_interface.is_none() && self.has_managed_dpus() {
            // DPU hosts still need some primary interface so boot/network callers have a host
            // primary to anchor on. A present primary interface without an attached DPU is valid:
            // ExpectedMachine can declare a non-DPU host admin NIC as primary, and in that case no
            // DPU should be promoted ahead of PCI order.
            return Err(ManagedHostStateSnapshotError::PrimaryInterfaceMissing(
                self.host_snapshot.host_machine_id()?,
            ));
        };

        Ok(())
    }
}

/// Represents the last_reboot_requested data
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum MachineLastRebootRequestedMode {
    Reboot,
    PowerOff,
    PowerOn,
    GracefulShutdown,
}

impl Display for MachineLastRebootRequestedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct MachineLastRebootRequested {
    pub time: DateTime<Utc>,
    pub mode: MachineLastRebootRequestedMode,
    pub restart_verified: Option<bool>,
    pub verification_attempts: Option<i32>,
}

impl Default for MachineLastRebootRequested {
    fn default() -> Self {
        MachineLastRebootRequested {
            time: Default::default(),
            mode: MachineLastRebootRequestedMode::Reboot,
            restart_verified: None,
            verification_attempts: None,
        }
    }
}

///
/// A machine is a standalone system that performs network booting via normal DHCP processes.
#[derive(Debug, Clone)]
pub struct Machine {
    /// The ID of the machine, this is an internal identifier in the database that's unique for
    /// all machines managed by this instance of carbide.
    pub id: MachineId,

    /// The current state of the machine.
    pub state: Versioned<ManagedHostState>,

    /// The current network state of the machine, excluding the tenant related
    /// configuration. The latter will be tracked as part of the InstanceNetworkConfig.
    pub network_config: Versioned<ManagedHostNetworkConfig>,

    /// The most recent status forge-dpu-agent observed. Tells us if network_config has been
    /// applied yet, and other useful things.
    pub network_status_observation: Option<MachineNetworkStatusObservation>,

    /// A list of [StateHistoryRecord]s that this machine has experienced
    pub history: Vec<StateHistoryRecord>,

    /// Machine metadata
    pub metadata: Metadata,

    /// Version field that tracks changes to
    /// - Metadata
    pub version: ConfigVersion,
    // Columns for these exist, but are unused in rust code
    // /// When this machine record was created
    // pub created: DateTime<Utc>,
    // /// When the machine record was last modified
    // pub updated: DateTime<Utc>,
    // /// When the machine was last deployed
    // pub deployed: Option<DateTime<Utc>>,
    /// The rack this machine is assigned to (sourced from the expected-machine record at
    /// ingestion time; not operator-mutable).
    pub rack_id: Option<RackId>,

    /// Desired machine configuration.
    pub config: MachineConfig,

    /// System-observed state.
    pub status: MachineStatus,

    /// All health report sources
    pub health_reports: HealthReportSources,

    /// Last time when machine reprovision requested.
    pub reprovision_requested: Option<ReprovisionRequest>,

    /// Last time when host reprovision requested
    pub host_reprovision_requested: Option<HostReprovisionRequest>,

    /// When set by an external entity, the state controller transitions the host into
    /// [`ManagedHostState::Maintenance`] to execute the requested operation.
    pub machine_maintenance_requested: Option<MachineMaintenanceRequest>,

    /// When set by the API, the state controller transitions a Ready managed host into the
    /// decommissioning workflow and clears the marker in the same transaction.
    pub decommission_requested: bool,

    /// Operator "force-converge this BMC now" request. Set on the machine
    /// that owns the BMC (a host machine for its host BMC, a DPU machine for its
    /// DPU BMC). When `true`, the machine state controller enters `RotatingBmc`
    /// and force-converges this machine's single BMC on its next sweep,
    /// bypassing the passive site-wide gate and the device's backoff quarantine.
    pub bmc_credential_rotation_requested: bool,

    /// Operator "force-converge this UEFI credential now" request.
    /// Set on the machine that owns the UEFI credential (a host machine for its
    /// host UEFI; a DPU machine for its DPU UEFI). When
    /// `true`, the machine state controller enters `RotatingHostUefi` (or
    /// `RotatingDpuUefi` for a DPU) and force-converges this machine's UEFI
    /// credential on its next sweep,
    /// bypassing the passive site-wide gate and the device's backoff quarantine.
    pub uefi_credential_rotation_requested: bool,

    /// Force the rotation of the NIC lockdown keys on this host.
    /// Bypasses the site-config flag for NIC lockdown rotation.
    pub lockdown_ikm_credential_rotation_requested: bool,

    /// Does the forge-dpu-agent on this DPU need upgrading?
    pub dpu_agent_upgrade_requested: Option<UpgradeDecision>,

    /// The result of the last attempt to change state
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,

    // Is the bios password set on the machine
    pub bios_password_set_time: Option<DateTime<Utc>>,

    /// Last host validation finished.
    pub last_machine_validation_time: Option<DateTime<Utc>>,

    /// current discovery validation id.
    pub discovery_machine_validation_id: Option<MachineValidationId>,

    /// current cleanup validation id.
    pub cleanup_machine_validation_id: Option<MachineValidationId>,

    /// current on demand validation id.
    pub on_demand_machine_validation_id: Option<MachineValidationId>,

    pub on_demand_machine_validation_request: Option<bool>,

    pub asn: Option<u32>,

    /// Per-host profile for state-machine-affecting settings, seeded from the
    /// expected-machine record. Future per-host knobs that influence ingestion
    /// or state transitions should be added here.
    pub host_profile: HostProfile,

    /// Rack-level firmware upgrade status, updated by the rack state machine.
    pub rack_fw_details: Option<RackFirmwareUpgradeStatus>,

    /// Timestamp when manual firmware upgrade was marked as completed
    /// TEMPORARY: Used for workflow where manual upgrades are required before automatic ones
    /// TODO: Remove after upgrade-through-scout is complete
    pub manual_firmware_upgrade_completed: Option<DateTime<Utc>>,
}

// Dpf status field.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Dpf {
    // This field is copied from expected_machines.
    pub enabled: bool,
    // If dpf is used for ingestion.
    pub used_for_ingestion: bool,
}

/// Per-host profile for settings that affect state-machine progression,
/// seeded from the expected-machine record at discovery time. Add new
/// per-host knobs here rather than creating separate JSONB columns.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostProfile {
    /// When `true` the ingestion state machine skips re-enabling lockdown
    /// after BIOS/UEFI configuration.
    pub disable_lockdown: bool,
}

impl HostProfile {
    /// Resolve a runtime `HostProfile` from an optional expected-machine
    /// record. When `None` (no expected-machine entry) every field falls
    /// back to its default.
    ///
    /// New profile fields should be resolved here so the
    /// expected-machine → machine conversion stays in one place.
    pub fn from_expected_machine(data: Option<&ExpectedMachineData>) -> Self {
        match data {
            Some(d) => Self {
                disable_lockdown: d
                    .host_lifecycle_profile
                    .disable_lockdown
                    .unwrap_or_default(),
            },
            None => Self::default(),
        }
    }
}

// We need to implement FromRow because we can't associate dependent tables with the default derive
// (i.e. it can't default unknown fields)
impl<'r> FromRow<'r, PgRow> for Machine {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // Json<T> deserializes the row bytes straight into the snapshot
        // struct, skipping the intermediate serde_json::Value DOM.
        let json: sqlx::types::Json<MachineSnapshotPgJson> = row.try_get(0)?;
        json.0.try_into()
    }
}

impl Machine {
    /// Returns whether the Machine is a DPU, based on the HardwareInfo that
    /// was available when the Machine was discovered
    pub fn is_dpu(&self) -> bool {
        self.id.machine_type().is_dpu()
    }

    pub fn dpu_machine_id(&self) -> Result<DpuMachineId, InvalidMachineType> {
        self.id.try_into()
    }

    pub fn host_machine_id(&self) -> Result<HostMachineId, InvalidMachineType> {
        self.id.try_into()
    }

    pub fn bmc_vendor(&self) -> bmc_vendor::BMCVendor {
        match self.status.hardware_info.as_ref() {
            Some(hw) => hw.bmc_vendor(),
            None => bmc_vendor::BMCVendor::Unknown,
        }
    }

    /// Whether this machine's BMC must have its lockdown disabled before its
    /// UEFI (BIOS setup) password can be changed, and re-enabled afterward.
    /// Only Dell BMCs enforce a lockdown that blocks the BIOS password change;
    /// other vendors accept it without unlocking. Centralizes the vendor check
    /// so the UEFI setup and rotation flows don't special-case Dell inline.
    pub fn needs_bmc_unlock_for_uefi_setup(&self) -> bool {
        self.bmc_vendor().is_dell()
    }

    /// Does the forge-dpu-agent on this DPU need upgrading?
    pub fn needs_agent_upgrade(&self) -> bool {
        self.dpu_agent_upgrade_requested
            .as_ref()
            .map(|d| d.should_upgrade)
            .unwrap_or(false)
    }

    /// Return the current state of the machine.
    pub fn current_state(&self) -> &ManagedHostState {
        &self.state.value
    }

    /// Return the current version of state of the machine.
    pub fn current_version(&self) -> ConfigVersion {
        self.state.version
    }

    /// Returns the desired boot-interface version whose persisted convergence
    /// status is not current, if any.
    ///
    /// Comparing versions keeps the Ready-state decision DB-only. Redfish is
    /// queried only after the controller has persisted
    /// [`ManagedHostState::BootConfiguring`].
    pub fn pending_boot_interface_config_version(&self) -> Option<ConfigVersion> {
        pending_boot_interface_config_version(
            self.config
                .desired_boot_interface
                .as_ref()
                .map(|desired| desired.version),
            self.status
                .boot_interface_status_observation
                .as_ref()
                .map(|observation| observation.config_version),
        )
    }

    /// Latest health report received from forge-dpu-agent.
    pub fn dpu_agent_health_report(&self) -> Option<&HealthReport> {
        self.health_reports
            .merges
            .get(HealthReport::DPU_AGENT_SOURCE)
    }

    /// Latest health report generated by validation tests.
    ///
    /// Machine validation is stored as a regular merge health report source in
    /// `health_reports`, but callsites that update it need a convenient default.
    pub fn machine_validation_health_report(&self) -> HealthReport {
        self.health_reports
            .merges
            .get(HealthReport::MACHINE_VALIDATION_SOURCE)
            .cloned()
            .unwrap_or_else(|| {
                HealthReport::empty(HealthReport::MACHINE_VALIDATION_SOURCE.to_string())
            })
    }

    /// Latest SKU validation health report, if validation found alerts.
    pub fn sku_validation_health_report(&self) -> Option<&HealthReport> {
        self.health_reports
            .merges
            .get(HealthReport::SKU_VALIDATION_SOURCE)
    }

    /// Latest site-explorer health report, if exploration found alerts.
    pub fn site_explorer_health_report(&self) -> Option<&HealthReport> {
        self.health_reports
            .merges
            .get(HealthReport::SITE_EXPLORER_SOURCE)
    }

    /// K8s-safe identifier derived from the BMC MAC address, used as both the
    /// DPF node and device component in CR names.
    /// e.g. `9C:63:C0:E6:B4:3D` -> `9c-63-c0-e6-b4-3d`.
    /// Not using Machine ID because it's too long, and not using IP because it's not stable.
    pub fn dpf_id(&self) -> Option<String> {
        self.status
            .bmc_info
            .mac
            .map(|mac| mac.to_string().to_lowercase().replace(':', "-"))
    }

    pub fn loopback_ip(&self) -> Option<IpAddr> {
        self.network_config.loopback_ip
    }

    /// Returns the DPU's reserved FNN IPv6 loopback when its optional pool is
    /// configured.
    pub fn loopback_ip_v6(&self) -> Option<Ipv6Addr> {
        self.network_config.loopback_ip_v6
    }

    /// Returns all associated DPU Machine IDs if this is Host Machine
    pub fn associated_dpu_machine_ids(&self) -> Vec<DpuMachineId> {
        if self.is_dpu() {
            return Vec::new();
        }

        self.status
            .interfaces
            .iter()
            .filter_map(|i| i.attached_dpu_machine_id)
            .collect::<Vec<DpuMachineId>>()
    }

    pub fn bmc_addr(&self) -> Option<SocketAddr> {
        self.status
            .bmc_info
            .ip
            .map(|ip| SocketAddr::new(ip, self.status.bmc_info.port.unwrap_or(443)))
    }

    /// If this machine is a DPU, returns whether the version of the
    /// given ManagedHostNetworkConfig (which is a host-level versioned
    /// config that is kept in sync across all DPUs on a host) has been
    /// applied + reported back as same by the carbide-dpu-agent.
    pub fn managed_host_network_config_version_synced(&self, host_version: ConfigVersion) -> bool {
        let dpu_observation = self.network_status_observation.as_ref();

        let dpu_observed_version: ConfigVersion = match dpu_observation {
            None => {
                return false;
            }
            Some(network_status) => match network_status.network_config_version {
                None => {
                    return false;
                }
                Some(version) => version,
            },
        };

        host_version == dpu_observed_version
    }

    pub fn to_capabilities(&self) -> Option<MachineCapabilitiesSet> {
        self.status.hardware_info.clone().map(|info| {
            MachineCapabilitiesSet::from_hardware_info(
                &info,
                self.status.infiniband_status_observation.as_ref(),
                self.associated_dpu_machine_ids(),
                &self.status.interfaces,
            )
        })
    }

    pub fn get_device_locator_for_dpu_id(
        &self,
        dpu_machine_id: &DpuMachineId,
    ) -> ModelResult<DeviceLocator> {
        let (id_to_device_map, device_to_id_map) = self.get_dpu_device_and_id_mappings()?;

        if let Some(device) = id_to_device_map.get(dpu_machine_id)
            && let Some(id_vec) = device_to_id_map.get(device)
            && let Some(instance) = id_vec.iter().position(|id| id == dpu_machine_id)
        {
            return Ok(DeviceLocator {
                device: device.clone(),
                device_instance: instance,
            });
        }
        Err(ModelError::DpuMappingError(format!(
            "No device instance found for dpu {} in machine {}",
            dpu_machine_id, self.id
        )))
    }

    pub fn primary_attached_dpu_machine_id(&self) -> Option<DpuMachineId> {
        self.status
            .interfaces
            .iter()
            .find(|iface| iface.primary_interface)
            .and_then(|iface| iface.attached_dpu_machine_id)
    }

    pub fn get_dpu_device_and_id_mappings(&self) -> ModelResult<DpuDeviceMappings> {
        if self.is_dpu() {
            return Err(ModelError::DpuMappingError(
                "get_device_instance_and_dpu_id_mapping called on dpu".to_string(),
            ));
        }

        let hardware_info =
            self.status
                .hardware_info
                .as_ref()
                .ok_or(ModelError::DpuMappingError(format!(
                    "Missing hardware information for machine {}",
                    self.id
                )))?;

        let mut id_to_device_map: HashMap<DpuMachineId, String> = HashMap::default();
        let mut device_to_id_map: HashMap<String, Vec<DpuMachineId>> = HashMap::default();
        // in order to ensure that the primary dpu is assigned a network config, it is configured first.
        // hardware_interfaces has the primary dpu as the first interface, self.status.interfaces may not.
        // iterate over hardware_interfaces and match it to self.status.interfaces using the mac address
        for hardware_iface in &hardware_info.network_interfaces {
            if let Some(pci) = &hardware_iface.pci_properties
                && let Some(iface) = self
                    .status
                    .interfaces
                    .iter()
                    .find(|i| i.mac_address == hardware_iface.mac_address)
                && let Some(dpu_machine_id) = iface.attached_dpu_machine_id
            {
                id_to_device_map.insert(dpu_machine_id, pci.device.clone());
                let id_vec = device_to_id_map.entry(pci.device.clone()).or_default();
                id_vec.push(dpu_machine_id);
            }
        }

        Ok((id_to_device_map, device_to_id_map))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DpuDiscoveringStates {
    pub states: HashMap<DpuMachineId, DpuDiscoveringState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DpuInitStates {
    pub states: HashMap<DpuMachineId, DpuInitState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DpuReprovisionStates {
    pub states: HashMap<DpuMachineId, ReprovisionState>,
}

/// Possible Machine state-machine implementation
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
/// Possible ManagedHost state-machine implementation
/// Only DPU machine field in DB will contain state. Host will be empty. DPU state field will be
/// used to derive state for DPU and Host both.
pub enum ManagedHostState {
    /// Dpu was discovered by a site-explorer and is being configuring via redfish.
    DpuDiscoveringState {
        dpu_states: DpuDiscoveringStates,
    },
    /// DPU is not yet ready.
    DPUInit {
        dpu_states: DpuInitStates,
    },
    /// DPU is ready, Host is not yet Ready.
    // We don't need dpu_states as DPU's machine state is always Ready here.
    HostInit {
        machine_state: MachineState,
    },
    /// Host validation state for machine and DPU validation
    Validation {
        validation_state: ValidationState,
    },
    /// Host is Ready for instance creation.
    Ready,

    /// Host is being removed from managed service.
    Decommissioning {
        decommissioning_state: DecommissioningState,
    },

    /// An unassigned Ready host is converging its Redfish boot configuration
    /// to the desired boot interface persisted on the machine.
    ///
    /// The desired target and version are captured when convergence starts.
    /// The controller checks that version before issuing new Redfish writes,
    /// uses the captured target while work is in flight, and records it
    /// verified only when the version is still current after final observation.
    BootConfiguring {
        desired_version: ConfigVersion,
        desired_boot_interface: MachineBootInterfaceTarget,
        /// Number of complete reconciliation passes retried because the final
        /// Redfish observation drifted after lockdown was restored.
        #[serde(default)]
        post_lock_verification_retry_count: u32,
        boot_config_state: ReadyBootConfigState,
    },

    /// Host is executing an operator-requested maintenance operation.
    Maintenance {
        operation: MachineMaintenanceOperation,
    },

    /// Host is assigned to an Instance.
    Assigned {
        instance_state: InstanceState,
    },
    /// Some cleanup is going on.
    // This is host specific state. We expect DPU to be in Ready state.
    WaitingForCleanup {
        cleanup_state: CleanupState,
        #[serde(default)]
        cleanup_context: CleanupContext,
    },

    /// A forced deletion process has been triggered by the admin CLI
    /// State controller will no longer manage the Machine
    ForceDeletion,

    /// A dummy state used to create DPU in beginning. State will sync to Init when host will be
    /// created.
    Created,

    /// Enable Astra on CX9 NICs if necessary
    ConfigureAstra {
        #[serde(default)]
        configure_astra_state: ConfigureAstraState,
    },

    /// Machine moved to failed state. Recovery will be based on FailedCause
    Failed {
        details: FailureDetails,
        machine_id: MachineId,
        #[serde(default)]
        retry_count: u32,
    },

    /// State used to indicate that DPU reprovisioning is going on.
    DPUReprovision {
        dpu_states: DpuReprovisionStates,
    },

    /// State used to indicate that host reprovisioning is going on
    HostReprovision {
        reprovision_state: HostReprovisionState,
        #[serde(default)]
        retry_count: u32,
    },

    /// The host and/or its DPUs are converging their BMC root credential to the
    /// staged site-wide rotation target. A pool-only, top-level state:
    /// it blocks instance creation (which requires exact `Ready`) for the bounded
    /// duration of the rotation. Per-device backoff/quarantine is owned by the
    /// rotation engine's `device_credential_rotation` bookkeeping, so this state
    /// carries only a retry budget for transient handler failures.
    RotatingBmc {
        #[serde(default)]
        retry_count: u32,
    },

    /// The host is converging its own UEFI (BIOS setup) password to the staged
    /// site-wide rotation target. A pool-only, top-level state: it
    /// blocks instance creation (which requires exact `Ready`) for the bounded
    /// duration of the rotation. Unlike `RotatingBmc`, applying a new UEFI
    /// password requires a BIOS config job plus a full host power-cycle, so this
    /// state carries the multi-tick [`UefiSetupInfo`] sub-state (job id + step).
    /// Unlike the single-tick `RotatingBmc`, there is no separate handler retry
    /// budget: transient failures use the state framework's built-in retry (as
    /// the ingestion UEFI-setup FSM does) and device-level faults are backed off
    /// by the rotation engine's `device_credential_rotation` bookkeeping.
    ///
    /// This state is host-specific on purpose: a DPU's UEFI password is a
    /// distinct device (keyed by the DPU BMC MAC), applied through a DPU restart
    /// rather than a host power-cycle, and a host can carry several DPUs. That
    /// gets its own sibling `RotatingDpuUefi` state rather than an overloaded
    /// discriminator here.
    RotatingHostUefi {
        uefi_setup_info: UefiSetupInfo,
    },

    /// One of the host's DPUs is converging its own UEFI (BIOS setup) password
    /// to the staged site-wide `dpu_uefi` rotation target. A
    /// pool-only, top-level state (same instance-creation block as
    /// `RotatingHostUefi`), but keyed to a single DPU: applying a DPU UEFI
    /// password stages a `Bios/Settings` change and commits it with a DPU
    /// restart (distinct from a host power-cycle), so the reboot is scoped to
    /// that DPU. Because a host can carry several DPUs, this state names the
    /// `dpu_machine_id` it is converging and processes one DPU per
    /// `Ready -> RotatingDpuUefi -> Ready` cycle; the Ready entry guard
    /// re-selects the next lagging or force-requested DPU on a later sweep.
    ///
    /// Unlike the host's multi-tick `RotatingHostUefi`, a DPU UEFI change is
    /// applied in a single tick -- stage the `Bios/Settings` change, issue the
    /// DPU restart that commits it, then record convergence -- so this state
    /// carries no [`UefiSetupInfo`] sub-state: it names only the DPU it targets
    /// and re-runs idempotently if the controller restarts mid-tick. Per-device
    /// backoff/quarantine is the rotation engine's `device_credential_rotation`
    /// bookkeeping keyed by that DPU's BMC MAC.
    RotatingDpuUefi {
        dpu_machine_id: DpuMachineId,
    },

    /// The host is rekeying its NIC lockdown keys to the staged
    /// site-wide `lockdown_ikm` target. This host state drives the
    /// cards through a tenant-free `RotateKeyUnlocking -> RotateKeyLocking`
    /// cycle and waits for them to converge.
    RotatingNicLockdown,

    /// State used to indicate the API is currently waiting on the
    /// machine to send attestation measurements, or waiting for
    /// measurements to match a valid/approved measurement bundle,
    /// before continuing on towards a Ready state.
    // This is host specific state. We expect DPU to be in Ready state.
    Measuring {
        measuring_state: MeasuringState,
    },

    // this includes MeasuredBoot and SPDM attestations
    PostAssignedMeasuring {
        attestation_mode: AttestationMode,
    },

    // Ready -> PreAssignedMeasuring -> StartAssignmentCycle -> Move into Assigned State(s)
    PreAssignedMeasuring {
        spdm_measuring_state: SpdmMeasuringState,
    },

    StartAssignmentCycle,

    BomValidating {
        bom_validating_state: BomValidating,
    },
}

/// Progress through the managed host decommissioning workflow.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DecommissioningState {
    /// Site Explorer is being suppressed before the destructive reset.
    SuppressingSiteExplorer,
    DeconfiguringHost {
        deconfiguring_state: DeconfiguringHostState,
    },
    DeconfiguringDpus {
        dpu_states: HashMap<MachineId, DeconfiguringDpuState>,
    },
    /// OOB DHCP is suppressed before the host power cycle so post-cycle discovers are ignored.
    SuppressingOobDhcp,
    /// Power-cycles the host to force OOB rediscovery against the pre-cycle suppression.
    PowerCyclingHost,
    /// Powers the host back on after the cycle so OOB rediscovery can proceed.
    PoweringOnHost,
    /// Waiting for the pre-cycle OOB DHCP suppression to be acknowledged.
    WaitingForOobDhcpAcknowledgement,
    /// BMC DHCP is suppressed before the BMC factory reset.
    SuppressingBmcDhcp,
    /// Issues BMC factory resets for the host and its DPUs.
    FactoryResettingBmcs {
        completed: HashSet<MachineId>,
    },
    /// Waiting for the pre-reset BMC DHCP suppression to be acknowledged.
    WaitingForBmcDhcpAcknowledgement,
    /// Managed per-device BMC and DPU credentials are being removed after factory reset.
    DeletingManagedCredentials,
    Decommissioned,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DeconfiguringHostState {
    DisableLockdown,
    RebootAfterLockdown,
    ClearSuperNicLockdown,
    WaitForSuperNicLockdown,
    ClearUefiPassword,
    WaitForUefiPasswordJobScheduled { job_id: String },
    RebootAfterUefiPassword { job_id: String },
    WaitForUefiPasswordJobCompletion { job_id: String },
    ResetUefiSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DeconfiguringDpuState {
    DeletingFromDpf,
    InstallingBfb,
    WaitForInstallComplete { task_id: String },
    WaitingForBootAfterBfbInstall,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AttestationMode {
    MeasuredBoot {
        measuring_state: MeasuringState,
    },
    SpdmAttestation {
        spdm_measuring_state: SpdmMeasuringState,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MachineValidatingState {
    RebootHost {
        validation_id: MachineValidationId,
    },
    MachineValidating {
        context: String,
        id: MachineValidationId,
        completed: usize,
        total: usize,
        #[serde(default = "default_true")]
        is_enabled: bool,
    },
    /// Machine validation ensures the host's boot device config is in place.
    /// When it reads reverted -- however it drifted (changed externally, a
    /// BIOS quirk, or the boot NIC dropping off the BMC's inventory during a
    /// reboot's POST) -- these states correct it, mirroring host boot repair:
    /// unlock the BMC, drive the boot-order flow, re-lock, resume validation.
    PrepareBootRepair {
        validation_id: MachineValidationId,
    },
    UnlockForBootRepair {
        validation_id: MachineValidationId,
        unlock_host_state: UnlockHostState,
    },
    CheckBootConfigForRepair {
        validation_id: MachineValidationId,
    },
    ConfigureBootBios {
        validation_id: MachineValidationId,
        #[serde(default)]
        retry_count: u32,
    },
    WaitingForBootBiosJob {
        validation_id: MachineValidationId,
        bios_config_info: BiosConfigInfo,
    },
    PollingBootBiosSetup {
        validation_id: MachineValidationId,
        #[serde(default)]
        retry_count: u32,
    },
    RepairBootConfig {
        validation_id: MachineValidationId,
        set_boot_order_info: SetBootOrderInfo,
    },
    LockAfterBootRepair {
        validation_id: MachineValidationId,
    },
}

/// Action to take after Ready boot convergence restores lockdown.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReadyBootConfigPostLockAction {
    /// Return to `Prepare` after cleanup, even if DPU network status becomes
    /// current while lockdown is being restored.
    #[serde(rename = "return_to_prepare")]
    ReturnToPrepare,
    /// The boot-config convergence flow could not complete automatically.
    Convergence { failure: String },
    /// An independent host or DPU failure appeared while lockdown was open.
    /// Preserve its original attribution while routing through LockHost.
    Machine {
        machine_id: MachineId,
        details: FailureDetails,
    },
}

/// Progress while a newly ingested host enables Astra (EastWestControl) on
/// its CX9 NICs and waits for the required AC power cycle to take effect.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ConfigureAstraState {
    /// PATCH Oem.Nvidia.EastWestControlEnabled on each declared CX9 NIC.
    #[default]
    EnableNics,
    /// Wait for the host AC power cycle issued after enabling CX9 NICs.
    WaitingForPowercycle,
}

impl Display for ConfigureAstraState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnableNics => write!(f, "EnableNics"),
            Self::WaitingForPowercycle => write!(f, "WaitingForPowercycle"),
        }
    }
}

/// `ReadyBootConfigState` persists progress while an unassigned Ready host
/// converges its desired Redfish boot configuration.
///
/// BIOS and boot-order job details reuse the same model types as HostInit,
/// assigned platform configuration, and validation so controller restarts
/// retain vendor job IDs, recovery substates, and retry budgets.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ReadyBootConfigState {
    /// Observe the target, then inspect lockdown only when convergence may
    /// require a write.
    Prepare,
    /// Disable lockdown, including any vendor-specific reboot and wait.
    UnlockHost {
        #[serde(default)]
        unlock_host_state: UnlockHostState,
    },
    /// Observe BIOS and boot order and select the smallest required update.
    CheckHostConfig,
    /// Run `machine_setup` for the desired boot interface.
    ConfigureBios {
        #[serde(default)]
        retry_count: u32,
    },
    /// Wait for the vendor BIOS configuration job returned by `machine_setup`.
    WaitingForBiosJob { bios_config_info: BiosConfigInfo },
    /// Verify that the BIOS configuration has been applied.
    PollingBiosSetup {
        #[serde(default)]
        retry_count: u32,
    },
    /// Set, apply, and verify boot order.
    SetBootOrder {
        set_boot_order_info: SetBootOrderInfo,
    },
    /// Restore the configured lockdown policy before either conditionally
    /// marking the desired boot-interface version verified or surfacing a
    /// terminal convergence failure.
    LockHost {
        /// Action deferred until lockdown has been restored. Absent on the
        /// successful convergence path. The persisted field keeps its original
        /// name for compatibility with existing controller state.
        #[serde(
            rename = "terminal_failure",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        post_lock_action: Option<ReadyBootConfigPostLockAction>,
    },
    /// Automated convergence could not complete safely after lockdown was
    /// restored. The host remains unavailable until an operator changes its
    /// desired boot interface, starting a fresh pass from
    /// [`ReadyBootConfigState::Prepare`], or successfully completes a
    /// maintenance operation, which returns the host to
    /// [`ManagedHostState::Ready`].
    Failed { failure: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "validation_type", rename_all = "lowercase")]
pub enum ValidationState {
    /// Host machine validation
    /// placeholder for DPU machine validation
    /// TODO: add DPU validation state
    /// SKU validatioon can also be moved here, so that all validation done @ one place
    MachineValidation {
        machine_validation: MachineValidatingState,
    },
}

impl std::fmt::Display for ValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The retry budget for a failed host firmware upgrade: once
/// [`ManagedHostState::HostReprovision`] has consumed this many retries, the
/// machine stays in its failure state until an operator intervenes.
pub const MAX_FIRMWARE_UPGRADE_RETRIES: u32 = 5;

impl ManagedHostState {
    /// Builds the controller state for a requested maintenance operation.
    pub fn maintenance_for_operation(operation: MachineMaintenanceOperation) -> Self {
        Self::Maintenance { operation }
    }

    /// Returns the DPU reprovision states embedded in either host allocation mode.
    pub fn dpu_reprovision_states(&self) -> Option<&DpuReprovisionStates> {
        match self {
            ManagedHostState::DPUReprovision { dpu_states }
            | ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision { dpu_states },
            } => Some(dpu_states),
            _ => None,
        }
    }

    pub fn as_reprovision_state(&self, dpu_id: &DpuMachineId) -> Option<&ReprovisionState> {
        self.dpu_reprovision_states()?.states.get(dpu_id)
    }

    pub fn suppress_dpu_alerts(&self) -> bool {
        matches!(
            self,
            ManagedHostState::DpuDiscoveringState { .. }
                | ManagedHostState::DPUInit { .. }
                | ManagedHostState::DPUReprovision { .. }
        )
    }

    pub fn get_host_repro_retry_count(&self) -> u32 {
        match self {
            ManagedHostState::HostReprovision { retry_count, .. } => *retry_count,
            _ => 0,
        }
    }

    /// True when this machine is in host reprovisioning with no
    /// firmware-upgrade retry budget left (see
    /// [`MAX_FIRMWARE_UPGRADE_RETRIES`]).
    pub fn host_repro_retries_exhausted(&self) -> bool {
        matches!(
            self,
            ManagedHostState::HostReprovision { retry_count, .. }
                if *retry_count >= MAX_FIRMWARE_UPGRADE_RETRIES
        )
    }
}

// Since order is derived, Enum members must be in initial to last state sequence.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ReprovisionState {
    // Deprecated
    BmcFirmwareUpgrade {
        substate: BmcFirmwareUpgradeSubstate,
    },
    // Deprecated
    FirmwareUpgrade,
    DpfStates {
        substate: DpfState,
    },
    InstallDpuOs {
        substate: InstallDpuOsState,
    },
    WaitingForNetworkInstall,
    PoweringOffHost,
    PowerDown,
    // Deprecated
    BufferTime,
    VerifyFirmareVersions,
    WaitingForNetworkConfig,
    PrepareHostBootRepair,
    UnlockHostForBootRepair {
        #[serde(default)]
        unlock_host_state: UnlockHostState,
    },
    CheckHostBootConfig,
    CheckHostBootConfigAfterHostReboot,
    ConfigureHostBoot {
        #[serde(default)]
        retry_count: u32,
    },
    WaitingForHostBiosJob {
        bios_config_info: BiosConfigInfo,
    },
    PollingHostBiosSetup {
        #[serde(default)]
        retry_count: u32,
    },
    SetHostBootOrder {
        set_boot_order_info: SetBootOrderInfo,
    },
    LockHostAfterBootRepair,
    RebootHostBmc,
    RebootHost,
    NotUnderReprovision,
}

pub trait NextStateBFBSupport<A> {
    fn next_substate_based_on_bfb_support(
        enable_secure_boot: bool,
        state: &ManagedHostStateSnapshot,
        dpf_enabled_at_site: bool,
    ) -> A;
}

impl NextStateBFBSupport<DpuDiscoveringState> for DpuDiscoveringState {
    fn next_substate_based_on_bfb_support(
        enable_secure_boot: bool,
        state: &ManagedHostStateSnapshot,
        dpf_enabled_at_site: bool,
    ) -> DpuDiscoveringState {
        // DPF should be given priority over secure boot.
        // DPF does not support Secure boot.
        let is_dpf_based_provisioning_possible =
            dpf_based_dpu_provisioning_possible(state, dpf_enabled_at_site, false);

        if !is_dpf_based_provisioning_possible
            && enable_secure_boot
            && bfb_install_support(&state.dpu_snapshots)
        {
            // Move with a redfish install path
            DpuDiscoveringState::EnableSecureBoot {
                count: 0,
                enable_secure_boot_state: SetSecureBootState::CheckSecureBootStatus,
            }
        } else {
            DpuDiscoveringState::DisableSecureBoot {
                count: 0,
                disable_secure_boot_state: Some(SetSecureBootState::CheckSecureBootStatus),
            }
        }
    }
}

impl NextStateBFBSupport<ReprovisionState> for ReprovisionState {
    fn next_substate_based_on_bfb_support(
        enable_secure_boot: bool,
        state: &ManagedHostStateSnapshot,
        dpf_enabled_at_site: bool,
    ) -> ReprovisionState {
        let bfb_support = bfb_install_support(&state.dpu_snapshots);
        let is_dpf_based_provisioning_possible =
            dpf_based_dpu_provisioning_possible(state, dpf_enabled_at_site, true);
        if is_dpf_based_provisioning_possible {
            ReprovisionState::DpfStates {
                substate: DpfState::Reprovisioning,
            }
        } else if enable_secure_boot && bfb_support {
            tracing::info!("All DPUs support BFB install via Redfish");
            // Move with a redfish install path
            ReprovisionState::InstallDpuOs {
                substate: InstallDpuOsState::InstallingBFB,
            }
        } else {
            ReprovisionState::WaitingForNetworkInstall
        }
    }
}

fn bfb_install_support(dpu_snapshots: &[Machine]) -> bool {
    !dpu_snapshots.is_empty()
        && dpu_snapshots
            .iter()
            .all(|m| m.status.bmc_info.supports_bfb_install())
}

/// MeasuringState contains states used for host attestion (or
/// measured boot).
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MeasuringState {
    /// WaitingForMeasurements is reported when the machine
    /// has reached a state where the API is now expecting
    /// measurements from the machine, which Scout sends upon
    /// receiving an Action::Measure from the API.
    WaitingForMeasurements,

    /// PendingBundle is reported when the API has received
    /// measurements from the machine, but the measurements
    /// do not match a known bundle. At this point, a matching
    /// bundle needs to be created, either via "promoting" a
    /// measurement report from a machine (through manual
    /// interaction or trusted approval automation), or by
    /// manually creating a new bundle.
    PendingBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SpdmMeasuringState {
    TriggerMeasurements,
    PollResult,
}

/// Tenant has requested network config update for the existing instance.
/// At this point, instance config, instance network config version are already increased.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkConfigUpdateState {
    WaitingForNetworkSegmentToBeReady,
    WaitingForConfigSynced,
    // State machine should identify the old resources which needs to be freed and free them.
    ReleaseOldResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HostReprovisionState {
    // deprecated, kept for backwards compatibility with existing database entries: FORGE-7975
    CheckingFirmware,
    // deprecated, kept for backwards compatibility with existing database entries: FORGE-7975
    CheckingFirmwareRepeat,
    CheckingFirmwareV2 {
        firmware_type: Option<FirmwareComponentType>,
        firmware_number: Option<u32>,
    },
    CheckingFirmwareRepeatV2 {
        firmware_type: Option<FirmwareComponentType>,
        firmware_number: Option<u32>,
    },
    InitialReset {
        phase: InitialResetPhase,
        last_time: DateTime<Utc>,
    },
    WaitingForManualUpgrade {
        manual_upgrade_started: DateTime<Utc>,
    },
    WaitingForScript {},
    WaitingForUpload {
        final_version: String,
        firmware_type: FirmwareComponentType,
        power_drains_needed: Option<u32>,
        firmware_number: Option<u32>,
    },
    WaitingForFirmwareUpgrade {
        task_id: String,
        final_version: String,
        firmware_type: FirmwareComponentType,
        power_drains_needed: Option<u32>,
        firmware_number: Option<u32>,
        started_waiting: Option<DateTime<Utc>>,
    },
    ResetForNewFirmware {
        final_version: String,
        firmware_type: FirmwareComponentType,
        firmware_number: Option<u32>,
        power_drains_needed: Option<u32>,
        delay_until: Option<i64>,
        last_power_drain_operation: Option<PowerDrainState>,
        #[serde(default)]
        reset_retry_count: u32,
    },
    NewFirmwareReportedWait {
        final_version: String,
        firmware_type: FirmwareComponentType,
        firmware_number: Option<u32>,
        previous_reset_time: Option<i64>,
        #[serde(default)]
        reset_retry_count: u32,
    },
    FailedFirmwareUpgrade {
        firmware_type: FirmwareComponentType,
        report_time: Option<DateTime<Utc>>,
        reason: Option<String>,
    },
    WaitingForRackFirmwareUpgrade,
    WaitingForScoutUpgrade {
        upgrade_task_id: String,
        firmware_type: FirmwareComponentType,
        final_version: String,
        #[serde(default)]
        power_drains_needed: Option<u32>,
        started_at: DateTime<Utc>,
        /// Absolute deadline; the API declares failure past this time.
        /// Derived from scout's execution/download timeouts plus slack.
        deadline: DateTime<Utc>,
        /// Serialized FirmwareUpgradeTask JSON for the scout
        task_json: String,
        #[serde(default)]
        result: Option<ScoutUpgradeResult>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ScoutUpgradeResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InitialResetPhase {
    Start,
    BMCWasReset,
    WaitHostBoot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PowerDrainState {
    Off,
    Powercycle,
    On,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FailureCause {
    NoError,
    NVMECleanFailed { err: String },
    Discovery { err: String },
    Reprovisioning { err: String },
    MachineValidation { err: String },
    UnhandledState { err: String },

    // Host Attestation / Measured Boot related failure causes.
    //
    // MeasurementsFailedSignatureCheck is returned when the
    // signed PCR quote fails signature verification. That is,
    // we cannot verify the PCR (Platform Configuration Register,
    // in the context of Trusted Platform Modules) values were
    // signed by the TPM. If this state is being reported, a TPM
    // event log should have been dumped by the API for viewing.
    MeasurementsFailedSignatureCheck { err: String },

    // MeasurementsRetired is returned when the measurements
    // provided by the machine match a bundle that has been
    // marked as retired, thus not allowing the machine to
    // move forward towards a Ready state.
    MeasurementsRetired { err: String },

    // MeasurementsRevoked is returned when the measurements
    // provided by the machine match a bundle that has been
    // marked as revoked, thus not allowing the machine to
    // move forward towards a Ready state.
    //
    // The difference between retired and revoked is that a
    // retired bundle can be moved out of retirement, whereas
    // a revoked bundle cannot.
    MeasurementsRevoked { err: String },

    MeasurementsCAValidationFailed { err: String },

    // MeasurementsNotReceived is returned when the host has been in
    // WaitingForMeasurements longer than the configured timeout without
    // Scout ever submitting a report. This typically means the host is
    // not network-booting at all.
    MeasurementsNotReceived { err: String },

    DpfProvisioning { err: String },

    SpdmAttestationFailed { err: String },

    BiosSetupFailed { err: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StateMachineArea {
    Default,
    HostInit,
    MainFlow,
    AssignedInstance,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FailureSource {
    NoError,
    Scout,
    StateMachine,
    StateMachineArea(StateMachineArea),
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct FailureDetails {
    pub cause: FailureCause,
    pub failed_at: DateTime<Utc>,
    pub source: FailureSource,
}

// Since order is derived, Enum members must be in initial to last state sequence.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "bmcfirmwareupdatesubstate", rename_all = "lowercase")]
pub enum BmcFirmwareUpgradeSubstate {
    CheckFwVersion,
    WaitForUpdateCompletion {
        firmware_type: FirmwareComponentType,
        task_id: String,
    },
    Reboot {
        count: u32,
    },
    // Wait for ERoT is not in the middle of a background copy of the new BMC image
    WaitForERoTBackgroundCopyToComplete,
    HostPowerCycle,
    Failed {
        failure_details: String,
    },
    FwUpdateCompleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "dpudiscoverystate", rename_all = "lowercase")]
pub enum DpuDiscoveringState {
    /// Dpu discovery via redfish states
    Initializing,
    Configuring,
    RebootAllDPUS,
    EnableSecureBoot {
        count: u32,
        enable_secure_boot_state: SetSecureBootState,
    },
    DisableSecureBoot {
        // this substate is optional because it was added after DisableSecureBoot was initially created (just in case we have a machine stuck in this state even though we shouldnt)
        disable_secure_boot_state: Option<SetSecureBootState>,
        count: u32,
    },
    SetUefiHttpBoot,
    EnableRshim,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq, Hash, Clone, Ord, PartialOrd)]
#[serde(tag = "installdpuosstate", rename_all = "lowercase")]
pub enum InstallDpuOsState {
    InstallingBFB,
    WaitForInstallComplete { task_id: String, progress: String },
    Completed,
    InstallationError { msg: String },
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq, Hash, Clone, Ord, PartialOrd)]
#[serde(tag = "disablesecurebootstate", rename_all = "lowercase")]
pub enum SetSecureBootState {
    CheckSecureBootStatus,
    DisableSecureBoot, // Deprecated
    SetSecureBoot,
    RebootDPU { reboot_count: u32 },
    WaitCertificateUpload { task_id: String },
}

// Derived ordering gates states through `Init` and selects the least-advanced
// DPU for SLA and status reporting. Host-wide power-cycle phases transition
// every DPU together, so their relative ordering is not observed.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "dpustate", rename_all = "lowercase")]
pub enum DpuInitState {
    InstallDpuOs {
        substate: InstallDpuOsState,
    },
    DpfStates {
        state: DpfState,
    },
    Init,
    WaitingForPlatformPowercycle {
        substate: PerformPowerOperation,
    },
    /// Waits for Redfish to confirm the platform is `Off` before the
    /// idempotent power-on phase begins.
    WaitingForPlatformPowerOff,
    WaitingForPlatformConfiguration,
    PollingBiosSetup,
    WaitingForNetworkConfig,
    WaitingForNetworkInstall, // Deprecated now, not used
}

/// DPF operator integration states.
///
/// The DPF operator manages all internal provisioning logic. Carbide only
/// declares the setup, waits for completion, and handles cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "dpfstate", rename_all = "lowercase")]
pub enum DpfState {
    /// Registering DPU devices and node with DPF operator.
    Provisioning,
    /// Waiting for DPF operator to complete provisioning.
    /// Watcher callbacks drive transitions (DPU ready, reboot required).
    WaitingForReady {
        /// Current DPU phase detail from DPF SDK while Provisioning (for debugging/observability only).
        /// Carbide should not care about non actionable DPF internal phases.
        #[serde(default)]
        phase_detail: Option<String>,
    },
    /// Executing a DPF-requested power cycle. `state.version.timestamp()`
    /// records when this step started; the handler uses it to enforce the
    /// configured power-transition delay (`reachability_params.power_down_wait`).
    HandleReboot {
        op: PerformPowerOperation,
        #[serde(default)]
        retry_count: u32,
    },
    /// DPU device reported ready by the DPF operator. Carbide
    /// waits for all DPUs to reach this state before proceeding.
    DeviceReady,
    /// Triggering reprovisioning via DPF operator.
    Reprovisioning,
    /// Catch-all for unrecognized variant tags stored by a previous implementation.
    /// The state handler transitions this back to `Provisioning`.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum PerformPowerOperation {
    Off,
    On,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum MachineState {
    Init,
    EnableIpmiOverLan,
    WaitingForPlatformConfiguration {
        /// Retries after BIOS job failure remediation; re-run machine_setup from this state.
        #[serde(default)]
        retry_count: u32,
    },
    /// Wait for BIOS config job (Dell) to complete before PollingBiosSetup / SetBootOrder.
    WaitingForBiosJob {
        bios_config_info: BiosConfigInfo,
    },
    PollingBiosSetup {
        #[serde(default)]
        retry_count: u32,
    },
    SetBootOrder {
        set_boot_order_info: Option<SetBootOrderInfo>,
    },
    UefiSetup {
        uefi_setup_info: UefiSetupInfo,
    },
    Measuring {
        measuring_state: MeasuringState,
    },
    SpdmMeasuring {
        spdm_measuring_state: SpdmMeasuringState,
    },
    WaitingForDiscovery,
    Discovered {
        #[serde(default)]
        skip_reboot_wait: bool,
    },
    /// Lockdown handling.
    WaitingForLockdown {
        lockdown_info: LockdownInfo,
    },
    // MachineValidating has been moved to ValidationState
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct LockdownInfo {
    pub state: LockdownState,
    pub mode: LockdownMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct UefiSetupInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uefi_password_jid: Option<String>,
    pub uefi_setup_state: UefiSetupState,
}

/// Substates of enabling/disabling lockdown
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, EnumIter)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum UefiSetupState {
    UnlockHost,
    SetUefiPassword,
    WaitForPasswordJobScheduled,
    PowercycleHost,
    WaitForPasswordJobCompletion,
    // Deprecated: no-op state, transitions directly to WaitingForLockdown::SetLockdown
    // Kept for backwards compatibility with hosts that may be in this state
    LockdownHost,
}

/// Tracks progress waiting for the Dell BIOS config job (from machine_setup PATCH) to complete
/// before configuring boot order. Same pattern as SetBootOrderInfo / SetBootOrderState.
///
/// `bios_job_id` is `Some` while polling a vendor BIOS job (e.g. Dell). `None` only during
/// `HandleBiosJobFailure` recovery from stuck PollingBiosSetup; non-Dell hosts reboot in
/// `configure_host_bios` and never enter job-polling substates.
///
/// Derived ordering is used by enclosing reprovision states to report the least advanced DPU.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub struct BiosConfigInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bios_job_id: Option<String>,
    pub bios_config_state: BiosConfigState,
    /// Shared host boot-configuration convergence retry count, including BIOS
    /// job-failure recovery cycles.
    #[serde(default)]
    pub retry_count: u32,
}

/// Variant order follows BIOS job progression for derived reprovision-state comparisons.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum BiosConfigState {
    WaitForBiosJobScheduled,
    RebootHost,
    WaitForBiosJobCompletion,
    /// Power off → BMC reset → power on when job fails or is scheduled with errors (same as boot order).
    HandleBiosJobFailure {
        failure: String,
        power_state: PowerState,
    },
}

/// Derived ordering is used by enclosing reprovision states to report the least advanced DPU.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub struct SetBootOrderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_boot_order_jid: Option<String>,
    pub set_boot_order_state: SetBootOrderState,
    /// Shared host boot-configuration convergence retry count. Defaults to 0
    /// for backwards compatibility.
    #[serde(default)]
    pub retry_count: u32,
}

/// Variant order follows boot-order job progression for derived reprovision-state comparisons.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SetBootOrderState {
    SetBootOrder,
    /// Legacy persisted state from the former inline HTTP-device repair flow.
    /// It polls the device across the already-started reboot, then either
    /// resumes `SetBootOrder` or migrates to the shared BIOS repair stages.
    WaitForHttpBootDeviceApplied,
    WaitForSetBootOrderJobScheduled,
    RebootHost,
    WaitForSetBootOrderJobCompletion,
    HandleJobFailure {
        failure: String,
        power_state: PowerState,
    },
    CheckBootOrder,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct SecureEraseBossContext {
    pub boss_controller_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure_erase_jid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
    pub secure_erase_boss_state: SecureEraseBossState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SecureEraseBossState {
    UnlockHost,
    SecureEraseBoss,
    WaitForJobCompletion,
    HandleJobFailure {
        failure: String,
        power_state: PowerState,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct CreateBossVolumeContext {
    pub boss_controller_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_boss_volume_jid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
    pub create_boss_volume_state: CreateBossVolumeState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum CreateBossVolumeState {
    CreateBossVolume,
    WaitForJobScheduled,
    RebootHost,
    WaitForJobCompletion,
    HandleJobFailure {
        failure: String,
        power_state: PowerState,
    },
    LockHost,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum CleanupState {
    Init,
    // Only for Dells with BOSS drives (currently on Dell XE9860s). This will also delete the volume on the BOSS controller.
    SecureEraseBoss {
        secure_erase_boss_context: SecureEraseBossContext,
    },
    HostCleanup {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boss_controller_id: Option<String>,
    },
    // Only for Dells with BOSS drives (currently on Dell XE9860s)
    CreateBossVolume {
        create_boss_volume_context: CreateBossVolumeContext,
    },
    // Unused
    DisableBIOSBMCLockdown,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CleanupContext {
    #[default]
    Deprovision,
    InitialDiscovery,
}
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, EnumIter)]
#[serde(rename_all = "lowercase")]
pub enum LockdownState {
    SetLockdown,
    TimeWaitForDPUDown,
    WaitForDPUUp,
    PollingLockdownStatus,
}

/// Whether lockdown should be enabled or disabled in an operation
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")] // No tag required - this will never be nested
pub enum LockdownMode {
    Enable,
    Disable,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")] // No tag required - this will never be nested
pub struct RetryInfo {
    pub count: u64,
}

/// Possible Instance state-machine implementation, for when the machine host is assigned to a tenant
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum InstanceState {
    Init, // Instance is created but not picked by state machine yet.
    // In case of vpc_prefix based interface config, state machine should wait until network
    // segment reaches to Ready state.
    WaitingForNetworkSegmentToBeReady,
    WaitingForNetworkConfig,
    WaitingForStorageConfig,
    DpaProvisioning,
    WaitingForDpaToBeReady,
    WaitingForExtensionServicesConfig,
    WaitingForRebootToReady,
    Ready,
    HostPlatformConfiguration {
        platform_config_state: HostPlatformConfigurationState,
    },
    WaitingForDpusToUp,
    BootingWithDiscoveryImage {
        #[serde(default)]
        retry: RetryInfo,
    },
    SwitchToAdminNetwork,
    WaitingForNetworkReconfig,
    DPUReprovision {
        dpu_states: DpuReprovisionStates,
    },
    Failed {
        details: FailureDetails,
        machine_id: MachineId,
    },
    HostReprovision {
        reprovision_state: HostReprovisionState,
    },
    NetworkConfigUpdate {
        network_config_update_state: NetworkConfigUpdateState,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum HostPlatformConfigurationState {
    PowerCycle {
        power_on: bool,
        /// Number of times we have sent power-on and are still waiting for the host to come up.
        #[serde(default)]
        power_on_retry_count: u32,
    },
    UnlockHost {
        #[serde(default)]
        unlock_host_state: UnlockHostState,
    },
    CheckHostConfig,
    /// Run `machine_setup` / BIOS PATCH; on job ID, transition to [`WaitingForBiosJob`].
    ConfigureBios {
        /// Legacy only: persisted `Some` is migrated to `WaitingForBiosJob` on next handle. New flows use [`WaitingForBiosJob`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bios_config_info: Option<BiosConfigInfo>,
        #[serde(default)]
        retry_count: u32,
    },
    /// Wait for Dell (etc.) BIOS config Redfish job to complete before `PollingBiosSetup` (mirrors HostInit `WaitingForBiosJob`).
    WaitingForBiosJob {
        bios_config_info: BiosConfigInfo,
    },
    PollingBiosSetup {
        #[serde(default)]
        retry_count: u32,
    },
    SetBootOrder {
        set_boot_order_info: SetBootOrderInfo,
    },
    LockHost,
    /// Deletion-only, site-gated BMC factory-reset sub-flow. Runs before
    /// `PowerCycle` to proactively clear wedged BMC state (bad/rejected boot
    /// order). Gating and per-step behavior are handled inside the sub-state
    /// machine; see [`FactoryResetBmcState`]. We have seen BMCs on GB200,
    /// Grace-Grace, and SMC machines that have been wedged in a bad state where
    /// they report an incorrect boot order and do NOT register a new boot
    /// order with the UEFI (https://github.com/NVIDIA/infra-controller/issues/4759).
    FactoryResetBmc {
        #[serde(default)]
        reset_state: FactoryResetBmcState,
    },
}

/// Variant order follows unlock progression for derived reprovision-state comparisons.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd, Ord, Default)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum UnlockHostState {
    #[default]
    DisableLockdown,
    RebootHost,
    WaitForUefiBoot,
}

/// Sub-states of [`HostPlatformConfigurationState::FactoryResetBmc`].
///
/// Ordering of the flow: check the site-config gate and verify the factory
/// credentials are recoverable, suppress site-explorer against the host BMC and
/// wait for the suppression to be acknowledged, issue the factory reset, wait
/// for the BMC to come back, verify the factory credentials and restore the
/// device to its previous per-device credential, then remove the suppression and
/// hand off to `PowerCycle`.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum FactoryResetBmcState {
    /// Entry point, run once. Checks the site-config gate (transparent
    /// pass-through to `PowerCycle` when disabled) and checks for a usable
    /// `expected_machines` factory-credential entry; if none exists the reset is
    /// skipped (straight to `PowerCycle`) rather than parked, since without those
    /// credentials the device could never be restored afterward and blocking the
    /// release on a config gap helps no one. Doing this here, rather than on every
    /// `SuppressExploration` dispatch, keeps the `expected_machines` lookup off
    /// the hot acknowledgement-wait loop.
    #[default]
    CheckPreconditions,
    /// Idempotently suppress site-explorer for the host BMC and wait up to a
    /// fixed budget for it to acknowledge, proceeding anyway once the budget
    /// elapses (a disabled or unavailable site-explorer can never acknowledge,
    /// and the suppression row alone already blocks new exploration). The budget
    /// is measured from the suppression row, so no retry/counter bookkeeping is
    /// needed.
    SuppressExploration,
    /// Issue `Manager.ResetToDefaults` against the BMC using stored credentials.
    ResetToDefaults,
    /// Wait for the BMC to respond to an anonymous service-root read. Polls at
    /// the controller dispatch cadence and never parks; a BMC that never returns
    /// is surfaced by the `HostPlatformConfiguration` time-in-state SLA. The
    /// anonymous probe consumes no auth attempt, so there is no lockout risk and
    /// hence no retry/backoff bookkeeping.
    WaitForBmc,
    /// Verify the factory credentials, then change the BMC root password from the
    /// factory default back to the device's previous per-device credential (read
    /// from Vault; Vault is never written). `retry_count` drives a login backoff
    /// derived from the state version timestamp (immediate first probe, then
    /// 5/10/15/20 min, then hourly) for the factory-credential verification: that
    /// path never parks and degrades to hourly polling to avoid BMC lockout. Once
    /// the factory credentials verify, a genuine password-change failure parks.
    RestoreCredentials {
        #[serde(default)]
        retry_count: u32,
    },
    /// Delete the site-explorer suppression, then hand off to `PowerCycle`.
    RemoveSuppression,
}

/// Struct to store information if Reprovision is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReprovisionRequest {
    pub requested_at: DateTime<Utc>,
    pub initiator: String,
    // Deprecated: Not used anymore. Now fw update is tried in every reprovision request.
    pub update_firmware: bool,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub user_approval_received: bool,
    #[serde(default)]
    pub restart_reprovision_requested_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "lowercase")]
#[allow(clippy::enum_variant_names)]
pub enum MachineMaintenanceOperation {
    /// Power on the host.
    PowerOn,
    /// Power off the host.
    PowerOff,
    /// Reset the host (restart / AC power cycle).
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineMaintenanceRequest {
    pub requested_at: DateTime<Utc>,
    pub initiator: String,
    pub operation: MachineMaintenanceOperation,
}

/// Struct to store information if host reprovision is requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostReprovisionRequest {
    pub requested_at: DateTime<Utc>,
    pub initiator: String,
    pub started_at: Option<DateTime<Utc>>,
    pub user_approval_received: bool,
    pub request_reset: Option<bool>,
}

pub use crate::rack::RackFirmwareUpgradeStatus;

/// Should a forge-dpu-agent upgrade itself?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeDecision {
    pub should_upgrade: bool,
    pub to_version: String,
    pub last_updated: DateTime<Utc>,
}

impl Display for DpuDiscoveringState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for DpuInitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for MachineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for InstanceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for CleanupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for LockdownState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for FailureSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl FailureCause {
    /// Stable snake_case token for use as a metric label value. Unlike
    /// [`Display`], never interpolates the free-text `err` (metric labels must
    /// come from a closed set).
    pub fn metric_label(&self) -> &'static str {
        match self {
            FailureCause::NoError => "no_error",
            FailureCause::NVMECleanFailed { .. } => "nvme_clean_failed",
            FailureCause::Discovery { .. } => "discovery",
            FailureCause::Reprovisioning { .. } => "reprovisioning",
            FailureCause::MachineValidation { .. } => "machine_validation",
            FailureCause::UnhandledState { .. } => "unhandled_state",
            FailureCause::MeasurementsFailedSignatureCheck { .. } => {
                "measurements_failed_signature_check"
            }
            FailureCause::MeasurementsRetired { .. } => "measurements_retired",
            FailureCause::MeasurementsRevoked { .. } => "measurements_revoked",
            FailureCause::MeasurementsCAValidationFailed { .. } => {
                "measurements_ca_validation_failed"
            }
            FailureCause::MeasurementsNotReceived { .. } => "measurements_not_received",
            FailureCause::DpfProvisioning { .. } => "dpf_provisioning",
            FailureCause::SpdmAttestationFailed { .. } => "spdm_attestation_failed",
            FailureCause::BiosSetupFailed { .. } => "bios_setup_failed",
        }
    }
}

impl Display for FailureCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureCause::NVMECleanFailed { .. } => write!(f, "NVMECleanFailed"),
            FailureCause::NoError => write!(f, "NoError"),
            FailureCause::Discovery { .. } => write!(f, "Discovery"),
            FailureCause::Reprovisioning { .. } => write!(f, "Reprovisioning"),
            FailureCause::UnhandledState { .. } => write!(f, "UnknownState"),
            FailureCause::MeasurementsFailedSignatureCheck { .. } => {
                write!(f, "MeasurementsFailedSignatureCheck")
            }
            FailureCause::MeasurementsRetired { .. } => write!(f, "MeasurementsRetired"),
            FailureCause::MeasurementsRevoked { .. } => write!(f, "MeasurementsRevoked"),
            FailureCause::MachineValidation { .. } => write!(f, "MachineValidation"),
            FailureCause::MeasurementsCAValidationFailed { .. } => {
                write!(f, "MeasurementsCAValidationFailed")
            }
            FailureCause::MeasurementsNotReceived { .. } => {
                write!(f, "MeasurementsNotReceived")
            }
            FailureCause::DpfProvisioning { err } => write!(f, "DpfProvisioning {err}"),
            FailureCause::SpdmAttestationFailed { .. } => {
                write!(f, "SpdmAttestationFailed")
            }
            FailureCause::BiosSetupFailed { .. } => write!(f, "BiosSetupFailed"),
        }
    }
}

impl Display for FailureDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.source, self.cause)
    }
}

impl Display for ReprovisionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for HostReprovisionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for MeasuringState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for SpdmMeasuringState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for ReadyBootConfigState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Prepare => "Prepare",
            Self::UnlockHost { .. } => "UnlockHost",
            Self::CheckHostConfig => "CheckHostConfig",
            Self::ConfigureBios { .. } => "ConfigureBios",
            Self::WaitingForBiosJob { .. } => "WaitingForBiosJob",
            Self::PollingBiosSetup { .. } => "PollingBiosSetup",
            Self::SetBootOrder { .. } => "SetBootOrder",
            Self::LockHost { .. } => "LockHost",
            Self::Failed { .. } => "Failed",
        };
        f.write_str(name)
    }
}

impl Display for DecommissioningState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecommissioningState::SuppressingSiteExplorer => write!(f, "SuppressingSiteExplorer"),
            DecommissioningState::DeconfiguringHost {
                deconfiguring_state,
            } => write!(f, "DeconfiguringHost/{deconfiguring_state:?}"),
            DecommissioningState::DeconfiguringDpus { .. } => write!(f, "DeconfiguringDpus"),
            DecommissioningState::SuppressingOobDhcp => write!(f, "SuppressingOobDhcp"),
            DecommissioningState::PowerCyclingHost => write!(f, "PowerCyclingHost"),
            DecommissioningState::PoweringOnHost => write!(f, "PoweringOnHost"),
            DecommissioningState::WaitingForOobDhcpAcknowledgement => {
                write!(f, "WaitingForOobDhcpAcknowledgement")
            }
            DecommissioningState::SuppressingBmcDhcp => write!(f, "SuppressingBmcDhcp"),
            DecommissioningState::FactoryResettingBmcs { .. } => write!(f, "FactoryResettingBmcs"),
            DecommissioningState::WaitingForBmcDhcpAcknowledgement => {
                write!(f, "WaitingForBmcDhcpAcknowledgement")
            }
            DecommissioningState::DeletingManagedCredentials => {
                write!(f, "DeletingManagedCredentials")
            }
            DecommissioningState::Decommissioned => write!(f, "Decommissioned"),
        }
    }
}

impl Display for ManagedHostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagedHostState::ConfigureAstra {
                configure_astra_state,
            } => {
                write!(f, "ConfigureAstra/{configure_astra_state}")
            }
            ManagedHostState::DpuDiscoveringState { dpu_states } => {
                // Min state indicates the least processed DPU. The state machine is blocked
                // becasue of this.
                let dpu_lowest_state = dpu_states
                    .states
                    .values()
                    .min()
                    .map(|x| x.to_string())
                    .unwrap_or("Unknown".to_string());

                write!(f, "DPUDiscovering/{dpu_lowest_state}")
            }
            ManagedHostState::DPUInit { dpu_states } => {
                let dpu_lowest_state = dpu_states
                    .states
                    .values()
                    .min()
                    .map(|x| x.to_string())
                    .unwrap_or("Unknown".to_string());
                write!(f, "DPUInitializing/{dpu_lowest_state}")
            }
            ManagedHostState::HostInit { machine_state } => {
                write!(f, "HostInitializing/{machine_state}")
            }
            ManagedHostState::Ready => write!(f, "Ready"),
            ManagedHostState::Decommissioning {
                decommissioning_state,
            } => write!(f, "Decommissioning/{decommissioning_state}"),
            ManagedHostState::BootConfiguring {
                boot_config_state, ..
            } => {
                write!(f, "BootConfiguring/{boot_config_state}")
            }
            ManagedHostState::Maintenance { operation } => {
                write!(f, "Maintenance({operation:?})")
            }
            ManagedHostState::Assigned { instance_state, .. } => match instance_state {
                InstanceState::DPUReprovision { dpu_states } => {
                    let dpu_lowest_state = dpu_states
                        .states
                        .values()
                        .min()
                        .map(|x| x.to_string())
                        .unwrap_or("Unknown".to_string());
                    write!(f, "Assigned/Reprovision/{dpu_lowest_state}")
                }
                _ => {
                    write!(f, "Assigned/{instance_state}")
                }
            },
            ManagedHostState::WaitingForCleanup { cleanup_state, .. } => {
                write!(f, "WaitingForCleanup/{cleanup_state}")
            }
            ManagedHostState::ForceDeletion => write!(f, "ForceDeletion"),
            ManagedHostState::Failed { details, .. } => {
                write!(f, "Failed/{}", details.cause)
            }
            ManagedHostState::DPUReprovision { dpu_states } => {
                let dpu_lowest_state = dpu_states
                    .states
                    .values()
                    .min()
                    .map(|x| x.to_string())
                    .unwrap_or("Unknown".to_string());
                write!(f, "Reprovisioning/{dpu_lowest_state}")
            }
            ManagedHostState::HostReprovision {
                reprovision_state, ..
            } => {
                write!(f, "HostReprovisioning/{reprovision_state}")
            }
            ManagedHostState::RotatingBmc { .. } => write!(f, "RotatingBmc"),
            ManagedHostState::RotatingHostUefi { uefi_setup_info } => {
                write!(f, "RotatingHostUefi/{:?}", uefi_setup_info.uefi_setup_state)
            }
            ManagedHostState::RotatingDpuUefi { dpu_machine_id } => {
                write!(f, "RotatingDpuUefi/{dpu_machine_id}")
            }
            ManagedHostState::RotatingNicLockdown => write!(f, "RotatingNicLockdown"),
            ManagedHostState::Measuring { measuring_state } => {
                write!(f, "Measuring/{measuring_state}")
            }
            ManagedHostState::PostAssignedMeasuring { attestation_mode } => {
                match attestation_mode {
                    AttestationMode::MeasuredBoot { measuring_state } => {
                        write!(f, "PostAssignedMeasuring/MeasuredBoot/{measuring_state}")
                    }
                    AttestationMode::SpdmAttestation {
                        spdm_measuring_state,
                    } => write!(
                        f,
                        "PostAssignedMeasuring/SpdmAttestation/{spdm_measuring_state}"
                    ),
                }
            }
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state,
            } => {
                write!(f, "PreAssignedMeasuring/{spdm_measuring_state}")
            }
            ManagedHostState::Created => write!(f, "Created"),
            ManagedHostState::BomValidating {
                bom_validating_state,
            } => {
                write!(f, "BomValidating/{bom_validating_state:?}")
            }
            ManagedHostState::Validation { validation_state } => {
                write!(f, "{validation_state}")
            }
            ManagedHostState::StartAssignmentCycle => {
                write!(f, "StartAssignmentCycle")
            }
        }
    }
}

impl ManagedHostState {
    pub fn dpu_state_string(&self, dpu_id: &DpuMachineId) -> String {
        match self {
            ManagedHostState::ConfigureAstra {
                configure_astra_state,
            } => format!("ConfigureAstra/{configure_astra_state}"),
            ManagedHostState::DpuDiscoveringState { dpu_states } => dpu_states
                .states
                .get(dpu_id)
                .map(|x| x.to_string())
                .unwrap_or("Unknown DPU".to_string()),
            ManagedHostState::DPUInit { dpu_states } => format!(
                "DPUInitializing/{}",
                dpu_states
                    .states
                    .get(dpu_id)
                    .map(|x| x.to_string())
                    .unwrap_or("Unknown DPU".to_string())
            ),
            ManagedHostState::HostInit { machine_state } => {
                format!("HostInitializing/{machine_state}")
            }
            ManagedHostState::Ready => "Ready".to_string(),
            ManagedHostState::Decommissioning {
                decommissioning_state,
            } => format!("Decommissioning/{decommissioning_state}"),
            ManagedHostState::BootConfiguring {
                boot_config_state, ..
            } => {
                format!("BootConfiguring/{boot_config_state}")
            }
            ManagedHostState::Maintenance { operation } => {
                format!("Maintenance({operation:?})")
            }
            ManagedHostState::Assigned { instance_state } => match instance_state {
                InstanceState::DPUReprovision { dpu_states } => {
                    format!(
                        "Assigned/Reprovision/{}",
                        dpu_states
                            .states
                            .get(dpu_id)
                            .map(|x| x.to_string())
                            .unwrap_or("Unknown DPU".to_string())
                    )
                }
                _ => format!("Assigned/{instance_state}"),
            },
            ManagedHostState::WaitingForCleanup { cleanup_state, .. } => {
                format!("WaitingForCleanup/{cleanup_state}")
            }
            ManagedHostState::ForceDeletion => "ForceDeletion".to_string(),
            ManagedHostState::Failed { details, .. } => {
                format!("Failed/{}", details.cause)
            }
            ManagedHostState::DPUReprovision { dpu_states } => {
                format!(
                    "Reprovisioning/{}",
                    dpu_states
                        .states
                        .get(dpu_id)
                        .map(|x| x.to_string())
                        .unwrap_or("Unknown DPU".to_string())
                )
            }
            ManagedHostState::HostReprovision {
                reprovision_state, ..
            } => {
                format!("HostReprovisioning/{reprovision_state}")
            }
            ManagedHostState::RotatingBmc { .. } => "RotatingBmc".to_string(),
            ManagedHostState::RotatingHostUefi { .. } => "RotatingHostUefi".to_string(),
            ManagedHostState::RotatingDpuUefi { .. } => "RotatingDpuUefi".to_string(),
            ManagedHostState::RotatingNicLockdown => "RotatingNicLockdown".to_string(),
            ManagedHostState::Measuring { measuring_state } => {
                format!("Measuring/{measuring_state}")
            }
            ManagedHostState::PostAssignedMeasuring { attestation_mode } => {
                match attestation_mode {
                    AttestationMode::MeasuredBoot { measuring_state } => {
                        format!("PostAssignedMeasuring/MeasuredBoot/{measuring_state}")
                    }
                    AttestationMode::SpdmAttestation {
                        spdm_measuring_state,
                    } => format!("PostAssignedMeasuring/SpdmAttestation/{spdm_measuring_state}"),
                }
            }
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state,
            } => {
                format!("PreAssignedMeasuring/{spdm_measuring_state}")
            }
            ManagedHostState::Created => "Created".to_string(),
            ManagedHostState::BomValidating {
                bom_validating_state,
            } => format!("BomValidating/{bom_validating_state:?}"),
            ManagedHostState::Validation { validation_state } => {
                format!("{validation_state}")
            }
            ManagedHostState::StartAssignmentCycle => "StartAssignmentCycle".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInterfaceSnapshot {
    pub id: MachineInterfaceId,
    pub hostname: String,
    pub interface_type: InterfaceType,
    pub primary_interface: bool,
    pub mac_address: MacAddress,
    /// Vendor-native Redfish `EthernetInterface.Id` for this interface, captured
    /// by site-explorer alongside the MAC. Combined with `mac_address` it forms a
    /// [`MachineBootInterface`]; for the `primary_interface` row that pair is the
    /// host's boot device.
    pub boot_interface_id: Option<String>,
    pub attached_dpu_machine_id: Option<DpuMachineId>,
    pub domain_id: Option<DomainId>,
    pub machine_id: Option<MachineId>,
    pub segment_id: NetworkSegmentId,
    pub vendors: Vec<String>,
    pub created: DateTime<Utc>,
    pub last_dhcp: Option<DateTime<Utc>>,
    pub addresses: Vec<IpAddr>,
    // Note: this field is denormalized, brought in from a JOIN when coming from machine_interface::find_by. It is otherwise not set.
    pub network_segment_type: Option<NetworkSegmentType>,
    pub power_shelf_id: Option<PowerShelfId>,
    pub switch_id: Option<SwitchId>,
    pub association_type: Option<InterfaceAssociationType>,
}

impl MachineInterfaceSnapshot {
    /// This row's [`MachineBootInterface`]: its MAC plus its captured Redfish
    /// interface id. `None` until site-explorer has recorded the id from an
    /// exploration report.
    pub fn boot_interface(&self) -> Option<MachineBootInterface> {
        MachineBootInterface::for_mac(self.mac_address, self.boot_interface_id.clone())
    }

    pub fn mock_with_mac(mac_address: MacAddress) -> Self {
        Self {
            id: MachineInterfaceId::from(uuid::Uuid::nil()),
            attached_dpu_machine_id: None,
            domain_id: None,
            machine_id: None,
            segment_id: uuid::Uuid::nil().into(),
            mac_address,
            boot_interface_id: None,
            hostname: String::new(),
            interface_type: InterfaceType::Data,
            primary_interface: true,
            addresses: Vec::new(),
            vendors: Vec::new(),
            created: chrono::DateTime::default(),
            last_dhcp: None,
            network_segment_type: None,
            power_shelf_id: None,
            switch_id: None,
            association_type: None,
        }
    }
}

pub struct DpuInitNextStateResolver;
pub struct InstanceNextStateResolver;
pub struct MachineNextStateResolver;

/// Returns the SLA for the current state.
///
/// If any alert in `aggregate_health` carries the `ExcludeFromStateMachineSla` classification,
/// no SLA applies regardless of the current state. This allows operators to suppress
/// SLA violations during manual operations without stopping the state machine.
pub fn state_sla(
    machine_id: &MachineId,
    state: &ManagedHostState,
    state_version: &ConfigVersion,
    aggregate_health: &health_report::HealthReport,
    sla_config: &slas::MachineSlaConfig,
) -> StateSla {
    let exclude = health_report::HealthAlertClassification::exclude_from_state_machine_sla();
    if aggregate_health
        .alerts
        .iter()
        .any(|a| a.classifications.contains(&exclude))
    {
        tracing::debug!(
            machine_id = %machine_id,
            health_alert_classification = %exclude,
            "Skipping state machine SLA due to classification",
        );
        return StateSla::no_sla();
    }

    let time_in_state = chrono::Utc::now()
        .signed_duration_since(state_version.timestamp())
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(60 * 60 * 24));

    match state {
        ManagedHostState::ConfigureAstra { .. } => {
            StateSla::with_sla(slas::CONFIGURE_ASTRA, time_in_state)
        }
        ManagedHostState::DpuDiscoveringState { dpu_states } => {
            // Min state indicates the least processed DPU. The state machine is blocked
            // because of this.
            let dpu_state = dpu_states.states.values().min();
            let Some(dpu_state) = dpu_state else {
                return StateSla::no_sla();
            };

            match dpu_state {
                DpuDiscoveringState::Initializing
                | DpuDiscoveringState::Configuring
                | DpuDiscoveringState::EnableSecureBoot { .. }
                | DpuDiscoveringState::DisableSecureBoot { .. }
                | DpuDiscoveringState::SetUefiHttpBoot
                | DpuDiscoveringState::RebootAllDPUS
                | DpuDiscoveringState::EnableRshim => {
                    StateSla::with_sla(slas::DPUDISCOVERING, time_in_state)
                }
            }
        }
        ManagedHostState::DPUInit { dpu_states } => {
            // Min state indicates the least processed DPU. The state machine is blocked
            // because of this.
            let dpu_state = dpu_states.states.values().min();
            let Some(dpu_state) = dpu_state else {
                return StateSla::no_sla();
            };

            // Init has no SLA since starting discovery requires a manual action
            match dpu_state {
                DpuInitState::Init => StateSla::no_sla(),
                _ => StateSla::with_sla(slas::DPUINIT_NOTINIT, time_in_state),
            }
        }
        ManagedHostState::HostInit { machine_state } => match machine_state {
            MachineState::Init => StateSla::no_sla(),
            _ => StateSla::with_sla(slas::HOST_INIT, time_in_state),
        },
        ManagedHostState::Ready => StateSla::no_sla(),
        ManagedHostState::Decommissioning {
            decommissioning_state,
        } => match decommissioning_state {
            DecommissioningState::SuppressingSiteExplorer => StateSla::with_sla(
                slas::DECOMMISSIONING_SUPPRESSING_SITE_EXPLORER,
                time_in_state,
            ),
            DecommissioningState::DeconfiguringHost { .. } => {
                StateSla::with_sla(slas::DECOMMISSIONING_DECONFIGURING_HOST, time_in_state)
            }
            DecommissioningState::DeconfiguringDpus { .. } => {
                StateSla::with_sla(slas::DECOMMISSIONING_DECONFIGURING_DPUS, time_in_state)
            }
            DecommissioningState::SuppressingOobDhcp => {
                StateSla::with_sla(slas::DECOMMISSIONING_SUPPRESSING_OOB_DHCP, time_in_state)
            }
            DecommissioningState::PowerCyclingHost => {
                StateSla::with_sla(slas::DECOMMISSIONING_POWER_CYCLING_HOST, time_in_state)
            }
            DecommissioningState::PoweringOnHost => {
                StateSla::with_sla(slas::DECOMMISSIONING_POWERING_ON_HOST, time_in_state)
            }
            DecommissioningState::WaitingForOobDhcpAcknowledgement => StateSla::with_sla(
                slas::DECOMMISSIONING_WAITING_FOR_OOB_DHCP_ACKNOWLEDGEMENT,
                time_in_state,
            ),
            DecommissioningState::SuppressingBmcDhcp => {
                StateSla::with_sla(slas::DECOMMISSIONING_SUPPRESSING_BMC_DHCP, time_in_state)
            }
            DecommissioningState::FactoryResettingBmcs { .. } => {
                StateSla::with_sla(slas::DECOMMISSIONING_FACTORY_RESETTING_BMCS, time_in_state)
            }
            DecommissioningState::WaitingForBmcDhcpAcknowledgement => StateSla::with_sla(
                slas::DECOMMISSIONING_WAITING_FOR_BMC_DHCP_ACKNOWLEDGEMENT,
                time_in_state,
            ),
            DecommissioningState::DeletingManagedCredentials => StateSla::with_sla(
                slas::DECOMMISSIONING_DELETING_MANAGED_CREDENTIALS,
                time_in_state,
            ),
            DecommissioningState::Decommissioned => StateSla::no_sla(),
        },
        ManagedHostState::BootConfiguring {
            boot_config_state: ReadyBootConfigState::Failed { .. },
            ..
        } => StateSla::with_sla(std::time::Duration::ZERO, time_in_state),
        ManagedHostState::BootConfiguring { .. } => {
            StateSla::with_sla(slas::BOOT_CONFIGURING, time_in_state)
        }
        ManagedHostState::Maintenance { .. } => {
            StateSla::with_sla(slas::MAINTENANCE, time_in_state)
        }
        ManagedHostState::Assigned { instance_state } => match instance_state {
            InstanceState::Ready => StateSla::no_sla(),
            InstanceState::BootingWithDiscoveryImage { retry } => {
                if retry.count > 1 {
                    StateSla::with_sla(std::time::Duration::ZERO, time_in_state)
                } else {
                    StateSla::with_sla(
                        sla_config.assigned_booting_with_discovery_image,
                        time_in_state,
                    )
                }
            }
            InstanceState::HostPlatformConfiguration { .. } => {
                StateSla::with_sla(slas::ASSIGNED_HOST_PLATFORM_CONFIGURATION, time_in_state)
            }
            _ => StateSla::with_sla(slas::ASSIGNED, time_in_state),
        },
        ManagedHostState::WaitingForCleanup { .. } => {
            StateSla::with_sla(slas::WAITING_FOR_CLEANUP, time_in_state)
        }
        ManagedHostState::Created => StateSla::with_sla(slas::CREATED, time_in_state),
        ManagedHostState::ForceDeletion => StateSla::with_sla(slas::FORCE_DELETION, time_in_state),
        ManagedHostState::Failed { .. } => {
            StateSla::with_sla(std::time::Duration::ZERO, time_in_state)
        }
        ManagedHostState::DPUReprovision { .. } => {
            StateSla::with_sla(slas::DPU_REPROVISION, time_in_state)
        }
        ManagedHostState::HostReprovision { .. } => {
            // Multiple types of firmware may need to be updated, and in some cases it can take a while.
            // This SHOULD be enough based on current observed behavior, but may need to be extended.
            StateSla::with_sla(slas::HOST_REPROVISION, time_in_state)
        }
        ManagedHostState::RotatingBmc { .. } => {
            StateSla::with_sla(slas::ROTATING_BMC, time_in_state)
        }
        ManagedHostState::RotatingHostUefi { .. } => {
            StateSla::with_sla(slas::ROTATING_HOST_UEFI, time_in_state)
        }
        ManagedHostState::RotatingDpuUefi { .. } => {
            StateSla::with_sla(slas::ROTATING_DPU_UEFI, time_in_state)
        }
        ManagedHostState::RotatingNicLockdown => {
            StateSla::with_sla(slas::ROTATING_NIC_LOCKDOWN, time_in_state)
        }
        ManagedHostState::Measuring { measuring_state } => match measuring_state {
            // The API shouldn't be waiting for measurements for long. As soon
            // as it transitions into this state, Scout should get an Action::Measure
            // action, and it should pretty quickly send measurements in (~seconds).
            MeasuringState::WaitingForMeasurements => {
                StateSla::with_sla(slas::MEASUREMENT_WAIT_FOR_MEASUREMENT, time_in_state)
            }
            // If the machine is waiting for a matching bundle, this could
            // take a bit, since it means either auto-bundle generation OR
            // manual bundle generation needs to happen. In the case of new
            // turn ups, this could take hours or even days (e.g. if new gear
            // is sitting there).
            MeasuringState::PendingBundle => StateSla::no_sla(),
        },
        ManagedHostState::PostAssignedMeasuring { attestation_mode } => match attestation_mode {
            AttestationMode::MeasuredBoot { measuring_state } => match measuring_state {
                // The API shouldn't be waiting for measurements for long. As soon
                // as it transitions into this state, Scout should get an Action::Measure
                // action, and it should pretty quickly send measurements in (~seconds).
                MeasuringState::WaitingForMeasurements => {
                    StateSla::with_sla(slas::MEASUREMENT_WAIT_FOR_MEASUREMENT, time_in_state)
                }
                // If the machine is waiting for a matching bundle, this could
                // take a bit, since it means either auto-bundle generation OR
                // manual bundle generation needs to happen. In the case of new
                // turn ups, this could take hours or even days (e.g. if new gear
                // is sitting there).
                MeasuringState::PendingBundle => StateSla::no_sla(),
            },
            AttestationMode::SpdmAttestation {
                spdm_measuring_state,
            } => match spdm_measuring_state {
                SpdmMeasuringState::PollResult => {
                    StateSla::with_sla(slas::SPDM_ATTESTATION_RESULT_POLL, time_in_state)
                }
                SpdmMeasuringState::TriggerMeasurements => {
                    StateSla::with_sla(slas::SPDM_ATTESTATION_TRIGGER, time_in_state)
                }
            },
        },
        ManagedHostState::PreAssignedMeasuring {
            spdm_measuring_state,
        } => match spdm_measuring_state {
            SpdmMeasuringState::PollResult => {
                StateSla::with_sla(slas::SPDM_ATTESTATION_RESULT_POLL, time_in_state)
            }
            SpdmMeasuringState::TriggerMeasurements => {
                StateSla::with_sla(slas::SPDM_ATTESTATION_TRIGGER, time_in_state)
            }
        },
        ManagedHostState::StartAssignmentCycle => {
            StateSla::with_sla(slas::START_ASSIGNMENT_CYCLE, time_in_state)
        }
        ManagedHostState::BomValidating {
            bom_validating_state,
        } => match bom_validating_state {
            BomValidating::SkuVerificationFailed(_bom_validating_context) => StateSla::no_sla(),
            BomValidating::WaitingForSkuAssignment(_bom_validating_context) => StateSla::no_sla(),
            _ => StateSla::with_sla(slas::BOM_VALIDATION, time_in_state),
        },
        ManagedHostState::Validation { validation_state } => match validation_state {
            ValidationState::MachineValidation { machine_validation } => match machine_validation {
                MachineValidatingState::MachineValidating { .. } => {
                    StateSla::with_sla(slas::VALIDATION, time_in_state)
                }
                MachineValidatingState::RebootHost { .. } => {
                    StateSla::with_sla(slas::VALIDATION, time_in_state)
                }
                MachineValidatingState::PrepareBootRepair { .. }
                | MachineValidatingState::UnlockForBootRepair { .. }
                | MachineValidatingState::CheckBootConfigForRepair { .. }
                | MachineValidatingState::ConfigureBootBios { .. }
                | MachineValidatingState::WaitingForBootBiosJob { .. }
                | MachineValidatingState::PollingBootBiosSetup { .. }
                | MachineValidatingState::RepairBootConfig { .. }
                | MachineValidatingState::LockAfterBootRepair { .. } => {
                    StateSla::with_sla(slas::VALIDATION, time_in_state)
                }
            },
        },
    }
}

/// A context for passing information between states thoughout the BOM validation
/// process.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct BomValidatingContext {
    // Machine validation works differently depending on how it is started.  In order
    // to preserve that behavior BOM validation must carry that context through
    // so that machine validation works properly.  Additionally, "None" may be
    // used to skip machine validation.  Note that "None" is not a valid
    // context for machine validation, but only services to skip it.
    pub machine_validation_context: Option<MachineValidationContext>,
    pub reboot_retry_count: Option<i64>,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum MachineValidationContext {
    Discovery,
    Cleanup,
    OnDemand,
}

impl AsRef<str> for MachineValidationContext {
    fn as_ref(&self) -> &str {
        match self {
            MachineValidationContext::Discovery => "Discovery",
            MachineValidationContext::Cleanup => "Cleanup",
            MachineValidationContext::OnDemand => "OnDemand",
        }
    }
}

impl Display for MachineValidationContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum BomValidating {
    MatchingSku(BomValidatingContext),
    UpdatingInventory(BomValidatingContext),
    VerifyingSku(BomValidatingContext),
    SkuVerificationFailed(BomValidatingContext),
    WaitingForSkuAssignment(BomValidatingContext),
    SkuMissing(BomValidatingContext),
}

/// Represents the machine validation test filter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MachineValidationFilter {
    pub tags: Vec<String>,
    pub allowed_tests: Vec<String>,
    pub run_unverfied_tests: Option<bool>,
    pub contexts: Option<Vec<String>>,
}

impl Display for MachineValidationFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

pub struct LoadSnapshotOptions {
    /// Whether to also load the Machines history
    pub include_history: bool,
    /// Whether to load instance details
    pub include_instance_data: bool,
    /// How to use hardware health for health report aggregation
    pub host_health_config: HostHealthConfig,
}

impl Default for LoadSnapshotOptions {
    fn default() -> Self {
        Self {
            include_history: false,
            include_instance_data: true,
            host_health_config: Default::default(),
        }
    }
}

impl LoadSnapshotOptions {
    pub fn with_host_health(mut self, value: HostHealthConfig) -> Self {
        self.host_health_config = value;
        self
    }
}

impl<'r> FromRow<'r, PgRow> for MachineInterfaceSnapshot {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        // Note: Make sure to use the MACHINE_INTERFACE_SNAPSHOT_QUERY when querying, or these
        // columns will not be present in the result.
        let addrs_json: sqlx::types::Json<Vec<Option<IpAddr>>> = row.try_get("addresses")?;
        let vendors_json: sqlx::types::Json<Vec<Option<String>>> = row.try_get("vendors")?;

        Ok(MachineInterfaceSnapshot {
            id: row.try_get("id")?,
            attached_dpu_machine_id: row.try_get("attached_dpu_machine_id")?,
            machine_id: row.try_get("machine_id")?,
            segment_id: row.try_get("segment_id")?,
            domain_id: row.try_get("domain_id")?,
            hostname: row.try_get("hostname")?,
            interface_type: row.try_get("interface_type")?,
            mac_address: row.try_get("mac_address")?,
            boot_interface_id: row.try_get("boot_interface_id")?,
            primary_interface: row.try_get("primary_interface")?,
            created: row.try_get("created")?,
            last_dhcp: row.try_get("last_dhcp")?,
            network_segment_type: row.try_get("network_segment_type")?,
            addresses: addrs_json.0.into_iter().flatten().collect(),
            vendors: vendors_json.0.into_iter().flatten().collect(),
            power_shelf_id: row.try_get("power_shelf_id")?,
            switch_id: row.try_get("switch_id")?,
            association_type: row.try_get("association_type")?,
        })
    }
}

// TODO: reconcile with site_explorer::PowerState. They are almost
// identical but here we have Reset enum item.
/// Variant order is a deterministic tie-breaker inside derived recovery-state comparisons.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PowerState {
    Off,
    On,
    PoweringOff,
    PoweringOn,
    Paused,
    Reset,
    Unknown,
}

impl Display for PowerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostHealthConfig {
    /// Whether or not to use hardware health reports in aggregate health reports
    /// and for restricting state transitions.
    #[serde(default)]
    pub hardware_health_reports: HardwareHealthReportsConfig,
    /// How old a DPU agent's version should be before considering stale
    #[serde(
        default = "HostHealthConfig::dpu_agent_version_staleness_threshold_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub dpu_agent_version_staleness_threshold: Duration,

    /// Whether to fail health checks if a DPU agent version is stale
    #[serde(default)]
    pub prevent_allocations_on_stale_dpu_agent_version: bool,

    /// Whether the scout heartbeat timeout alert should prevent allocations
    #[serde(default)]
    pub prevent_allocations_on_scout_heartbeat_timeout: bool,

    /// Whether the scout heartbeat timeout alert should suppress external alerting
    #[serde(default = "HostHealthConfig::default_suppress_ext_alert_on_scout_heartbeat")]
    pub suppress_external_alerting_on_scout_heartbeat_timeout: bool,
}

/// As of now, chrono::Duration does not support Serialization, so we have to handle it manually.
fn as_duration<S>(d: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{}s", d.num_seconds()))
}

impl Default for HostHealthConfig {
    fn default() -> Self {
        HostHealthConfig {
            hardware_health_reports: HardwareHealthReportsConfig::default(),
            dpu_agent_version_staleness_threshold:
                Self::dpu_agent_version_staleness_threshold_default(),
            prevent_allocations_on_stale_dpu_agent_version: false,
            prevent_allocations_on_scout_heartbeat_timeout: false,
            suppress_external_alerting_on_scout_heartbeat_timeout:
                Self::default_suppress_ext_alert_on_scout_heartbeat(),
        }
    }
}

impl HostHealthConfig {
    pub fn dpu_agent_version_staleness_threshold_default() -> Duration {
        Duration::days(1)
    }

    fn default_suppress_ext_alert_on_scout_heartbeat() -> bool {
        true
    }
}

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq)]
pub enum HardwareHealthReportsConfig {
    #[default]
    Disabled,
    /// Include successes and alerts but remove their classifications
    MonitorOnly,
    /// Include successes, alerts, and classifications.
    Enabled,
}

pub fn dpf_based_dpu_provisioning_possible(
    state: &ManagedHostStateSnapshot,
    dpf_enabled_at_site: bool,
    reprovisioning_case: bool,
) -> bool {
    // DPF is disabled at site.
    if !dpf_enabled_at_site {
        return false;
    }

    // DPF should be enabled for host.
    if !state.host_snapshot.config.dpf.enabled {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "DPF based DPU provisioning is not possible because DPF is not enabled for the host.",
        );
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
            "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
        );

        return false;
    }

    if state.dpu_snapshots.is_empty() {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "DPF based DPU provisioning is not possible because the host has no DPUs.",
        );
        return false;
    }

    // if it is reprovisioning case, initial ingestion should be done with dpf
    // to continue or we should be trying to reprovision all the dpus (switching
    // to DPF). Reprovisioning only a subset of DPUs cannot flip the host to DPF.
    if reprovisioning_case
        && !state.host_snapshot.config.dpf.used_for_ingestion
        && !state
            .dpu_snapshots
            .iter()
            .all(|dpu| dpu.reprovision_requested.is_some())
    {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "DPF based DPU reprovisioning is not possible for host because initial ingestion is not done with DPF and not all DPUs are being reprovisioned.",
        );
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
            "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
        );
        return false;
    }

    // Flipping a host to DPF also flips the extension-service delivery path
    // from the DPU agent to DPUDevice placement labels. Attachments admitted
    // against the agent path cannot follow that flip -- only a detach moves
    // them -- so keep such a host on the legacy path rather than stranding
    // services the new path will never reconcile. Any service attached to a
    // host that is not yet DPF-managed is agent-delivered by admission, so no
    // service-type lookup is needed here.
    if reprovisioning_case
        && !state.host_snapshot.config.dpf.used_for_ingestion
        && state.instance.as_ref().is_some_and(|instance| {
            !instance
                .config
                .extension_services
                .service_configs
                .is_empty()
        })
    {
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            "DPF based DPU reprovisioning is not possible for host because its instance has attached extension services; detach them before migrating the host to DPF.",
        );
        return false;
    }

    // All DPUs should not be Bluefield 2.
    if state.dpu_snapshots.iter().any(|dpu| {
        dpu.status
            .hardware_info
            .as_ref()
            .and_then(|hardware_info| hardware_info.dpu_info.as_ref())
            .map(|dpu_data| crate::site_explorer::is_bf2_dpu_part_number(&dpu_data.part_number))
            .unwrap_or(false)
    }) {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "DPF-based DPU provisioning is not possible because some DPUs are BlueField-2",
        );
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
            "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
        );
        return false;
    }

    // All DPUs support BFB install via Redfish.
    if !state
        .dpu_snapshots
        .iter()
        .all(|dpu| dpu.status.bmc_info.supports_bfb_install())
    {
        tracing::info!(
            "DPF based DPU provisioning is not possible because some DPUs do not support BFB install via Redfish."
        );
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            docs = "https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/dpf-setup",
            "iPXE provisioning strategy (internally) is deprecated; enable DPF management for DPUs to migrate"
        );
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;
    use crate::test_support::machine_snapshot::{
        dpu_machine, host_machine, managed_host_state_snapshot,
    };

    #[derive(Clone, Copy)]
    struct DpuProvisioningInput {
        part_number: &'static str,
        firmware_version: Option<&'static str>,
        reprovision_requested: bool,
    }

    const BF2_SUPPORTED: DpuProvisioningInput = DpuProvisioningInput {
        part_number: "MBF2M516C",
        firmware_version: Some("BF-24.10"),
        reprovision_requested: false,
    };
    const BF3_SUPPORTED: DpuProvisioningInput = DpuProvisioningInput {
        part_number: "900-9D3B6",
        firmware_version: Some("BF-24.10"),
        reprovision_requested: false,
    };
    const BF3_REQUESTED: DpuProvisioningInput = DpuProvisioningInput {
        reprovision_requested: true,
        ..BF3_SUPPORTED
    };
    const BF3_UNSUPPORTED_BFB: DpuProvisioningInput = DpuProvisioningInput {
        firmware_version: Some("BF-24.04"),
        ..BF3_SUPPORTED
    };
    const BF4_SUPPORTED: DpuProvisioningInput = DpuProvisioningInput {
        part_number: "900-9D4B4",
        firmware_version: Some("BF4-26.04"),
        reprovision_requested: false,
    };

    #[test]
    fn pending_boot_interface_version_requires_matching_verification() {
        let desired = ConfigVersion::new(7);
        let stale = ConfigVersion::new(6);

        value_scenarios!(
            run = |(desired_version, verified_version)| {
                pending_boot_interface_config_version(desired_version, verified_version)
            };
            "no desired target needs no verification" {
                (None, None) => None,
            }
            "an unobserved desired target needs verification" {
                (Some(desired), None) => Some(desired),
            }
            "a stale observation needs verification" {
                (Some(desired), Some(stale)) => Some(desired),
            }
            "a matching observation is converged" {
                (Some(desired), Some(desired)) => None,
            }
        );
    }

    #[test]
    fn managed_host_network_config_sync_requires_every_expected_dpu() {
        enum DpuNetworkConfigCase {
            ZeroDpu,
            AllExpectedCurrent,
            MissingSnapshots,
            PartialSnapshots,
        }

        value_scenarios!(run = |case| {
                let mut state = managed_host_state_snapshot();

                match case {
                    DpuNetworkConfigCase::ZeroDpu => {
                        for interface in &mut state.host_snapshot.status.interfaces {
                            interface.attached_dpu_machine_id = None;
                        }
                        state.dpu_snapshots.clear();
                    }
                    DpuNetworkConfigCase::AllExpectedCurrent => {}
                    DpuNetworkConfigCase::MissingSnapshots => state.dpu_snapshots.clear(),
                    DpuNetworkConfigCase::PartialSnapshots => state.dpu_snapshots.truncate(1),
                }

                state.managed_host_network_config_version_synced()
            };
            "DPU network configuration" {
                DpuNetworkConfigCase::ZeroDpu => true,
                DpuNetworkConfigCase::AllExpectedCurrent => true,
                DpuNetworkConfigCase::MissingSnapshots => false,
                DpuNetworkConfigCase::PartialSnapshots => false,
            }
        );
    }

    #[test]
    fn ready_boot_config_defaults_survive_persisted_state_loading() {
        scenarios!(
            run = |json| serde_json::from_str::<ReadyBootConfigState>(json).map_err(drop);
            "unlock starts by disabling lockdown" {
                r#"{"state":"unlockhost"}"# => Yields(ReadyBootConfigState::UnlockHost {
                    unlock_host_state: UnlockHostState::DisableLockdown,
                }),
            }

            "BIOS setup starts with no retries" {
                r#"{"state":"configurebios"}"# => Yields(ReadyBootConfigState::ConfigureBios {
                    retry_count: 0,
                }),
            }

            "BIOS verification starts with no retries" {
                r#"{"state":"pollingbiossetup"}"# => Yields(
                    ReadyBootConfigState::PollingBiosSetup { retry_count: 0 },
                ),
            }

            "lockdown restoration defaults to the success path" {
                r#"{"state":"lockhost"}"# => Yields(ReadyBootConfigState::LockHost {
                    post_lock_action: None,
                }),
            }

            "existing convergence failure field remains readable" {
                r#"{"state":"lockhost","terminal_failure":{"kind":"convergence","failure":"stopped"}}"# =>
                    Yields(ReadyBootConfigState::LockHost {
                        post_lock_action: Some(ReadyBootConfigPostLockAction::Convergence {
                            failure: "stopped".to_string(),
                        }),
                    }),
            }
        );
    }

    #[test]
    fn ready_boot_config_post_lock_actions_round_trip() {
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let failure_details = FailureDetails {
            cause: FailureCause::BiosSetupFailed {
                err: "BIOS job retries exhausted".to_string(),
            },
            failed_at: DateTime::<Utc>::UNIX_EPOCH,
            source: FailureSource::StateMachine,
        };

        check_values(
            [
                Check {
                    scenario: "stale DPU network status returns to Prepare after cleanup",
                    input: ReadyBootConfigState::LockHost {
                        post_lock_action: Some(ReadyBootConfigPostLockAction::ReturnToPrepare),
                    },
                    expect: true,
                },
                Check {
                    scenario: "convergence failure waits for lockdown",
                    input: ReadyBootConfigState::LockHost {
                        post_lock_action: Some(ReadyBootConfigPostLockAction::Convergence {
                            failure: "BIOS job retries exhausted".to_string(),
                        }),
                    },
                    expect: true,
                },
                Check {
                    scenario: "independent machine failure keeps its attribution",
                    input: ReadyBootConfigState::LockHost {
                        post_lock_action: Some(ReadyBootConfigPostLockAction::Machine {
                            machine_id,
                            details: failure_details,
                        }),
                    },
                    expect: true,
                },
                Check {
                    scenario: "terminal convergence failure persists",
                    input: ReadyBootConfigState::Failed {
                        failure: "BIOS job retries exhausted".to_string(),
                    },
                    expect: true,
                },
            ],
            |state| {
                serde_json::from_str::<ReadyBootConfigState>(
                    &serde_json::to_string(&state).unwrap(),
                )
                .unwrap()
                    == state
            },
        );

        assert_eq!(
            serde_json::to_value(ReadyBootConfigState::LockHost {
                post_lock_action: Some(ReadyBootConfigPostLockAction::ReturnToPrepare),
            })
            .unwrap(),
            serde_json::json!({
                "state": "lockhost",
                "terminal_failure": { "kind": "return_to_prepare" },
            }),
        );
    }

    #[test]
    fn configure_astra_defaults_survive_persisted_state_loading() {
        scenarios!(
            run = |json| serde_json::from_str::<ManagedHostState>(json).map_err(drop);
            "legacy unit-shaped ConfigureAstra starts at EnableNics" {
                r#"{"state":"configureastra"}"# => Yields(ManagedHostState::ConfigureAstra {
                    configure_astra_state: ConfigureAstraState::EnableNics,
                }),
            }
            "explicit EnableNics substate round-trips" {
                r#"{"state":"configureastra","configure_astra_state":{"state":"enablenics"}}"# =>
                    Yields(ManagedHostState::ConfigureAstra {
                        configure_astra_state: ConfigureAstraState::EnableNics,
                    }),
            }
            "WaitingForPowercycle substate round-trips" {
                r#"{"state":"configureastra","configure_astra_state":{"state":"waitingforpowercycle"}}"# =>
                    Yields(ManagedHostState::ConfigureAstra {
                        configure_astra_state: ConfigureAstraState::WaitingForPowercycle,
                    }),
            }
        );
    }

    #[test]
    fn ready_host_with_unverified_boot_interface_is_not_allocatable() {
        let mut snapshot = managed_host_state_snapshot();
        let desired_version = ConfigVersion::new(7);
        let desired_boot_interface =
            MachineBootInterfaceTarget::MacOnly(MacAddress::new([1, 2, 3, 4, 5, 6]));
        snapshot.host_snapshot.config.desired_boot_interface =
            Some(Versioned::new(desired_boot_interface, desired_version));
        snapshot
            .host_snapshot
            .status
            .boot_interface_status_observation = None;

        assert_eq!(
            snapshot.is_usable_as_instance(false),
            Err(NotAllocatableReason::PendingBootConfiguration)
        );
    }

    #[test]
    fn boot_configuring_state_has_stable_state_strings() {
        let state = ManagedHostState::BootConfiguring {
            desired_version: ConfigVersion::new(7),
            desired_boot_interface: MachineBootInterfaceTarget::MacOnly(MacAddress::new([
                1, 2, 3, 4, 5, 6,
            ])),
            post_lock_verification_retry_count: 0,
            boot_config_state: ReadyBootConfigState::Failed {
                failure: "payload must not enter state labels".to_string(),
            },
        };
        let dpu_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap()
                .try_into()
                .unwrap();

        assert_eq!(state.to_string(), "BootConfiguring/Failed");
        assert_eq!(state.dpu_state_string(&dpu_id), "BootConfiguring/Failed");
    }

    #[test]
    fn machine_bmc_vendor_delegates_to_hardware_info() {
        let mut with_hardware_info = host_machine();
        with_hardware_info
            .status
            .hardware_info
            .as_mut()
            .and_then(|hardware_info| hardware_info.dmi_data.as_mut())
            .expect("fixture host has DMI data")
            .sys_vendor = "Dell Inc.".to_string();

        let mut without_hardware_info = host_machine();
        without_hardware_info.status.hardware_info = None;

        check_values(
            [
                Check {
                    scenario: "hardware info delegates to its DMI vendor",
                    input: with_hardware_info,
                    expect: bmc_vendor::BMCVendor::Dell,
                },
                Check {
                    scenario: "missing hardware info falls back to unknown",
                    input: without_hardware_info,
                    expect: bmc_vendor::BMCVendor::Unknown,
                },
            ],
            |machine| machine.bmc_vendor(),
        );
    }

    #[derive(Clone, Copy)]
    struct DpfProvisioningInput {
        dpf_enabled_at_site: bool,
        dpf_enabled_for_host: bool,
        used_for_ingestion: bool,
        reprovisioning_case: bool,
        dpus: &'static [DpuProvisioningInput],
    }

    fn dpf_input(dpus: &'static [DpuProvisioningInput]) -> DpfProvisioningInput {
        DpfProvisioningInput {
            dpf_enabled_at_site: true,
            dpf_enabled_for_host: true,
            used_for_ingestion: false,
            reprovisioning_case: false,
            dpus,
        }
    }

    fn provisioning_state(input: DpfProvisioningInput) -> ManagedHostStateSnapshot {
        let mut state = managed_host_state_snapshot();
        state.host_snapshot.config.dpf = Dpf {
            enabled: input.dpf_enabled_for_host,
            used_for_ingestion: input.used_for_ingestion,
        };
        state.dpu_snapshots = input
            .dpus
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let mut dpu = dpu_machine(index as u8);
                dpu.status.bmc_info.firmware_version = input.firmware_version.map(str::to_string);
                dpu.status
                    .hardware_info
                    .as_mut()
                    .expect("fixture DPU has hardware info")
                    .dpu_info
                    .as_mut()
                    .expect("fixture DPU has DPU info")
                    .part_number = input.part_number.to_string();
                dpu.reprovision_requested =
                    input.reprovision_requested.then(|| ReprovisionRequest {
                        requested_at: DateTime::<Utc>::UNIX_EPOCH,
                        initiator: "test".to_string(),
                        update_firmware: false,
                        started_at: None,
                        user_approval_received: false,
                        restart_reprovision_requested_at: DateTime::<Utc>::UNIX_EPOCH,
                    });
                dpu
            })
            .collect();
        state
    }

    #[test]
    fn dpf_provisioning_policy_matrix() {
        check_values(
            [
                Check {
                    scenario: "site flag disables DPF provisioning",
                    input: DpfProvisioningInput {
                        dpf_enabled_at_site: false,
                        ..dpf_input(&[BF3_SUPPORTED])
                    },
                    expect: false,
                },
                Check {
                    scenario: "host flag disables DPF provisioning",
                    input: DpfProvisioningInput {
                        dpf_enabled_for_host: false,
                        ..dpf_input(&[BF3_SUPPORTED])
                    },
                    expect: false,
                },
                Check {
                    scenario: "initial BF3 provisioning is eligible",
                    input: dpf_input(&[BF3_SUPPORTED]),
                    expect: true,
                },
                Check {
                    scenario: "legacy ingestion can switch when every DPU is requested",
                    input: DpfProvisioningInput {
                        reprovisioning_case: true,
                        dpus: &[BF3_REQUESTED, BF3_REQUESTED],
                        ..dpf_input(&[])
                    },
                    expect: true,
                },
                Check {
                    scenario: "legacy ingestion cannot switch a requested subset",
                    input: DpfProvisioningInput {
                        reprovisioning_case: true,
                        dpus: &[BF3_REQUESTED, BF3_SUPPORTED],
                        ..dpf_input(&[])
                    },
                    expect: false,
                },
                Check {
                    scenario: "DPF ingestion permits a requested subset",
                    input: DpfProvisioningInput {
                        used_for_ingestion: true,
                        reprovisioning_case: true,
                        dpus: &[BF3_REQUESTED, BF3_SUPPORTED],
                        ..dpf_input(&[])
                    },
                    expect: true,
                },
                Check {
                    scenario: "a BF2 DPU prevents DPF provisioning",
                    input: dpf_input(&[BF3_SUPPORTED, BF2_SUPPORTED]),
                    expect: false,
                },
                Check {
                    scenario: "BF4 provisioning is eligible",
                    input: dpf_input(&[BF4_SUPPORTED]),
                    expect: true,
                },
                Check {
                    scenario: "unsupported BFB firmware prevents DPF provisioning",
                    input: dpf_input(&[BF3_UNSUPPORTED_BFB]),
                    expect: false,
                },
                Check {
                    scenario: "mixed BFB support prevents DPF provisioning",
                    input: dpf_input(&[BF3_SUPPORTED, BF3_UNSUPPORTED_BFB]),
                    expect: false,
                },
                Check {
                    scenario: "a host without DPUs is ineligible",
                    input: dpf_input(&[]),
                    expect: false,
                },
            ],
            |input| {
                let state = provisioning_state(input);
                dpf_based_dpu_provisioning_possible(
                    &state,
                    input.dpf_enabled_at_site,
                    input.reprovisioning_case,
                )
            },
        );
    }

    #[derive(Clone, Copy)]
    struct DpuProvisioningRouteInput {
        dpf: DpfProvisioningInput,
        enable_secure_boot: bool,
    }

    #[test]
    fn dpu_provisioning_route_matrix() {
        check_values(
            [
                Check {
                    scenario: "DPF takes priority over secure boot",
                    input: DpuProvisioningRouteInput {
                        dpf: DpfProvisioningInput {
                            used_for_ingestion: true,
                            ..dpf_input(&[BF3_SUPPORTED])
                        },
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::DisableSecureBoot {
                            count: 0,
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                        },
                        ReprovisionState::DpfStates {
                            substate: DpfState::Reprovisioning,
                        },
                    ),
                },
                Check {
                    scenario: "Redfish BFB is selected when site DPF is disabled",
                    input: DpuProvisioningRouteInput {
                        dpf: DpfProvisioningInput {
                            dpf_enabled_at_site: false,
                            ..dpf_input(&[BF3_SUPPORTED])
                        },
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::EnableSecureBoot {
                            count: 0,
                            enable_secure_boot_state: SetSecureBootState::CheckSecureBootStatus,
                        },
                        ReprovisionState::InstallDpuOs {
                            substate: InstallDpuOsState::InstallingBFB,
                        },
                    ),
                },
                Check {
                    scenario: "secure boot disabled bypasses Redfish BFB",
                    input: DpuProvisioningRouteInput {
                        dpf: DpfProvisioningInput {
                            dpf_enabled_at_site: false,
                            ..dpf_input(&[BF3_SUPPORTED])
                        },
                        enable_secure_boot: false,
                    },
                    expect: (
                        DpuDiscoveringState::DisableSecureBoot {
                            count: 0,
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                        },
                        ReprovisionState::WaitingForNetworkInstall,
                    ),
                },
                Check {
                    scenario: "mixed BFB support uses network installation",
                    input: DpuProvisioningRouteInput {
                        dpf: DpfProvisioningInput {
                            dpf_enabled_at_site: false,
                            dpus: &[BF3_SUPPORTED, BF3_UNSUPPORTED_BFB],
                            ..dpf_input(&[])
                        },
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::DisableSecureBoot {
                            count: 0,
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                        },
                        ReprovisionState::WaitingForNetworkInstall,
                    ),
                },
                Check {
                    scenario: "legacy subset only blocks the reprovision DPF route",
                    input: DpuProvisioningRouteInput {
                        dpf: DpfProvisioningInput {
                            dpus: &[BF3_REQUESTED, BF3_SUPPORTED],
                            ..dpf_input(&[])
                        },
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::DisableSecureBoot {
                            count: 0,
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                        },
                        ReprovisionState::InstallDpuOs {
                            substate: InstallDpuOsState::InstallingBFB,
                        },
                    ),
                },
                Check {
                    scenario: "BF2 falls back to Redfish BFB installation",
                    input: DpuProvisioningRouteInput {
                        dpf: dpf_input(&[BF2_SUPPORTED]),
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::EnableSecureBoot {
                            count: 0,
                            enable_secure_boot_state: SetSecureBootState::CheckSecureBootStatus,
                        },
                        ReprovisionState::InstallDpuOs {
                            substate: InstallDpuOsState::InstallingBFB,
                        },
                    ),
                },
                Check {
                    scenario: "an empty DPU set cannot select aggregate routes",
                    input: DpuProvisioningRouteInput {
                        dpf: dpf_input(&[]),
                        enable_secure_boot: true,
                    },
                    expect: (
                        DpuDiscoveringState::DisableSecureBoot {
                            count: 0,
                            disable_secure_boot_state: Some(
                                SetSecureBootState::CheckSecureBootStatus,
                            ),
                        },
                        ReprovisionState::WaitingForNetworkInstall,
                    ),
                },
            ],
            |input| {
                let state = provisioning_state(input.dpf);
                (
                    DpuDiscoveringState::next_substate_based_on_bfb_support(
                        input.enable_secure_boot,
                        &state,
                        input.dpf.dpf_enabled_at_site,
                    ),
                    ReprovisionState::next_substate_based_on_bfb_support(
                        input.enable_secure_boot,
                        &state,
                        input.dpf.dpf_enabled_at_site,
                    ),
                )
            },
        );
    }

    // Deserializing a `FailureDetails` JSON blob: the parsed value must match the
    // expected struct (cause + failed_at + source). The type is PartialEq, so we
    // yield the whole struct.
    #[test]
    fn test_json_deserialize_failure_details() {
        let failed_at = chrono::DateTime::parse_from_rfc3339("2023-07-31T11:26:18.261228950+00:00")
            .unwrap()
            .with_timezone(&Utc);
        scenarios!(
            run = |s| serde_json::from_str::<FailureDetails>(s).map_err(drop);
            "no error" {
                r#"{"cause": "noerror", "source": "noerror", "failed_at": "2023-07-31T11:26:18.261228950Z"}"# => Yields(FailureDetails {
                    cause: FailureCause::NoError,
                    failed_at,
                    source: FailureSource::NoError,
                }),
            }

            "nvme clean failed" {
                r#"{"cause": {"nvmecleanfailed":{"err": "error1"}},  "source": "noerror","failed_at": "2023-07-31T11:26:18.261228950Z"}"# => Yields(FailureDetails {
                    cause: FailureCause::NVMECleanFailed {
                        err: "error1".to_string(),
                    },
                    failed_at,
                    source: FailureSource::NoError,
                }),
            }
        );
    }

    // Reprovisioning states deserialize to the expected `ManagedHostState` AND
    // render the expected `Display` string; we yield the (state, display) pair so
    // both assertions ride along.
    #[test]
    fn test_json_deserialize_reprovisioning_states() {
        let machine_id: DpuMachineId =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap()
                .try_into()
                .unwrap();
        scenarios!(
            run = |s| {
                serde_json::from_str::<ManagedHostState>(s)
                    .map(|state| (state.clone(), state.to_string()))
                    .map_err(drop)
            };
            "dpu reprovision firmware upgrade" {
                r#"{"state":"dpureprovision","dpu_states":{"states":{"fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng":"firmwareupgrade"}}}"# => Yields((
                    ManagedHostState::DPUReprovision {
                        dpu_states: DpuReprovisionStates {
                            states: HashMap::from([(
                                machine_id,
                                ReprovisionState::FirmwareUpgrade,
                            )]),
                        },
                    },
                    "Reprovisioning/FirmwareUpgrade".to_string(),
                )),
            }

            "assigned dpu reprovision firmware upgrade" {
                r#"{"state":"assigned","instance_state":{"state":"dpureprovision","dpu_states":{"states":{"fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng":"firmwareupgrade"}}}}"# => Yields((
                    ManagedHostState::Assigned {
                        instance_state: InstanceState::DPUReprovision {
                            dpu_states: DpuReprovisionStates {
                                states: HashMap::from([(
                                    machine_id,
                                    ReprovisionState::FirmwareUpgrade,
                                )]),
                            },
                        },
                    },
                    "Assigned/Reprovision/FirmwareUpgrade".to_string(),
                )),
            }
        );
    }

    // The remaining `ManagedHostState` JSON blobs each deserialize to a specific
    // variant; the parsed value (PartialEq) is the whole assertion.
    #[test]
    fn test_json_deserialize_managed_host_states() {
        let machine_id: DpuMachineId =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap()
                .try_into()
                .unwrap();

        scenarios!(
            run = |s| serde_json::from_str::<ManagedHostState>(s).map_err(drop);
            "assigned booting with discovery image, default retry" {
                r#"{"state":"assigned","instance_state":{"state":"bootingwithdiscoveryimage"}}"# => Yields(ManagedHostState::Assigned {
                    instance_state: InstanceState::BootingWithDiscoveryImage {
                        retry: RetryInfo { count: 0 },
                    },
                }),
            }

            "assigned booting with discovery image, explicit retry" {
                r#"{"state":"assigned","instance_state":{"state":"bootingwithdiscoveryimage", "retry":{"count": 10}}}"# => Yields(ManagedHostState::Assigned {
                    instance_state: InstanceState::BootingWithDiscoveryImage {
                        retry: RetryInfo { count: 10 },
                    },
                }),
            }

            "dpu reprovision host boot configure state" {
                r#"{"state":"dpureprovision","dpu_states":{"states":{"fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng":{"configurehostboot":{"retry_count":2}}}}}"# => Yields(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates {
                        states: HashMap::from([(
                            machine_id,
                            ReprovisionState::ConfigureHostBoot { retry_count: 2 },
                        )]),
                    },
                }),
            }

            "dpu reprovision host boot unlock default state" {
                r#"{"state":"dpureprovision","dpu_states":{"states":{"fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng":{"unlockhostforbootrepair":{}}}}}"# => Yields(ManagedHostState::DPUReprovision {
                    dpu_states: DpuReprovisionStates {
                        states: HashMap::from([(
                            machine_id,
                            ReprovisionState::UnlockHostForBootRepair {
                                unlock_host_state: UnlockHostState::DisableLockdown,
                            },
                        )]),
                    },
                }),
            }

            "host init polling bios setup, default retry" {
                r#"{"state":"hostinit","machine_state":{"state":"pollingbiossetup"}}"# => Yields(ManagedHostState::HostInit {
                    machine_state: MachineState::PollingBiosSetup { retry_count: 0 },
                }),
            }

            "host init polling bios setup, explicit retry count" {
                r#"{"state":"hostinit","machine_state":{"state":"pollingbiossetup","retry_count":2}}"# => Yields(ManagedHostState::HostInit {
                    machine_state: MachineState::PollingBiosSetup { retry_count: 2 },
                }),
            }

            "assigned host platform configuration polling bios setup (legacy)" {
                r#"{"state":"assigned","instance_state":{"state":"hostplatformconfiguration","platform_config_state":{"state":"pollingbiossetup"}}}"# => Yields(ManagedHostState::Assigned {
                    instance_state: InstanceState::HostPlatformConfiguration {
                        platform_config_state:
                            HostPlatformConfigurationState::PollingBiosSetup { retry_count: 0 },
                    },
                }),
            }

            "host init waiting for lockdown" {
                r#"{"state":"hostinit","machine_state":{"state":"waitingforlockdown","lockdown_info":{"state":"setlockdown","mode":"enable"}}}"# => Yields(ManagedHostState::HostInit {
                    machine_state: MachineState::WaitingForLockdown {
                        lockdown_info: LockdownInfo {
                            state: LockdownState::SetLockdown,
                            mode: LockdownMode::Enable,
                        },
                    },
                }),
            }
        );
    }

    #[test]
    fn test_json_deserialize_machine_last_reboot_requested() {
        let serialized = r#"{"time":"2023-07-31T11:26:18.261228950+00:00","mode":"Reboot"}"#;
        let deserialized: MachineLastRebootRequested = serde_json::from_str(serialized).unwrap();

        assert_eq!(
            chrono::DateTime::parse_from_rfc3339("2023-07-31T11:26:18.261228950+00:00").unwrap(),
            deserialized.time,
        );
        assert!(matches!(
            deserialized.mode,
            MachineLastRebootRequestedMode::Reboot,
        ));
    }

    // Current `DpfState` tags deserialize to the expected variant and survive a
    // serialize/deserialize round-trip; the unknown-tag rows verify the lenient
    // fall-back to `DpfState::Unknown`. We yield the (parsed, round-tripped) pair
    // so both the direct parse and the round-trip are asserted.
    #[test]
    fn test_dpf_state_deserialize_and_roundtrip() {
        scenarios!(
            run = |s| {
                let parsed: DpfState = serde_json::from_str(s).map_err(drop)?;
                let serialized = serde_json::to_string(&parsed).map_err(drop)?;
                let roundtrip: DpfState = serde_json::from_str(&serialized).map_err(drop)?;
                Ok::<_, ()>((parsed, roundtrip))
            };
            "provisioning" {
                r#"{"dpfstate":"provisioning"}"# => Yields((DpfState::Provisioning, DpfState::Provisioning)),
            }

            "waiting for ready, no phase detail" {
                r#"{"dpfstate":"waitingforready"}"# => Yields((
                    DpfState::WaitingForReady { phase_detail: None },
                    DpfState::WaitingForReady { phase_detail: None },
                )),
            }

            "waiting for ready, with phase detail" {
                r#"{"dpfstate":"waitingforready","phase_detail":"some-detail"}"# => Yields((
                    DpfState::WaitingForReady {
                        phase_detail: Some("some-detail".to_string()),
                    },
                    DpfState::WaitingForReady {
                        phase_detail: Some("some-detail".to_string()),
                    },
                )),
            }

            "device ready" {
                r#"{"dpfstate":"deviceready"}"# => Yields((DpfState::DeviceReady, DpfState::DeviceReady)),
            }

            "reprovisioning" {
                r#"{"dpfstate":"reprovisioning"}"# => Yields((DpfState::Reprovisioning, DpfState::Reprovisioning)),
            }

            "unknown tag falls back to Unknown" {
                r#"{"dpfstate":"somethingold"}"# => Yields((DpfState::Unknown, DpfState::Unknown)),
            }

            "bogus tag with extra field falls back to Unknown" {
                r#"{"dpfstate":"bogus","extra":"field"}"# => Yields((DpfState::Unknown, DpfState::Unknown)),
            }
        );
    }

    #[test]
    fn test_factory_reset_bmc_state_deserialize_and_roundtrip() {
        scenarios!(
            run = |s| {
                let parsed: HostPlatformConfigurationState =
                    serde_json::from_str(s).map_err(drop)?;
                let serialized = serde_json::to_string(&parsed).map_err(drop)?;
                let roundtrip: HostPlatformConfigurationState =
                    serde_json::from_str(&serialized).map_err(drop)?;
                Ok::<_, ()>((parsed, roundtrip))
            };
            "factory reset defaults to check preconditions when reset_state omitted" {
                r#"{"state":"factoryresetbmc"}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::CheckPreconditions,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::CheckPreconditions,
                    },
                )),
            }

            "explicit check preconditions" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"checkpreconditions"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::CheckPreconditions,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::CheckPreconditions,
                    },
                )),
            }

            "explicit suppress exploration" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"suppressexploration"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::SuppressExploration,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::SuppressExploration,
                    },
                )),
            }

            "reset to defaults" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"resettodefaults"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::ResetToDefaults,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::ResetToDefaults,
                    },
                )),
            }

            "wait for bmc" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"waitforbmc"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::WaitForBmc,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::WaitForBmc,
                    },
                )),
            }

            "restore credentials, explicit retry" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"restorecredentials","retry_count":5}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RestoreCredentials { retry_count: 5 },
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RestoreCredentials { retry_count: 5 },
                    },
                )),
            }

            "restore credentials, default retry" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"restorecredentials"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RestoreCredentials { retry_count: 0 },
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RestoreCredentials { retry_count: 0 },
                    },
                )),
            }

            "remove suppression" {
                r#"{"state":"factoryresetbmc","reset_state":{"state":"removesuppression"}}"# => Yields((
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RemoveSuppression,
                    },
                    HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: FactoryResetBmcState::RemoveSuppression,
                    },
                )),
            }
        );
    }

    fn alert_with_classifications(
        classifications: Vec<health_report::HealthAlertClassification>,
    ) -> health_report::HealthProbeAlert {
        health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::heartbeat_timeout(),
            target: None,
            in_alert_since: Some(chrono::Utc::now()),
            message: "test alert".to_string(),
            tenant_message: None,
            classifications,
        }
    }

    fn health_report_with_alerts(
        alerts: Vec<health_report::HealthProbeAlert>,
    ) -> health_report::HealthReport {
        health_report::HealthReport {
            source: "test".to_string(),
            triggered_by: None,
            observed_at: Some(chrono::Utc::now()),
            successes: vec![],
            alerts,
        }
    }

    #[test]
    fn state_sla_maps_machine_states_and_health_to_limits() {
        struct Inputs {
            state: ManagedHostState,
            state_version: ConfigVersion,
            aggregate_health: health_report::HealthReport,
        }

        let machine_id: DpuMachineId =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap()
                .try_into()
                .unwrap();
        let other_dpu_id: DpuMachineId =
            MachineId::from_str("fm100dtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0")
                .unwrap()
                .try_into()
                .unwrap();
        let validation_id = MachineValidationId::nil();
        let sla_config = slas::MachineSlaConfig::new(chrono::Duration::minutes(10));
        let failed = || ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::NoError,
                failed_at: chrono::Utc::now(),
                source: FailureSource::NoError,
            },
            machine_id: machine_id.into(),
            retry_count: 1,
        };
        let excluded = || {
            alert_with_classifications(vec![
                health_report::HealthAlertClassification::exclude_from_state_machine_sla(),
            ])
        };
        let unrelated = || {
            alert_with_classifications(vec![
                health_report::HealthAlertClassification::prevent_allocations(),
            ])
        };
        let seconds = |value| Some(std::time::Duration::from_secs(value));
        // ConfigVersion::invalid() makes breached rows deterministic; the sole
        // ConfigVersion::initial() row is the below-SLA countercheck.
        let stale = |state| Inputs {
            state,
            state_version: ConfigVersion::invalid(),
            aggregate_health: health_report_with_alerts(vec![]),
        };

        check_values(
            [
                Check {
                    scenario: "one exclusion among several alerts suppresses a failed-state SLA",
                    input: Inputs {
                        state: failed(),
                        state_version: ConfigVersion::invalid(),
                        aggregate_health: health_report_with_alerts(vec![unrelated(), excluded()]),
                    },
                    expect: (None, false),
                },
                Check {
                    scenario: "a sole exclusion suppresses a positive SLA",
                    input: Inputs {
                        state: ManagedHostState::Created,
                        state_version: ConfigVersion::invalid(),
                        aggregate_health: health_report_with_alerts(vec![excluded()]),
                    },
                    expect: (None, false),
                },
                Check {
                    scenario: "an unrelated alert leaves the positive SLA in effect",
                    input: Inputs {
                        state: ManagedHostState::Created,
                        state_version: ConfigVersion::initial(),
                        aggregate_health: health_report_with_alerts(vec![unrelated()]),
                    },
                    expect: (seconds(1_800), false),
                },
                Check {
                    scenario: "DPU discovery without a DPU has no SLA",
                    input: stale(ManagedHostState::DpuDiscoveringState {
                        dpu_states: DpuDiscoveringStates {
                            states: HashMap::new(),
                        },
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "nonempty DPU discovery uses the discovery SLA",
                    input: stale(ManagedHostState::DpuDiscoveringState {
                        dpu_states: DpuDiscoveringStates {
                            states: HashMap::from([
                                (machine_id, DpuDiscoveringState::EnableRshim),
                                (other_dpu_id, DpuDiscoveringState::Initializing),
                            ]),
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "DPU initialization without a DPU has no SLA",
                    input: stale(ManagedHostState::DPUInit {
                        dpu_states: DpuInitStates {
                            states: HashMap::new(),
                        },
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "DPU initialization at Init has no SLA",
                    input: stale(ManagedHostState::DPUInit {
                        dpu_states: DpuInitStates {
                            states: HashMap::from([
                                (machine_id, DpuInitState::WaitingForPlatformConfiguration),
                                (other_dpu_id, DpuInitState::Init),
                            ]),
                        },
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "a pre-Init DPU sets the initialization SLA",
                    input: stale(ManagedHostState::DPUInit {
                        dpu_states: DpuInitStates {
                            states: HashMap::from([
                                (machine_id, DpuInitState::Init),
                                (
                                    other_dpu_id,
                                    DpuInitState::InstallDpuOs {
                                        substate: InstallDpuOsState::InstallingBFB,
                                    },
                                ),
                            ]),
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "host initialization at Init has no SLA",
                    input: stale(ManagedHostState::HostInit {
                        machine_state: MachineState::Init,
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "host initialization waiting for discovery uses the host SLA",
                    input: stale(ManagedHostState::HostInit {
                        machine_state: MachineState::WaitingForDiscovery,
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "Ready has no SLA",
                    input: stale(ManagedHostState::Ready),
                    expect: (None, false),
                },
                Check {
                    scenario: "active boot configuration uses the convergence SLA",
                    input: stale(ManagedHostState::BootConfiguring {
                        desired_version: ConfigVersion::initial(),
                        desired_boot_interface: MachineBootInterfaceTarget::MacOnly(
                            MacAddress::new([1, 2, 3, 4, 5, 6]),
                        ),
                        post_lock_verification_retry_count: 0,
                        boot_config_state: ReadyBootConfigState::Prepare,
                    }),
                    expect: (seconds(5_400), true),
                },
                Check {
                    scenario: "terminal boot configuration immediately breaches its SLA",
                    input: stale(ManagedHostState::BootConfiguring {
                        desired_version: ConfigVersion::initial(),
                        desired_boot_interface: MachineBootInterfaceTarget::MacOnly(
                            MacAddress::new([1, 2, 3, 4, 5, 6]),
                        ),
                        post_lock_verification_retry_count: 0,
                        boot_config_state: ReadyBootConfigState::Failed {
                            failure: "BIOS job retries exhausted".to_string(),
                        },
                    }),
                    expect: (seconds(0), true),
                },
                Check {
                    scenario: "maintenance uses the maintenance SLA",
                    input: stale(ManagedHostState::Maintenance {
                        operation: MachineMaintenanceOperation::PowerOn,
                    }),
                    expect: (seconds(300), true),
                },
                Check {
                    scenario: "an assigned Ready machine has no SLA",
                    input: stale(ManagedHostState::Assigned {
                        instance_state: InstanceState::Ready,
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "the first discovery-image retry uses the configured SLA",
                    input: stale(ManagedHostState::Assigned {
                        instance_state: InstanceState::BootingWithDiscoveryImage {
                            retry: RetryInfo { count: 1 },
                        },
                    }),
                    expect: (seconds(660), true),
                },
                Check {
                    scenario: "the second discovery-image retry has a zero SLA",
                    input: stale(ManagedHostState::Assigned {
                        instance_state: InstanceState::BootingWithDiscoveryImage {
                            retry: RetryInfo { count: 2 },
                        },
                    }),
                    expect: (seconds(0), true),
                },
                Check {
                    scenario: "host platform configuration uses its extended SLA",
                    input: stale(ManagedHostState::Assigned {
                        instance_state: InstanceState::HostPlatformConfiguration {
                            platform_config_state: HostPlatformConfigurationState::CheckHostConfig,
                        },
                    }),
                    expect: (seconds(5_400), true),
                },
                Check {
                    scenario: "other assigned substates use the assigned SLA",
                    input: stale(ManagedHostState::Assigned {
                        instance_state: InstanceState::Init,
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "cleanup uses the cleanup SLA",
                    input: stale(ManagedHostState::WaitingForCleanup {
                        cleanup_state: CleanupState::Init,
                        cleanup_context: CleanupContext::Deprovision,
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "force deletion uses the force-deletion SLA",
                    input: stale(ManagedHostState::ForceDeletion),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "a failed state immediately exceeds its zero SLA",
                    input: stale(failed()),
                    expect: (seconds(0), true),
                },
                Check {
                    scenario: "DPU reprovisioning uses the DPU reprovision SLA",
                    input: stale(ManagedHostState::DPUReprovision {
                        dpu_states: DpuReprovisionStates {
                            states: HashMap::new(),
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "host reprovisioning uses the host reprovision SLA",
                    input: stale(ManagedHostState::HostReprovision {
                        reprovision_state: HostReprovisionState::CheckingFirmware,
                        retry_count: 0,
                    }),
                    expect: (seconds(2_400), true),
                },
                Check {
                    scenario: "pre-assignment measurement collection uses the measurement SLA",
                    input: stale(ManagedHostState::Measuring {
                        measuring_state: MeasuringState::WaitingForMeasurements,
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "pre-assignment pending bundles have no SLA",
                    input: stale(ManagedHostState::Measuring {
                        measuring_state: MeasuringState::PendingBundle,
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "post-assignment measured boot collection uses the measurement SLA",
                    input: stale(ManagedHostState::PostAssignedMeasuring {
                        attestation_mode: AttestationMode::MeasuredBoot {
                            measuring_state: MeasuringState::WaitingForMeasurements,
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "post-assignment measured boot pending bundles have no SLA",
                    input: stale(ManagedHostState::PostAssignedMeasuring {
                        attestation_mode: AttestationMode::MeasuredBoot {
                            measuring_state: MeasuringState::PendingBundle,
                        },
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "post-assignment SPDM polling uses the poll SLA",
                    input: stale(ManagedHostState::PostAssignedMeasuring {
                        attestation_mode: AttestationMode::SpdmAttestation {
                            spdm_measuring_state: SpdmMeasuringState::PollResult,
                        },
                    }),
                    expect: (seconds(600), true),
                },
                Check {
                    scenario: "post-assignment SPDM collection uses the trigger SLA",
                    input: stale(ManagedHostState::PostAssignedMeasuring {
                        attestation_mode: AttestationMode::SpdmAttestation {
                            spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                        },
                    }),
                    expect: (seconds(30), true),
                },
                Check {
                    scenario: "pre-assignment SPDM polling uses the poll SLA",
                    input: stale(ManagedHostState::PreAssignedMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::PollResult,
                    }),
                    expect: (seconds(600), true),
                },
                Check {
                    scenario: "pre-assignment SPDM collection uses the trigger SLA",
                    input: stale(ManagedHostState::PreAssignedMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                    }),
                    expect: (seconds(30), true),
                },
                Check {
                    scenario: "assignment-cycle startup uses the assignment SLA",
                    input: stale(ManagedHostState::StartAssignmentCycle),
                    expect: (seconds(60), true),
                },
                Check {
                    scenario: "SKU verification failure has no SLA",
                    input: stale(ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::SkuVerificationFailed(
                            BomValidatingContext::default(),
                        ),
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "waiting for SKU assignment has no SLA",
                    input: stale(ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::WaitingForSkuAssignment(
                            BomValidatingContext::default(),
                        ),
                    }),
                    expect: (None, false),
                },
                Check {
                    scenario: "active BOM validation uses the BOM SLA",
                    input: stale(ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::MatchingSku(
                            BomValidatingContext::default(),
                        ),
                    }),
                    expect: (seconds(300), true),
                },
                Check {
                    scenario: "machine validation uses the validation SLA",
                    input: stale(ManagedHostState::Validation {
                        validation_state: ValidationState::MachineValidation {
                            machine_validation: MachineValidatingState::MachineValidating {
                                context: "Discovery".to_string(),
                                id: validation_id,
                                completed: 0,
                                total: 1,
                                is_enabled: true,
                            },
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "validation reboot uses the validation SLA",
                    input: stale(ManagedHostState::Validation {
                        validation_state: ValidationState::MachineValidation {
                            machine_validation: MachineValidatingState::RebootHost {
                                validation_id,
                            },
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
                Check {
                    scenario: "boot repair validation uses the validation SLA",
                    input: stale(ManagedHostState::Validation {
                        validation_state: ValidationState::MachineValidation {
                            machine_validation: MachineValidatingState::PrepareBootRepair {
                                validation_id,
                            },
                        },
                    }),
                    expect: (seconds(1_800), true),
                },
            ],
            |Inputs {
                 state,
                 state_version,
                 aggregate_health,
             }| {
                let state_sla = state_sla(
                    &machine_id,
                    &state,
                    &state_version,
                    &aggregate_health,
                    &sla_config,
                );
                (state_sla.sla, state_sla.time_in_state_above_sla)
            },
        );
    }

    /// The pool BMC-rotation state is wired end to end: `ManagedHostState`
    /// deserializes (defaulting the transient `retry_count` when the field is
    /// absent, and round-tripping it when present), renders its stable `Display`
    /// label, and carries the dedicated rotation SLA rather than falling through
    /// to a default. A mis-wired serde tag, `Display` arm, or `state_sla` mapping
    /// for a freshly added state is an easy and silent regression, so pin all
    /// three. (The tenant-side `Assigned/RotatingBmc` state is deferred to a
    /// later PR; see the plan.)
    #[test]
    fn rotating_bmc_state_serde_display_and_sla() {
        // The literal wire form pins the `state` tag and the `#[serde(default)]`
        // retry_count (absent means 0); the `parse -> serialize -> reparse` run
        // then pins serializer symmetry, so a dropped/renamed retry_count or a
        // mis-wired tag can't slip through a deserialize-only check. Each row
        // yields `(parsed, round-tripped, Display)` so the serde tag and the
        // stable, retry-count-free label are asserted together.
        scenarios!(
            run = |s| {
                let parsed = serde_json::from_str::<ManagedHostState>(s).map_err(drop)?;
                let serialized = serde_json::to_string(&parsed).map_err(drop)?;
                let roundtrip =
                    serde_json::from_str::<ManagedHostState>(&serialized).map_err(drop)?;
                Ok::<_, ()>((parsed.clone(), roundtrip, parsed.to_string()))
            };
            "absent retry_count defaults to 0" {
                r#"{"state":"rotatingbmc"}"# => Yields((
                    ManagedHostState::RotatingBmc { retry_count: 0 },
                    ManagedHostState::RotatingBmc { retry_count: 0 },
                    "RotatingBmc".to_string(),
                )),
            }

            "explicit retry_count round-trips" {
                r#"{"state":"rotatingbmc","retry_count":4}"# => Yields((
                    ManagedHostState::RotatingBmc { retry_count: 4 },
                    ManagedHostState::RotatingBmc { retry_count: 4 },
                    "RotatingBmc".to_string(),
                )),
            }
        );

        // It carries the dedicated rotation SLA (not a default), and a freshly
        // entered state is within it.
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let sla = state_sla(
            &machine_id,
            &ManagedHostState::RotatingBmc { retry_count: 0 },
            &ConfigVersion::initial(),
            &health_report_with_alerts(vec![]),
            &slas::MachineSlaConfig::default(),
        );
        assert_eq!(sla.sla, Some(slas::ROTATING_BMC));
        assert!(
            !sla.time_in_state_above_sla,
            "a freshly entered RotatingBmc state is within its SLA"
        );
    }

    #[test]
    fn rotating_nic_lockdown_state_serde_display_and_sla() {
        // The unit variant pins to the bare `state` tag with no payload; the
        // `parse -> serialize -> reparse` run pins serializer symmetry and the
        // stable Display label.
        scenarios!(
            run = |s| {
                let parsed = serde_json::from_str::<ManagedHostState>(s).map_err(drop)?;
                let serialized = serde_json::to_string(&parsed).map_err(drop)?;
                let roundtrip =
                    serde_json::from_str::<ManagedHostState>(&serialized).map_err(drop)?;
                Ok::<_, ()>((parsed.clone(), roundtrip, parsed.to_string()))
            };
            "bare tag round-trips" {
                r#"{"state":"rotatingniclockdown"}"# => Yields((
                    ManagedHostState::RotatingNicLockdown,
                    ManagedHostState::RotatingNicLockdown,
                    "RotatingNicLockdown".to_string(),
                )),
            }
        );

        // It carries the dedicated rekey SLA (not a default), and a freshly
        // entered state is within it.
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let sla = state_sla(
            &machine_id,
            &ManagedHostState::RotatingNicLockdown,
            &ConfigVersion::initial(),
            &health_report_with_alerts(vec![]),
            &slas::MachineSlaConfig::default(),
        );
        assert_eq!(sla.sla, Some(slas::ROTATING_NIC_LOCKDOWN));
        assert!(
            !sla.time_in_state_above_sla,
            "a freshly entered RotatingNicLockdown state is within its SLA"
        );
    }

    /// Build a mock `MachineInterfaceSnapshot` with the fields
    /// `pick_boot_interface_mac` actually inspects (MAC, primary flag,
    /// segment type) set, and everything else left at the mock default.
    fn build_mock_interface(
        mac: &str,
        primary: bool,
        segment_type: Option<NetworkSegmentType>,
    ) -> MachineInterfaceSnapshot {
        MachineInterfaceSnapshot {
            primary_interface: primary,
            network_segment_type: segment_type,
            ..MachineInterfaceSnapshot::mock_with_mac(mac.parse().unwrap())
        }
    }

    // Whichever interface is flagged `primary_interface` wins, regardless
    // of MAC ordering or segment type of the other interfaces. This covers
    // both paths that can set the flag, whether it be site-explorer w/ DPU
    // ingestion, or operator-driven `ExpectedInterface.primary` for zero-DPU
    // hosts.
    #[test]
    fn pick_boot_interface_mac_returns_primary_interface_when_set() {
        let primary_mac = "10:00:00:00:00:01";
        let other_mac = "05:00:00:00:00:01"; // numerically lower but not primary
        let interfaces = vec![
            build_mock_interface(other_mac, false, Some(NetworkSegmentType::HostInband)),
            build_mock_interface(primary_mac, true, Some(NetworkSegmentType::Admin)),
        ];

        assert_eq!(
            pick_boot_interface_mac(&interfaces),
            Some(primary_mac.parse().unwrap())
        );
    }

    // This is our zero DPU fallback case -- no interface is flagged primary,
    // so pick the lowest-MAC non-underlay interface. Verifies (a) the underlay
    // BMC interface is excluded, and (b) ordering is deterministic across
    // multiple non-underlay candidates.
    #[test]
    fn pick_boot_interface_mac_falls_back_to_lowest_non_underlay_mac_when_no_primary() {
        let bmc_mac = "01:00:00:00:00:01"; // numerically lowest, but BMC!
        let onboard_mac_lo = "10:00:00:00:00:01";
        let onboard_mac_hi = "20:00:00:00:00:01";
        let interfaces = vec![
            build_mock_interface(bmc_mac, false, Some(NetworkSegmentType::Underlay)),
            build_mock_interface(onboard_mac_hi, false, Some(NetworkSegmentType::HostInband)),
            build_mock_interface(onboard_mac_lo, false, Some(NetworkSegmentType::HostInband)),
        ];

        assert_eq!(
            pick_boot_interface_mac(&interfaces),
            Some(onboard_mac_lo.parse().unwrap())
        );
    }

    // The default pick deliberately ignores the primary flag: it answers "what
    // would the automation choose if nothing were declared?", so a primary on a
    // higher MAC must not win here even though `pick_boot_interface` returns it.
    #[test]
    fn pick_default_boot_interface_ignores_the_primary_flag() {
        let primary_mac = "10:00:00:00:00:01";
        let lower_mac = "05:00:00:00:00:01";
        let interfaces = vec![
            build_mock_interface(lower_mac, false, Some(NetworkSegmentType::HostInband)),
            build_mock_interface(primary_mac, true, Some(NetworkSegmentType::Admin)),
        ];

        assert_eq!(
            pick_boot_interface(&interfaces).map(|i| i.mac_address),
            Some(primary_mac.parse().unwrap()),
            "the effective pick honors the primary flag"
        );
        assert_eq!(
            pick_default_boot_interface(&interfaces).map(|i| i.mac_address),
            Some(lower_mac.parse().unwrap()),
            "the default pick masks the primary flag and takes the lowest non-underlay MAC"
        );
    }

    // Underlay rows are never default-pick candidates, and an all-underlay set
    // yields no default at all.
    #[test]
    fn pick_default_boot_interface_excludes_underlay_rows() {
        let underlay_mac = "01:00:00:00:00:01";
        let interfaces = vec![build_mock_interface(
            underlay_mac,
            false,
            Some(NetworkSegmentType::Underlay),
        )];

        assert!(pick_default_boot_interface(&interfaces).is_none());
    }

    // boot_interface() derives the full pair from the SAME primary row that the
    // MAC selection uses, so the MAC and id can never name different interfaces.
    #[test]
    fn pick_boot_interface_pair_uses_primary_rows_mac_and_id() {
        let other = build_mock_interface(
            "05:00:00:00:00:01",
            false,
            Some(NetworkSegmentType::HostInband),
        );
        let primary = MachineInterfaceSnapshot {
            boot_interface_id: Some("NIC.Slot.7-1-1".to_string()),
            ..build_mock_interface("10:00:00:00:00:01", true, Some(NetworkSegmentType::Admin))
        };

        assert_eq!(
            pick_boot_interface_pair(&[other, primary]),
            Some(MachineBootInterface {
                mac_address: "10:00:00:00:00:01".parse().unwrap(),
                interface_id: "NIC.Slot.7-1-1".to_string(),
            })
        );
    }

    // When the primary row hasn't captured a Redfish interface id yet, there's no
    // complete pair -- callers fall back to the MAC alone.
    #[test]
    fn pick_boot_interface_pair_is_none_without_captured_id() {
        let primary =
            build_mock_interface("10:00:00:00:00:01", true, Some(NetworkSegmentType::Admin));
        assert_eq!(pick_boot_interface_pair(&[primary]), None);
    }

    /// Build a mock `PredictedMachineInterface` with the fields
    /// `pick_boot_prediction` inspects (MAC, primary flag, segment type).
    fn build_mock_prediction(
        mac: &str,
        primary: bool,
        segment_type: NetworkSegmentType,
    ) -> PredictedMachineInterface {
        PredictedMachineInterface {
            id: uuid::Uuid::nil(),
            machine_id: MachineId::from_str(
                "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng",
            )
            .unwrap(),
            mac_address: mac.parse().unwrap(),
            expected_network_segment_type: segment_type,
            boot_interface_id: None,
            primary_interface: primary,
        }
    }

    // A declared-primary prediction wins outright, mirroring pick_boot_interface
    // -- regardless of how many other predictions there are.
    #[test]
    fn pick_boot_prediction_returns_the_declared_primary() {
        let predictions = vec![
            build_mock_prediction("05:00:00:00:00:01", false, NetworkSegmentType::HostInband),
            build_mock_prediction("10:00:00:00:00:01", true, NetworkSegmentType::HostInband),
            build_mock_prediction("20:00:00:00:00:01", false, NetworkSegmentType::HostInband),
        ];
        assert_eq!(
            pick_boot_prediction(&predictions).map(|p| p.mac_address),
            Some("10:00:00:00:00:01".parse().unwrap())
        );
    }

    // With no declared primary, a sole non-underlay prediction is unambiguous.
    #[test]
    fn pick_boot_prediction_returns_the_sole_non_underlay_prediction() {
        let predictions = vec![
            build_mock_prediction("01:00:00:00:00:01", false, NetworkSegmentType::Underlay),
            build_mock_prediction("10:00:00:00:00:01", false, NetworkSegmentType::HostInband),
        ];
        assert_eq!(
            pick_boot_prediction(&predictions).map(|p| p.mac_address),
            Some("10:00:00:00:00:01".parse().unwrap())
        );
    }

    // Several non-underlay predictions and none declared primary: the boot NIC
    // is unknowable, so refuse to guess rather than program boot order against
    // whichever sorts first (the Gigawatt SuperNIC safety case).
    #[test]
    fn pick_boot_prediction_refuses_multiple_non_primary_predictions() {
        let predictions = vec![
            build_mock_prediction("10:00:00:00:00:01", false, NetworkSegmentType::HostInband),
            build_mock_prediction("20:00:00:00:00:01", false, NetworkSegmentType::HostInband),
        ];
        assert!(pick_boot_prediction(&predictions).is_none());
    }

    // Underlay predictions are never a boot candidate on their own.
    #[test]
    fn pick_boot_prediction_ignores_underlay_only_predictions() {
        let predictions = vec![build_mock_prediction(
            "01:00:00:00:00:01",
            false,
            NetworkSegmentType::Underlay,
        )];
        assert!(pick_boot_prediction(&predictions).is_none());
    }

    // Check the case  where only the BMC has been discovered so far (which
    // is common during early ingestion). In this case, there's no valid boot MAC
    // yet; callers fall back to the `::NoDpu` handling downstream.
    #[test]
    fn pick_boot_interface_mac_returns_none_when_only_underlay_interfaces() {
        let bmc_mac = "01:00:00:00:00:01";
        let interfaces = vec![build_mock_interface(
            bmc_mac,
            false,
            Some(NetworkSegmentType::Underlay),
        )];

        assert_eq!(pick_boot_interface_mac(&interfaces), None);
    }

    #[test]
    fn host_profile_defaults_to_lockdown_enabled() {
        let profile = HostProfile::default();
        assert!(!profile.disable_lockdown);
    }

    // A `HostProfile` serializes to the expected JSON and deserializes back to an
    // equal value. We yield the (serialized json, round-tripped profile) pair so
    // the exact serialized form and the round-trip equality are both asserted.
    #[test]
    fn host_profile_serde_round_trip() {
        scenarios!(
            run = |profile| {
                let json = serde_json::to_string(&profile).map_err(drop)?;
                let back: HostProfile = serde_json::from_str(&json).map_err(drop)?;
                Ok::<_, ()>((json, back))
            };
            "lockdown disabled (true)" {
                HostProfile {
                    disable_lockdown: true,
                } => Yields((
                    r#"{"disable_lockdown":true}"#.to_string(),
                    HostProfile {
                        disable_lockdown: true,
                    },
                )),
            }

            "lockdown enabled (false)" {
                HostProfile {
                    disable_lockdown: false,
                } => Yields((
                    r#"{"disable_lockdown":false}"#.to_string(),
                    HostProfile {
                        disable_lockdown: false,
                    },
                )),
            }
        );
    }

    #[test]
    fn host_profile_deserializes_from_db_default() {
        let db_default = r#"{"disable_lockdown": false}"#;
        let profile: HostProfile = serde_json::from_str(db_default).unwrap();
        assert!(!profile.disable_lockdown);
    }

    #[test]
    fn host_profile_default_used_when_parent_field_missing() {
        assert_eq!(
            HostProfile::default(),
            HostProfile {
                disable_lockdown: false
            }
        );
    }

    #[test]
    fn host_profile_from_expected_none_uses_defaults() {
        let profile = HostProfile::from_expected_machine(None);
        assert_eq!(profile, HostProfile::default());
    }

    #[test]
    fn host_profile_from_expected_empty_profile_uses_defaults() {
        let data = ExpectedMachineData::default();
        let profile = HostProfile::from_expected_machine(Some(&data));
        assert!(!profile.disable_lockdown);
    }

    #[test]
    fn host_profile_from_expected_resolves_disable_lockdown() {
        let mut data = ExpectedMachineData::default();

        data.host_lifecycle_profile.disable_lockdown = Some(true);
        assert!(HostProfile::from_expected_machine(Some(&data)).disable_lockdown);

        data.host_lifecycle_profile.disable_lockdown = Some(false);
        assert!(!HostProfile::from_expected_machine(Some(&data)).disable_lockdown);
    }

    #[test]
    fn dpf_error_deserialization() {
        let machine_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")
                .unwrap();
        let state = ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::DpfProvisioning {
                    err: "This should be in display".to_string(),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::NoError,
            },
            machine_id,
            retry_count: 1,
        };

        let output = state.to_string();
        assert!(output.contains("This should be in display"));
    }
}
