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

use std::net::SocketAddr;

use ::rpc::forge as rpc;
use ::rpc::model::machine::machine_id::try_parse_machine_id;
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::device::DeviceId;
use carbide_uuid::machine::{HostMachineId, HostMachineIdSubtype, MachineId};
use db::WithTransaction;
use db::machine_interface::find_by_ip;
use libredfish::RoleId;
use mac_address::MacAddress;
use model::expected_entity::ExpectedEntity;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{LoadSnapshotOptions, MachineInterfaceSnapshot, ManagedHostState};
use model::machine_boot_interface::{
    BootInterfaceSelectionAuthority, MachineBootInterface, MachineBootInterfaceTarget,
    canonical_redfish_boot_interface_id,
};
use model::predicted_machine_interface::PredictedMachineInterface;
use model::site_explorer::{BlueFieldOperatingMode, PreingestionState};
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data, log_request_data_redacted};
use crate::handlers::utils::{enqueue_boot_interface_reconciliation, resolve_bmc_address};

/// Converts the admin request into the authority used by boot reconciliation.
///
/// An entered MAC is the request's evidence that the operator selected the
/// target. Omitting it reapplies the stored target and must preserve the source
/// and decision time that originally selected it.
fn admin_selection_authority(entered_mac: Option<MacAddress>) -> BootInterfaceSelectionAuthority {
    match entered_mac {
        Some(_) => BootInterfaceSelectionAuthority::Operator,
        None => BootInterfaceSelectionAuthority::Existing,
    }
}

/// Resolves the boot interface an admin Redfish action should target.
///
/// When a machine exists for the endpoint, its persisted desired target
/// decides first. This preserves an operator-selected boot NIC independently
/// from the networking-primary interface. Hosts without persisted intent fall
/// back to the owned-interface and prediction selection the machine-controller
/// uses today; controller convergence to the persisted target is separate.
///
/// A machine with no `machine_interfaces` rows yet (a zero-DPU/NIC-mode
/// machine awaiting its first DHCP lease) resolves from its
/// `predicted_machine_interfaces` instead: the predicted NIC's MAC and
/// recorded Redfish interface id form the same [`MachineBootInterface`] the
/// real row will hold once the lease promotes it. The candidate is chosen by
/// the shared `pick_boot_prediction` -- the declared `ExpectedInterface.primary`
/// (recorded on the prediction), else the sole non-underlay prediction. With
/// several (e.g. a host whose report lists SuperNICs alongside the boot NIC) and
/// none declared primary the boot NIC is unknowable; resolution refuses to guess
/// and the action keeps requiring an explicit MAC, which the matching
/// prediction's recorded id completes. The machine-controller resolves the same
/// way, through the same `pick_boot_prediction`.
///
/// Site-explorer's stored default (`ExploredEndpoint::boot_interface()`)
/// answers only for endpoints no machine owns. An owned machine resolves
/// from its own rows and predictions alone -- when neither offers an
/// unambiguous candidate, there is no target and the action requires an
/// explicit MAC, matching the machine-controller (which never consults the
/// explored default either).
///
/// An explicitly entered MAC is always honored as given, never redirected to
/// another NIC. It is completed to a pair only when the owned rows, then the
/// predictions, offer one unambiguous non-empty interface id. Conflicting
/// owned ids are an ambiguity barrier and never fall through to predictions.
/// A persisted pair for the same MAC is retained rather than degraded to
/// MAC-only.
fn resolve_admin_boot_interface_target(
    stored: Option<MachineBootInterface>,
    desired: Option<&MachineBootInterfaceTarget>,
    candidates: Option<&BootInterfaceCandidates>,
    entered_mac: Option<MacAddress>,
) -> Option<BootInterfaceTarget> {
    enum UniqueInterfaceId {
        Missing,
        One(String),
        Conflicting,
    }

    fn unique_interface_id<'a>(ids: impl Iterator<Item = &'a str>) -> UniqueInterfaceId {
        let mut unique = ids.filter_map(canonical_redfish_boot_interface_id);
        let Some(first) = unique.next() else {
            return UniqueInterfaceId::Missing;
        };
        if unique.any(|id| id != first) {
            UniqueInterfaceId::Conflicting
        } else {
            UniqueInterfaceId::One(first.to_string())
        }
    }

    // The machine's unambiguous `MachineBootInterface` for `mac`, if known:
    // owned rows first, then predictions only when owned rows offer no id.
    let pair_from_candidates = |mac: MacAddress| -> Option<MachineBootInterface> {
        let candidates = candidates?;
        let owned = unique_interface_id(
            candidates
                .interfaces
                .iter()
                .filter(|row| row.mac_address == mac)
                .filter_map(|row| row.boot_interface_id.as_deref()),
        );
        let interface_id = match owned {
            UniqueInterfaceId::One(interface_id) => interface_id,
            UniqueInterfaceId::Conflicting => return None,
            UniqueInterfaceId::Missing => match unique_interface_id(
                candidates
                    .predicted
                    .iter()
                    .filter(|predicted| predicted.mac_address == mac)
                    .filter_map(|predicted| predicted.boot_interface_id.as_deref()),
            ) {
                UniqueInterfaceId::One(interface_id) => interface_id,
                UniqueInterfaceId::Missing | UniqueInterfaceId::Conflicting => return None,
            },
        };
        Some(MachineBootInterface {
            mac_address: mac,
            interface_id,
        })
    };
    let desired_pair_for = |mac: MacAddress| match desired {
        Some(MachineBootInterfaceTarget::Pair(pair)) if pair.mac_address == mac => {
            Some(pair.clone())
        }
        Some(MachineBootInterfaceTarget::Pair(_))
        | Some(MachineBootInterfaceTarget::MacOnly(_))
        | None => None,
    };
    // Resolution chose `mac`; use its candidate pair when known, or target the
    // MAC alone when no `interface_id` has been captured.
    let target_from_candidates = |mac: MacAddress| -> BootInterfaceTarget {
        pair_from_candidates(mac)
            .map_or(BootInterfaceTarget::MacOnly(mac), BootInterfaceTarget::Pair)
    };

    match entered_mac {
        Some(mac) => Some(
            pair_from_candidates(mac)
                .or_else(|| desired_pair_for(mac))
                .or_else(|| {
                    candidates
                        .is_none()
                        .then(|| stored.filter(|pair| pair.mac_address == mac))
                        .flatten()
                })
                .map_or(BootInterfaceTarget::MacOnly(mac), BootInterfaceTarget::Pair),
        ),
        None => {
            let Some(candidates) = candidates else {
                // No machine owns the endpoint -- the explored default
                // answers, when site-explorer has recorded one.
                return stored.map(BootInterfaceTarget::Pair);
            };
            if let Some(desired) = desired {
                return Some(match desired {
                    // An explicit administrative reapply may refresh the
                    // Redfish ID from the current owned row. Passive Site
                    // Explorer enrichment changes only a target that has no
                    // interface ID, so an existing pair is never replaced
                    // silently.
                    MachineBootInterfaceTarget::Pair(pair) => BootInterfaceTarget::Pair(
                        pair_from_candidates(pair.mac_address).unwrap_or_else(|| pair.clone()),
                    ),
                    MachineBootInterfaceTarget::MacOnly(mac_address) => {
                        target_from_candidates(*mac_address)
                    }
                });
            }
            if let Some(picked) = model::machine::pick_boot_interface(&candidates.interfaces) {
                return Some(target_from_candidates(picked.mac_address));
            }
            // The rows offered no boot candidate: the machine's predicted NICs
            // answer, via the shared `pick_boot_prediction` -- the declared
            // primary, else the sole non-underlay prediction. With several and
            // none declared primary the boot NIC is unknowable, so it returns
            // `None` and the action keeps requiring an explicit MAC.
            if let Some(predicted) = model::machine::pick_boot_prediction(&candidates.predicted) {
                return Some(target_from_candidates(predicted.mac_address));
            }
            // An owned machine resolves from its own data alone: no
            // unambiguous candidate means no target, and the action requires
            // an explicit MAC -- never a guess from the explored default.
            None
        }
    }
}

