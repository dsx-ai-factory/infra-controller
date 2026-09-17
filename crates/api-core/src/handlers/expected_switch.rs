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

use std::collections::HashSet;

use ::rpc::forge as rpc;
use ::rpc::forge_api_client::{EXPECTED_SWITCH_UPDATE_MASK_HEADER, ExpectedSwitchUpdateField};
use carbide_instrument::emit;
use db::{DatabaseError, expected_switch as db_expected_switch};
use mac_address::MacAddress;
use model::expected_switch::{ExpectedSwitch, ExpectedSwitchRequest};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;
use crate::handlers::machine_interface_address::update_preallocated_machine_interface;
use crate::handlers::static_address_metrics::StaticAddressPreallocationCompleted;

fn parse_expected_switch_update_mask(
    request: &Request<rpc::ExpectedSwitch>,
) -> Result<Option<HashSet<ExpectedSwitchUpdateField>>, CarbideError> {
    let Some(value) = request.metadata().get(EXPECTED_SWITCH_UPDATE_MASK_HEADER) else {
        return Ok(None);
    };

    let value = value.to_str().map_err(|error| {
        CarbideError::InvalidArgument(format!("invalid expected-switch update mask: {error}"))
    })?;

    let fields = value
        .split(',')
        .map(str::parse)
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|_| {
            CarbideError::InvalidArgument(format!("invalid expected-switch update mask: {value}"))
        })?;

    Ok(Some(fields))
}

fn merge_expected_switch_patch(
    mut patch: rpc::ExpectedSwitch,
    current: rpc::ExpectedSwitch,
    fields: &HashSet<ExpectedSwitchUpdateField>,
) -> rpc::ExpectedSwitch {
    patch.expected_switch_id = current.expected_switch_id;
    patch.bmc_mac_address = current.bmc_mac_address;

    if !fields.contains(&ExpectedSwitchUpdateField::BmcUsername) {
        patch.bmc_username = current.bmc_username;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::BmcPassword) {
        patch.bmc_password = current.bmc_password;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::SwitchSerialNumber) {
        patch.switch_serial_number = current.switch_serial_number;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::NvosMacAddresses) {
        patch.nvos_mac_addresses = current.nvos_mac_addresses;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::NvosUsername) {
        patch.nvos_username = current.nvos_username;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::NvosPassword) {
        patch.nvos_password = current.nvos_password;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::RackId) {
        patch.rack_id = current.rack_id;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::BmcIpAddress) {
        patch.bmc_ip_address = current.bmc_ip_address;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::NvosIpAddress) {
        patch.nvos_ip_address = current.nvos_ip_address;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::BmcRetainCredentials) {
        patch.bmc_retain_credentials = current.bmc_retain_credentials;
    }

    let mut patch_metadata = patch.metadata.unwrap_or_default();
    let current_metadata = current.metadata.unwrap_or_default();

    if !fields.contains(&ExpectedSwitchUpdateField::MetadataName) {
        patch_metadata.name = current_metadata.name;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::MetadataDescription) {
        patch_metadata.description = current_metadata.description;
    }

    if !fields.contains(&ExpectedSwitchUpdateField::MetadataLabels) {
        patch_metadata.labels = current_metadata.labels;
    }

    patch.metadata = Some(patch_metadata);

    patch
}

/// `nvos_ip_address` is paired with the single wired NVOS port. We reject any
/// caller that sets it alongside zero-or-multiple `nvos_mac_addresses`, so the
/// (mac, ip) pairing stays unambiguous for the discover hook and the
/// reconciliation pass.
fn validate_nvos_ip_pairing(switch: &ExpectedSwitch) -> Result<(), CarbideError> {
    if switch.nvos_ip_address.is_some() && switch.nvos_mac_addresses.len() != 1 {
        return Err(CarbideError::InvalidArgument(format!(
            "nvos_ip_address requires exactly one nvos_mac_addresses entry, got {}",
            switch.nvos_mac_addresses.len(),
        )));
    }
    Ok(())
}

/// Requires NVOS username and password to be present together and non-empty.
fn validate_nvos_credentials_pair(switch: &ExpectedSwitch) -> Result<(), CarbideError> {
    match (&switch.nvos_username, &switch.nvos_password) {
        (Some(username), Some(_)) if username.is_empty() => Err(CarbideError::InvalidArgument(
            "nvos_username must not be empty".to_string(),
        )),
        (Some(_), Some(password)) if password.is_empty() => Err(CarbideError::InvalidArgument(
            "nvos_password must not be empty".to_string(),
        )),
        (Some(_), Some(_)) | (None, None) => Ok(()),
        _ => Err(CarbideError::InvalidArgument(
            "nvos_username and nvos_password must be set together".to_string(),
        )),
    }
}

fn validate_expected_switch(switch: &ExpectedSwitch) -> Result<(), CarbideError> {
    validate_nvos_ip_pairing(switch)?;
    validate_nvos_credentials_pair(switch)?;

    Ok(())
}

