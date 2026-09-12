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

use ::rpc::forge as rpc;
use carbide_instrument::emit;
use lazy_static::lazy_static;
use mac_address::MacAddress;
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedMachine, ExpectedMachineData,
    ExpectedMachineRequest, LegacyHostBmcOverrides,
};
use regex::Regex;
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::machine_interface_address::update_preallocated_expected_machine_interface;
use crate::handlers::static_address_metrics::{
    PreallocationSuccess, StaticAddressPreallocationCompleted,
};

lazy_static! {
    // Verify what serial is alphanumeric string with, allows dashes '-' and underscores '_'
    static ref CHASSIS_SERIAL_REGEX: Regex = Regex::new(r"^[A-Za-z0-9_-]{4,64}$").unwrap();
}

/// Returns one expected machine by database id or BMC MAC (from `ExpectedMachineRequest`).
pub(crate) async fn get(
    api: &Api,
    request: tonic::Request<rpc::ExpectedMachineRequest>,
) -> Result<tonic::Response<rpc::ExpectedMachine>, tonic::Status> {
    log_request_data(&request);

    let req: ExpectedMachineRequest = request
        .into_inner()
        .try_into()
        .map_err(|e| CarbideError::InvalidArgument(format!("{}", e)))?;

    let target_id = req
        .id
        .map(|u| u.to_string())
        .or(req.bmc_mac_address.map(|m| m.to_string()))
        .unwrap_or_default();

    let expected_machine = db::expected_machine::find(&api.database_connection, &req)
        .await
        .map_err(CarbideError::from)?
        .ok_or(CarbideError::NotFoundError {
            kind: "expected_machine",
            id: target_id,
        })?;

    let response = rpc::ExpectedMachine::from(expected_machine);
    Ok(tonic::Response::new(response))
}

/// Adds an expected machine.
pub(crate) async fn add(
    api: &Api,
    request: tonic::Request<rpc::ExpectedMachine>,
) -> Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);

    let machine = parse_expected_machine_for_insert(request.into_inner(), None)?;

    let mut txn = api.txn_begin().await?;
    db::expected_machine::create(&mut txn, machine).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

/// `parse_expected_machine_for_insert` converts an RPC request and runs every
/// validation required before creating an `ExpectedMachine`.
///
/// Keeping parsing separate from [`add`] lets [`replace_all`] reject every bad
/// replacement before it clears the current inventory.
fn parse_expected_machine_for_insert(
    request: rpc::ExpectedMachine,
    previous: Option<&ExpectedMachine>,
) -> Result<ExpectedMachine, CarbideError> {
    if carbide_utils::has_duplicates(&request.fallback_dpu_serial_numbers) {
        return Err(CarbideError::InvalidArgument(
            "duplicate dpu serial number found".to_string(),
        ));
    }

    if !CHASSIS_SERIAL_REGEX.is_match(&request.chassis_serial_number) {
        return Err(CarbideError::InvalidArgument(format!(
            "chassis serial is not formatted properly {}",
            request.chassis_serial_number
        )));
    }

    let parsed_mac: MacAddress = request
        .bmc_mac_address
        .parse::<MacAddress>()
        .map_err(CarbideError::from)?;

    let id = request
        .id
        .as_ref()
        .map(|u| {
            Uuid::parse_str(&u.value).map_err(|_| {
                CarbideError::InvalidArgument("invalid expected_machine id".to_string())
            })
        })
        .transpose()?;
    let mut overrides = LegacyHostBmcOverrides::try_from(&request)?;
    let previous_has_host_bmc = previous.is_some_and(|machine| {
        machine
            .data
            .interfaces
            .iter()
            .any(|interface| interface.role.is_host_bmc())
    });
    let request_has_host_bmc = request
        .interfaces()
        .iter()
        .any(|interface| interface.role == Some(rpc::ExpectedInterfaceRole::HostBmc as i32));
    if previous.is_some()
        && !request_has_host_bmc
        && (request.replace_host_nics || !previous_has_host_bmc)
    {
        // This helper's only update caller is replace-all, where omitted
        // compatibility fields historically meant replacement with their
        // defaults. An authoritative list without HostBmc follows that same
        // rule; an older client's omitted list still preserves nested data it
        // cannot represent.
        overrides.ip_address.get_or_insert(None);
        overrides
            .ip_allocation
            .get_or_insert(BmcIpAllocationType::Auto);
    }
    let db_data: ExpectedMachineData = request.try_into()?;

    let mut machine = ExpectedMachine {
        id,
        bmc_mac_address: parsed_mac,
        data: db_data,
    };

    normalize_host_bmc_configuration(&mut machine, previous, overrides)?;
    validate_expected_machine_for_insert(&machine)?;
    Ok(machine)
}

/// `validate_expected_machine_for_insert` applies the validation shared by the
/// `add` handler and the `expected_machines.json` import flow.
fn validate_expected_machine_for_insert(machine: &ExpectedMachine) -> Result<(), CarbideError> {
    validate_expected_interfaces(&machine.data.interfaces)?;
    validate_host_bmc_declaration(machine)?;
    machine
        .data
        .bmc_ip_allocation
        .validate(machine.data.bmc_ip_address.is_some())
        .map_err(|msg| CarbideError::InvalidArgument(msg.to_string()))?;

    Ok(())
}

