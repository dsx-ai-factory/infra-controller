// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Selects saved firmware evidence without reading hardware or advancing work.
//! Host requests are overwritable records; operation targets and task metadata
//! are frozen separately in controller state. Neither source supplies missing
//! facts for the other.
//!
//! Each projector uses a supplied rack only when its ID matches the member's
//! saved rack ID. It does not discover membership or BMC aliases. Missing or
//! unrelated rack snapshots cannot supply their job/scope records. Saved
//! per-device requests and status retain their own association rules.

use bmc_vendor::BMCVendor;
use carbide_firmware::FirmwareConfigSnapshot;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use model::firmware::{DesiredFirmwareVersions, FirmwareComponentType};
use model::machine::{HostMachine, HostReprovisionState, InstanceState, ManagedHostState};
use model::power_shelf::{PowerShelf, PowerShelfControllerState};
use model::rack::{
    FirmwareUpgradeDeviceStatus, FirmwareUpgradeJob, FirmwareUpgradeState, MaintenanceActivity,
    MaintenanceScope, NvosPasswordUpdateState, NvosUpdateJob, NvosUpdateState,
    NvosUpdateSwitchStatus, Rack, RackFirmwareUpgradeStatus, RackMaintenanceState, RackState,
};
use model::site_explorer::EndpointExplorationReport;
use model::switch::{ReProvisioningState, Switch, SwitchControllerState};

mod context;

#[cfg(test)]
mod tests;

/// Configured defaults are independent of any operation's frozen target.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ConfiguredHostFirmware {
    pub(super) vendor: BMCVendor,
    pub(super) model: String,
    pub(super) versions: DesiredFirmwareVersions,
}

/// Projects configured defaults using the host firmware controller's lookup. The
/// caller supplies the effective catalog, including runtime overrides; neither
/// installed inventory nor an operation target determines these expectations.
pub(super) fn host_expectations(
    catalog: &FirmwareConfigSnapshot,
    report: &EndpointExplorationReport,
) -> Option<ConfiguredHostFirmware> {
    let firmware = catalog.find_fw_info_for_host_report(report)?;
    Some(ConfiguredHostFirmware {
        vendor: firmware.vendor,
        model: firmware.model.clone(),
        versions: DesiredFirmwareVersions::from(firmware),
    })
}

/// Only selected target metadata crosses this boundary, never apply payloads,
/// artifact URLs, scout task JSON, or diagnostics stored alongside the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FirmwareWork {
    Host {
        component: Option<FirmwareComponentType>,
        version: Option<String>,
    },
    FirmwareObject {
        object_id: Option<String>,
    },
    Nvos {
        object_id: Option<String>,
        version: Option<String>,
    },
}

/// Describes controller work, including cleanup after a parent failure; it does
/// not claim that RMS is running. An active rack phase owns its published job.
/// Device status needs a matching request cycle before supplying job metadata.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CurrentFirmwareWork {
    pub(super) work: FirmwareWork,
    /// The firmware task handle, or the image handle for NVOS work.
    pub(super) job_id: Option<String>,
    pub(super) started_at: Option<DateTime<Utc>>,
    /// NVOS password recovery has its own handle, separate from the image job.
    pub(super) password_recovery_job_id: Option<String>,
}

/// Identifies the saved record that supplied evidence, not an operation ID.
/// Rack and device records can describe the same work without proving equality.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FirmwareRequestSource {
    HostReprovision,
    SwitchReprovision,
    ShelfReprovision,
    RackMaintenance,
}

/// An outstanding record may describe the current work or a replacement. Its
/// presence does not establish a separately admitted or queued operation.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct FirmwareRequestEvidence {
    pub(super) source: FirmwareRequestSource,
    pub(super) work: FirmwareWork,
    pub(super) requested_at: Option<DateTime<Utc>>,
    pub(super) initiator: Option<String>,
}

/// Backend observations are intentionally absent from these inputs and fields;
/// their failure cannot erase saved controller or request evidence.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct StoredFirmwareActions {
    pub(super) current: Vec<CurrentFirmwareWork>,
    pub(super) outstanding_requests: Vec<FirmwareRequestEvidence>,
}

