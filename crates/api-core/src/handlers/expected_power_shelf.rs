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

use ::rpc::forge as rpc;
use carbide_instrument::emit;
use db::{DatabaseError, expected_power_shelf as db_expected_power_shelf};
use mac_address::MacAddress;
use model::expected_power_shelf::{ExpectedPowerShelf, ExpectedPowerShelfRequest};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data_redacted};
use crate::handlers::expected_component_patch::{
    ExpectedComponent, UpdateField, UpdateMask, parse_bmc_ip, required_id, required_value,
    validate_bmc_mac,
};
use crate::handlers::machine_interface_address::update_preallocated_machine_interface;
use crate::handlers::static_address_metrics::{
    PreallocationSuccess, StaticAddressPreallocationCompleted,
};

pub(crate) async fn add_expected_power_shelf(
    api: &Api,
    request: Request<rpc::ExpectedPowerShelf>,
) -> Result<Response<()>, Status> {
    let rpc_power_shelf = request.into_inner();
    let power_shelf: ExpectedPowerShelf =
        rpc_power_shelf
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

    db_expected_power_shelf::create(&mut txn, power_shelf)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn delete_expected_power_shelf(
    api: &Api,
    request: Request<rpc::ExpectedPowerShelfRequest>,
) -> Result<Response<()>, Status> {
    let req: ExpectedPowerShelfRequest =
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

    db_expected_power_shelf::delete(&mut txn, &req)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn update_expected_power_shelf(
    api: &Api,
    request: Request<rpc::ExpectedPowerShelf>,
) -> Result<Response<()>, Status> {
    let power_shelf: ExpectedPowerShelf =
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

    let preallocation = update_power_shelf_in_transaction(
        &mut txn,
        &power_shelf,
        api.runtime_config.retained_boot_interface_window,
    )
    .await?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    if let Some(outcome) = preallocation {
        emit(StaticAddressPreallocationCompleted::from(outcome));
    }

    Ok(Response::new(()))
}

pub(crate) async fn patch_expected_power_shelf(
    api: &Api,
    request: Request<rpc::PatchExpectedPowerShelfRequest>,
) -> Result<Response<()>, Status> {
    let request = request.into_inner();
    let fields = UpdateMask::parse(
        request.update_mask.map(|mask| mask.paths),
        ExpectedComponent::PowerShelf,
    )?;
    let mut patch = request.expected_power_shelf.ok_or_else(|| {
        CarbideError::InvalidArgument("expected_power_shelf is required".to_string())
    })?;
    let expected_power_shelf_id = required_id(
        patch.expected_power_shelf_id.take(),
        "expected_power_shelf_id",
    )?;
    fields.validate_bmc_credentials(&patch.bmc_username, &patch.bmc_password)?;
    log_request_data_redacted(format!(
        "expected_power_shelf_id: {expected_power_shelf_id}"
    ));

    let mut txn = api.txn_begin().await?;
    let mut power_shelf =
        db_expected_power_shelf::find_by_id_for_update(&mut txn, expected_power_shelf_id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "expected_power_shelf",
                id: expected_power_shelf_id.to_string(),
            })?;
    validate_bmc_mac(&patch.bmc_mac_address, power_shelf.bmc_mac_address)?;
    if fields.is_empty() {
        txn.commit().await?;
        return Ok(Response::new(()));
    }
    if fields.contains(UpdateField::BmcUsername) {
        power_shelf.bmc_username = patch.bmc_username;
        power_shelf.bmc_password = patch.bmc_password;
    }
    if fields.contains(UpdateField::ShelfSerialNumber) {
        power_shelf.serial_number = patch.shelf_serial_number;
    }
    if fields.contains(UpdateField::BmcIpAddress) {
        power_shelf.bmc_ip_address = parse_bmc_ip(&patch.bmc_ip_address)?;
    }
    if fields.contains(UpdateField::BmcRetainCredentials) {
        power_shelf.bmc_retain_credentials = Some(required_value(
            patch.bmc_retain_credentials,
            "bmc_retain_credentials",
        )?);
    }
    if fields.contains(UpdateField::RackId) {
        power_shelf.rack_id = patch.rack_id;
    }
    fields.update_metadata(patch.metadata, &mut power_shelf.metadata)?;
    let preallocation = update_power_shelf_in_transaction(
        &mut txn,
        &power_shelf,
        api.runtime_config.retained_boot_interface_window,
    )
    .await?;
    txn.commit().await?;
    if let Some(preallocation) = preallocation {
        emit(StaticAddressPreallocationCompleted::from(preallocation));
    }
    Ok(Response::new(()))
}

async fn update_power_shelf_in_transaction(
    txn: &mut sqlx::PgConnection,
    power_shelf: &ExpectedPowerShelf,
    retained_window: Option<chrono::Duration>,
) -> Result<Option<PreallocationSuccess>, CarbideError> {
    // Lock the inventory before allocating addresses so PATCH and legacy
    // updates cannot wait on each other's locks.
    db_expected_power_shelf::update(txn, power_shelf).await?;

    let preallocation = if let Some(bmc_ip) = power_shelf.bmc_ip_address {
        Some(
            update_preallocated_machine_interface(
                &mut *txn,
                power_shelf.bmc_mac_address,
                bmc_ip,
                retained_window,
            )
            .await?,
        )
    } else {
        None
    };
    Ok(preallocation)
}

pub(crate) async fn get_expected_power_shelf(
    api: &Api,
    request: Request<rpc::ExpectedPowerShelfRequest>,
) -> Result<Response<rpc::ExpectedPowerShelf>, Status> {
    let req: ExpectedPowerShelfRequest =
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

    let expected_power_shelf = db_expected_power_shelf::find(&mut txn, &req)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_power_shelf",
            id: req
                .expected_power_shelf_id
                .map(|u| u.to_string())
                .or(req.bmc_mac_address.map(|m| m.to_string()))
                .unwrap_or_default(),
        })?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let response = rpc::ExpectedPowerShelf::from(expected_power_shelf);
    Ok(Response::new(response))
}