/// Parses the optional admin field after treating whitespace-only input as
/// absent.
fn parse_boot_interface_mac(value: Option<&str>) -> Result<Option<MacAddress>, CarbideError> {
    value
        .map(str::trim)
        .none_if_empty()
        .map(str::parse::<MacAddress>)
        .transpose()
        .map_err(|error| {
            CarbideError::InvalidArgument(format!("invalid boot_interface_mac: {error}"))
        })
}

/// Locks a managed host's desired generation so target resolution and the
/// forced reapply cannot race another desired-state writer.
async fn desired_boot_interface_target(
    txn: &mut PgConnection,
    machine_id: Option<MachineId>,
) -> Result<Option<MachineBootInterfaceTarget>, CarbideError> {
    let Some(Ok(host_machine_id)) = machine_id.map(HostMachineId::try_from) else {
        return Ok(None);
    };
    Ok(
        db::machine_desired_boot_interface::lock(txn, &host_machine_id)
            .await?
            .map(|desired| desired.value),
    )
}

/// Returns whether a confirmed host can start reconciliation immediately.
///
/// Predicted, assigned, and otherwise in-flight hosts keep the new generation
/// pending. An unassigned `Ready` host is safe to wake only when no instance is
/// attached.
async fn boot_interface_reconciliation_eligible(
    txn: &mut PgConnection,
    machine_id: Option<HostMachineId>,
) -> Result<bool, CarbideError> {
    let Some(HostMachineIdSubtype::Stable(machine_id)) =
        machine_id.map(|id| id.host_machine_id_subtype())
    else {
        return Ok(false);
    };
    let machine = db::machine::find_one(&mut *txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    if !matches!(machine.current_state(), ManagedHostState::Ready) {
        return Ok(false);
    }

    Ok(db::instance::find_id_by_machine_id(txn, &machine_id)
        .await?
        .is_none())
}

/// Resolves a required declarative target when the endpoint's actual owner is
/// a confirmed or predicted host.
///
/// Once owned, the endpoint cannot fall back to Site Explorer's explored
/// default: missing machine data is an operator-visible error rather than a
/// guess at a stale interface.
fn managed_boot_interface_target(
    machine_id: Option<MachineId>,
    desired: Option<&MachineBootInterfaceTarget>,
    candidates: Option<&BootInterfaceCandidates>,
    entered_mac: Option<MacAddress>,
) -> Result<Option<(HostMachineId, BootInterfaceTarget)>, CarbideError> {
    let Some(Ok(host_machine_id)) = machine_id.map(HostMachineId::try_from) else {
        return Ok(None);
    };
    let target = resolve_admin_boot_interface_target(None, desired, candidates, entered_mac)
        .ok_or_else(|| {
            CarbideError::InvalidArgument(
                "no boot interface available: enter a MAC or explore the host first".to_string(),
            )
        })?;
    Ok(Some((host_machine_id, target)))
}

/// What a host machine offers boot-interface resolution to select from: its
/// real `machine_interfaces` rows, and -- for the window before a NIC's first
/// DHCP lease creates a real row -- its `predicted_machine_interfaces`.
struct BootInterfaceCandidates {
    /// The machine's non-BMC `machine_interfaces` rows. When they offer a
    /// boot candidate (the machine-controller's own `pick_boot_interface`
    /// selection), it is the first fallback when no desired target is stored.
    interfaces: Vec<MachineInterfaceSnapshot>,
    /// The machine's predicted interfaces, consulted only when the rows
    /// offer no fallback candidate -- none exist yet (zero-DPU/NIC-mode
    /// machines awaiting their first lease), or none are selectable (e.g.
    /// only underlay-typed declared NICs).
    predicted: Vec<PredictedMachineInterface>,
}

/// Load what boot-interface resolution selects from, when the BMC endpoint
/// belongs to a (predicted or confirmed) host machine.
///
/// Returns `None` -- meaning resolution falls through to the explored
/// default -- for endpoints with no machine, and for DPU machines, whose own
/// setup runs without a boot-interface target, exactly like the
/// machine-controller path. A host machine always gets `Some`, though both
/// lists can be empty (`find_by_machine_ids` filters BMC rows, so a host
/// whose only discovered interface is its BMC offers no real candidates).
async fn boot_interface_candidates(
    txn: &mut PgConnection,
    machine_id: Option<MachineId>,
) -> Result<Option<BootInterfaceCandidates>, CarbideError> {
    let Some(machine_id) = machine_id.filter(|id| !id.machine_type().is_dpu()) else {
        return Ok(None);
    };
    let interfaces = db::machine_interface::find_by_machine_ids(txn, &[machine_id])
        .await?
        .remove(&machine_id)
        .unwrap_or_default();
    let predicted = db::predicted_machine_interface::find_by_machine_id(txn, &machine_id).await?;
    Ok(Some(BootInterfaceCandidates {
        interfaces,
        predicted,
    }))
}

/// Summarize boot-interface candidates for crate-local integration-style unit tests without
/// exposing the production candidate type or its fields.
#[cfg(test)]
pub(crate) async fn summarize_boot_interface_candidates_for_test(
    txn: &mut PgConnection,
    machine_id: Option<MachineId>,
) -> Result<Option<(bool, bool)>, CarbideError> {
    Ok(boot_interface_candidates(txn, machine_id)
        .await?
        .map(|candidates| {
            (
                candidates
                    .interfaces
                    .iter()
                    .any(|interface| interface.primary_interface),
                candidates.predicted.is_empty(),
            )
        }))
}

/// Map the request's `ResetType` to the libredfish `Manager.Reset` type.
/// `Unspecified` maps to `None`, which each vendor resolves to its default
/// (`GracefulRestart` for the standard/switch/power-shelf path, `ForceRestart`
/// for AMI/Viking machine BMCs).
fn map_reset_type(
    reset_type: rpc::admin_bmc_reset_request::ResetType,
) -> Option<libredfish::ManagerResetType> {
    use rpc::admin_bmc_reset_request::ResetType;
    match reset_type {
        ResetType::Unspecified => None,
        ResetType::GracefulRestart => Some(libredfish::ManagerResetType::GracefulRestart),
        ResetType::ForceRestart => Some(libredfish::ManagerResetType::ForceRestart),
    }
}

pub(crate) async fn admin_bmc_reset(
    api: &Api,
    request: Request<rpc::AdminBmcResetRequest>,
) -> Result<Response<rpc::AdminBmcResetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // `reset_type` selects the Redfish `Manager.Reset` action and has no
    // meaning for the ipmitool path, so reject the combination rather than
    // silently ignore the operator's choice.
    let requested_reset_type = req.reset_type();
    if req.use_ipmitool
        && requested_reset_type != rpc::admin_bmc_reset_request::ResetType::Unspecified
    {
        return Err(Status::invalid_argument(
            "reset_type is only supported for the redfish path, not with use_ipmitool",
        ));
    }
    let reset_type = map_reset_type(requested_reset_type);

    // The top-level `machine_id` is deprecated in favor of `device_id`; accept
    // it as sugar for `DeviceId::Machine` but reject setting both.
    #[allow(deprecated)]
    let legacy_machine_id = req.machine_id;
    let device_id = match (req.device_id, legacy_machine_id) {
        (Some(_), Some(_)) => {
            return Err(Status::invalid_argument(
                "machine_id is deprecated; do not combine it with device_id",
            ));
        }
        (Some(device_id), None) => Some(device_id),
        (None, Some(machine_id)) => Some(DeviceId::Machine(try_parse_machine_id(&machine_id)?)),
        (None, None) => None,
    };

    let mut txn = api.txn_begin().await?;

    let bmc_endpoint_request = match (req.bmc_endpoint_request, device_id) {
        (Some(_), Some(_)) => {
            return Err(Status::invalid_argument(
                "targets are mutually exclusive: provide exactly one of bmc_endpoint_request or device_id",
            ));
        }
        (None, None) => {
            return Err(Status::invalid_argument(
                "a target is required: provide bmc_endpoint_request or device_id",
            ));
        }
        (Some(endpoint), None) => {
            let (completed, _) =
                validate_and_complete_bmc_endpoint_request(&mut txn, Some(endpoint), None).await?;
            completed
        }
        (None, Some(DeviceId::Machine(machine_id))) => {
            let (completed, _) =
                validate_and_complete_bmc_endpoint_request(&mut txn, None, Some(machine_id))
                    .await?;
            completed
        }
        (None, Some(DeviceId::Switch(switch_id))) => {
            let row = db::switch::find_switch_endpoints_by_ids(
                &mut txn,
                std::slice::from_ref(&switch_id),
            )
            .await
            .map_err(|e| Status::internal(format!("db error resolving switch BMC endpoint: {e}")))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Status::not_found(format!(
                    "switch {switch_id} not found or has no resolvable BMC endpoint"
                ))
            })?;
            rpc::BmcEndpointRequest {
                ip_address: row.bmc_ip.to_string(),
                mac_address: Some(row.bmc_mac.to_string()),
            }
        }
        (None, Some(DeviceId::PowerShelf(power_shelf_id))) => {
            let row = db::power_shelf::find_power_shelf_endpoints_by_ids(
                &mut txn,
                std::slice::from_ref(&power_shelf_id),
            )
            .await
            .map_err(|e| {
                Status::internal(format!("db error resolving power shelf PMC endpoint: {e}"))
            })?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Status::not_found(format!(
                    "power shelf {power_shelf_id} not found or has no resolvable PMC endpoint"
                ))
            })?;
            rpc::BmcEndpointRequest {
                ip_address: row.pmc_ip.to_string(),
                mac_address: Some(row.pmc_mac.to_string()),
            }
        }
    };

    txn.commit().await?;

    let endpoint_address = bmc_endpoint_request.ip_address.clone();

    tracing::info!(
        use_ipmitool = req.use_ipmitool,
        bmc_ip_address = %endpoint_address,
        "Resetting BMC",
    );

    if req.use_ipmitool {
        ipmitool_reset_bmc(api, bmc_endpoint_request).await?;
    } else {
        redfish_reset_bmc(api, bmc_endpoint_request, reset_type).await?;
    }

    tracing::info!(
        use_ipmitool = req.use_ipmitool,
        bmc_ip_address = %endpoint_address,
        "BMC reset request succeeded",
    );

    Ok(Response::new(rpc::AdminBmcResetResponse {}))
}