/// Create missing expected_machines that aren't already in the database,
/// calling `validate_expected_machine_for_insert` for each new entry. This is currently
/// purely used by the expected_machines.json import path only, but lives
/// here so it can re-leverage `validate_expected_machine_for_insert` and share the
/// same validation codepath as the API handler.
pub(crate) async fn create_missing_from(
    txn: &mut sqlx::PgConnection,
    expected_machines: &[ExpectedMachine],
) -> Result<(), CarbideError> {
    let existing_macs: HashSet<String> = db::expected_machine::find_all(&mut *txn)
        .await?
        .into_iter()
        .map(|m| m.bmc_mac_address.to_string())
        .collect();

    for expected_machine in expected_machines {
        if existing_macs.contains(&expected_machine.bmc_mac_address.to_string()) {
            tracing::debug!(
                bmc_mac_address = %expected_machine.bmc_mac_address,
                "Expected machine already exists; not overwriting",
            );
            continue;
        }
        let mut expected_machine = expected_machine.clone();
        let overrides = if expected_machine
            .data
            .interfaces
            .iter()
            .any(|interface| interface.role.is_host_bmc())
        {
            LegacyHostBmcOverrides {
                ip_address: expected_machine.data.bmc_ip_address.map(Some),
                ip_allocation: (expected_machine.data.bmc_ip_allocation
                    != model::expected_machine::BmcIpAllocationType::Auto)
                    .then_some(expected_machine.data.bmc_ip_allocation),
                ..Default::default()
            }
        } else {
            LegacyHostBmcOverrides::default()
        };
        normalize_host_bmc_configuration(&mut expected_machine, None, overrides)?;
        validate_expected_machine_for_insert(&expected_machine)?;
        db::expected_machine::create(&mut *txn, expected_machine).await?;
    }

    Ok(())
}

/// Deletes an expected machine by id or BMC MAC.
pub(crate) async fn delete(
    api: &Api,
    request: tonic::Request<rpc::ExpectedMachineRequest>,
) -> Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);

    let req: ExpectedMachineRequest = request
        .into_inner()
        .try_into()
        .map_err(|e| CarbideError::InvalidArgument(format!("{}", e)))?;

    let mut txn = api.txn_begin().await?;

    db::expected_machine::delete(&mut txn, &req)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

/// Updates an expected machine row and reconciles configured fixed BMC and
/// nested-interface addresses. Existing addressed interfaces remain unchanged;
/// operators use `machine-interfaces assign-address` to update those rows.
pub(crate) async fn update(
    api: &Api,
    request: tonic::Request<rpc::ExpectedMachine>,
) -> Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);

    let mut request = request.into_inner();
    if carbide_utils::has_duplicates(&request.fallback_dpu_serial_numbers) {
        return Err(
            CarbideError::InvalidArgument("duplicate dpu serial number found".to_string()).into(),
        );
    }
    // Save fields needed later before moving `request` into data conversion
    let id = request
        .id
        .as_ref()
        .map(|u| {
            Uuid::parse_str(&u.value).map_err(|_| {
                CarbideError::InvalidArgument("invalid expected_machine id".to_string())
            })
        })
        .transpose()?;
    let parsed_mac: MacAddress = request
        .bmc_mac_address
        .parse::<MacAddress>()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    let existing = db::expected_machine::find_for_update(
        &mut txn,
        &ExpectedMachineRequest {
            id,
            bmc_mac_address: Some(parsed_mac),
        },
    )
    .await?;
    ensure_bmc_mac_unchanged(existing.as_ref(), parsed_mac)?;
    preserve_omitted_rpc_role_and_allocation(&mut request, existing.as_ref());
    let overrides = LegacyHostBmcOverrides::try_from(&request)?;
    let data: ExpectedMachineData = request.try_into()?;

    let mut machine = ExpectedMachine {
        id,
        bmc_mac_address: parsed_mac,
        data,
    };
    normalize_host_bmc_configuration(&mut machine, existing.as_ref(), overrides)?;
    validate_expected_machine_for_insert(&machine)?;

    let preallocations = update_preallocated_interfaces(
        &mut txn,
        &machine,
        api.runtime_config.retained_boot_interface_window,
    )
    .await?;

    db::expected_machine::update(&mut txn, &machine)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    for outcome in preallocations {
        emit(StaticAddressPreallocationCompleted::from(outcome));
    }

    Ok(tonic::Response::new(()))
}

/// Replace the complete expected-machine inventory in one transaction.
///
/// Every entry is parsed and validated before the current inventory is
/// cleared. The clear and replacement inserts then commit together, so any
/// database failure also leaves the previous inventory intact.
pub(crate) async fn replace_all(
    api: &Api,
    request: tonic::Request<rpc::ExpectedMachineList>,
) -> Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let mut txn = api.txn_begin().await?;
    let previous = db::expected_machine::find_all_for_replace(&mut txn).await?;
    let mut replacements = request.expected_machines;
    for replacement in &mut replacements {
        let existing = find_previous_expected_machine(&previous, replacement);
        preserve_omitted_rpc_role_and_allocation(replacement, existing);
    }

    let mut seen_bmc_macs = HashSet::with_capacity(replacements.len());
    let mut seen_ids = HashSet::with_capacity(replacements.len());
    let mut parsed_replacements = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        let existing = find_previous_expected_machine(&previous, &replacement);
        let parsed = parse_expected_machine_for_insert(replacement, existing)?;
        if !seen_bmc_macs.insert(parsed.bmc_mac_address) {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate expected machine BMC MAC address {} in replacement list",
                parsed.bmc_mac_address,
            ))
            .into());
        }
        if let Some(id) = parsed.id
            && !seen_ids.insert(id)
        {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate expected machine id {id} in replacement list",
            ))
            .into());
        }
        parsed_replacements.push(parsed);
    }

    db::expected_machine::clear(&mut txn).await?;
    for expected_machine in parsed_replacements {
        db::expected_machine::create(&mut txn, expected_machine).await?;
    }
    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