pub(crate) async fn get_all_expected_power_shelves(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::ExpectedPowerShelfList>, Status> {
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let expected_power_shelves = db_expected_power_shelf::find_all(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let expected_power_shelves: Vec<rpc::ExpectedPowerShelf> = expected_power_shelves
        .into_iter()
        .map(rpc::ExpectedPowerShelf::from)
        .collect();

    Ok(Response::new(rpc::ExpectedPowerShelfList {
        expected_power_shelves,
    }))
}

pub(crate) async fn replace_all_expected_power_shelves(
    api: &Api,
    request: Request<rpc::ExpectedPowerShelfList>,
) -> Result<Response<()>, Status> {
    let req = request.into_inner();

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    // Clear all existing expected power shelves
    db_expected_power_shelf::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    // Add all new expected power shelves
    for rpc_power_shelf in req.expected_power_shelves {
        let power_shelf: ExpectedPowerShelf =
            rpc_power_shelf
                .try_into()
                .map_err(|e: ::rpc::errors::RpcDataConversionError| {
                    CarbideError::InvalidArgument(e.to_string())
                })?;
        db_expected_power_shelf::create(&mut txn, power_shelf)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("Failed to create expected power shelf: {}", e),
            })?;
    }

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn delete_all_expected_power_shelves(
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

    db_expected_power_shelf::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(()))
}

pub(crate) async fn get_all_expected_power_shelves_linked(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::LinkedExpectedPowerShelfList>, Status> {
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let linked_expected_power_shelves = db_expected_power_shelf::find_all_linked(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let linked_expected_power_shelves: Vec<rpc::LinkedExpectedPowerShelf> =
        linked_expected_power_shelves
            .into_iter()
            .map(rpc::LinkedExpectedPowerShelf::from)
            .collect();

    Ok(Response::new(rpc::LinkedExpectedPowerShelfList {
        expected_power_shelves: linked_expected_power_shelves,
    }))
}

// Utility method called by `explore`. Not a grpc handler.
// TODO(chet): Remove dead_code once the exploration is wired up.
pub(super) async fn query(
    api: &Api,
    mac: MacAddress,
) -> Result<Option<model::expected_power_shelf::ExpectedPowerShelf>, CarbideError> {
    let mut txn = api.database_connection.begin().await.map_err(|e| {
        CarbideError::from(DatabaseError::new("begin find_many_by_bmc_mac_address", e))
    })?;

    let mut expected =
        db_expected_power_shelf::find_many_by_bmc_mac_address(&mut txn, &[mac]).await?;

    txn.commit().await.map_err(|e| {
        CarbideError::from(DatabaseError::new("commit find_many_by_bmc_mac_address", e))
    })?;

    Ok(expected.remove(&mac))
}
