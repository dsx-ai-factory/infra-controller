// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Reads installed firmware, configured defaults and current tracked work.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use ::rpc::forge::component_firmware_expected::Result as ExpectedResult;
use ::rpc::forge::component_firmware_installed::Result as InstalledResult;
use ::rpc::forge::component_firmware_inventory_entry::Result as MemberResult;
use ::rpc::forge::get_component_firmware_details_request::Target;
use ::rpc::forge::{self as rpc};
use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::RackId;
use component_manager::compute_tray_manager::{
    Backend as ComputeTrayBackend, ComputeTrayEndpoint, ComputeTrayManager, ComputeTrayVendor,
};
use component_manager::error::ComponentManagerError;
use component_manager::firmware_inventory::{
    ComponentFirmwareInventoryResult, FirmwareInventorySource, FirmwareReadError,
};
use component_manager::firmware_jobs::{
    ComponentFirmwareJobsResult, FirmwareJobKind, FirmwareJobReadError, FirmwareJobSource,
    StagedFirmwareJob,
};
use component_manager::nv_switch_manager::SwitchEndpoint;
use component_manager::power_shelf_manager::{PowerShelfEndpoint, PowerShelfVendor};
use component_manager::{
    DeviceFirmwareExpectations, FirmwareExpectationError, FirmwareExpectationStatus,
    RackFirmwareExpectationsRequest, RackFirmwareUpdateManager,
};
use futures_util::future::BoxFuture;
use futures_util::{FutureExt, StreamExt, stream};
use mac_address::MacAddress;
use model::component_manager::FirmwareState;
use model::machine::{MachineInterfaceSnapshot, ManagedHostState};
use model::machine_interface::InterfaceType;
use model::power_shelf::PowerShelfControllerState;
use model::rack::{FirmwareUpgradeDeviceInfo, Rack};
use model::rack_type::RackProfile;
use model::switch::SwitchControllerState;
use rpc::{ComponentFirmwareErrorCode as ErrorCode, ComponentFirmwareSource as Source};
use tokio::time::Instant;
use tonic::{Request, Response, Status};

use super::super::{RackComponent, fetch_credentials, is_rack_scale_server};
use super::context::{
    self, FirmwareReadContext, FirmwareReadEntry, FirmwareTargets, MAX_TARGET_SLOTS,
    PreparationError,
};
use super::{FirmwareRequestSource, FirmwareWork, StoredFirmwareActions};
use crate::CarbideError;
use crate::api::{Api, RequestReceivedAt, log_request_data};

#[cfg(test)]
mod tests;

// Caps this request's active workflows: rack expectations, the job batch and
// one installed read per target. Backend limits apply separately to each call.
// The request owns and cancels all pending futures.
const READ_CONCURRENCY: usize = 8;
// Credential preparation may use eight slots while the outer expected workflow
// retains one manifest. Installed reads fetch their credentials independently.
const EXPECTED_CREDENTIAL_CONCURRENCY: usize = 8;
// Inventory members and comparison need more time than power reads. All work
// after database preparation shares this budget, capped by caller expiry.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `ReadEndpoint` is authorized by the preparation snapshot before credentials
/// are loaded. It is not a fence against later endpoint reassignment.
#[derive(Clone)]
enum ReadEndpoint {
    Compute {
        ip: IpAddr,
        mac: MacAddress,
        vendor: ComputeTrayVendor,
        standalone: bool,
    },
    Switch {
        row: Arc<db::switch::SwitchEndpointRow>,
        nvos_owned: bool,
    },
    Shelf(Arc<db::power_shelf::PowerShelfEndpointRow>),
}

/// Credentials belong only to the external-read phase, after snapshot release.
enum AuthenticatedEndpoint {
    Compute(ComputeTrayEndpoint),
    Switch(SwitchEndpoint),
    Shelf(PowerShelfEndpoint),
}

/// One prepared target combines stored evidence with independently read sections.
struct PendingEntry {
    details: rpc::ComponentFirmwareDetails,
    endpoint: Option<ReadEndpoint>,
}

/// Targets owned by one rack share its profile and one manifest fetch.
struct ExpectedGroup<'a> {
    rack: &'a Rack,
    profile: &'a RackProfile,
    targets: Vec<(usize, String, ReadEndpoint)>,
}

/// Selects the matching role field in an RMS comparison request.
#[derive(Clone, Copy)]
enum FirmwareRole {
    Compute,
    Switch,
    Shelf,
}

/// Associates a completed section with its original public target slot.
enum ReadUpdate {
    Installed(usize, rpc::ComponentFirmwareInstalled),
    Backend(Vec<(usize, rpc::ComponentFirmwareBackendWork)>),
    Expected(Vec<(usize, rpc::ComponentFirmwareExpected)>),
}