/// Lists all expected machines (includes configured `bmc_ip_address` when set).
pub(crate) async fn get_all(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<tonic::Response<rpc::ExpectedMachineList>, tonic::Status> {
    log_request_data(&request);

    let expected_machine_list: Vec<ExpectedMachine> =
        db::expected_machine::find_all(&api.database_connection).await?;

    Ok(tonic::Response::new(rpc::ExpectedMachineList {
        expected_machines: expected_machine_list.into_iter().map(Into::into).collect(),
    }))
}

/// Lists expected machines joined to explored interfaces / machines (linkage view).
pub(crate) async fn get_linked(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<tonic::Response<rpc::LinkedExpectedMachineList>, tonic::Status> {
    log_request_data(&request);

    let out = db::expected_machine::find_all_linked(&api.database_connection).await?;
    let list = rpc::LinkedExpectedMachineList {
        expected_machines: out.into_iter().map(|m| m.into()).collect(),
    };
    Ok(tonic::Response::new(list))
}

/// Lists host BMC endpoints that Site Explorer has explored but whose MAC is
/// not listed in any of `expected_machines`, `expected_power_shelf`, or
/// `expected_switch`. DPUs, power shelves, and switches are filtered out so the
/// response only contains actual host BMCs.
///
/// An entry with a non-null `machine_id` is an orphan: the host was ingested
/// before its `expected_machines` row was removed.
pub(crate) async fn get_all_unexpected_machines(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<tonic::Response<rpc::UnexpectedMachineList>, tonic::Status> {
    log_request_data(&request);

    let out = db::expected_machine::find_all_unexpected(&api.database_connection).await?;
    let list = rpc::UnexpectedMachineList {
        unexpected_machines: out.into_iter().map(Into::into).collect(),
    };
    Ok(tonic::Response::new(list))
}

/// Deletes every expected machine row.
pub(crate) async fn delete_all(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    db::expected_machine::clear(&mut txn).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

/// Reject invalid expected-interface allocation and primary declarations.
fn validate_expected_interfaces(interfaces: &[ExpectedInterface]) -> Result<(), CarbideError> {
    validate_expected_interface_role_and_allocation(interfaces)?;
    validate_at_most_one_primary_interface(interfaces)
}

/// `validate_expected_interface_role_and_allocation` checks only the role and
/// allocation fields added to expected interfaces.
///
/// Batch update historically did not apply the older Expected Machine
/// validators, so it uses this narrower check to avoid changing legacy batch
/// behavior.
fn validate_expected_interface_role_and_allocation(
    interfaces: &[ExpectedInterface],
) -> Result<(), CarbideError> {
    let mut roles_by_mac = HashMap::new();
    for interface in interfaces {
        interface.validate_ip_allocation().map_err(|message| {
            CarbideError::InvalidArgument(format!(
                "interfaces entry {}: {message}",
                interface.mac_address,
            ))
        })?;
        let host_bmc_compatibility_false =
            interface.role.is_host_bmc() && interface.primary == Some(false);
        if interface.primary.is_some() && !interface.role.is_host() && !host_bmc_compatibility_false
        {
            return Err(CarbideError::InvalidArgument(format!(
                "only a role=host interface may set primary; {} has role {}",
                interface.mac_address, interface.role,
            )));
        }
        // One MAC may have separate IPv4 and IPv6 fixed reservations, so
        // duplicate entries remain valid. Their role still has to agree:
        // DHCP consumes one matching declaration while Site Explorer consumes
        // every declaration, and role controls row type, primary behavior, and
        // Redfish scanning in both paths.
        if let Some(existing_role) = roles_by_mac.insert(interface.mac_address, interface.role)
            && existing_role != interface.role
        {
            return Err(CarbideError::InvalidArgument(format!(
                "interfaces entries for MAC {} must use the same role; found {existing_role} and {}",
                interface.mac_address, interface.role,
            )));
        }
    }

    Ok(())
}

/// Validate the Host BMC declaration before normalization can correct its
/// identity fields.
///
/// The top-level MAC remains the ExpectedMachine alternate key, so a nested
/// Host BMC must use that same address. An explicit `primary=false` remains
/// accepted for clients that serialize false for every interface; the
/// normalized declaration omits it.
fn validate_host_bmc_declaration(machine: &ExpectedMachine) -> Result<(), CarbideError> {
    let host_bmcs = machine
        .data
        .interfaces
        .iter()
        .filter(|interface| interface.role.is_host_bmc())
        .collect::<Vec<_>>();
    if host_bmcs.len() > 1 {
        return Err(CarbideError::InvalidArgument(format!(
            "at most one role=host_bmc interface may be configured, got {}",
            host_bmcs.len(),
        )));
    }

    if let Some(host_bmc) = host_bmcs.first() {
        if host_bmc.mac_address != machine.bmc_mac_address {
            return Err(CarbideError::InvalidArgument(format!(
                "role=host_bmc interface MAC {} must match expected machine BMC MAC {}",
                host_bmc.mac_address, machine.bmc_mac_address,
            )));
        }
        if host_bmc.primary == Some(true) {
            return Err(CarbideError::InvalidArgument(format!(
                "role=host_bmc interface {} cannot set primary=true",
                host_bmc.mac_address,
            )));
        }
    }

    Ok(())
}

/// Reject a new interface role that conflicts with the machine's BMC identity.
///
/// Existing rows may predate the HostBmc role, so an update may carry an
/// unchanged conflicting declaration forward. Runtime lookup still gives the
/// top-level BMC identity precedence for those rows.
fn validate_bmc_identity_role(
    machine: &ExpectedMachine,
    previous: Option<&ExpectedMachine>,
) -> Result<(), CarbideError> {
    let conflicts = machine
        .data
        .interfaces
        .iter()
        .filter(|interface| {
            interface.mac_address == machine.bmc_mac_address && !interface.role.is_host_bmc()
        })
        .collect::<Vec<_>>();
    if conflicts.is_empty() {
        return Ok(());
    }

    let unchanged_legacy_conflicts = previous.is_some_and(|previous| {
        let mut previous_conflicts = previous
            .data
            .interfaces
            .iter()
            .filter(|interface| {
                interface.mac_address == previous.bmc_mac_address && !interface.role.is_host_bmc()
            })
            .collect::<Vec<_>>();
        conflicts.len() == previous_conflicts.len()
            && conflicts.iter().all(|interface| {
                previous_conflicts
                    .iter()
                    .position(|previous| *previous == *interface)
                    .map(|index| previous_conflicts.swap_remove(index))
                    .is_some()
            })
    });
    if unchanged_legacy_conflicts {
        return Ok(());
    }

    let interface = conflicts[0];
    Err(CarbideError::InvalidArgument(format!(
        "expected machine BMC MAC {} may only be configured with role=host_bmc, got role={}",
        machine.bmc_mac_address, interface.role,
    )))
}

/// Apply Host BMC compatibility fields and normalize a configured nested
/// declaration without adding one to legacy-only rows.
fn normalize_host_bmc_configuration(
    machine: &mut ExpectedMachine,
    previous: Option<&ExpectedMachine>,
    overrides: LegacyHostBmcOverrides,
) -> Result<(), CarbideError> {
    validate_host_bmc_declaration(machine)?;
    validate_bmc_identity_role(machine, previous)?;
    machine
        .normalize_host_bmc(previous, overrides)
        .map_err(|message| CarbideError::InvalidArgument(format!("host BMC: {message}")))
}

/// ExpectedMachine updates cannot change the BMC MAC stored as the alternate
/// key.
///
/// The database update selects by ID when one is present but intentionally
/// leaves `bmc_mac_address` unchanged. Reject a different submitted value
/// before it can be normalized into `interfaces`.
fn ensure_bmc_mac_unchanged(
    existing: Option<&ExpectedMachine>,
    submitted_bmc_mac_address: MacAddress,
) -> Result<(), CarbideError> {
    if let Some(existing) = existing
        && existing.bmc_mac_address != submitted_bmc_mac_address
    {
        return Err(CarbideError::InvalidArgument(format!(
            "expected machine update cannot change BMC MAC address from {} to {}",
            existing.bmc_mac_address, submitted_bmc_mac_address,
        )));
    }
    Ok(())
}

/// `validate_at_most_one_primary_interface` rejects competing machine-wide Host
/// primary declarations. DPU roles define their own primary behavior and may
/// not set this field.
fn validate_at_most_one_primary_interface(
    interfaces: &[ExpectedInterface],
) -> Result<(), CarbideError> {
    let primaries: Vec<_> = interfaces
        .iter()
        .filter(|n| n.primary == Some(true))
        .map(|n| n.mac_address.to_string())
        .collect();
    if primaries.len() > 1 {
        return Err(CarbideError::InvalidArgument(format!(
            "at most one role=host interface may be flagged primary=true, got {}: {}",
            primaries.len(),
            primaries.join(", ")
        )));
    }
    Ok(())
}

/// `find_previous_expected_machine` finds the stored row whose newly-added
/// interface fields may need to survive a replacement request.
///
/// Prefer the stable ID so a BMC MAC change still matches the same row. Older
/// clients may omit the ID, so the BMC MAC remains the compatibility fallback.
fn find_previous_expected_machine<'a>(
    previous: &'a [ExpectedMachine],
    replacement: &rpc::ExpectedMachine,
) -> Option<&'a ExpectedMachine> {
    replacement
        .id
        .as_ref()
        .and_then(|id| Uuid::parse_str(&id.value).ok())
        .and_then(|id| previous.iter().find(|machine| machine.id == Some(id)))
        .or_else(|| {
            replacement
                .bmc_mac_address
                .parse::<MacAddress>()
                .ok()
                .and_then(|mac_address| {
                    previous
                        .iter()
                        .find(|machine| machine.bmc_mac_address == mac_address)
                })
        })
}

/// Preserve omitted role and allocation fields unknown to older RPC clients.
///
/// `None` means the field was absent, while an explicit `Unspecified` asks to
/// restore legacy defaults. Match each interface at the same list index first
/// so duplicate legacy MAC entries retain independent settings, then fall back
/// to MAC for clients that reorder the list.
///
/// Other optional fields keep their existing full-replacement behavior. For
/// example, omitting `network_segment_type` removes that guard.
fn preserve_omitted_rpc_role_and_allocation(
    replacement: &mut rpc::ExpectedMachine,
    existing: Option<&ExpectedMachine>,
) {
    let Some(existing) = existing else {
        return;
    };

    let effective_host_bmc = existing.effective_host_bmc();
    for (index, interface) in replacement.interfaces_mut().iter_mut().enumerate() {
        let Ok(mac_address) = interface.mac_address.parse::<MacAddress>() else {
            continue;
        };
        let Some(existing_interface) = existing
            .data
            .interfaces
            .get(index)
            .filter(|candidate| candidate.mac_address == mac_address)
            .or_else(|| {
                existing
                    .data
                    .interfaces
                    .iter()
                    .find(|candidate| candidate.mac_address == mac_address)
            })
            .or_else(|| (mac_address == existing.bmc_mac_address).then_some(&effective_host_bmc))
        else {
            // A new interface has no stored value to preserve. Missing fields
            // retain their normal inference/default behavior.
            continue;
        };

        let preserve_role = interface.role.is_none();
        let preserve_ip_allocation = interface.ip_allocation.is_none();
        if preserve_role {
            interface.role = (!existing_interface.role.is_host())
                .then(|| rpc::ExpectedInterfaceRole::from(existing_interface.role) as i32);
            if !existing_interface.role.is_host() && interface.primary == Some(false) {
                // Older clients commonly send primary=false for every entry.
                // Once the stored DPU role is restored, that legacy field no
                // longer applies and must not make the payload invalid.
                interface.primary = None;
            }
        }
        if preserve_ip_allocation
            // An old client still controls legacy fixed/dynamic intent through
            // `fixed_ip`. Preserve a newer explicit policy only while that
            // legacy signal remains unchanged.
            && existing_interface.fixed_ip.is_some()
                == interface
                    .fixed_ip
                    .as_deref()
                    .is_some_and(|ip| !ip.is_empty())
        {
            interface.ip_allocation = existing_interface
                .ip_allocation
                .map(|policy| rpc::ExpectedInterfaceIpAllocation::from(policy) as i32);
        }
    }
}

/// `update_preallocated_interfaces` applies the update-time fixed-address
/// behavior shared by the single and batch APIs.
///
/// The lower-level helpers create a missing reservation or fill an addressless
/// row before that family's first stateful allocation, but leave existing
/// addresses alone. An associated row may receive its configured fixed address,
/// but its role-derived type and primary setting remain managed state.
/// Operators still use the machine-interface address APIs to replace a live
/// address. The effective Host BMC is applied separately so legacy-only rows
/// use the same path without storing a nested declaration.
async fn update_preallocated_interfaces(
    txn: &mut sqlx::PgConnection,
    machine: &ExpectedMachine,
    retained_window: Option<chrono::Duration>,
) -> Result<Vec<PreallocationSuccess>, CarbideError> {
    let mut preallocations = Vec::new();
    let host_bmc = machine.effective_host_bmc();
    if host_bmc.fixed_ip.is_some() {
        preallocations.push(
            update_preallocated_expected_machine_interface(&mut *txn, &host_bmc, retained_window)
                .await?,
        );
    }

    for interface in machine
        .data
        .interfaces
        .iter()
        .filter(|interface| interface.mac_address != machine.bmc_mac_address)
    {
        if interface.fixed_ip.is_some() {
            preallocations.push(
                update_preallocated_expected_machine_interface(
                    &mut *txn,
                    interface,
                    retained_window,
                )
                .await?,
            );
        }
    }

    Ok(preallocations)
}

/// Preserve the request representation used by batch results and replace only
/// the ID with its parsed value. Older clients expect their payload back rather
/// than a model-normalized copy.
fn batch_result_with_id(mut machine: rpc::ExpectedMachine, id: Uuid) -> rpc::ExpectedMachine {
    machine.id = Some(::rpc::common::Uuid {
        value: id.to_string(),
    });
    machine
}

/// Helper function to sanitize expected machine and return parsed IDs (ID+MAC)
fn sanitize_expected_machine_and_get_ids(
    _api: &Api,
    request: rpc::ExpectedMachine,
    _is_update: bool,
) -> Result<(Uuid, MacAddress), CarbideError> {
    // Validate id is present
    let id = match &request.id {
        Some(uuid_val) => Uuid::parse_str(&uuid_val.value).map_err(|_| {
            CarbideError::InvalidArgument("invalid expected_machine id".to_string())
        })?,
        None => {
            return Err(CarbideError::InvalidArgument(
                "id is mandatory for batch operations".to_string(),
            ));
        }
    };

    // Validate bmc_mac_address is present and parseable
    if request.bmc_mac_address.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "bmc_mac_address is mandatory".to_string(),
        ));
    }

    let parsed_mac: MacAddress = request
        .bmc_mac_address
        .parse::<MacAddress>()
        .map_err(CarbideError::from)?;

    // Validate duplicates in fallback DPU serial numbers
    if carbide_utils::has_duplicates(&request.fallback_dpu_serial_numbers) {
        return Err(CarbideError::InvalidArgument(
            "duplicate dpu serial number found".to_string(),
        ));
    }

    // Validate chassis serial format
    if !CHASSIS_SERIAL_REGEX.is_match(&request.chassis_serial_number) {
        return Err(CarbideError::InvalidArgument(format!(
            "chassis serial is not formatted properly {}",
            request.chassis_serial_number
        )));
    }

    Ok((id, parsed_mac))
}

