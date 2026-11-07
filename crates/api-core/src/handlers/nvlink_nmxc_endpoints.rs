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
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

pub(crate) async fn list_nvlink_nmxc_endpoints(
    api: &Api,
    request: Request<()>,
) -> Result<Response<rpc::NvlinkNmxcEndpointList>, Status> {
    log_request_data(&request);
    let mut txn = api.txn_begin().await?;
    let rows = db::nvlink_nmxc_endpoints::find_all(&mut txn).await?;
    txn.commit().await?;
    Ok(Response::new(rpc::NvlinkNmxcEndpointList {
        entries: rows.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn create_nvlink_nmxc_endpoint(
    api: &Api,
    request: Request<rpc::NvlinkNmxcEndpoint>,
) -> Result<Response<rpc::NvlinkNmxcEndpoint>, Status> {
    log_request_data(&request);
    let inner = request.into_inner();
    if inner.chassis_serial.is_empty() {
        return Err(Status::invalid_argument("chassis_serial must not be empty"));
    }
    if inner.endpoint.is_empty() {
        return Err(Status::invalid_argument("endpoint must not be empty"));
    }
    let mut txn = api.txn_begin().await?;
    let row = db::nvlink_nmxc_endpoints::create(&mut txn, &inner.chassis_serial, &inner.endpoint)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(row.into()))
}

pub(crate) async fn update_nvlink_nmxc_endpoint(
    api: &Api,
    request: Request<rpc::NvlinkNmxcEndpoint>,
) -> Result<Response<rpc::NvlinkNmxcEndpoint>, Status> {
    log_request_data(&request);
    let inner = request.into_inner();
    if inner.chassis_serial.is_empty() {
        return Err(Status::invalid_argument("chassis_serial must not be empty"));
    }
    if inner.endpoint.is_empty() {
        return Err(Status::invalid_argument("endpoint must not be empty"));
    }
    let mut txn = api.txn_begin().await?;
    let updated =
        db::nvlink_nmxc_endpoints::update(&mut txn, &inner.chassis_serial, &inner.endpoint).await?;
    txn.commit().await?;
    let Some(row) = updated else {
        return Err(Status::not_found(
            "nvlink_nmxc_endpoints: no row for chassis_serial",
        ));
    };
    Ok(Response::new(row.into()))
}

pub(crate) async fn delete_nvlink_nmxc_endpoint(
    api: &Api,
    request: Request<rpc::DeleteNvlinkNmxcEndpointRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let inner = request.into_inner();
    if inner.chassis_serial.is_empty() {
        return Err(Status::invalid_argument("chassis_serial must not be empty"));
    }
    let mut txn = api.txn_begin().await?;
    let deleted = db::nvlink_nmxc_endpoints::delete(&mut txn, &inner.chassis_serial).await?;
    txn.commit().await?;
    if !deleted {
        return Err(Status::not_found(
            "nvlink_nmxc_endpoints: no row for chassis_serial",
        ));
    }
    Ok(Response::new(()))
}