/// Projects host firmware and owning-rack records. DPU reprovisioning and
/// standalone update windows are not admitted host firmware actions.
pub(super) fn machine(machine: &HostMachine, rack: Option<&Rack>) -> StoredFirmwareActions {
    let member = RackMember::Machine(machine);
    let rack = rack.filter(|rack| machine.rack_id.as_ref() == Some(&rack.id));
    let mut actions = rack_actions(member, rack);
    let request = machine.host_reprovision_requested.as_ref();
    let associated_request = request.filter(|request| {
        machine
            .rack_id
            .as_ref()
            .is_some_and(|id| request.initiator == format!("rack-{id}"))
    });
    let reprovision = match &machine.state.value {
        ManagedHostState::HostReprovision {
            reprovision_state, ..
        }
        | ManagedHostState::Assigned {
            instance_state: InstanceState::HostReprovision { reprovision_state },
        } => Some(reprovision_state),
        _ => None,
    };
    match reprovision {
        Some(HostReprovisionState::WaitingForRackFirmwareUpgrade) => {
            if actions.current.is_empty() {
                actions.current.extend(device_firmware_current(
                    associated_request.map(|request| request.requested_at),
                    machine.rack_fw_details.as_ref(),
                ));
            }
        }
        Some(HostReprovisionState::FailedFirmwareUpgrade { .. })
            if machine.state.value.host_repro_retries_exhausted() => {}
        Some(state) => actions.current.push(host_current(state)),
        None => {}
    }
    if let Some(request) = request {
        // The controller routes every `rack-` initiator to rack firmware, but
        // only the exact owning rack can associate its per-device metadata.
        let rack_initiated = request.initiator.starts_with("rack-");
        // The saved request is cleared by the host controller, not by a
        // hardware result. Assigned hosts may still be awaiting approval, and
        // an exhausted failure cannot identify a replacement or reset request.
        actions.outstanding_requests.push(FirmwareRequestEvidence {
            source: FirmwareRequestSource::HostReprovision,
            work: if rack_initiated {
                FirmwareWork::FirmwareObject { object_id: None }
            } else {
                FirmwareWork::Host {
                    component: None,
                    version: None,
                }
            },
            requested_at: Some(request.requested_at),
            initiator: Some(request.initiator.clone()),
        });
    }
    actions
}

/// Preserves the target frozen in the controller state, even if the request or
/// effective catalog has since changed. Poll-start times are not submission times.
fn host_current(state: &HostReprovisionState) -> CurrentFirmwareWork {
    let (component, version) = match state {
        HostReprovisionState::CheckingFirmwareV2 { firmware_type, .. }
        | HostReprovisionState::CheckingFirmwareRepeatV2 { firmware_type, .. } => {
            (*firmware_type, None)
        }
        HostReprovisionState::WaitingForUpload {
            firmware_type,
            final_version,
            ..
        }
        | HostReprovisionState::WaitingForFirmwareUpgrade {
            firmware_type,
            final_version,
            ..
        }
        | HostReprovisionState::ResetForNewFirmware {
            firmware_type,
            final_version,
            ..
        }
        | HostReprovisionState::NewFirmwareReportedWait {
            firmware_type,
            final_version,
            ..
        }
        | HostReprovisionState::WaitingForScoutUpgrade {
            firmware_type,
            final_version,
            ..
        } => (Some(*firmware_type), Some(final_version.clone())),
        HostReprovisionState::FailedFirmwareUpgrade { firmware_type, .. } => {
            (Some(*firmware_type), None)
        }
        _ => (None, None),
    };
    let (job_id, started_at) = match state {
        HostReprovisionState::WaitingForFirmwareUpgrade { task_id, .. } => {
            (nonempty_id(task_id), None)
        }
        HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id,
            started_at,
            ..
        } => (nonempty_id(upgrade_task_id), Some(*started_at)),
        _ => (None, None),
    };
    CurrentFirmwareWork {
        work: FirmwareWork::Host {
            component: component.filter(|component| *component != FirmwareComponentType::Unknown),
            version,
        },
        job_id,
        started_at,
        password_recovery_job_id: None,
    }
}