pub(crate) async fn disable_secure_boot(
    api: &Api,
    request: Request<rpc::BmcEndpointRequest>,
) -> Result<Response<rpc::DisableSecureBootResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, Some(req), None).await?;

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .disable_secure_boot(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    let endpoint_address = bmc_endpoint_request.ip_address.clone();
    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Disable secure boot request succeeded",
    );

    Ok(Response::new(rpc::DisableSecureBootResponse {}))
}

pub(crate) async fn lockdown(
    api: &Api,
    request: Request<rpc::LockdownRequest>,
) -> Result<Response<rpc::LockdownResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let action = req.action();
    let action = match action {
        rpc::LockdownAction::Enable => libredfish::EnabledDisabled::Enabled,
        rpc::LockdownAction::Disable => libredfish::EnabledDisabled::Disabled,
    };

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) = validate_and_complete_bmc_endpoint_request(
        &mut txn,
        req.bmc_endpoint_request,
        req.machine_id,
    )
    .await?;

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .lockdown(bmc_addr, &machine_interface, action)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    let endpoint_address = bmc_endpoint_request.ip_address.clone();
    tracing::info!(
        action = %action.to_string().to_lowercase(),
        bmc_ip_address = %endpoint_address,
        "lockdown request succeeded",
    );

    Ok(Response::new(rpc::LockdownResponse {}))
}

pub(crate) async fn lockdown_status(
    api: &Api,
    request: Request<rpc::LockdownStatusRequest>,
) -> Result<Response<::rpc::site_explorer::LockdownStatus>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) = validate_and_complete_bmc_endpoint_request(
        &mut txn,
        req.bmc_endpoint_request,
        req.machine_id,
    )
    .await?;

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    let response = api
        .bmc_client
        .lockdown_status(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(response.into()))
}

pub(crate) async fn enable_infinite_boot(
    api: &Api,
    request: Request<rpc::EnableInfiniteBootRequest>,
) -> Result<Response<rpc::EnableInfiniteBootResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // Note: EnableInfiniteBootRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .enable_infinite_boot(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    let endpoint_address = bmc_endpoint_request.ip_address.clone();
    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Enable infinite boot request succeeded",
    );

    Ok(Response::new(rpc::EnableInfiniteBootResponse {}))
}

pub(crate) async fn is_infinite_boot_enabled(
    api: &Api,
    request: Request<rpc::IsInfiniteBootEnabledRequest>,
) -> Result<Response<rpc::IsInfiniteBootEnabledResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // Note: IsInfiniteBootEnabledRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    let is_enabled = api
        .bmc_client
        .is_infinite_boot_enabled(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    tracing::info!(
        bmc_ip_address = %bmc_endpoint_request.ip_address,
        is_enabled,
        "Infinite boot status request succeeded",
    );

    Ok(Response::new(rpc::IsInfiniteBootEnabledResponse {
        is_enabled,
    }))
}