/// Creates one expected machine inside an existing transaction (batch API).
async fn create_expected_machine(
    txn: &mut sqlx::PgConnection,
    machine: rpc::ExpectedMachine,
    id: Uuid,
    parsed_mac: MacAddress,
) -> Result<(rpc::ExpectedMachine, Vec<PreallocationSuccess>), CarbideError> {
    let result_machine = batch_result_with_id(machine.clone(), id);
    let overrides = LegacyHostBmcOverrides::try_from(&machine)?;
    let db_data: ExpectedMachineData = machine.try_into()?;

    let mut expected_machine = ExpectedMachine {
        id: Some(id),
        bmc_mac_address: parsed_mac,
        data: db_data,
    };

    normalize_host_bmc_configuration(&mut expected_machine, None, overrides)?;
    validate_expected_machine_for_insert(&expected_machine)?;
    db::expected_machine::create(txn, expected_machine).await?;

    Ok((result_machine, Vec::new()))
}

/// Updates one expected machine inside an existing transaction (batch API).
///
/// Fixed BMC and nested-interface addresses use the same safe reconciliation
/// as [`update`].
async fn update_expected_machine(
    txn: &mut sqlx::PgConnection,
    mut machine: rpc::ExpectedMachine,
    id: Uuid,
    parsed_mac: MacAddress,
    retained_window: Option<chrono::Duration>,
) -> Result<(rpc::ExpectedMachine, Vec<PreallocationSuccess>), CarbideError> {
    let result_machine = batch_result_with_id(machine.clone(), id);
    let existing = db::expected_machine::find_for_update(
        &mut *txn,
        &ExpectedMachineRequest {
            id: Some(id),
            bmc_mac_address: Some(parsed_mac),
        },
    )
    .await?;
    ensure_bmc_mac_unchanged(existing.as_ref(), parsed_mac)?;
    preserve_omitted_rpc_role_and_allocation(&mut machine, existing.as_ref());
    let overrides = LegacyHostBmcOverrides::try_from(&machine)?;
    let data: ExpectedMachineData = machine.try_into()?;

    let mut expected_machine = ExpectedMachine {
        id: Some(id),
        bmc_mac_address: parsed_mac,
        data,
    };
    normalize_host_bmc_configuration(&mut expected_machine, existing.as_ref(), overrides)?;
    validate_expected_machine_for_insert(&expected_machine)?;
    let preallocations =
        update_preallocated_interfaces(txn, &expected_machine, retained_window).await?;

    db::expected_machine::update(txn, &expected_machine).await?;

    Ok((result_machine, preallocations))
}