/// Projects switch firmware/NVOS and owning-rack records. NVOS-only submission
/// has no switch request, so its rack job can describe work while Ready.
pub(super) fn switch(switch: &Switch, rack: Option<&Rack>) -> StoredFirmwareActions {
    let rack = rack.filter(|rack| switch.rack_id.as_ref() == Some(&rack.id));
    let mut actions = rack_actions(RackMember::Switch(switch), rack);
    let request = switch
        .switch_reprovisioning_requested
        .as_ref()
        .filter(|request| request.initiator.starts_with("rack-"));
    let associated_request = request.filter(|request| {
        switch
            .rack_id
            .as_ref()
            .is_some_and(|id| request.initiator == format!("rack-{id}"))
    });
    let state = match &switch.controller_state.value {
        SwitchControllerState::ReProvisioning {
            reprovisioning_state,
        } => Some(reprovisioning_state),
        _ => None,
    };
    if actions.current.is_empty() {
        match state {
            Some(ReProvisioningState::WaitingForRackFirmwareUpgrade) => {
                actions.current.extend(device_firmware_current(
                    associated_request.map(|request| request.requested_at),
                    switch.firmware_upgrade_status.as_ref(),
                ))
            }
            Some(ReProvisioningState::WaitingForNVOSUpgrade) => {
                let status = associated_request.and_then(|request| {
                    switch
                        .nvos_update_status
                        .as_ref()
                        .filter(|status| status.is_current_for(request.requested_at))
                });
                if status.is_none_or(|status| !status.is_terminal()) {
                    actions.current.push(CurrentFirmwareWork {
                        work: FirmwareWork::Nvos {
                            object_id: status.and_then(|status| nonempty_id(&status.firmware_id)),
                            version: None,
                        },
                        job_id: status.and_then(|status| known_job_id(&status.task_id)),
                        started_at: status.and_then(|status| status.started_at),
                        password_recovery_job_id: None,
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(request) = request {
        if !matches!(
            state,
            Some(
                ReProvisioningState::WaitingForNVOSUpgrade
                    | ReProvisioningState::WaitingForNMXCConfigure
            )
        ) && associated_request.is_none_or(|request| {
            !current_terminal(
                switch.firmware_upgrade_status.as_ref(),
                request.requested_at,
            )
        }) {
            append_device_request(
                &mut actions,
                FirmwareRequestSource::SwitchReprovision,
                &request.activities,
                FirmwareActivity::FirmwareObject,
                request.requested_at,
                &request.initiator,
            );
        }
        if !matches!(state, Some(ReProvisioningState::WaitingForNMXCConfigure))
            && associated_request.is_none_or(|request| {
                !switch.nvos_update_status.as_ref().is_some_and(|status| {
                    status.is_current_for(request.requested_at) && status.is_terminal()
                })
            })
        {
            append_device_request(
                &mut actions,
                FirmwareRequestSource::SwitchReprovision,
                &request.activities,
                FirmwareActivity::Nvos,
                request.requested_at,
                &request.initiator,
            );
        }
    }
    actions
}

/// Projects shelf and owning-rack records. Current terminal status can unblock
/// Core without a shelf update; an unassociated status cannot identify its work.
pub(super) fn shelf(shelf: &PowerShelf, rack: Option<&Rack>) -> StoredFirmwareActions {
    let rack = rack.filter(|rack| shelf.rack_id.as_ref() == Some(&rack.id));
    let mut actions = rack_actions(RackMember::Shelf(shelf), rack);
    let request = shelf
        .power_shelf_reprovisioning_requested
        .as_ref()
        .filter(|request| request.initiator.starts_with("rack-"));
    let associated_request = request.filter(|request| {
        shelf
            .rack_id
            .as_ref()
            .is_some_and(|id| request.initiator == format!("rack-{id}"))
    });
    if actions.current.is_empty()
        && matches!(
            shelf.controller_state.value,
            PowerShelfControllerState::ReProvisioning { .. }
        )
    {
        actions.current.extend(device_firmware_current(
            associated_request.map(|request| request.requested_at),
            shelf.firmware_upgrade_status.as_ref(),
        ));
    }
    if let Some(request) = request
        && associated_request.is_none_or(|request| {
            !current_terminal(shelf.firmware_upgrade_status.as_ref(), request.requested_at)
        })
    {
        append_device_request(
            &mut actions,
            FirmwareRequestSource::ShelfReprovision,
            &request.activities,
            FirmwareActivity::FirmwareObject,
            request.requested_at,
            &request.initiator,
        );
    }
    actions
}

/// Device status supplies metadata only with a known request cycle. Its error
/// cause and the producer's `unknown` task-ID placeholder are never described.
fn device_firmware_current(
    requested_at: Option<DateTime<Utc>>,
    status: Option<&RackFirmwareUpgradeStatus>,
) -> Option<CurrentFirmwareWork> {
    let status = requested_at.and_then(|time| status.filter(|status| status.is_current_for(time)));
    if status.is_some_and(|status| status.is_terminal()) {
        return None;
    }
    Some(CurrentFirmwareWork {
        work: FirmwareWork::FirmwareObject { object_id: None },
        job_id: status.and_then(|status| known_job_id(&status.task_id)),
        started_at: status.and_then(|status| status.started_at),
        password_recovery_job_id: None,
    })
}

/// Uses the same start or end cycle boundary as the device's wait handler.
fn current_terminal(
    status: Option<&RackFirmwareUpgradeStatus>,
    requested_at: DateTime<Utc>,
) -> bool {
    status.is_some_and(|status| status.is_current_for(requested_at) && status.is_terminal())
}

/// Selects the first activity of this kind, as the controller does. An empty
/// list selects both phases, but does not provide an explicit operation target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FirmwareActivity {
    FirmwareObject,
    Nvos,
}

impl FirmwareActivity {
    /// Ignores repeated later activities rather than combining their payloads.
    fn selected_work(self, activities: &[MaintenanceActivity]) -> Option<FirmwareWork> {
        let kind = match self {
            Self::FirmwareObject => MaintenanceActivity::FirmwareUpgrade {
                firmware_version: None,
                components: Vec::new(),
                force_update: false,
            },
            Self::Nvos => MaintenanceActivity::NvosUpdate {
                config_json: String::new(),
            },
        };
        let selected = activities.iter().find(|activity| activity.same_kind(&kind));
        if selected.is_none() && !activities.is_empty() {
            return None;
        }
        let object_id = match selected {
            Some(MaintenanceActivity::FirmwareUpgrade {
                firmware_version, ..
            }) => firmware_version.as_deref().and_then(manifest_object_id),
            Some(MaintenanceActivity::NvosUpdate { config_json }) => {
                manifest_object_id(config_json)
            }
            _ => None,
        };
        Some(match self {
            Self::FirmwareObject => FirmwareWork::FirmwareObject { object_id },
            Self::Nvos => FirmwareWork::Nvos {
                object_id,
                version: None,
            },
        })
    }

    /// A later maintenance phase cannot recreate an earlier activity's request.
    fn pending_in(self, state: &RackState) -> bool {
        match state {
            RackState::Ready
            | RackState::Error { .. }
            | RackState::Maintenance {
                maintenance_state: RackMaintenanceState::FirmwareUpgrade { .. },
            } => true,
            RackState::Maintenance {
                maintenance_state: RackMaintenanceState::NVOSUpdate { .. },
            } => self == Self::Nvos,
            _ => false,
        }
    }
}

/// Device requests retain their saved origin and initiator even when the owning
/// rack is unknown. That uncertainty gates job metadata, not request existence.
fn append_device_request(
    actions: &mut StoredFirmwareActions,
    source: FirmwareRequestSource,
    activities: &[MaintenanceActivity],
    activity: FirmwareActivity,
    requested_at: DateTime<Utc>,
    initiator: &str,
) {
    if let Some(work) = activity.selected_work(activities) {
        actions.outstanding_requests.push(FirmwareRequestEvidence {
            source,
            work,
            requested_at: Some(requested_at),
            initiator: Some(initiator.to_owned()),
        });
    }
}

/// The actual member supplies identity and Core consumption evidence. A host's
/// selected BMC is the only MAC available to these projectors. Additional stored
/// MACs are retained in context::FirmwareReadContext::bmc_aliases.
#[derive(Clone, Copy)]
enum RackMember<'a> {
    Machine(&'a HostMachine),
    Switch(&'a Switch),
    Shelf(&'a PowerShelf),
}

impl RackMember<'_> {
    /// Follows the writer's typed ID lookup before considering a MAC address.
    fn firmware_device(self, job: &FirmwareUpgradeJob) -> Option<&FirmwareUpgradeDeviceStatus> {
        let devices = match self {
            Self::Machine(_) => &job.machines,
            Self::Switch(_) => &job.switches,
            Self::Shelf(_) => &job.power_shelves,
        };
        devices
            .iter()
            .find(|device| self.matches(&device.node_id, &device.mac))
    }

    /// NVOS records describe switches, including their password recovery phase.
    fn nvos_device(self, job: &NvosUpdateJob) -> Option<&NvosUpdateSwitchStatus> {
        job.switches
            .iter()
            .find(|device| self.matches(&device.node_id, &device.mac))
    }

    /// Full-rack scope includes each kind; partial scope selects typed IDs.
    fn selected(self, scope: &MaintenanceScope) -> bool {
        scope.is_full_rack()
            || match self {
                Self::Machine(host) => scope.machine_ids.contains(&host.id),
                Self::Switch(switch) => scope.switch_ids.contains(&switch.id),
                Self::Shelf(shelf) => scope.power_shelf_ids.contains(&shelf.id),
            }
    }

    /// A nonempty unrelated node ID cannot be repaired by guessing from its MAC.
    fn matches(self, node_id: &str, mac: &str) -> bool {
        if !node_id.is_empty() {
            return match self {
                Self::Machine(host) => node_id
                    .parse::<HostMachineId>()
                    .is_ok_and(|node| node == host.id),
                Self::Switch(switch) => node_id
                    .parse::<SwitchId>()
                    .is_ok_and(|node| node == switch.id),
                Self::Shelf(shelf) => node_id
                    .parse::<PowerShelfId>()
                    .is_ok_and(|node| node == shelf.id),
            };
        }
        let mac_address = match self {
            Self::Machine(host) => host.status.bmc_info.mac,
            Self::Switch(switch) => switch.bmc_mac_address,
            Self::Shelf(shelf) => shelf.bmc_mac_address,
        };
        mac_address
            .is_some_and(|expected| mac.parse::<MacAddress>().is_ok_and(|mac| mac == expected))
    }
}

/// The active owning phase and matching published job prove current metadata
/// independently of outstanding request association. Start without a job is not
/// submission; a saved default scope can still describe requested intent.
fn rack_actions(member: RackMember<'_>, rack: Option<&Rack>) -> StoredFirmwareActions {
    let Some(rack) = rack else {
        return StoredFirmwareActions::default();
    };
    let mut actions = StoredFirmwareActions::default();
    match &rack.controller_state.value {
        RackState::Maintenance {
            maintenance_state:
                RackMaintenanceState::FirmwareUpgrade {
                    rack_firmware_upgrade: FirmwareUpgradeState::WaitForComplete,
                },
        } => {
            if let Some(job) = &rack.firmware_upgrade_job
                && let Some(device) = member.firmware_device(job)
                && !device.status.is_terminal()
            {
                actions.current.push(CurrentFirmwareWork {
                    work: FirmwareWork::FirmwareObject {
                        object_id: job.firmware_id.as_deref().and_then(nonempty_id),
                    },
                    job_id: device
                        .job_id
                        .as_deref()
                        .or(device.parent_job_id.as_deref())
                        .or(job.job_id.as_deref())
                        .and_then(known_job_id),
                    started_at: job.started_at,
                    password_recovery_job_id: None,
                });
            }
        }
        RackState::Maintenance {
            maintenance_state:
                RackMaintenanceState::NVOSUpdate {
                    nvos_update: NvosUpdateState::WaitForComplete,
                },
        } if matches!(member, RackMember::Switch(_)) => {
            if let Some(job) = &rack.nvos_update_job
                && let Some(device) = member.nvos_device(job)
                && !nvos_device_settled(device)
            {
                // Initial image failure/completion can precede recovery. The
                // aggregate image result does not settle this member's workflow.
                actions.current.push(CurrentFirmwareWork {
                    work: FirmwareWork::Nvos {
                        object_id: nonempty_id(&job.firmware_id),
                        version: job.version.clone(),
                    },
                    job_id: device
                        .job_id
                        .as_deref()
                        .or(job.job_id.as_deref())
                        .and_then(known_job_id),
                    started_at: job.started_at,
                    password_recovery_job_id: match &device.password_update {
                        NvosPasswordUpdateState::InProgress { job_id } => nonempty_id(job_id),
                        _ => None,
                    },
                });
            }
        }
        _ => {}
    }
    if let Some(scope) = &rack.config.maintenance_requested
        && member.selected(scope)
    {
        for activity in [FirmwareActivity::FirmwareObject, FirmwareActivity::Nvos] {
            if activity == FirmwareActivity::Nvos && !matches!(member, RackMember::Switch(_)) {
                continue;
            }
            if activity.pending_in(&rack.controller_state.value)
                && !activity_settled(member, rack, activity)
                && let Some(work) = activity.selected_work(&scope.activities)
            {
                actions.outstanding_requests.push(FirmwareRequestEvidence {
                    source: FirmwareRequestSource::RackMaintenance,
                    work,
                    requested_at: scope.requested_at,
                    initiator: None,
                });
            }
        }
    }
    actions
}

/// Both image progress and desired-password recovery belong to one member's
/// NVOS workflow, including a failed image whose recovery has not started yet.
fn nvos_device_settled(device: &NvosUpdateSwitchStatus) -> bool {
    matches!(device.status.as_str(), "completed" | "failed")
        && device.password_update == NvosPasswordUpdateState::Completed
}

/// A scope outlives individual members. Retained jobs under Ready/Error need a
/// known cycle before consuming intent, and hardware cannot consume a host's
/// unapproved request. Shelf Completed unblock status is not backend work.
fn activity_settled(member: RackMember<'_>, rack: &Rack, activity: FirmwareActivity) -> bool {
    let Some(requested_at) = rack
        .config
        .maintenance_requested
        .as_ref()
        .and_then(|scope| scope.requested_at)
    else {
        return false;
    };
    match activity {
        FirmwareActivity::FirmwareObject => {
            if let RackMember::Machine(host) = member
                && host.host_reprovision_requested.is_some()
            {
                return false;
            }
            if let RackMember::Shelf(shelf) = member
                && current_terminal(shelf.firmware_upgrade_status.as_ref(), requested_at)
            {
                return true;
            }
            rack.firmware_upgrade_job
                .as_ref()
                .filter(|job| job.started_at.is_some_and(|start| start >= requested_at))
                .and_then(|job| member.firmware_device(job))
                .is_some_and(|device| device.status.is_terminal())
        }
        FirmwareActivity::Nvos => rack
            .nvos_update_job
            .as_ref()
            .filter(|job| job.started_at.is_some_and(|start| start >= requested_at))
            .and_then(|job| member.nvos_device(job))
            .is_some_and(nvos_device_settled),
    }
}

/// Read only the SOT identity field. Malformed JSON is unknown target metadata,
/// not a reason to return raw inline content as a display string.
fn manifest_object_id(json: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(json).ok()?;
    json.get("Id")
        .or_else(|| json.get("object_id"))?
        .as_str()
        .and_then(nonempty_id)
}

/// Empty scalar IDs establish no identity; other opaque values remain exact.
fn nonempty_id(id: &str) -> Option<String> {
    (!id.is_empty()).then(|| id.to_owned())
}

/// The rack status writer uses `unknown` when no backend supplied an ID.
fn known_job_id(id: &str) -> Option<String> {
    (!id.is_empty() && id != "unknown").then(|| id.to_owned())
}