/// Loads stored facts first, then owns every external read until reply or drop.
/// The reply keeps configured defaults, current work and saved requests separate.
pub(crate) async fn get_component_firmware_details(
    api: &Api,
    request: Request<rpc::GetComponentFirmwareDetailsRequest>,
) -> Result<Response<rpc::GetComponentFirmwareDetailsResponse>, Status> {
    log_request_data(&request);
    let caller_deadline = caller_deadline(&request)?;
    let target = request
        .into_inner()
        .target
        .ok_or(CarbideError::MissingArgument("target is required"))?;
    validate_target(&target)?;
    let targets = match &target {
        Target::MachineIds(ids) => FirmwareTargets::Machines(&ids.machine_ids),
        Target::SwitchIds(ids) => FirmwareTargets::Switches(&ids.ids),
        Target::PowerShelfIds(ids) => FirmwareTargets::Shelves(&ids.ids),
    };
    let context = context::load(api.pg_pool(), &api.runtime_config, targets, caller_deadline)
        .await
        .map_err(preparation_failure)?;
    let deadline = caller_deadline.map_or_else(
        || Instant::now() + READ_TIMEOUT,
        |caller| caller.min(Instant::now() + READ_TIMEOUT),
    );
    let mut pending = Vec::with_capacity(context.entries.len());
    let mut expected = BTreeMap::<RackId, ExpectedGroup<'_>>::new();
    for entry in &context.entries {
        let index = pending.len();
        let (prepared, group) = prepare_entry(api, &context, entry, deadline);
        if let Some((rack, profile)) = group
            && let Some(endpoint) = prepared.endpoint.clone()
        {
            expected
                .entry(rack.id.clone())
                .or_insert_with(|| ExpectedGroup {
                    rack,
                    profile,
                    targets: Vec::new(),
                })
                .targets
                .push((index, prepared.details.component_id.clone(), endpoint));
        }
        pending.push(prepared);
    }

    // Admit expected work first so a slow series of installed reads cannot
    // consume its whole budget before the first manifest fetch. This single
    // workflow retains at most one bounded manifest at a time.
    let mut reads: Vec<BoxFuture<'_, ReadUpdate>> = Vec::new();
    if !expected.is_empty() {
        reads.push(
            async {
                ReadUpdate::Expected(
                    read_expected_groups(api, expected.into_values(), deadline).await,
                )
            }
            .boxed(),
        );
    }
    let endpoints = pending
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.endpoint.clone().map(|endpoint| (index, endpoint)))
        .collect::<Vec<_>>();
    if !endpoints.is_empty() {
        reads.push(
            async move { ReadUpdate::Backend(read_backend_batch(api, &endpoints, deadline).await) }
                .boxed(),
        );
    }
    for (index, entry) in pending.iter().enumerate() {
        if let Some(endpoint) = entry.endpoint.clone() {
            reads.push(async move {
                ReadUpdate::Installed(index, read_actual(api, &endpoint, deadline).await)
            }.boxed());
        }
    }
    let updates = stream::iter(reads)
        .buffer_unordered(READ_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for update in updates {
        match update {
            ReadUpdate::Installed(index, actual) => {
                pending[index].details.actual = Some(actual);
            }
            ReadUpdate::Backend(entries) => {
                for (index, backend) in entries {
                    pending[index]
                        .details
                        .actions
                        .as_mut()
                        .expect("prepare_entry always initializes actions")
                        .backend = Some(backend);
                }
            }
            ReadUpdate::Expected(entries) => {
                for (index, desired) in entries {
                    pending[index].details.desired = Some(desired);
                }
            }
        }
    }
    Ok(Response::new(rpc::GetComponentFirmwareDetailsResponse {
        entries: pending.into_iter().map(|entry| entry.details).collect(),
    }))
}

/// Converts the standard grpc-timeout using the original ingress clock. Direct
/// handler callers without listener middleware start at handler receipt instead.
fn caller_deadline<T>(request: &Request<T>) -> Result<Option<Instant>, CarbideError> {
    let Some(timeout) = request.metadata().get("grpc-timeout") else {
        return Ok(None);
    };
    let invalid = || CarbideError::InvalidArgument("invalid grpc-timeout".into());
    let text = timeout.to_str().map_err(|_| invalid())?;
    let (digits, unit) = text
        .split_at_checked(text.len().saturating_sub(1))
        .ok_or_else(invalid)?;
    // gRPC TimeoutValue permits at most eight ASCII digits.
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let amount = digits.parse::<u64>().map_err(|_| invalid())?;
    let duration = match unit {
        "H" => Duration::from_secs(amount * 3600),
        "M" => Duration::from_secs(amount * 60),
        "S" => Duration::from_secs(amount),
        "m" => Duration::from_millis(amount),
        "u" => Duration::from_micros(amount),
        "n" => Duration::from_nanos(amount),
        _ => return Err(invalid()),
    };
    request
        .extensions()
        .get::<RequestReceivedAt>()
        .map_or_else(Instant::now, |at| at.0)
        .checked_add(duration)
        .map(Some)
        .ok_or_else(invalid)
}

/// Requires bounded unique typed IDs before catalog or database work starts.
fn validate_target(target: &Target) -> Result<(), CarbideError> {
    /// Applies the same slot and uniqueness rule to each typed target list.
    fn ids<Id: Eq + Hash>(ids: &[Id]) -> Result<(), CarbideError> {
        if ids.is_empty()
            || ids.len() > MAX_TARGET_SLOTS
            || ids.iter().collect::<HashSet<_>>().len() != ids.len()
        {
            return Err(CarbideError::InvalidArgument(format!(
                "firmware details requires 1-{MAX_TARGET_SLOTS} unique IDs"
            )));
        }
        Ok(())
    }
    match target {
        Target::MachineIds(list) => ids(&list.machine_ids),
        Target::SwitchIds(list) => ids(&list.ids),
        Target::PowerShelfIds(list) => ids(&list.ids),
    }
}

/// Logs a bounded category and returns fixed public text without stored values.
fn preparation_failure(error: PreparationError) -> CarbideError {
    let error_category = match error {
        PreparationError::DeadlineExceeded => "deadline",
        PreparationError::Load(error) => match error.as_ref() {
            CarbideError::DBError(_) => "database",
            CarbideError::DatabaseTypeConversionError(_) => "model_conversion",
            _ => "preparation",
        },
    };
    tracing::warn!(error_category, "firmware details preparation failed");
    CarbideError::Internal {
        message: "firmware details preparation failed".into(),
    }
}