pub(crate) async fn machine_setup(
    api: &Api,
    request: Request<rpc::MachineSetupRequest>,
) -> Result<Response<rpc::MachineSetupResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let entered_mac = parse_boot_interface_mac(req.boot_interface_mac.as_deref())?;

    // Note: MachineSetupRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, owning_machine_id) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;
    let desired = desired_boot_interface_target(&mut txn, owning_machine_id).await?;
    let candidates = boot_interface_candidates(&mut txn, owning_machine_id).await?;
    let endpoint_address = &bmc_endpoint_request.ip_address;

    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Starting machine setup",
    );

    // Unlike a boot-order-only request, machine setup still has useful BIOS
    // work when the managed host has no resolvable boot target.
    let managed_machine_id = owning_machine_id.and_then(|id| HostMachineId::try_from(id).ok());
    let managed_target = managed_machine_id.zip(resolve_admin_boot_interface_target(
        None,
        desired.as_ref(),
        candidates.as_ref(),
        entered_mac,
    ));
    if let Some((machine_id, boot_interface)) = managed_target {
        let reconciliation_eligible =
            boot_interface_reconciliation_eligible(&mut txn, Some(machine_id)).await?;
        let desired = MachineBootInterfaceTarget::from(&boot_interface);
        db::machine_desired_boot_interface::force_reconcile(
            &mut txn,
            &machine_id,
            &desired,
            admin_selection_authority(entered_mac),
        )
        .await?;
        txn.commit().await?;
        enqueue_boot_interface_reconciliation(api, machine_id, reconciliation_eligible).await;

        tracing::info!(
            bmc_ip_address = %endpoint_address,
            "Machine setup request succeeded",
        );
        return Ok(Response::new(rpc::MachineSetupResponse {}));
    }

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    let stored = db::explored_endpoints::find_by_ips(&api.database_connection, vec![bmc_addr.ip()])
        .await?
        .into_iter()
        .next()
        .and_then(|ep| ep.boot_interface());
    let boot_interface = resolve_admin_boot_interface_target(
        stored,
        desired.as_ref(),
        candidates.as_ref(),
        entered_mac,
    );

    api.bmc_client
        .machine_setup(bmc_addr, &machine_interface, boot_interface.as_ref())
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Machine setup request succeeded",
    );

    Ok(Response::new(rpc::MachineSetupResponse {}))
}

pub(crate) async fn set_dpu_first_boot_order(
    api: &Api,
    request: Request<rpc::SetDpuFirstBootOrderRequest>,
) -> Result<Response<rpc::SetDpuFirstBootOrderResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let entered_mac = parse_boot_interface_mac(req.boot_interface_mac.as_deref())?;

    // Note: SetDpuFirstBootOrderRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, owning_machine_id) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;
    let desired = desired_boot_interface_target(&mut txn, owning_machine_id).await?;
    let candidates = boot_interface_candidates(&mut txn, owning_machine_id).await?;
    let endpoint_address = &bmc_endpoint_request.ip_address;

    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Setting DPU first in boot order",
    );

    if let Some((machine_id, boot_interface)) = managed_boot_interface_target(
        owning_machine_id,
        desired.as_ref(),
        candidates.as_ref(),
        entered_mac,
    )? {
        let reconciliation_eligible =
            boot_interface_reconciliation_eligible(&mut txn, Some(machine_id)).await?;
        let desired = MachineBootInterfaceTarget::from(&boot_interface);
        db::machine_desired_boot_interface::force_reconcile(
            &mut txn,
            &machine_id,
            &desired,
            admin_selection_authority(entered_mac),
        )
        .await?;
        txn.commit().await?;
        enqueue_boot_interface_reconciliation(api, machine_id, reconciliation_eligible).await;

        tracing::info!(
            bmc_ip_address = %endpoint_address,
            "Set DPU first in boot order request succeeded",
        );
        return Ok(Response::new(rpc::SetDpuFirstBootOrderResponse {}));
    }

    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    let stored = db::explored_endpoints::find_by_ips(&api.database_connection, vec![bmc_addr.ip()])
        .await?
        .into_iter()
        .next()
        .and_then(|ep| ep.boot_interface());
    let boot_interface = resolve_admin_boot_interface_target(
        stored,
        desired.as_ref(),
        candidates.as_ref(),
        entered_mac,
    )
    .ok_or_else(|| {
        CarbideError::InvalidArgument(
            "no boot interface available: enter a MAC or explore the host first".to_string(),
        )
    })?;

    api.bmc_client
        .set_boot_order_dpu_first(bmc_addr, &machine_interface, &boot_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    tracing::info!(
        bmc_ip_address = %endpoint_address,
        "Set DPU first in boot order request succeeded",
    );

    Ok(Response::new(rpc::SetDpuFirstBootOrderResponse {}))
}

pub(crate) async fn admin_power_control(
    api: &Api,
    request: Request<rpc::AdminPowerControlRequest>,
) -> Result<Response<rpc::AdminPowerControlResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // Note: AdminPowerControlRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let action = req.action();

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, machine_id) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;

    let action = match action {
        rpc::admin_power_control_request::SystemPowerControl::On => {
            libredfish::SystemPowerControl::On
        }
        rpc::admin_power_control_request::SystemPowerControl::GracefulShutdown => {
            libredfish::SystemPowerControl::GracefulShutdown
        }
        rpc::admin_power_control_request::SystemPowerControl::ForceOff => {
            libredfish::SystemPowerControl::ForceOff
        }
        rpc::admin_power_control_request::SystemPowerControl::GracefulRestart => {
            libredfish::SystemPowerControl::GracefulRestart
        }
        rpc::admin_power_control_request::SystemPowerControl::ForceRestart => {
            libredfish::SystemPowerControl::ForceRestart
        }
        rpc::admin_power_control_request::SystemPowerControl::AcPowercycle => {
            libredfish::SystemPowerControl::ACPowercycle
        }
    };

    let mut msg: Option<String> = None;
    if let Some(machine_id) = machine_id {
        let power_manager_enabled = api.runtime_config.power_manager_options.enabled;
        if power_manager_enabled {
            let snapshot = db::managed_host::load_snapshot(
                &mut txn,
                &machine_id,
                LoadSnapshotOptions {
                    include_history: true,
                    include_instance_data: false,
                    host_health_config: api.runtime_config.host_health,
                },
            )
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;

            if let Some(power_state) = snapshot
                .host_snapshot
                .status
                .power_options
                .map(|x| x.desired_power_state)
                && power_state == model::power_manager::PowerState::On
                && action == libredfish::SystemPowerControl::ForceOff
            {
                msg = Some(
                        "!!WARNING!! Desired power state for the host is set as On while the requested action is Off. Carbide will attempt to bring the host online after some time.".to_string(),
                    )
            }
        }
    }

    txn.commit().await?;

    redfish_power_control(api, bmc_endpoint_request, action).await?;

    Ok(Response::new(rpc::AdminPowerControlResponse { msg }))
}

// Ad-hoc BMC exploration
pub(crate) async fn explore(
    api: &Api,
    request: tonic::Request<rpc::BmcEndpointRequest>,
) -> Result<Response<::rpc::site_explorer::EndpointExplorationReport>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &req).await?;

    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    // TODO(chet): Track down Vinod's Jira to optimize code for
    // existing sites where there is no nvswitch or power shelf.
    let expected = if let Some(expected_machine) =
        crate::handlers::expected_machine::query(api, bmc_mac_address).await?
    {
        Some(ExpectedEntity::Machine(expected_machine))
    } else if let Some(expected_switch) =
        crate::handlers::expected_switch::query(api, bmc_mac_address).await?
    {
        Some(ExpectedEntity::Switch(expected_switch))
    } else {
        crate::handlers::expected_power_shelf::query(api, bmc_mac_address)
            .await?
            .map(ExpectedEntity::PowerShelf)
    };

    // Use the same stored boot-interface target as periodic exploration.
    let mut txn = api.txn_begin().await?;
    let boot_interface = db::explored_endpoints::find_by_ips(&mut txn, vec![bmc_addr.ip()])
        .await?
        .first()
        .and_then(|ep| ep.boot_interface_target())
        .map(Into::into);
    txn.commit().await?;

    let report = api
        .endpoint_explorer
        .explore_endpoint(
            bmc_addr,
            &machine_interface,
            expected.as_ref(),
            None,
            boot_interface.as_ref(),
        )
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(tonic::Response::new(report.into()))
}