pub(crate) async fn add_expected_switch(
    api: &Api,
    request: Request<rpc::ExpectedSwitch>,
) -> Result<Response<()>, Status> {
    let switch: ExpectedSwitch =
        request
            .into_inner()
            .try_into()
            .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                CarbideError::InvalidArgument(e.to_string())
            })?;

    validate_expected_switch(&switch)?;

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    db_expected_switch::create(&mut txn, switch)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn delete_expected_switch(
    api: &Api,
    request: Request<rpc::ExpectedSwitchRequest>,
) -> Result<Response<()>, Status> {
    let req: ExpectedSwitchRequest =
        request
            .into_inner()
            .try_into()
            .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                CarbideError::InvalidArgument(e.to_string())
            })?;

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    db_expected_switch::delete(&mut txn, &req)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn update_expected_switch(
    api: &Api,
    request: Request<rpc::ExpectedSwitch>,
) -> Result<Response<()>, Status> {
    let update_mask = parse_expected_switch_update_mask(&request)?;
    let patch = request.into_inner();

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let switch: ExpectedSwitch = if let Some(update_mask) = update_mask {
        let lookup: ExpectedSwitchRequest = rpc::ExpectedSwitchRequest {
            bmc_mac_address: patch.bmc_mac_address.clone(),
            expected_switch_id: patch.expected_switch_id.clone(),
        }
        .try_into()
        .map_err(|e: ::rpc::errors::RpcDataConversionError| {
            CarbideError::InvalidArgument(e.to_string())
        })?;

        let current = db_expected_switch::find_for_update(&mut txn, &lookup)
            .await
            .map_err(CarbideError::from)?
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "expected_switch",
                id: lookup
                    .expected_switch_id
                    .map(|id| id.to_string())
                    .or_else(|| lookup.bmc_mac_address.map(|mac| mac.to_string()))
                    .unwrap_or_default(),
            })
            .map_err(crate::CarbideError::from)?;

        merge_expected_switch_patch(patch, current.into(), &update_mask)
            .try_into()
            .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                CarbideError::InvalidArgument(e.to_string())
            })?
    } else {
        patch
            .try_into()
            .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                CarbideError::InvalidArgument(e.to_string())
            })?
    };

    validate_expected_switch(&switch)?;

    let mut preallocations = Vec::with_capacity(2);
    if let Some(bmc_ip) = switch.bmc_ip_address {
        preallocations.push(
            update_preallocated_machine_interface(
                &mut txn,
                switch.bmc_mac_address,
                bmc_ip,
                api.runtime_config.retained_boot_interface_window,
            )
            .await?,
        );
    }
    if let Some(nvos_ip) = switch.nvos_ip_address {
        // Pairing already validated above; nvos_mac_addresses has exactly one entry.
        let nvos_mac = switch.nvos_mac_addresses[0];
        preallocations.push(
            update_preallocated_machine_interface(
                &mut txn,
                nvos_mac,
                nvos_ip,
                api.runtime_config.retained_boot_interface_window,
            )
            .await?,
        );
    }

    db_expected_switch::update(&mut txn, &switch)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    for outcome in preallocations {
        emit(StaticAddressPreallocationCompleted::from(outcome));
    }

    Ok(Response::new(()))
}

pub(crate) async fn get_expected_switch(
    api: &Api,
    request: Request<rpc::ExpectedSwitchRequest>,
) -> Result<Response<rpc::ExpectedSwitch>, Status> {
    let req: ExpectedSwitchRequest =
        request
            .into_inner()
            .try_into()
            .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                CarbideError::InvalidArgument(e.to_string())
            })?;

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let expected_switch = db_expected_switch::find(&mut txn, &req)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_switch",
            id: req
                .expected_switch_id
                .map(|u| u.to_string())
                .or(req.bmc_mac_address.map(|m| m.to_string()))
                .unwrap_or_default(),
        })?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let response = rpc::ExpectedSwitch::from(expected_switch);
    Ok(Response::new(response))
}

pub(crate) async fn get_all_expected_switches(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::ExpectedSwitchList>, Status> {
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let expected_switches = db_expected_switch::find_all(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let expected_switches: Vec<rpc::ExpectedSwitch> = expected_switches
        .into_iter()
        .map(rpc::ExpectedSwitch::from)
        .collect();

    Ok(Response::new(rpc::ExpectedSwitchList { expected_switches }))
}

pub(crate) async fn replace_all_expected_switches(
    api: &Api,
    request: Request<rpc::ExpectedSwitchList>,
) -> Result<Response<()>, Status> {
    let req = request.into_inner();

    let mut switches = Vec::with_capacity(req.expected_switches.len());

    for expected_switch in req.expected_switches {
        let switch: ExpectedSwitch =
            expected_switch
                .try_into()
                .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                    CarbideError::InvalidArgument(e.to_string())
                })?;

        validate_expected_switch(&switch)?;

        switches.push(switch);
    }

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    // Clear all existing expected switches
    db_expected_switch::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    // Add all new expected switches
    for switch in switches {
        db_expected_switch::create(&mut txn, switch)
            .await
            .map_err(CarbideError::from)?;
    }

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn delete_all_expected_switches(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<()>, Status> {
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    db_expected_switch::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn get_all_expected_switches_linked(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::LinkedExpectedSwitchList>, Status> {
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let linked_expected_switches = db_expected_switch::find_all_linked(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let linked_expected_switches: Vec<rpc::LinkedExpectedSwitch> = linked_expected_switches
        .into_iter()
        .map(rpc::LinkedExpectedSwitch::from)
        .collect();

    Ok(Response::new(rpc::LinkedExpectedSwitchList {
        expected_switches: linked_expected_switches,
    }))
}

// Utility method called by `explore`. Not a grpc handler.
// TODO(chet): Remove dead_code once wired up with the explorer.
pub(super) async fn query(
    api: &Api,
    mac: MacAddress,
) -> Result<Option<model::expected_switch::ExpectedSwitch>, CarbideError> {
    let mut txn = api.database_connection.begin().await.map_err(|e| {
        CarbideError::from(DatabaseError::new("begin find_many_by_bmc_mac_address", e))
    })?;

    let mut expected = db_expected_switch::find_many_by_bmc_mac_address(&mut txn, &[mac]).await?;

    txn.commit().await.map_err(|e| {
        CarbideError::from(DatabaseError::new("commit find_many_by_bmc_mac_address", e))
    })?;

    Ok(expected.remove(&mac))
}