/// Resolves eligibility, stored evidence and source ownership without I/O.
fn prepare_entry<'a>(
    api: &Api,
    context: &'a FirmwareReadContext,
    entry: &FirmwareReadEntry,
    deadline: Instant,
) -> (PendingEntry, Option<(&'a Rack, &'a RackProfile)>) {
    let (id, rack_id, endpoint, actions, host) = match entry {
        FirmwareReadEntry::Machine {
            machine_id,
            machine: Some(machine),
        } => {
            if matches!(
                machine.state.value,
                ManagedHostState::ForceDeletion | ManagedHostState::Decommissioning { .. }
            ) {
                return (
                    unavailable_entry(machine_id.to_string(), ErrorCode::Ineligible),
                    None,
                );
            }
            let rack = machine
                .rack_id
                .as_ref()
                .and_then(|id| context.racks.get(id))
                .filter(|rack| rack.deleted.is_none());
            let endpoint = machine
                .status
                .bmc_info
                .ip
                .zip(machine.status.bmc_info.mac)
                .filter(|(ip, mac)| {
                    context.endpoint_owners.get(ip).is_some_and(|owner| {
                        owner.interface_type == InterfaceType::Bmc
                            && owner.mac_address == *mac
                            && owner.machine_id == Some((*machine_id).into())
                    })
                })
                .map(|(ip, mac)| ReadEndpoint::Compute {
                    ip,
                    mac,
                    vendor: machine.bmc_vendor().into(),
                    standalone: !is_rack_scale_server(machine),
                });
            (
                machine_id.to_string(),
                machine.rack_id.as_ref(),
                endpoint,
                super::machine(
                    machine,
                    rack,
                    context
                        .bmc_aliases
                        .get(machine_id)
                        .map_or(&[], Vec::as_slice),
                ),
                Some(machine.as_ref()),
            )
        }
        FirmwareReadEntry::Switch {
            switch_id,
            switch: Some(switch),
            endpoint,
        } => {
            if switch.deleted.is_some()
                || matches!(
                    switch.controller_state.value,
                    SwitchControllerState::Decommissioning { .. }
                )
            {
                return (
                    unavailable_entry(switch_id.to_string(), ErrorCode::Ineligible),
                    None,
                );
            }
            let rack = switch
                .rack_id
                .as_ref()
                .and_then(|id| context.racks.get(id))
                .filter(|rack| rack.deleted.is_none());
            let endpoint = endpoint
                .as_ref()
                .filter(|endpoint| {
                    switch.bmc_mac_address == Some(endpoint.bmc_mac)
                        && owns_switch(
                            context,
                            endpoint.bmc_ip,
                            endpoint.bmc_mac,
                            *switch_id,
                            InterfaceType::Bmc,
                        )
                })
                .cloned()
                .map(|row| {
                    // BMC ownership is enough for job reads and comparisons.
                    // Inventory additionally requires the selected NVOS role.
                    let nvos_owned = row.nvos_ip.zip(row.nvos_mac).is_some_and(|(ip, mac)| {
                        owns_switch(context, ip, mac, *switch_id, InterfaceType::Data)
                    });
                    ReadEndpoint::Switch { row, nvos_owned }
                });
            (
                switch_id.to_string(),
                switch.rack_id.as_ref(),
                endpoint,
                super::switch(switch, rack),
                None,
            )
        }
        FirmwareReadEntry::Shelf {
            power_shelf_id,
            shelf: Some(shelf),
            endpoint,
        } => {
            if shelf.deleted.is_some()
                || matches!(
                    shelf.controller_state.value,
                    PowerShelfControllerState::Decommissioning { .. }
                )
            {
                return (
                    unavailable_entry(power_shelf_id.to_string(), ErrorCode::Ineligible),
                    None,
                );
            }
            let rack = shelf
                .rack_id
                .as_ref()
                .and_then(|id| context.racks.get(id))
                .filter(|rack| rack.deleted.is_none());
            let endpoint = endpoint
                .as_ref()
                .filter(|endpoint| {
                    shelf.bmc_mac_address == Some(endpoint.pmc_mac)
                        && context
                            .endpoint_owners
                            .get(&endpoint.pmc_ip)
                            .is_some_and(|owner| {
                                owner.interface_type == InterfaceType::Bmc
                                    && owner.mac_address == endpoint.pmc_mac
                                    && owner.power_shelf_id == Some(*power_shelf_id)
                            })
                })
                .cloned()
                .map(ReadEndpoint::Shelf);
            (
                power_shelf_id.to_string(),
                shelf.rack_id.as_ref(),
                endpoint,
                super::shelf(shelf, rack),
                None,
            )
        }
        FirmwareReadEntry::Machine { machine_id, .. } => {
            return (
                unavailable_entry(machine_id.to_string(), ErrorCode::NotFound),
                None,
            );
        }
        FirmwareReadEntry::Switch { switch_id, .. } => {
            return (
                unavailable_entry(switch_id.to_string(), ErrorCode::NotFound),
                None,
            );
        }
        FirmwareReadEntry::Shelf { power_shelf_id, .. } => {
            return (
                unavailable_entry(power_shelf_id.to_string(), ErrorCode::NotFound),
                None,
            );
        }
    };
    let rack = rack_id.and_then(|id| context.racks.get(id));
    let rack_error = rack_id.and_then(|_| match rack {
        None => Some(ErrorCode::NotFound),
        Some(rack) if rack.deleted.is_some() => Some(ErrorCode::Ineligible),
        _ => None,
    });
    let mut stored = stored_actions(actions);
    stored.rack_context_error = rack_error.map(firmware_error);
    let mut desired = expected_error(ErrorCode::Unsupported, None);
    let mut group = None;
    if let Some(host) = host.filter(|host| !is_rack_scale_server(host)) {
        desired = host_expected(context, host, deadline);
    } else if let Some(cm) = &api.component_manager {
        // Mirror the normal routes with bypass_state_controller unset in
        // update_compute_tray_firmware_by_machine_ids, update_switch_firmware_by_ids
        // and update_power_shelf_firmware_by_ids in component_manager.rs.
        let uses_profile_firmware_object = match entry {
            FirmwareReadEntry::Machine { .. } => {
                cm.compute_tray_use_state_controller
                    || cm.compute_tray.backend() == ComputeTrayBackend::Rms
            }
            FirmwareReadEntry::Switch { .. } => {
                cm.nv_switch_use_state_controller || cm.nv_switch.supports_firmware_object_json()
            }
            FirmwareReadEntry::Shelf { .. } => {
                cm.power_shelf_use_state_controller
                    || cm.power_shelf.supports_firmware_object_json()
            }
        };
        if uses_profile_firmware_object {
            desired = expected_error(
                rack_error.unwrap_or(ErrorCode::ConfigurationUnavailable),
                Some(Source::Rms),
            );
            if let Some(rack) = rack.filter(|rack| rack.deleted.is_none())
                && let Some(profile) = rack
                    .rack_profile_id
                    .as_ref()
                    .and_then(|id| context.profiles.get(id))
            {
                if endpoint.is_some() {
                    // The scheduled rack comparison replaces this placeholder.
                    desired = expected_error(ErrorCode::DeadlineExceeded, Some(Source::Rms));
                    group = Some((rack, profile));
                } else {
                    desired = expected_error(ErrorCode::EndpointUnavailable, Some(Source::Rms));
                }
            }
        }
    } else {
        desired = expected_error(ErrorCode::Disabled, None);
    }
    (
        PendingEntry {
            details: rpc::ComponentFirmwareDetails {
                component_id: id,
                actual: Some(installed_error(ErrorCode::EndpointUnavailable, None)),
                desired: Some(desired),
                actions: Some(rpc::ComponentFirmwareActions {
                    stored: Some(stored),
                    backend: Some(backend_error(ErrorCode::EndpointUnavailable, None)),
                }),
            },
            endpoint,
        },
        group,
    )
}

/// Both switch roles must belong to the selected switch in the same snapshot.
fn owns_switch(
    context: &FirmwareReadContext,
    ip: IpAddr,
    mac: MacAddress,
    id: carbide_uuid::switch::SwitchId,
    role: InterfaceType,
) -> bool {
    context
        .endpoint_owners
        .get(&ip)
        .is_some_and(|owner: &MachineInterfaceSnapshot| {
            owner.interface_type == role && owner.mac_address == mac && owner.switch_id == Some(id)
        })
}