async fn redfish_reset_bmc(
    api: &Api,
    request: rpc::BmcEndpointRequest,
    reset_type: Option<libredfish::ManagerResetType>,
) -> Result<Response<()>, Status> {
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .redfish_reset_bmc(bmc_addr, &machine_interface, reset_type)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

async fn ipmitool_reset_bmc(
    api: &Api,
    request: rpc::BmcEndpointRequest,
) -> Result<Response<()>, Status> {
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .ipmitool_reset_bmc(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

async fn redfish_power_control(
    api: &Api,
    request: rpc::BmcEndpointRequest,
    action: libredfish::SystemPowerControl,
) -> Result<Response<()>, Status> {
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .redfish_power_control(bmc_addr, &machine_interface, action)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

pub(crate) async fn bmc_credential_status(
    api: &Api,
    request: tonic::Request<rpc::BmcEndpointRequest>,
) -> Result<Response<rpc::BmcCredentialStatusResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let (_bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &req).await?;

    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);
    let have_credentials = api.bmc_client.have_credentials(&machine_interface).await;

    Ok(Response::new(rpc::BmcCredentialStatusResponse {
        have_credentials,
    }))
}

pub(crate) async fn copy_bfb_to_dpu_rshim(
    api: &Api,
    request: Request<rpc::CopyBfbToDpuRshimRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let ip_str = match &req.ssh_request {
        Some(ssh_req) => match &ssh_req.endpoint_request {
            Some(bmc_request) => bmc_request.ip_address.clone(),
            None => return Err(CarbideError::MissingArgument("bmc_endpoint_request").into()),
        },
        None => return Err(CarbideError::MissingArgument("ssh_request").into()),
    };

    let dpu_ip: std::net::IpAddr = ip_str
        .parse()
        .map_err(|_| CarbideError::InvalidArgument(format!("invalid DPU IP: {ip_str}")))?;

    if req.host_bmc_ip.is_empty() {
        return Err(CarbideError::MissingArgument("host_bmc_ip").into());
    }
    let host_bmc_ip: std::net::IpAddr = req.host_bmc_ip.parse().map_err(|_| {
        CarbideError::InvalidArgument(format!("invalid host BMC IP: {}", req.host_bmc_ip))
    })?;

    let pre_copy_powercycle = req.pre_copy_powercycle;

    let dpu_in_managed_host =
        carbide_site_explorer::is_endpoint_in_managed_host(dpu_ip, &api.database_connection)
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?;
    if dpu_in_managed_host {
        return Err(CarbideError::InvalidArgument(format!(
            "cannot trigger BFB recovery: DPU {dpu_ip} is already ingested. \
             force-delete the managed host first",
        ))
        .into());
    }

    let dpu_endpoints = db::explored_endpoints::find_by_ips(&api.database_connection, vec![dpu_ip])
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;
    let dpu_endpoint = dpu_endpoints.first().ok_or(CarbideError::NotFoundError {
        kind: "explored_endpoint",
        id: dpu_ip.to_string(),
    })?;

    // If the DPU is in NIC mode, don't allow operators to copy_bfb_to_dpu_rshim
    // at all to begin with. While the rshim + copy part will technically
    // work, the problem is there's no ARM OS to actually reboot into. The
    // BFB preingestion flow will work its way through the states, and then
    // wait for the ARM OS to come up, which it never will. Waiting will
    // eventually, time out (SLA), and then the host will mark as failed.
    if dpu_endpoint.report.bluefield_operating_mode() == Some(BlueFieldOperatingMode::Nic) {
        return Err(CarbideError::InvalidArgument(format!(
            "cannot trigger BFB recovery: DPU {dpu_ip} is in NIC mode. \
             ensure the host's resolved DPU policy is `manage` \
             (update it with `--dpu-policy manage` and adjust the site policy as needed) \
             and wait for site-explorer to reconcile the DPU back to \
             DPU mode before retrying",
        ))
        .into());
    }

    match &dpu_endpoint.preingestion_state {
        PreingestionState::Initial
        | PreingestionState::Complete
        | PreingestionState::Failed { .. } => {}
        other => {
            return Err(CarbideError::InvalidArgument(format!(
                "cannot trigger BFB recovery: DPU endpoint is in state {other:?}. \
                 wait for it to complete or fail first",
            ))
            .into());
        }
    }

    {
        let host_endpoints =
            db::explored_endpoints::find_by_ips(&api.database_connection, vec![host_bmc_ip])
                .await
                .map_err(|e| CarbideError::internal(e.to_string()))?;
        let host_ep = host_endpoints.first().ok_or(CarbideError::NotFoundError {
            kind: "explored_endpoint",
            id: host_bmc_ip.to_string(),
        })?;
        match &host_ep.preingestion_state {
            PreingestionState::Complete | PreingestionState::Failed { .. } => {}
            other => {
                return Err(CarbideError::InvalidArgument(format!(
                    "cannot power-cycle host: host {host_bmc_ip} is in state {other:?}. \
                     retry after host preingestion completes",
                ))
                .into());
            }
        }
    }

    api.database_connection
        .with_txn(|txn| {
            Box::pin(async move {
                db::explored_endpoints::set_preingestion_bfb_recovery_needed(
                    dpu_ip,
                    "Triggered via CLI".to_string(),
                    host_bmc_ip,
                    pre_copy_powercycle,
                    txn,
                )
                .await?;

                // Pause site explorer remediation on the host so it doesn't
                // issue BMC resets during the power-cycle phases.
                db::explored_endpoints::set_pause_remediation(host_bmc_ip, true, txn).await?;

                Ok::<(), db::DatabaseError>(())
            })
        })
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

pub(super) async fn resolve_bmc_interface(
    api: &Api,
    request: &rpc::BmcEndpointRequest,
) -> Result<(SocketAddr, MacAddress), Status> {
    let bmc_addr = resolve_bmc_address(&request.ip_address).await?;

    let bmc_mac_address: MacAddress;
    if let Some(mac_str) = &request.mac_address {
        bmc_mac_address = mac_str.parse::<MacAddress>().map_err(CarbideError::from)?;
    } else if let Some(bmc_machine_interface) =
        find_by_ip(&api.database_connection, bmc_addr.ip()).await?
    {
        bmc_mac_address = bmc_machine_interface.mac_address;
    } else {
        return Err(CarbideError::InvalidArgument(format!(
            "could not find a mac address for the specified IP: {request:#?}"
        ))
        .into());
    };

    Ok((bmc_addr, bmc_mac_address))
}

pub(crate) async fn create_bmc_user(
    api: &Api,
    request: Request<rpc::CreateBmcUserRequest>,
) -> Result<Response<rpc::CreateBmcUserResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // Note: CreateBmcUserRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;

    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;

    txn.commit().await?;

    let endpoint_address = &bmc_endpoint_request.ip_address;

    let role: RoleId = match req
        .create_role_id
        .unwrap_or("Administrator".to_string())
        .to_lowercase()
        .as_str()
    {
        "administrator" => RoleId::Administrator,
        "operator" => RoleId::Operator,
        "readonly" => RoleId::ReadOnly,
        "noaccess" => RoleId::NoAccess,
        _ => RoleId::Administrator,
    };

    tracing::info!(
        username = %req.create_username,
        role = %role,
        bmc_ip_address = %endpoint_address,
        "Creating BMC user",
    );

    do_create_bmc_user(
        api,
        &bmc_endpoint_request,
        &req.create_username,
        &req.create_password,
        role,
    )
    .await?;

    tracing::info!(
        username = %req.create_username,
        role = %role,
        bmc_ip_address = %endpoint_address,
        "Successfully created BMC user",
    );

    Ok(Response::new(rpc::CreateBmcUserResponse {}))
}

pub(crate) async fn delete_bmc_user(
    api: &Api,
    request: Request<rpc::DeleteBmcUserRequest>,
) -> Result<Response<rpc::DeleteBmcUserResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    // Note: DeleteBmcUserRequest uses a string for machine_id instead of a real MachineId, which is wrong.
    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;
    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;

    txn.commit().await?;

    let endpoint_address = &bmc_endpoint_request.ip_address;

    tracing::info!(
        username = %req.delete_username,
        bmc_ip_address = %endpoint_address,
        "Deleting BMC user",
    );

    do_delete_bmc_user(api, &bmc_endpoint_request, &req.delete_username).await?;

    tracing::info!(
        username = %req.delete_username,
        bmc_ip_address = %endpoint_address,
        "Successfully deleted BMC user",
    );

    Ok(Response::new(rpc::DeleteBmcUserResponse {}))
}

pub(crate) async fn set_bmc_root_password(
    api: &Api,
    request: Request<rpc::SetBmcRootPasswordRequest>,
) -> Result<Response<rpc::SetBmcRootPasswordResponse>, Status> {
    // Redact: the request carries the plaintext BMC root password. Log only the
    // non-secret targeting fields.
    {
        let r = request.get_ref();
        log_request_data_redacted(format!(
            "SetBmcRootPasswordRequest {{ bmc_endpoint_request: {:?}, machine_id: {:?}, new_password: <redacted> }}",
            r.bmc_endpoint_request, r.machine_id,
        ));
    }
    let req = request.into_inner();

    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;
    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;
    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    tracing::info!(bmc_address = %bmc_addr, "Setting BMC root password");

    api.bmc_client
        .set_bmc_root_password(bmc_addr, &machine_interface, &req.new_password)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    tracing::info!(bmc_address = %bmc_addr, "Successfully set BMC root password");

    Ok(Response::new(rpc::SetBmcRootPasswordResponse {}))
}

pub(crate) async fn probe_bmc_vendor(
    api: &Api,
    request: Request<rpc::ProbeBmcVendorRequest>,
) -> Result<Response<rpc::ProbeBmcVendorResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let machine_id = req
        .machine_id
        .as_ref()
        .map(|id| try_parse_machine_id(id))
        .transpose()?;

    let mut txn = api.txn_begin().await?;
    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, req.bmc_endpoint_request, machine_id)
            .await?;
    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    let vendor = api
        .bmc_client
        .probe_bmc_vendor(bmc_addr, &machine_interface)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    tracing::info!(bmc_address = %bmc_addr, %vendor, "Probed BMC vendor");

    Ok(Response::new(rpc::ProbeBmcVendorResponse {
        vendor: vendor.to_string(),
    }))
}