#[derive(Copy, Clone)]
enum BatchOperation {
    Create,
    Update,
}

impl BatchOperation {
    fn is_update(&self) -> bool {
        matches!(self, BatchOperation::Update)
    }
}

fn build_success_result(machine: rpc::ExpectedMachine) -> rpc::ExpectedMachineOperationResult {
    // Ensure the id is set in the returned machine payload.
    let id = machine
        .id
        .as_ref()
        .and_then(|u| Uuid::parse_str(&u.value).ok());

    rpc::ExpectedMachineOperationResult {
        id: id.map(|value| ::rpc::common::Uuid {
            value: value.to_string(),
        }),
        success: true,
        error_message: None,
        expected_machine: Some(machine),
    }
}

fn build_failure_result(id: Uuid, error_message: String) -> rpc::ExpectedMachineOperationResult {
    rpc::ExpectedMachineOperationResult {
        id: Some(::rpc::common::Uuid {
            value: id.to_string(),
        }),
        success: false,
        error_message: Some(error_message),
        expected_machine: None,
    }
}

async fn apply_operation(
    op: BatchOperation,
    txn: &mut sqlx::PgConnection,
    machine: rpc::ExpectedMachine,
    id: Uuid,
    parsed_mac: MacAddress,
    retained_window: Option<chrono::Duration>,
) -> Result<(rpc::ExpectedMachine, Vec<PreallocationSuccess>), CarbideError> {
    match op {
        BatchOperation::Create => create_expected_machine(txn, machine, id, parsed_mac).await,
        BatchOperation::Update => {
            update_expected_machine(txn, machine, id, parsed_mac, retained_window).await
        }
    }
}