/// Missing/ineligible targets cannot supply outstanding work or external reads.
fn unavailable_entry(component_id: String, code: ErrorCode) -> PendingEntry {
    PendingEntry {
        details: rpc::ComponentFirmwareDetails {
            component_id,
            actual: Some(installed_error(code, None)),
            desired: Some(expected_error(code, None)),
            actions: Some(rpc::ComponentFirmwareActions {
                stored: Some(rpc::ComponentStoredFirmwareActions {
                    error: Some(firmware_error(code)),
                    ..Default::default()
                }),
                backend: Some(backend_error(code, None)),
            }),
        },
        endpoint: None,
    }
}

/// Stored reports choose host catalog identity only; they never supply installed
/// versions, collection freshness or an operation's frozen target.
fn host_expected(
    context: &FirmwareReadContext,
    host: &model::machine::HostMachine,
    deadline: Instant,
) -> rpc::ComponentFirmwareExpected {
    let selected = context
        .host_catalog
        .as_ref()
        .zip(
            host.status
                .bmc_info
                .ip
                .and_then(|ip| context.host_reports.get(&ip)),
        )
        .and_then(|(catalog, report)| super::host_expectations(catalog, report));
    if Instant::now() >= deadline {
        return expected_error(
            ErrorCode::DeadlineExceeded,
            Some(Source::EffectiveHostConfiguration),
        );
    }
    let Some(selected) = selected else {
        return expected_error(
            ErrorCode::ConfigurationUnavailable,
            Some(Source::EffectiveHostConfiguration),
        );
    };
    let mut entries = selected
        .versions
        .versions
        .into_iter()
        .map(
            |(component, version)| rpc::ComponentHostFirmwareExpectation {
                component: component.config_name().into_owned(),
                version,
            },
        )
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.component.cmp(&right.component));
    if Instant::now() >= deadline {
        return expected_error(
            ErrorCode::DeadlineExceeded,
            Some(Source::EffectiveHostConfiguration),
        );
    }
    rpc::ComponentFirmwareExpected {
        source: Some(Source::EffectiveHostConfiguration as i32),
        observed_at: None,
        result: Some(ExpectedResult::Host(
            rpc::ComponentHostFirmwareExpectations {
                vendor: selected.vendor.to_string(),
                model: selected.model,
                entries,
            },
        )),
    }
}

impl ReadEndpoint {
    /// Tracking uses the selected role-correct management MAC, never an OS MAC.
    fn mac(&self) -> MacAddress {
        match self {
            Self::Compute { mac, .. } => *mac,
            Self::Switch { row, .. } => row.bmc_mac,
            Self::Shelf(row) => row.pmc_mac,
        }
    }

    /// Loads inventory endpoint credentials after capability and ownership
    /// checks, with every lookup inside the enclosing read deadline.
    async fn authenticate(
        &self,
        api: &Api,
        deadline: Instant,
    ) -> Result<AuthenticatedEndpoint, ErrorCode> {
        until_deadline(deadline, async {
            let kind = match self {
                Self::Compute { .. } => RackComponent::ComputeBmc,
                Self::Switch { .. } => RackComponent::SwitchBmc,
                Self::Shelf(_) => RackComponent::PowerShelfPmc,
            };
            let credentials = read_credentials(api, kind, self.mac()).await?;
            if Instant::now() >= deadline {
                return Err(ErrorCode::DeadlineExceeded);
            }
            Ok(match self {
                Self::Compute {
                    ip, mac, vendor, ..
                } => AuthenticatedEndpoint::Compute(ComputeTrayEndpoint {
                    bmc_ip: *ip,
                    bmc_mac: *mac,
                    vendor: *vendor,
                    bmc_credentials: credentials,
                }),
                Self::Switch { row, .. } => {
                    let (Some(nvos_ip), Some(nvos_mac)) = (row.nvos_ip, row.nvos_mac) else {
                        return Err(ErrorCode::EndpointUnavailable);
                    };
                    let nvos_credentials =
                        read_credentials(api, RackComponent::SwitchNvos, row.bmc_mac).await?;
                    AuthenticatedEndpoint::Switch(SwitchEndpoint {
                        bmc_ip: row.bmc_ip,
                        bmc_mac: row.bmc_mac,
                        nvos_ip,
                        nvos_mac,
                        bmc_credentials: credentials,
                        nvos_credentials,
                        nvos_host_name: row.nvos_hostname.clone(),
                    })
                }
                Self::Shelf(row) => AuthenticatedEndpoint::Shelf(PowerShelfEndpoint {
                    pmc_ip: row.pmc_ip,
                    pmc_mac: row.pmc_mac,
                    pmc_vendor: PowerShelfVendor::DEFAULT,
                    pmc_credentials: credentials,
                }),
            })
        })
        .await
    }
}

/// Reports a bounded credential failure at the read boundary, without formatting
/// dependency text. The shared helper owns Vault and expected-inventory policy.
async fn read_credentials(
    api: &Api,
    role: RackComponent,
    mac: MacAddress,
) -> Result<Credentials, ErrorCode> {
    fetch_credentials(api, role, mac).await.map_err(|error| {
        let error_category = match error {
            ComponentManagerError::Internal(_) => "internal",
            _ => "credential_read_failed",
        };
        tracing::warn!(
            rack_component = ?role,
            management_mac_address = %mac,
            error_category,
            "component firmware credentials are unavailable"
        );
        ErrorCode::CredentialsUnavailable
    })
}

/// Rejects late ready results as well as timers, before another effect can start.
async fn until_deadline<T>(
    deadline: Instant,
    read: impl Future<Output = Result<T, ErrorCode>>,
) -> Result<T, ErrorCode> {
    if Instant::now() >= deadline {
        return Err(ErrorCode::DeadlineExceeded);
    }
    let result = tokio::time::timeout_at(deadline, read)
        .await
        .map_err(|_| ErrorCode::DeadlineExceeded)?;
    if Instant::now() >= deadline {
        return Err(ErrorCode::DeadlineExceeded);
    }
    result
}

