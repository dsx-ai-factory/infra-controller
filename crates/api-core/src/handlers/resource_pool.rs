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

use std::collections::HashMap;

use ::rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

pub(crate) async fn grow(
    api: &Api,
    request: Request<rpc::GrowResourcePoolRequest>,
) -> Result<Response<rpc::GrowResourcePoolResponse>, Status> {
    crate::api::log_request_data(&request);

    let toml_text = request.into_inner().text;

    let mut txn = api.txn_begin().await?;

    let mut pools = HashMap::new();
    let table: toml::Table = toml_text
        .parse()
        .map_err(|e: toml::de::Error| CarbideError::InvalidArgument(e.to_string()))?;
    for (name, def) in table {
        let d: model::resource_pool::ResourcePoolDef = def
            .try_into()
            .map_err(|e: toml::de::Error| CarbideError::InvalidArgument(e.to_string()))?;
        pools.insert(name, d);
    }
    let reconcile_dpu_loopbacks = pools.contains_key(model::resource_pool::common::LOOPBACK_IP_V6);
    use db::resource_pool::DefineResourcePoolError as DE;
    match db::resource_pool::define_all_from(&mut txn, &pools).await {
        Ok(()) => {
            txn.commit().await?;
            if reconcile_dpu_loopbacks {
                // The backfill opens one transaction per DPU, so publish the
                // new pool rows before it starts reconciliation.
                db::machine::update_dpu_loopback_ips_v6(
                    &api.database_connection,
                    &api.common_pools,
                )
                .await
                .map_err(CarbideError::from)?;
            }
            Ok(Response::new(rpc::GrowResourcePoolResponse {}))
        }
        Err(DE::InvalidArgument(msg)) => Err(CarbideError::InvalidArgument(msg).into()),
        Err(DE::ResourcePoolError(msg)) => Err(CarbideError::Internal {
            message: msg.to_string(),
        }
        .into()),
        Err(DE::DatabaseError(err)) => Err(CarbideError::Internal {
            message: err.to_string(),
        }
        .into()),
        Err(err @ DE::TooBig(_, _)) => Err(tonic::Status::out_of_range(err.to_string())),
    }
}

pub(crate) async fn list(
    api: &Api,
    request: Request<rpc::ListResourcePoolsRequest>,
) -> Result<tonic::Response<rpc::ResourcePools>, tonic::Status> {
    crate::api::log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let snapshot = db::resource_pool::all(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::ResourcePools {
        pools: snapshot.into_iter().map(|s| s.into()).collect(),
    }))
}