async fn process_batch_operations(
    api: &Api,
    machines: Vec<rpc::ExpectedMachine>,
    accept_partial: bool,
    op: BatchOperation,
) -> Result<Vec<rpc::ExpectedMachineOperationResult>, CarbideError> {
    let mut results = Vec::new();

    if accept_partial {
        for machine in machines {
            let request_id = machine
                .id
                .as_ref()
                .and_then(|u| Uuid::parse_str(&u.value).ok())
                .unwrap_or_else(Uuid::nil);

            let (id, parsed_mac) =
                match sanitize_expected_machine_and_get_ids(api, machine.clone(), op.is_update()) {
                    Ok(ids) => ids,
                    Err(e) => {
                        results.push(build_failure_result(
                            request_id,
                            format!("Validation failed: {}", e),
                        ));
                        continue;
                    }
                };

            let mut txn = match api.txn_begin().await {
                Ok(txn) => txn,
                Err(e) => {
                    results.push(build_failure_result(
                        id,
                        format!("Failed to begin transaction: {}", e),
                    ));
                    continue;
                }
            };

            match apply_operation(
                op,
                txn.as_pgconn(),
                machine,
                id,
                parsed_mac,
                api.runtime_config.retained_boot_interface_window,
            )
            .await
            {
                Ok((result_machine, preallocations)) => match txn.commit().await {
                    Ok(_) => {
                        for outcome in preallocations {
                            emit(StaticAddressPreallocationCompleted::from(outcome));
                        }
                        results.push(build_success_result(result_machine));
                    }
                    Err(e) => {
                        results.push(build_failure_result(id, format!("Failed to commit: {}", e)))
                    }
                },
                Err(e) => {
                    txn.rollback_or_log("expected-machine write after operation failure")
                        .await;
                    results.push(build_failure_result(id, format!("Operation failed: {}", e)));
                }
            }
        }

        return Ok(results);
    }

    let mut prepared = Vec::with_capacity(machines.len());
    for (request_index, machine) in machines.into_iter().enumerate() {
        let (id, parsed_mac) =
            sanitize_expected_machine_and_get_ids(api, machine.clone(), op.is_update())?;
        prepared.push((request_index, machine, id, parsed_mac));
    }
    if op.is_update() {
        // Every update locks its stored row before preserving fields omitted by
        // older clients. A stable order keeps overlapping all-or-nothing
        // batches from taking those row locks in opposite directions.
        prepared.sort_by_key(|(_, _, id, _)| *id);
    }

    let mut txn = api.txn_begin().await?;
    let mut ordered_results = Vec::with_capacity(prepared.len());
    let mut preallocations = Vec::new();

    for (request_index, machine, id, parsed_mac) in prepared {
        let (result_machine, operation_preallocations) = match apply_operation(
            op,
            txn.as_pgconn(),
            machine,
            id,
            parsed_mac,
            api.runtime_config.retained_boot_interface_window,
        )
        .await
        {
            Ok(machine) => machine,
            Err(error) => {
                txn.rollback_or_log("expected-machine write after operation failure")
                    .await;
                return Err(error);
            }
        };
        preallocations.extend(operation_preallocations);
        ordered_results.push((request_index, build_success_result(result_machine)));
    }

    txn.commit().await?;

    for outcome in preallocations {
        emit(StaticAddressPreallocationCompleted::from(outcome));
    }

    ordered_results.sort_by_key(|(request_index, _)| *request_index);
    Ok(ordered_results
        .into_iter()
        .map(|(_, result)| result)
        .collect())
}