/// Selects passive inventory before credentials. Direct standalone reads use
/// the same credential-operations pool as power, including at proxied sites.
async fn read_actual(
    api: &Api,
    endpoint: &ReadEndpoint,
    deadline: Instant,
) -> rpc::ComponentFirmwareInstalled {
    if Instant::now() >= deadline {
        return installed_error(ErrorCode::DeadlineExceeded, None);
    }
    if matches!(
        endpoint,
        ReadEndpoint::Switch {
            nvos_owned: false,
            ..
        }
    ) {
        return installed_error(ErrorCode::EndpointUnavailable, None);
    }
    let Some(cm) = &api.component_manager else {
        return installed_error(ErrorCode::Disabled, None);
    };
    let core = crate::handlers::component_manager::standalone_compute_tray_manager(api);
    let compute: &dyn ComputeTrayManager = if matches!(
        endpoint,
        ReadEndpoint::Compute {
            standalone: true,
            ..
        }
    ) {
        &core
    } else {
        cm.compute_tray.as_ref()
    };
    let source = match endpoint {
        ReadEndpoint::Compute { .. } => compute.firmware_inventory_source(),
        ReadEndpoint::Switch { .. } => cm.nv_switch.firmware_inventory_source(),
        ReadEndpoint::Shelf(_) => cm.power_shelf.firmware_inventory_source(),
    };
    let Some(source) = source else {
        return installed_error(ErrorCode::Unsupported, None);
    };
    let source = match source {
        FirmwareInventorySource::Redfish => Source::Redfish,
        FirmwareInventorySource::Rms => Source::Rms,
    };
    let result = until_deadline(deadline, async {
        let endpoint = endpoint.authenticate(api, deadline).await?;
        match endpoint {
            AuthenticatedEndpoint::Compute(endpoint) => {
                compute.get_firmware_inventory(&[endpoint]).await
            }
            AuthenticatedEndpoint::Switch(endpoint) => {
                cm.nv_switch.get_firmware_inventory(&[endpoint]).await
            }
            AuthenticatedEndpoint::Shelf(endpoint) => {
                cm.power_shelf.get_firmware_inventory(&[endpoint]).await
            }
        }
        .map_err(backend_failure)
    })
    .await;
    if let Err(code) = &result {
        tracing::debug!(
            management_mac_address = %endpoint.mac(),
            ?source,
            error_category = ?code,
            "component firmware inventory is unavailable"
        );
    }
    installed_from_inventory(result, endpoint.mac(), source, deadline)
}

/// Only one matching endpoint result may publish inventory and a receipt time.
fn installed_from_inventory(
    result: Result<Vec<ComponentFirmwareInventoryResult>, ErrorCode>,
    mac: MacAddress,
    source: Source,
    deadline: Instant,
) -> rpc::ComponentFirmwareInstalled {
    if Instant::now() >= deadline {
        return installed_error(ErrorCode::DeadlineExceeded, Some(source));
    }
    let result = result.and_then(|mut results| {
        if results.len() != 1 || results[0].mac_address != mac {
            return Err(ErrorCode::ReadFailed);
        }
        results.remove(0).inventory.map_err(inventory_failure)
    });
    let inventory = match result {
        Ok(entries) => rpc::ComponentFirmwareInventory {
            entries: entries
                .into_iter()
                .map(|entry| rpc::ComponentFirmwareInventoryEntry {
                    inventory_id: entry.inventory_id,
                    result: Some(match entry.firmware {
                        Ok(firmware) => {
                            MemberResult::Component(rpc::ComponentFirmwareInstalledComponent {
                                id: firmware.id,
                                name: firmware.name,
                                version: firmware.version,
                            })
                        }
                        Err(code) => MemberResult::Error(firmware_error(inventory_failure(code))),
                    }),
                })
                .collect(),
        },
        Err(code) => return installed_error(code, Some(source)),
    };
    if Instant::now() >= deadline {
        return installed_error(ErrorCode::DeadlineExceeded, Some(source));
    }
    rpc::ComponentFirmwareInstalled {
        source: Some(source as i32),
        observed_at: Some(chrono::Utc::now().into()),
        result: Some(InstalledResult::Inventory(inventory)),
    }
}

/// One homogeneous passive call retains the backend's bounded tracking reads and
/// polls. Its Vec is available only on completion; an unfinished outer timeout
/// has no partial reply to publish. Other public sections remain independent.
async fn read_backend_batch(
    api: &Api,
    endpoints: &[(usize, ReadEndpoint)],
    deadline: Instant,
) -> Vec<(usize, rpc::ComponentFirmwareBackendWork)> {
    let mut replies = Vec::new();
    let mut eligible = Vec::new();
    for (index, endpoint) in endpoints {
        let code = if Instant::now() >= deadline {
            Some(ErrorCode::DeadlineExceeded)
        } else if api.component_manager.is_none() {
            Some(ErrorCode::Disabled)
        } else if matches!(
            endpoint,
            ReadEndpoint::Compute {
                standalone: true,
                ..
            }
        ) {
            Some(ErrorCode::Unsupported)
        } else {
            None
        };
        if let Some(code) = code {
            replies.push((*index, backend_error(code, None)))
        } else {
            eligible.push((*index, endpoint))
        }
    }
    let Some((_, first)) = eligible.first() else {
        return replies;
    };
    let cm = api
        .component_manager
        .as_ref()
        .expect("eligible endpoints require a configured component manager");
    let source = match first {
        ReadEndpoint::Compute { .. } => cm.compute_tray.firmware_job_source(),
        ReadEndpoint::Switch { .. } => cm.nv_switch.firmware_job_source(),
        ReadEndpoint::Shelf(_) => cm.power_shelf.firmware_job_source(),
    };
    let Some(source) = source else {
        replies.extend(
            eligible
                .iter()
                .map(|(index, _)| (*index, backend_error(ErrorCode::Unsupported, None))),
        );
        return replies;
    };
    let source = match source {
        FirmwareJobSource::Rms => Source::Rms,
    };
    let macs = eligible
        .iter()
        .map(|(_, endpoint)| endpoint.mac())
        .collect::<Vec<_>>();
    let result = until_deadline(deadline, async {
        match first {
            ReadEndpoint::Compute { .. } => cm.compute_tray.get_firmware_jobs(&macs).await,
            ReadEndpoint::Switch { .. } => cm.nv_switch.get_firmware_jobs(&macs).await,
            ReadEndpoint::Shelf(_) => cm.power_shelf.get_firmware_jobs(&macs).await,
        }
        .map_err(backend_failure)
    })
    .await
    .and_then(|results| {
        if results.len() != eligible.len() {
            return Err(ErrorCode::ReadFailed);
        }
        Ok(results)
    });
    match result {
        Ok(results) => {
            for ((index, endpoint), result) in eligible.into_iter().zip(results) {
                let work = if result.mac_address == endpoint.mac() {
                    backend_work(result, source, deadline)
                } else {
                    backend_error(ErrorCode::ReadFailed, Some(source))
                };
                replies.push((index, work));
            }
        }
        Err(code) => replies.extend(
            eligible
                .iter()
                .map(|(index, _)| (*index, backend_error(code, Some(source)))),
        ),
    }
    replies
}

