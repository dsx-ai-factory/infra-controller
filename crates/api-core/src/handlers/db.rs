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

use crate::api::{Api, log_request_data};

pub(crate) async fn trim_table(
    api: &Api,
    request: Request<rpc::TrimTableRequest>,
) -> Result<Response<rpc::TrimTableResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let target: model::trim_table::TrimTableTarget = request.get_ref().target().into();
    let total_deleted =
        db::trim_table::trim_table(&mut txn, target, request.get_ref().keep_entries).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::TrimTableResponse {
        total_deleted: total_deleted.to_string(),
    }))
}