/// Batch-create expected machines.
pub(crate) async fn create_expected_machines(
    api: &Api,
    request: tonic::Request<rpc::BatchExpectedMachineOperationRequest>,
) -> Result<tonic::Response<rpc::BatchExpectedMachineOperationResponse>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let accept_partial = request.accept_partial_results;
    let machines = request
        .expected_machines
        .ok_or_else(|| CarbideError::InvalidArgument("expected_machines is required".to_string()))?
        .expected_machines;

    let results =
        process_batch_operations(api, machines, accept_partial, BatchOperation::Create).await?;

    Ok(tonic::Response::new(
        rpc::BatchExpectedMachineOperationResponse { results },
    ))
}

/// Batch-update expected machines. Static BMC IP handling matches single [`update`] for each row.
pub(crate) async fn update_expected_machines(
    api: &Api,
    request: tonic::Request<rpc::BatchExpectedMachineOperationRequest>,
) -> Result<tonic::Response<rpc::BatchExpectedMachineOperationResponse>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let accept_partial = request.accept_partial_results;
    let machines = request
        .expected_machines
        .ok_or_else(|| CarbideError::InvalidArgument("expected_machines is required".to_string()))?
        .expected_machines;

    let results =
        process_batch_operations(api, machines, accept_partial, BatchOperation::Update).await?;

    Ok(tonic::Response::new(
        rpc::BatchExpectedMachineOperationResponse { results },
    ))
}