async fn do_create_bmc_user(
    api: &Api,
    request: &rpc::BmcEndpointRequest,
    create_username: &str,
    create_password: &str,
    create_role_id: RoleId,
) -> Result<Response<()>, Status> {
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .create_bmc_user(
            bmc_addr,
            &machine_interface,
            create_username,
            create_password,
            create_role_id,
        )
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

async fn do_delete_bmc_user(
    api: &Api,
    request: &rpc::BmcEndpointRequest,
    delete_user: &str,
) -> Result<Response<()>, Status> {
    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .delete_bmc_user(bmc_addr, &machine_interface, delete_user)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(()))
}

/// Accepts an optional partial or complete BmcEndpointRequest and optional machine ID and returns a complete and valid BmcEndpointRequest.
///
/// * `txn`                  - Active database transaction
/// * `bmc_endpoint_request` - Optional BmcEndpointRequest.  Can supply _only_ ip_address or all fields.
/// * `machine_id`           - Optional machine ID that can be used to build a new BmcEndpointRequest.
pub(super) async fn validate_and_complete_bmc_endpoint_request(
    txn: &mut PgConnection,
    bmc_endpoint_request: Option<rpc::BmcEndpointRequest>,
    machine_id: Option<MachineId>,
) -> Result<(rpc::BmcEndpointRequest, Option<MachineId>), CarbideError> {
    match (bmc_endpoint_request, machine_id) {
        (Some(bmc_endpoint_request), _) => {
            let parsed_ip = bmc_endpoint_request.ip_address.parse().map_err(|e| {
                CarbideError::InvalidArgument(format!(
                    "invalid ip_address {:?}: {e}",
                    bmc_endpoint_request.ip_address
                ))
            })?;
            let interface = db::machine_interface::find_by_ip(txn, parsed_ip)
                .await?
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "machine_interface",
                    id: bmc_endpoint_request.ip_address.clone(),
                })?;

            let bmc_mac = match bmc_endpoint_request.mac_address {
                // No MAC in the request, use the interface MAC
                None => interface.mac_address.to_string(),

                // MAC passed in the request, check if it matches the interface MAC
                Some(request_mac) => {
                    let parsed_mac = request_mac
                        .parse::<MacAddress>()
                        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

                    if parsed_mac != interface.mac_address {
                        return Err(CarbideError::BmcMacIpMismatch {
                            requested_ip: bmc_endpoint_request.ip_address.clone(),
                            requested_mac: request_mac,
                            found_mac: interface.mac_address.to_string(),
                        });
                    }

                    request_mac
                }
            };

            Ok((
                rpc::BmcEndpointRequest {
                    ip_address: bmc_endpoint_request.ip_address,
                    mac_address: Some(bmc_mac),
                },
                interface.machine_id,
            ))
        }
        // User provided machine_id
        (_, Some(machine_id)) => {
            log_machine_id(&machine_id);

            let machine = db::machine::find_one(txn, &machine_id, MachineSearchConfig::default())
                .await?
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "machine",
                    id: machine_id.to_string(),
                })?;

            let bmc_ip = machine.status.bmc_info.ip.as_ref().ok_or_else(|| {
                CarbideError::internal(format!(
                    "machine found for {machine_id} but BMC IP is missing"
                ))
            })?;

            let bmc_mac_address = machine.status.bmc_info.mac.ok_or_else(|| {
                CarbideError::internal(format!("BMC endpoint for {bmc_ip} ({machine_id}) found but does not have associated MAC"))
            })?;

            Ok((
                rpc::BmcEndpointRequest {
                    ip_address: bmc_ip.to_string(),
                    mac_address: Some(bmc_mac_address.to_string()),
                },
                Some(machine_id),
            ))
        }

        _ => Err(CarbideError::InvalidArgument(
            "provide either machine_id or BmcEndpointRequest with at least ip_address".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use model::network_segment::NetworkSegmentType;

    use super::*;

    fn row(mac: &str, primary: bool, boot_interface_id: Option<&str>) -> MachineInterfaceSnapshot {
        let mut row = MachineInterfaceSnapshot::mock_with_mac(mac.parse().unwrap());
        row.primary_interface = primary;
        row.boot_interface_id = boot_interface_id.map(String::from);
        row
    }

    // `Unspecified` defers to the per-vendor default (None); the explicit types
    // map one-to-one onto the libredfish `Manager.Reset` type.
    #[test]
    fn reset_type_maps_to_manager_reset_type() {
        use super::rpc::admin_bmc_reset_request::ResetType;

        value_scenarios!(run = |reset_type: ResetType| { map_reset_type(reset_type) };
            "unspecified defers to the vendor default" {
                ResetType::Unspecified => None,
            }

            "graceful maps to GracefulRestart" {
                ResetType::GracefulRestart => Some(libredfish::ManagerResetType::GracefulRestart),
            }

            "force maps to ForceRestart" {
                ResetType::ForceRestart => Some(libredfish::ManagerResetType::ForceRestart),
            }
        );
    }

    fn predicted(mac: &str, boot_interface_id: Option<&str>) -> PredictedMachineInterface {
        PredictedMachineInterface {
            id: uuid::Uuid::nil(),
            // Any valid machine id -- the resolver never reads it.
            machine_id: "fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0"
                .parse()
                .unwrap(),
            mac_address: mac.parse().unwrap(),
            expected_network_segment_type: NetworkSegmentType::HostInband,
            boot_interface_id: boot_interface_id.map(String::from),
            primary_interface: false,
        }
    }

    fn pair(mac: &str, interface_id: &str) -> MachineBootInterface {
        MachineBootInterface {
            mac_address: mac.parse().unwrap(),
            interface_id: interface_id.to_string(),
        }
    }

    #[test]
    fn no_mac_keeps_the_desired_mac_and_refreshes_its_redfish_id() {
        let c = BootInterfaceCandidates {
            interfaces: vec![
                row("00:00:5e:00:53:01", true, Some("NIC.Integrated.1-1-1")),
                row("00:00:5e:00:53:02", false, Some("NIC.Slot.7-1-1")),
            ],
            predicted: vec![],
        };
        let desired =
            MachineBootInterfaceTarget::Pair(pair("00:00:5e:00:53:02", "NIC.Remembered.7-1-1"));

        assert_eq!(
            resolve_admin_boot_interface_target(None, Some(&desired), Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    #[test]
    fn no_mac_completes_a_persisted_mac_only_target_from_current_rows() {
        let desired = MachineBootInterfaceTarget::MacOnly("00:00:5e:00:53:02".parse().unwrap());
        let c = BootInterfaceCandidates {
            interfaces: vec![
                row("00:00:5e:00:53:01", true, Some("NIC.Integrated.1-1-1")),
                row("00:00:5e:00:53:02", false, Some("NIC.Slot.7-1-1")),
            ],
            predicted: vec![],
        };

        assert_eq!(
            resolve_admin_boot_interface_target(None, Some(&desired), Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    #[test]
    fn explicit_mac_uses_only_unambiguous_interface_ids() {
        let mac = "00:00:5e:00:53:02";
        let entered_mac = mac.parse().unwrap();

        value_scenarios!(run = |(desired, candidates): (
            Option<MachineBootInterfaceTarget>,
            BootInterfaceCandidates,
        )| {
            resolve_admin_boot_interface_target(
                None,
                desired.as_ref(),
                Some(&candidates),
                Some(entered_mac),
            )
        };
            "same-MAC desired pair" {
                (
                    Some(MachineBootInterfaceTarget::Pair(pair(
                        mac,
                        "NIC.Remembered.7-1-1",
                    ))),
                    BootInterfaceCandidates {
                        interfaces: vec![row(mac, true, None)],
                        predicted: vec![],
                    },
                ) => Some(BootInterfaceTarget::Pair(pair(
                    mac,
                    "NIC.Remembered.7-1-1",
                ))),
            }

            "same-MAC desired pair survives conflicting owned ids" {
                (
                    Some(MachineBootInterfaceTarget::Pair(pair(
                        mac,
                        "NIC.Remembered.7-1-1",
                    ))),
                    BootInterfaceCandidates {
                        interfaces: vec![
                            row(mac, true, Some("NIC.Conflicting.1")),
                            row(mac, false, Some("NIC.Conflicting.2")),
                        ],
                        predicted: vec![predicted(mac, Some("NIC.Predicted.1"))],
                    },
                ) => Some(BootInterfaceTarget::Pair(pair(
                    mac,
                    "NIC.Remembered.7-1-1",
                ))),
            }

            "duplicate owned id is unambiguous" {
                (
                    None,
                    BootInterfaceCandidates {
                        interfaces: vec![
                            row(mac, true, Some(" \tNIC.Owned.1\n ")),
                            row(mac, false, Some("NIC.Owned.1")),
                        ],
                        predicted: vec![predicted(mac, Some("NIC.Predicted.1"))],
                    },
                ) => Some(BootInterfaceTarget::Pair(pair(mac, "NIC.Owned.1"))),
            }

            "conflicting owned ids block prediction fallback" {
                (
                    None,
                    BootInterfaceCandidates {
                        interfaces: vec![
                            row(mac, true, Some("NIC.Owned.1")),
                            row(mac, false, Some("NIC.Owned.2")),
                        ],
                        predicted: vec![predicted(mac, Some("NIC.Predicted.1"))],
                    },
                ) => Some(BootInterfaceTarget::MacOnly(entered_mac)),
            }

            "duplicate prediction id is unambiguous" {
                (
                    None,
                    BootInterfaceCandidates {
                        interfaces: vec![row(mac, true, None)],
                        predicted: vec![
                            predicted(mac, Some("NIC.Predicted.1")),
                            predicted(mac, Some("NIC.Predicted.1")),
                        ],
                    },
                ) => Some(BootInterfaceTarget::Pair(pair(mac, "NIC.Predicted.1"))),
            }

            "whitespace-only owned id falls through to prediction" {
                (
                    None,
                    BootInterfaceCandidates {
                        interfaces: vec![row(mac, true, Some("\t\n"))],
                        predicted: vec![predicted(mac, Some("NIC.Predicted.1"))],
                    },
                ) => Some(BootInterfaceTarget::Pair(pair(mac, "NIC.Predicted.1"))),
            }

            "conflicting prediction ids remain MAC-only" {
                (
                    None,
                    BootInterfaceCandidates {
                        interfaces: vec![row(mac, true, None)],
                        predicted: vec![
                            predicted(mac, Some("NIC.Predicted.1")),
                            predicted(mac, Some("NIC.Predicted.2")),
                        ],
                    },
                ) => Some(BootInterfaceTarget::MacOnly(entered_mac)),
            }
        );
    }

    #[test]
    fn entered_mac_upgrades_to_a_pair_from_the_machines_own_row() {
        // The operator picked a NIC; its machine_interface row holds the Redfish
        // id, so the target is the full pair -- even though the explored default
        // names a different NIC.
        let c = BootInterfaceCandidates {
            interfaces: vec![
                row("00:00:5e:00:53:01", true, Some("NIC.Integrated.1-1-1")),
                row("00:00:5e:00:53:02", false, Some("NIC.Slot.7-1-1")),
            ],
            predicted: vec![],
        };
        let stored = Some(pair("00:00:5e:00:53:01", "NIC.Integrated.1-1-1"));
        let target = resolve_admin_boot_interface_target(
            stored,
            None,
            Some(&c),
            Some("00:00:5e:00:53:02".parse().unwrap()),
        );
        assert_eq!(
            target,
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    #[test]
    fn entered_mac_upgrades_to_a_pair_from_a_predicted_interface() {
        // The named NIC has no machine_interfaces row yet (its first lease is
        // still pending), but the machine's prediction for it recorded the
        // Redfish id -- the entered MAC is completed from there.
        let c = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![predicted("00:00:5e:00:53:02", Some("NIC.Embedded.1-1-1"))],
        };
        let target = resolve_admin_boot_interface_target(
            None,
            None,
            Some(&c),
            Some("00:00:5e:00:53:02".parse().unwrap()),
        );
        assert_eq!(
            target,
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Embedded.1-1-1"
            ))),
        );
    }

    #[test]
    fn entered_mac_falls_back_to_the_explored_default_then_mac_only() {
        // No machine: the explored default completes the pair only when it
        // names the entered MAC; any other entered MAC is targeted alone.
        let stored = pair("00:00:5e:00:53:01", "NIC.Integrated.1-1-1");
        assert_eq!(
            resolve_admin_boot_interface_target(
                Some(stored.clone()),
                None,
                None,
                Some("00:00:5e:00:53:01".parse().unwrap()),
            ),
            Some(BootInterfaceTarget::Pair(stored.clone())),
        );
        assert_eq!(
            resolve_admin_boot_interface_target(
                Some(stored),
                None,
                None,
                Some("00:00:5e:00:53:99".parse().unwrap()),
            ),
            Some(BootInterfaceTarget::MacOnly(
                "00:00:5e:00:53:99".parse().unwrap()
            )),
        );
    }

    #[test]
    fn no_mac_prefers_the_machines_designation_over_the_explored_default() {
        // The machine's primary row is the authority; the explored default
        // (site-explorer's automatic pick) names a different NIC and loses.
        let c = BootInterfaceCandidates {
            interfaces: vec![
                row("00:00:5e:00:53:01", false, Some("NIC.Integrated.1-1-1")),
                row("00:00:5e:00:53:02", true, Some("NIC.Slot.7-1-1")),
            ],
            predicted: vec![],
        };
        let stored = Some(pair("00:00:5e:00:53:01", "NIC.Integrated.1-1-1"));
        assert_eq!(
            resolve_admin_boot_interface_target(stored, None, Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    #[test]
    fn no_mac_real_rows_beat_predicted_interfaces() {
        // Once any real machine_interfaces row exists, predictions are out of
        // the running -- even a fully-populated one.
        let c = BootInterfaceCandidates {
            interfaces: vec![row("00:00:5e:00:53:02", true, Some("NIC.Slot.7-1-1"))],
            predicted: vec![predicted("00:00:5e:00:53:01", Some("NIC.Embedded.1-1-1"))],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    #[test]
    fn no_mac_machine_row_without_an_id_targets_the_mac_alone() {
        // The designated row hasn't captured an id yet: the action targets the
        // MAC alone, exactly like the machine-controller's
        // boot_interface_target. The explored default is not consulted for an
        // owned machine -- even when it holds an id for the very same NIC.
        let c = BootInterfaceCandidates {
            interfaces: vec![row("00:00:5e:00:53:02", true, None)],
            predicted: vec![],
        };
        for stored in [
            Some(pair("00:00:5e:00:53:02", "NIC.Slot.7-1-1")),
            Some(pair("00:00:5e:00:53:01", "NIC.Integrated.1-1-1")),
            None,
        ] {
            assert_eq!(
                resolve_admin_boot_interface_target(stored, None, Some(&c), None),
                Some(BootInterfaceTarget::MacOnly(
                    "00:00:5e:00:53:02".parse().unwrap()
                )),
            );
        }
    }

    #[test]
    fn no_mac_a_sole_prediction_decides_when_no_rows_exist() {
        // A machine awaiting its first lease resolves from its prediction when
        // there is exactly one: the recorded id completes the pair, and a
        // prediction without an id is targeted by MAC alone.
        let c = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![predicted("00:00:5e:00:53:01", Some("NIC.Embedded.1-1-1"))],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:01",
                "NIC.Embedded.1-1-1"
            ))),
        );

        let idless = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![predicted("00:00:5e:00:53:01", None)],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, Some(&idless), None),
            Some(BootInterfaceTarget::MacOnly(
                "00:00:5e:00:53:01".parse().unwrap()
            )),
        );
    }

    #[test]
    fn no_mac_multiple_predictions_refuse_to_guess_a_boot_device() {
        // These predictions are non-primary and this resolver doesn't consult
        // the primary flag yet, so with several (a report listing SuperNICs
        // alongside the boot NIC) the declared intent is unknowable: resolution
        // refuses to guess rather than silently programming boot order against
        // whichever NIC sorts lowest. The operator's explicit MAC still
        // resolves, completed from the matching prediction.
        let c = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![
                predicted("00:00:5e:00:53:02", Some("NIC.Slot.7-1-1")),
                predicted("00:00:5e:00:53:01", Some("NIC.Embedded.1-1-1")),
            ],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, Some(&c), None),
            None
        );
        let stored = Some(pair("00:00:5e:00:53:09", "NIC.Other.9-9-9"));
        assert_eq!(
            resolve_admin_boot_interface_target(stored, None, Some(&c), None),
            None,
            "an explored default must never answer for an owned machine",
        );
        assert_eq!(
            resolve_admin_boot_interface_target(
                None,
                None,
                Some(&c),
                Some("00:00:5e:00:53:02".parse().unwrap()),
            ),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:02",
                "NIC.Slot.7-1-1"
            ))),
        );
    }

    // A declared-primary prediction disambiguates a multi-prediction host:
    // `pick_boot_prediction` selects it, so resolution targets the declared NIC
    // rather than refusing. (Multiple NON-primary predictions still refuse --
    // see `no_mac_multiple_predictions_refuse_to_guess_a_boot_device`.)
    #[test]
    fn no_mac_declared_primary_prediction_wins_over_other_predictions() {
        let declared_primary = PredictedMachineInterface {
            primary_interface: true,
            ..predicted("00:00:5e:00:53:01", Some("NIC.Embedded.1-1-1"))
        };
        let other = predicted("00:00:5e:00:53:02", Some("NIC.Slot.7-1-1"));
        let c = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![other, declared_primary],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:01",
                "NIC.Embedded.1-1-1"
            ))),
        );
    }

    #[test]
    fn no_mac_underlay_only_rows_let_a_sole_prediction_answer() {
        // Real rows exist but none is a boot candidate (declared bmc/oob NICs
        // land on Underlay segments); the sole prediction answers, ahead of
        // the explored default.
        let mut underlay = row("00:00:5e:00:53:09", false, None);
        underlay.network_segment_type = Some(NetworkSegmentType::Underlay);
        let c = BootInterfaceCandidates {
            interfaces: vec![underlay],
            predicted: vec![predicted("00:00:5e:00:53:01", Some("NIC.Embedded.1-1-1"))],
        };
        let stored = Some(pair("00:00:5e:00:53:09", "NIC.Other.9-9-9"));
        assert_eq!(
            resolve_admin_boot_interface_target(stored, None, Some(&c), None),
            Some(BootInterfaceTarget::Pair(pair(
                "00:00:5e:00:53:01",
                "NIC.Embedded.1-1-1"
            ))),
        );
    }

    #[test]
    fn no_mac_only_an_unowned_endpoint_uses_the_explored_default() {
        // The explored default answers for endpoints no machine owns. An
        // owned machine resolves from its own data alone: with no candidate
        // at all there is no target, even when a stored default exists.
        let stored = pair("00:00:5e:00:53:01", "NIC.Integrated.1-1-1");
        assert_eq!(
            resolve_admin_boot_interface_target(Some(stored.clone()), None, None, None),
            Some(BootInterfaceTarget::Pair(stored.clone())),
        );
        let empty = BootInterfaceCandidates {
            interfaces: vec![],
            predicted: vec![],
        };
        assert_eq!(
            resolve_admin_boot_interface_target(Some(stored), None, Some(&empty), None),
            None,
        );
        assert_eq!(
            resolve_admin_boot_interface_target(None, None, None, None),
            None
        );
    }
}