/// Publishes only nonterminal jobs. Unresolved current references remain
/// diagnostics, never synthetic active work or history attached to a new request.
fn backend_work(
    result: ComponentFirmwareJobsResult,
    source: Source,
    deadline: Instant,
) -> rpc::ComponentFirmwareBackendWork {
    let local_stage = match result.staged {
        Some(StagedFirmwareJob::Queued {
            system_image_job_id,
        }) => Some(rpc::ComponentFirmwareLocalStage {
            state: rpc::ComponentFirmwareLocalStageState::Queued as i32,
            system_image_job_id,
        }),
        Some(StagedFirmwareJob::Submitting {
            system_image_job_id,
        }) => Some(rpc::ComponentFirmwareLocalStage {
            state: rpc::ComponentFirmwareLocalStageState::Submitting as i32,
            system_image_job_id,
        }),
        Some(StagedFirmwareJob::Failed) | None => None,
    };
    let mut reply = rpc::ComponentFirmwareBackendWork {
        source: Some(source as i32),
        observed_at: Some(chrono::Utc::now().into()),
        local_stage,
        ..Default::default()
    };
    match result.jobs {
        Err(code) => reply.error = Some(firmware_error(job_failure(code))),
        Ok(jobs) => {
            for job in jobs {
                let kind = match job.kind {
                    FirmwareJobKind::FirmwareObject => {
                        rpc::ComponentFirmwareJobKind::FirmwareObject
                    }
                    FirmwareJobKind::SwitchSystemImage => {
                        rpc::ComponentFirmwareJobKind::SwitchSystemImage
                    }
                } as i32;
                let state = match job.state {
                    Ok(FirmwareState::Queued) => {
                        Ok(Some(rpc::ComponentFirmwareActiveJobState::Queued))
                    }
                    Ok(FirmwareState::InProgress) => {
                        Ok(Some(rpc::ComponentFirmwareActiveJobState::InProgress))
                    }
                    Ok(FirmwareState::Verifying) => {
                        Ok(Some(rpc::ComponentFirmwareActiveJobState::Verifying))
                    }
                    Ok(
                        FirmwareState::Completed | FirmwareState::Failed | FirmwareState::Cancelled,
                    ) => Ok(None),
                    Ok(FirmwareState::Unknown) => Err(ErrorCode::StatusUnavailable),
                    Err(code) => Err(job_failure(code)),
                };
                match state {
                    Ok(Some(state)) => reply.active_jobs.push(rpc::ComponentFirmwareActiveJob {
                        kind,
                        job_id: job.job_id,
                        state: state as i32,
                    }),
                    Ok(None) => {}
                    Err(code) => reply.unresolved_references.push(
                        rpc::ComponentFirmwareUnresolvedReference {
                            kind,
                            job_id: job.job_id,
                            error: Some(firmware_error(code)),
                        },
                    ),
                }
            }
        }
    }
    if Instant::now() >= deadline {
        return backend_error(ErrorCode::DeadlineExceeded, Some(source));
    }
    reply
}