// Utility method called by `explore`. Not a grpc handler.
pub(super) async fn query(
    api: &Api,
    mac: MacAddress,
) -> Result<Option<ExpectedMachine>, CarbideError> {
    let mut txn = api.txn_begin().await?;

    let mut expected = db::expected_machine::find_many_by_bmc_mac_address(&mut txn, &[mac]).await?;

    txn.commit().await?;

    Ok(expected.remove(&mac))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn test_chassis_serial_regex() {
        assert!(CHASSIS_SERIAL_REGEX.is_match("ABC123"));
        assert!(CHASSIS_SERIAL_REGEX.is_match("ABC-123"));
        assert!(CHASSIS_SERIAL_REGEX.is_match("ABC_123"));
        assert!(CHASSIS_SERIAL_REGEX.is_match("DELL-R740-12345"));
        assert!(CHASSIS_SERIAL_REGEX.is_match("A495122X5503847"));

        assert!(!CHASSIS_SERIAL_REGEX.is_match("ABC"));
        assert!(!CHASSIS_SERIAL_REGEX.is_match("ABC 123"));
        assert!(!CHASSIS_SERIAL_REGEX.is_match("A495122X5503847\r"));
        assert!(!CHASSIS_SERIAL_REGEX.is_match("ABC.123"));

        let too_long = "A".repeat(65);
        assert!(!CHASSIS_SERIAL_REGEX.is_match(&too_long));
    }

    #[test]
    fn expected_interface_primary_is_host_only() {
        check_values(
            [
                Check {
                    scenario: "Host may be primary",
                    input: (
                        model::expected_machine::ExpectedInterfaceRole::Host,
                        Some(true),
                    ),
                    expect: true,
                },
                Check {
                    scenario: "Host may be secondary",
                    input: (
                        model::expected_machine::ExpectedInterfaceRole::Host,
                        Some(false),
                    ),
                    expect: true,
                },
                Check {
                    scenario: "DPU OS derives primary behavior from its role",
                    input: (model::expected_machine::ExpectedInterfaceRole::DpuOs, None),
                    expect: true,
                },
                Check {
                    scenario: "DPU BMC derives primary behavior from its role",
                    input: (model::expected_machine::ExpectedInterfaceRole::DpuBmc, None),
                    expect: true,
                },
                Check {
                    scenario: "DPU OS cannot override primary behavior",
                    input: (
                        model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        Some(false),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "DPU BMC cannot override primary behavior",
                    input: (
                        model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        Some(true),
                    ),
                    expect: false,
                },
            ],
            |(role, primary)| {
                validate_expected_interface_role_and_allocation(&[ExpectedInterface {
                    role,
                    primary,
                    ..Default::default()
                }])
                .is_ok()
            },
        );
    }

    #[test]
    fn duplicate_expected_interface_macs_require_matching_roles() {
        let shared_mac: MacAddress = "7A:7B:7C:7D:7E:81".parse().unwrap();
        let other_mac: MacAddress = "7A:7B:7C:7D:7E:82".parse().unwrap();
        let interface = |mac_address, role| ExpectedInterface {
            mac_address,
            role,
            ..Default::default()
        };
        let fixed_host = |fixed_ip| ExpectedInterface {
            mac_address: shared_mac,
            fixed_ip: Some(fixed_ip),
            ..Default::default()
        };

        check_values(
            [
                Check {
                    scenario: "legacy Host dual-stack reservations",
                    input: vec![
                        fixed_host("192.0.2.81".parse().unwrap()),
                        fixed_host("2001:db8::81".parse().unwrap()),
                    ],
                    expect: true,
                },
                Check {
                    scenario: "same DPU OS role",
                    input: vec![
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ),
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ),
                    ],
                    expect: true,
                },
                Check {
                    scenario: "different MACs may use different roles",
                    input: vec![
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ),
                        interface(
                            other_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        ),
                    ],
                    expect: true,
                },
                Check {
                    scenario: "DPU OS then DPU BMC",
                    input: vec![
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ),
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        ),
                    ],
                    expect: false,
                },
                Check {
                    scenario: "DPU BMC then DPU OS",
                    input: vec![
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        ),
                        interface(
                            shared_mac,
                            model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ),
                    ],
                    expect: false,
                },
            ],
            |interfaces| validate_expected_interface_role_and_allocation(&interfaces).is_ok(),
        );
    }

    #[test]
    fn omitted_role_and_allocation_are_preserved_per_list_entry() {
        let mac_address: MacAddress = "7A:7B:7C:7D:7E:91".parse().unwrap();
        let existing = ExpectedMachine {
            id: Some(Uuid::new_v4()),
            bmc_mac_address: "7A:7B:7C:7D:7E:90".parse().unwrap(),
            data: ExpectedMachineData {
                interfaces: vec![
                    ExpectedInterface {
                        mac_address,
                        role: model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        ip_allocation: Some(
                            model::expected_machine::ExpectedInterfaceIpAllocation::Retained,
                        ),
                        network_segment_type: Some(
                            model::network_segment::NetworkSegmentType::Underlay,
                        ),
                        ..Default::default()
                    },
                    ExpectedInterface {
                        mac_address,
                        role: model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                        ip_allocation: Some(
                            model::expected_machine::ExpectedInterfaceIpAllocation::Dynamic,
                        ),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        };
        let mut replacement = rpc::ExpectedMachine {
            host_nics: vec![
                rpc::ExpectedInterface {
                    mac_address: mac_address.to_string(),
                    primary: Some(false),
                    ..Default::default()
                },
                rpc::ExpectedInterface {
                    mac_address: mac_address.to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        preserve_omitted_rpc_role_and_allocation(&mut replacement, Some(&existing));

        assert_eq!(
            replacement.host_nics[0].role,
            Some(rpc::ExpectedInterfaceRole::DpuBmc as i32),
        );
        assert_eq!(
            replacement.host_nics[0].ip_allocation,
            Some(rpc::ExpectedInterfaceIpAllocation::Retained as i32),
        );
        assert_eq!(replacement.host_nics[0].primary, None);
        assert_eq!(replacement.host_nics[0].network_segment_type, None);
        assert_eq!(
            replacement.host_nics[1].role,
            Some(rpc::ExpectedInterfaceRole::DpuBmc as i32),
        );
        assert_eq!(
            replacement.host_nics[1].ip_allocation,
            Some(rpc::ExpectedInterfaceIpAllocation::Dynamic as i32),
        );
        let parsed: ExpectedMachineData = replacement.try_into().unwrap();
        assert!(validate_expected_interface_role_and_allocation(&parsed.interfaces).is_ok());
    }

    #[test]
    fn omitted_dpu_role_does_not_hide_primary_true() {
        let mac_address: MacAddress = "7A:7B:7C:7D:7E:93".parse().unwrap();
        let existing = ExpectedMachine {
            id: Some(Uuid::new_v4()),
            bmc_mac_address: "7A:7B:7C:7D:7E:92".parse().unwrap(),
            data: ExpectedMachineData {
                interfaces: vec![ExpectedInterface {
                    mac_address,
                    role: model::expected_machine::ExpectedInterfaceRole::DpuBmc,
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        let mut replacement = rpc::ExpectedMachine {
            host_nics: vec![rpc::ExpectedInterface {
                mac_address: mac_address.to_string(),
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };

        preserve_omitted_rpc_role_and_allocation(&mut replacement, Some(&existing));

        assert_eq!(
            replacement.host_nics[0].role,
            Some(rpc::ExpectedInterfaceRole::DpuBmc as i32),
        );
        assert_eq!(replacement.host_nics[0].primary, Some(true));
        let parsed: ExpectedMachineData = replacement.try_into().unwrap();
        assert!(validate_expected_interface_role_and_allocation(&parsed.interfaces).is_err());
    }
}
