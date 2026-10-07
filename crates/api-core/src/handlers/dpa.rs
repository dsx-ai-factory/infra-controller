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

use model::dpa_interface::{DpaInterface, NewDpaInterface};
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::{CarbideError, CarbideResult};

// This is called from the grpc interface and is mainly for debugging purposes.
pub(crate) async fn create(
    api: &Api,
    request: Request<::rpc::forge::DpaInterfaceCreationRequest>,
) -> Result<Response<::rpc::forge::DpaInterface>, Status> {
    if !api.runtime_config.is_ewethers_enabled() {
        return Err(CarbideError::InvalidArgument(
            "CreateDpaInterface cannot be done as ewethers_enabled is false".to_string(),
        )
        .into());
    }
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let new_dpa =
        db::dpa_interface::persist(NewDpaInterface::try_from(request.into_inner())?, &mut txn)
            .await?;

    let dpa_out: rpc::forge::DpaInterface = new_dpa.into();

    txn.commit().await?;

    Ok(Response::new(dpa_out))
}

/// ensure creates an interface if one doesn't already exist for the given
/// (machine_id, mac_address), or returns the existing one. Idempotent.
pub(crate) async fn ensure(
    api: &Api,
    request: Request<::rpc::forge::DpaInterfaceCreationRequest>,
) -> Result<Response<::rpc::forge::DpaInterface>, Status> {
    if !api.runtime_config.is_ewethers_enabled() {
        return Err(CarbideError::InvalidArgument(
            "EnsureDpaInterface cannot be done as ewethers_enabled is false".to_string(),
        )
        .into());
    }
    log_request_data(&request);

    let new_interface = NewDpaInterface::try_from(request.into_inner())?;
    let interface = ensure_interface(api, new_interface).await?;
    let response: rpc::forge::DpaInterface = interface.into();
    Ok(Response::new(response))
}

/// ensure_interface is the internal helper used by
/// publish_mlx_device_report and the public ensure handler.
pub(super) async fn ensure_interface(
    api: &Api,
    new_interface: NewDpaInterface,
) -> CarbideResult<DpaInterface> {
    let mut txn = api.txn_begin().await?;
    let interface = db::dpa_interface::ensure(new_interface, &mut txn).await?;
    txn.commit().await?;
    Ok(interface)
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<::rpc::forge::DpaInterfaceDeletionRequest>,
) -> Result<Response<::rpc::forge::DpaInterfaceDeletionResult>, Status> {
    if !api.runtime_config.is_ewethers_enabled() {
        return Err(CarbideError::InvalidArgument(
            "DeleteDpaInterface cannot be done as ewethers_enabled is false".to_string(),
        )
        .into());
    }
    log_request_data(&request);

    let req = request.into_inner();

    let id = req.id.ok_or(CarbideError::InvalidArgument(
        "at least one ID must be provided to delete dpa interface".to_string(),
    ))?;

    // Prepare our txn to grab the NetworkSecurityGroups from the DB
    let mut txn = api.txn_begin().await?;

    let dpa_ifs_int = db::dpa_interface::find_by_ids(&mut txn, &[id], false).await?;

    let dpa_if_int = match dpa_ifs_int.len() {
        1 => dpa_ifs_int[0].clone(),
        _ => {
            return Err(CarbideError::InvalidArgument(
                "ID could not be used to locate interface".to_string(),
            )
            .into());
        }
    };

    db::dpa_interface::delete(dpa_if_int, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(::rpc::forge::DpaInterfaceDeletionResult {}))
}

pub(crate) async fn get_all_ids(
    api: &Api,
    request: Request<()>,
) -> Result<Response<::rpc::forge::DpaInterfaceIdList>, Status> {
    log_request_data(&request);

    let ids = db::dpa_interface::find_ids(&api.database_connection).await?;

    Ok(Response::new(::rpc::forge::DpaInterfaceIdList { ids }))
}

pub(crate) async fn find_dpa_interfaces_by_ids(
    api: &Api,
    request: Request<::rpc::forge::DpaInterfacesByIdsRequest>,
) -> Result<Response<::rpc::forge::DpaInterfaceList>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be submitted to find_dpa_interfaces_by_ids"
        ))
        .into());
    }

    if req.ids.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one ID must be provided to find_dpa_interfaces_by_ids".to_string(),
        )
        .into());
    }

    let dpa_ifs_int =
        db::dpa_interface::find_by_ids(&api.database_connection, &req.ids, req.include_history)
            .await?;

    let rpc_dpa_ifs = dpa_ifs_int
        .into_iter()
        .map(|i| i.into())
        .collect::<Vec<rpc::forge::DpaInterface>>();

    Ok(Response::new(rpc::forge::DpaInterfaceList {
        interfaces: rpc_dpa_ifs,
    }))
}
