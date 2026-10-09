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
use db::ObjectFilter;
use db::network_devices::NetworkDeviceSearchConfig;
use itertools::Itertools;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};

pub(crate) async fn get_network_topology(
    api: &Api,
    request: Request<rpc::NetworkTopologyRequest>,
) -> Result<Response<rpc::NetworkTopologyData>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let query = match &req.id {
        Some(x) => ObjectFilter::One(x.as_str()),
        None => ObjectFilter::All,
    };

    let data = db::network_devices::get_topology(&mut txn, query).await?;

    txn.commit().await?;

    Ok(Response::new(data.into()))
}

pub(crate) async fn find_network_devices_by_device_ids(
    api: &Api,
    request: Request<rpc::NetworkDeviceIdList>,
) -> Result<Response<rpc::NetworkTopologyData>, Status> {
    log_request_data(&request);

    let request = request.into_inner(); // keep lifetime for this scope
    let network_device_ids: Vec<&str> = request
        .network_device_ids
        .iter()
        .map(|d| d.as_str())
        .collect();
    let search_config = NetworkDeviceSearchConfig::new(false);
    let network_devices = db::network_devices::find(
        &mut api.db_reader(),
        ObjectFilter::List(&network_device_ids),
        &search_config,
    )
    .await?;

    Ok(Response::new(rpc::NetworkTopologyData {
        network_devices: network_devices.into_iter().map_into().collect(),
    }))
}

pub(crate) async fn find_connected_devices_by_dpu_machine_ids(
    api: &Api,
    request: Request<::rpc::common::DpuMachineIdList>,
) -> Result<Response<rpc::ConnectedDeviceList>, Status> {
    log_request_data(&request);

    let dpu_ids = request
        .into_inner()
        .machine_ids
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();

    let connected_devices = db::network_devices::dpu_to_network_device_map::find_by_dpu_ids(
        &api.database_connection,
        &dpu_ids,
    )
    .await?;

    Ok(Response::new(rpc::ConnectedDeviceList {
        connected_devices: connected_devices.into_iter().map_into().collect(),
    }))
}