/// Fetches the configured manifest once per rack, retaining completed comparisons
/// when a later rack fails. No database session spans the fetch or comparison;
/// artifact locations and authentication never enter the public response.
async fn read_expected_groups<'a>(
    api: &Api,
    groups: impl Iterator<Item = ExpectedGroup<'a>>,
    deadline: Instant,
) -> Vec<(usize, rpc::ComponentFirmwareExpected)> {
    let mut results = Vec::new();
    for group in groups {
        if Instant::now() >= deadline {
            results.extend(group.targets.iter().map(|(index, _, _)| {
                (
                    *index,
                    expected_error(ErrorCode::DeadlineExceeded, Some(Source::Rms)),
                )
            }));
            continue;
        }
        let Some(client) = &api.rms_client else {
            results.extend(group.targets.iter().map(|(index, _, _)| {
                (
                    *index,
                    expected_error(ErrorCode::Unsupported, Some(Source::Rms)),
                )
            }));
            continue;
        };
        let Some(source) = &group.profile.firmware_object else {
            results.extend(group.targets.iter().map(|(index, _, _)| {
                (
                    *index,
                    expected_error(ErrorCode::ConfigurationUnavailable, Some(Source::Rms)),
                )
            }));
            continue;
        };
        let manifest = until_deadline(deadline, async {
            let timeout = source
                .fetch_timeout
                .min(deadline.saturating_duration_since(Instant::now()));
            let json = api
                .firmware_object_fetcher
                .fetch(source.url.as_str(), timeout)
                .await
                .map_err(|_| ErrorCode::ManifestUnavailable)?;
            serde_json::from_str::<std::collections::HashMap<String, serde::de::IgnoredAny>>(&json)
                .map_err(|_| ErrorCode::ManifestUnavailable)?;
            Ok(json)
        })
        .await;
        let manifest = match manifest {
            Ok(manifest) => manifest,
            Err(code) => {
                results.extend(
                    group
                        .targets
                        .iter()
                        .map(|(index, _, _)| (*index, expected_error(code, Some(Source::Rms)))),
                );
                continue;
            }
        };
        let mut devices = Vec::new();
        let prepared = stream::iter(group.targets)
            .map(|(index, node_id, endpoint)| async move {
                (
                    index,
                    endpoint.comparison_device(node_id, api, deadline).await,
                )
            })
            .buffer_unordered(EXPECTED_CREDENTIAL_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for (index, device) in prepared {
            match device {
                Ok(device) => devices.push((index, device)),
                Err(code) => results.push((index, expected_error(code, Some(Source::Rms)))),
            }
        }
        if devices.is_empty() {
            continue;
        }
        // A homogeneous public request supplies exactly one role. Canonical
        // managed IDs remain the RMS caller identities, even if preparation
        // finished in a different order or some peers failed credentials.
        let mut machines = Vec::new();
        let mut switches = Vec::new();
        let mut shelves = Vec::new();
        let mut slots = Vec::with_capacity(devices.len());
        for (index, (role, device)) in devices {
            slots.push((index, device.node_id.clone()));
            match role {
                FirmwareRole::Compute => machines.push(device),
                FirmwareRole::Switch => switches.push(device),
                FirmwareRole::Shelf => shelves.push(device),
            }
        }
        let manager = component_manager::rms::rms_rack_firmware_update_manager(client.clone());
        let response = until_deadline(deadline, async {
            manager
                .get_firmware_expectations(RackFirmwareExpectationsRequest {
                    rack_id: &group.rack.id,
                    profile: group.profile,
                    config_json: &manifest,
                    components: &[],
                    machines: &machines,
                    switches: &switches,
                    power_shelves: &shelves,
                })
                .await
                .map_err(|error| match error {
                    // Public node IDs are typed and nonempty; all remaining
                    // InvalidArgument causes are comparison application limits.
                    ComponentManagerError::InvalidArgument(_) => ErrorCode::LimitExceeded,
                    error => backend_failure(error),
                })
        })
        .await
        .and_then(|response| {
            let requested = slots
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<HashSet<_>>();
            let returned = response
                .iter()
                .map(|entry| entry.node_id.as_str())
                .collect::<HashSet<_>>();
            if response.len() != slots.len() || returned != requested {
                return Err(ErrorCode::ReadFailed);
            }
            Ok(response)
        });
        for (index, node_id) in slots {
            let desired = expected_from_comparisons(&response, &node_id, deadline);
            results.push((index, desired));
        }
    }
    results
}

impl ReadEndpoint {
    /// Builds a passive comparison descriptor from the owned management endpoint.
    /// NVOS is optional; unowned endpoints never receive credentials. Its absence
    /// does not erase BMC-authorized configured expectations or tracked jobs.
    async fn comparison_device(
        &self,
        node_id: String,
        api: &Api,
        deadline: Instant,
    ) -> Result<(FirmwareRole, FirmwareUpgradeDeviceInfo), ErrorCode> {
        until_deadline(deadline, async {
            let (role, ip, mac, kind) = match self {
                Self::Compute { ip, mac, .. } => {
                    (FirmwareRole::Compute, *ip, *mac, RackComponent::ComputeBmc)
                }
                Self::Switch { row, .. } => (
                    FirmwareRole::Switch,
                    row.bmc_ip,
                    row.bmc_mac,
                    RackComponent::SwitchBmc,
                ),
                Self::Shelf(row) => (
                    FirmwareRole::Shelf,
                    row.pmc_ip,
                    row.pmc_mac,
                    RackComponent::PowerShelfPmc,
                ),
            };
            let Credentials::UsernamePassword { username, password } =
                read_credentials(api, kind, mac).await?;
            let mut device = FirmwareUpgradeDeviceInfo {
                node_id,
                mac: mac.to_string(),
                bmc_ip: ip.to_string(),
                bmc_username: username,
                bmc_password: password,
                os_mac: None,
                os_ip: None,
                os_username: None,
                os_password: None,
                os_hostname: None,
            };
            if Instant::now() >= deadline {
                return Err(ErrorCode::DeadlineExceeded);
            }
            if let Self::Switch {
                row,
                nvos_owned: true,
            } = self
            {
                // Ownership was validated in the same preparation snapshot.
                // A credential miss leaves only the independently owned BMC.
                if let Some((ip, mac)) = row.nvos_ip.zip(row.nvos_mac)
                    && let Ok(Credentials::UsernamePassword { username, password }) =
                        read_credentials(api, RackComponent::SwitchNvos, row.bmc_mac).await
                {
                    device.os_ip = Some(ip.to_string());
                    device.os_mac = Some(mac.to_string());
                    device.os_username = Some(username);
                    device.os_password = Some(password);
                    device.os_hostname = row.nvos_hostname.clone().filter(|name| !name.is_empty());
                }
            }
            Ok((role, device))
        })
        .await
    }
}

/// Maps one exact node's comparison rows into the public expected section.
/// Missing/duplicate/foreign identity is a failure; opaque targets are omitted.
fn expected_from_comparisons(
    response: &Result<Vec<DeviceFirmwareExpectations>, ErrorCode>,
    id: &str,
    deadline: Instant,
) -> rpc::ComponentFirmwareExpected {
    if Instant::now() >= deadline {
        return expected_error(ErrorCode::DeadlineExceeded, Some(Source::Rms));
    }
    let entries = match response {
        Err(code) => return expected_error(*code, Some(Source::Rms)),
        Ok(entries) => entries,
    };
    let matching = entries
        .iter()
        .filter(|entry| entry.node_id == id)
        .collect::<Vec<_>>();
    let [entry] = matching.as_slice() else {
        return expected_error(ErrorCode::ReadFailed, Some(Source::Rms));
    };
    let rows = match &entry.expectations {
        Err(code) => return expected_error(expectation_failure(*code), Some(Source::Rms)),
        Ok(rows) => rows,
    };
    let entries = rows
        .iter()
        .map(|row| {
            let (status, unrecognized_status) = match row.status {
                FirmwareExpectationStatus::Unspecified => {
                    (rpc::ComponentFirmwareComparisonStatus::Unspecified, None)
                }
                FirmwareExpectationStatus::Matched => {
                    (rpc::ComponentFirmwareComparisonStatus::Matched, None)
                }
                FirmwareExpectationStatus::Mismatched => {
                    (rpc::ComponentFirmwareComparisonStatus::Mismatched, None)
                }
                FirmwareExpectationStatus::Missing => {
                    (rpc::ComponentFirmwareComparisonStatus::Missing, None)
                }
                FirmwareExpectationStatus::Unverified => {
                    (rpc::ComponentFirmwareComparisonStatus::Unverified, None)
                }
                FirmwareExpectationStatus::Unknown(value) => (
                    rpc::ComponentFirmwareComparisonStatus::Unrecognized,
                    Some(value),
                ),
            };
            rpc::ComponentFirmwareComparison {
                component: row.component.clone(),
                proposed_version: row.proposed_version.clone(),
                sku_id: row.sku_id.clone(),
                status: status as i32,
                unrecognized_status,
            }
        })
        .collect();
    if Instant::now() >= deadline {
        return expected_error(ErrorCode::DeadlineExceeded, Some(Source::Rms));
    }
    rpc::ComponentFirmwareExpected {
        source: Some(Source::Rms as i32),
        observed_at: Some(chrono::Utc::now().into()),
        result: Some(ExpectedResult::Comparisons(
            rpc::ComponentFirmwareComparisons { entries },
        )),
    }
}

/// Preserves sanitized projector fields without reinterpreting saved requests.
fn stored_actions(actions: StoredFirmwareActions) -> rpc::ComponentStoredFirmwareActions {
    rpc::ComponentStoredFirmwareActions {
        current: actions
            .current
            .into_iter()
            .map(|work| rpc::ComponentCurrentFirmwareWork {
                target: Some(work_target(work.work)),
                job_id: work.job_id,
                started_at: work.started_at.map(Into::into),
                password_recovery_job_id: work.password_recovery_job_id,
            })
            .collect(),
        outstanding_requests: actions
            .outstanding_requests
            .into_iter()
            .map(|request| rpc::ComponentFirmwareRequest {
                source: match request.source {
                    FirmwareRequestSource::HostReprovision => {
                        rpc::ComponentFirmwareRequestSource::HostReprovision
                    }
                    FirmwareRequestSource::SwitchReprovision => {
                        rpc::ComponentFirmwareRequestSource::SwitchReprovision
                    }
                    FirmwareRequestSource::ShelfReprovision => {
                        rpc::ComponentFirmwareRequestSource::ShelfReprovision
                    }
                    FirmwareRequestSource::RackMaintenance => {
                        rpc::ComponentFirmwareRequestSource::RackMaintenance
                    }
                } as i32,
                target: Some(work_target(request.work)),
                requested_at: request.requested_at.map(Into::into),
                initiator: request.initiator,
            })
            .collect(),
        error: None,
        rack_context_error: None,
    }
}

/// Maps projector-selected target facts into the public work variant.
fn work_target(work: FirmwareWork) -> rpc::ComponentFirmwareWorkTarget {
    use rpc::component_firmware_work_target::Target;
    rpc::ComponentFirmwareWorkTarget {
        target: Some(match work {
            FirmwareWork::Host { component, version } => {
                Target::Host(rpc::ComponentHostFirmwareTarget {
                    component: component.map(|component| component.config_name().into_owned()),
                    version,
                })
            }
            FirmwareWork::FirmwareObject { object_id } => {
                Target::FirmwareObject(rpc::ComponentFirmwareObjectTarget { object_id })
            }
            FirmwareWork::Nvos { object_id, version } => {
                Target::Nvos(rpc::ComponentNvosFirmwareTarget { object_id, version })
            }
        }),
    }
}

/// Only fixed failure text crosses the public boundary. Free-form diagnostics
/// from dependencies, manifests and stored records are never formatted here.
fn firmware_error(code: ErrorCode) -> rpc::ComponentFirmwareError {
    let message = match code {
        ErrorCode::NotFound => "managed component or owning rack not found",
        ErrorCode::Ineligible => "managed component or owning rack is being removed",
        ErrorCode::Disabled => "component manager is not configured",
        ErrorCode::Unsupported => "passive firmware read is unsupported",
        ErrorCode::EndpointUnavailable => "management endpoint is unavailable",
        ErrorCode::CredentialsUnavailable => "management credentials are unavailable",
        ErrorCode::ConfigurationUnavailable => "firmware configuration is unavailable",
        ErrorCode::ManifestUnavailable => "configured firmware object is unavailable",
        ErrorCode::LimitExceeded => "firmware evidence exceeds read limits",
        ErrorCode::DeadlineExceeded => "firmware read deadline exceeded",
        ErrorCode::TrackingReadFailed => "firmware job tracking is unavailable",
        ErrorCode::JobNotFound => "backend no longer knows the retained job",
        ErrorCode::PollFailed => "firmware job poll failed",
        ErrorCode::StatusUnavailable => "firmware job status is unavailable",
        ErrorCode::ReadFailed | ErrorCode::Unspecified => "firmware read failed",
    };
    rpc::ComponentFirmwareError {
        code: code as i32,
        message: message.into(),
    }
}

/// An unavailable installed section has no receipt time or successful default.
fn installed_error(code: ErrorCode, source: Option<Source>) -> rpc::ComponentFirmwareInstalled {
    rpc::ComponentFirmwareInstalled {
        source: source.map(|source| source as i32),
        observed_at: None,
        result: Some(InstalledResult::Error(firmware_error(code))),
    }
}

/// Builds an unavailable expected section without a receipt timestamp.
fn expected_error(code: ErrorCode, source: Option<Source>) -> rpc::ComponentFirmwareExpected {
    rpc::ComponentFirmwareExpected {
        source: source.map(|source| source as i32),
        observed_at: None,
        result: Some(ExpectedResult::Error(firmware_error(code))),
    }
}

/// Builds a failed backend section before a typed reader reply is available.
fn backend_error(code: ErrorCode, source: Option<Source>) -> rpc::ComponentFirmwareBackendWork {
    rpc::ComponentFirmwareBackendWork {
        source: source.map(|source| source as i32),
        error: Some(firmware_error(code)),
        ..Default::default()
    }
}

/// Keeps whole-call dependency text out of both wire errors and diagnostics.
fn backend_failure(error: ComponentManagerError) -> ErrorCode {
    let code = if matches!(error, ComponentManagerError::Unsupported(_)) {
        ErrorCode::Unsupported
    } else {
        ErrorCode::ReadFailed
    };
    tracing::debug!(error_category = ?code, "passive firmware backend call failed");
    code
}

/// Maps bounded inventory failures without displaying dependency payloads.
fn inventory_failure(error: FirmwareReadError) -> ErrorCode {
    match error {
        FirmwareReadError::ReadFailed => ErrorCode::ReadFailed,
        FirmwareReadError::DeadlineExceeded => ErrorCode::DeadlineExceeded,
        FirmwareReadError::LimitExceeded => ErrorCode::LimitExceeded,
        FirmwareReadError::Unsupported => ErrorCode::Unsupported,
    }
}

/// Maps the passive comparison reader's bounded per-node failure.
fn expectation_failure(error: FirmwareExpectationError) -> ErrorCode {
    match error {
        FirmwareExpectationError::ReadFailed => ErrorCode::ReadFailed,
        FirmwareExpectationError::DeadlineExceeded => ErrorCode::DeadlineExceeded,
        FirmwareExpectationError::LimitExceeded => ErrorCode::LimitExceeded,
        FirmwareExpectationError::Unsupported => ErrorCode::Unsupported,
    }
}

/// Maps known job absence, poll failure and unknown status into public errors.
fn job_failure(error: FirmwareJobReadError) -> ErrorCode {
    match error {
        FirmwareJobReadError::TrackingReadFailed => ErrorCode::TrackingReadFailed,
        FirmwareJobReadError::JobNotFound => ErrorCode::JobNotFound,
        FirmwareJobReadError::PollFailed => ErrorCode::PollFailed,
        FirmwareJobReadError::StatusUnavailable => ErrorCode::StatusUnavailable,
        FirmwareJobReadError::DeadlineExceeded => ErrorCode::DeadlineExceeded,
    }
}
