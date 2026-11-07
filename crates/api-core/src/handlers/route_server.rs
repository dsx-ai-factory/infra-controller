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

use std::net::IpAddr;
use std::str::FromStr;

use ::rpc::forge as rpc;
use tonic::Status;

use crate::api::{Api, log_request_data};
use crate::{CarbideError, CarbideResult};

// get returns all RouteServer entries, including the
// address and source_type.
pub(crate) async fn get(
    api: &Api,
    request: tonic::Request<()>,
) -> Result<tonic::Response<rpc::RouteServerEntries>, Status> {
    log_request_data(&request);

    let route_servers = db::route_servers::get(&api.database_connection).await?;

    Ok(tonic::Response::new(rpc::RouteServerEntries {
        route_servers: route_servers.into_iter().map(Into::into).collect(),
    }))
}

// add will add a new RouteServer entries. Since this comes in
// via the API, all new entries here will be tagged with the
// admin_api source type.
pub(crate) async fn add(
    api: &Api,
    request: tonic::Request<rpc::RouteServers>,
) -> Result<tonic::Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let route_servers = get_route_server_ip_addrs(&request.route_servers)?;
    let source_type: rpc::RouteServerSourceType = request
        .source_type
        .try_into()
        .map_err(|_| CarbideError::InvalidArgument("source_type".to_string()))?;

    let mut txn = api.txn_begin().await?;
    db::route_servers::add(&mut txn, &route_servers, source_type.into()).await?;
    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

// remove will remove RouteServer entries matching the given
// addresses and source_type supplied in the request.
pub(crate) async fn remove(
    api: &Api,
    request: tonic::Request<rpc::RouteServers>,
) -> Result<tonic::Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let route_servers = get_route_server_ip_addrs(&request.route_servers)?;
    let source_type: rpc::RouteServerSourceType = request
        .source_type
        .try_into()
        .map_err(|_| CarbideError::InvalidArgument("source_type".to_string()))?;

    let mut txn = api.txn_begin().await?;
    let deleted = db::route_servers::remove(&mut txn, &route_servers, source_type.into()).await?;
    if !route_servers.is_empty() && deleted == 0 {
        return Err(Status::not_found(
            "no route servers found matching the given addresses for the requested \
             source_type; config-file-sourced entries must be targeted with \
             source_type config_file",
        ));
    }
    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

// replace will replace the existing route server addresses
// for the given source_type with provided list of route server
// addresses. Since this comes in via the API, all new entries
// here will be tagged with the admin_api source type.
pub(crate) async fn replace(
    api: &Api,
    request: tonic::Request<rpc::RouteServers>,
) -> Result<tonic::Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let route_servers = get_route_server_ip_addrs(&request.route_servers)?;
    let source_type: rpc::RouteServerSourceType = request
        .source_type
        .try_into()
        .map_err(|_| CarbideError::InvalidArgument("source_type".to_string()))?;

    let mut txn = api.txn_begin().await?;
    db::route_servers::replace(&mut txn, &route_servers, source_type.into()).await?;
    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

// get_route_server_ip_addrs is a little helper to
// pluck out the route server addresses from an
// incoming request and convert them into IpAddrs.
fn get_route_server_ip_addrs(route_servers: &[String]) -> CarbideResult<Vec<IpAddr>> {
    route_servers
        .iter()
        .map(|rs| IpAddr::from_str(rs))
        .collect::<Result<Vec<IpAddr>, _>>()
        .map_err(CarbideError::AddressParseError)
}
