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

//! `nv show sdn partition`: the NVLink partitions the switch's fabric manager
//! reports.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::NvueState;
use crate::json::JsonExt;

/// `GET /nvue_v1/sdn/partition`. Every configured partition is healthy. NVOS
/// renders `num-gpus` as a string and `mcast-limit` as a number.
pub(super) async fn partitions(State(state): State<Arc<NvueState>>) -> Response {
    let partitions: Map<String, Value> = state
        .config
        .partitions
        .iter()
        .map(|partition| {
            (
                partition.id.clone(),
                json!({
                    "health": "healthy",
                    "mcast-limit": 1024,
                    "name": partition.name,
                    "num-gpus": partition.num_gpus.to_string(),
                    "partition-type": partition.partition_type,
                    "resiliency-mode": partition.resiliency_mode,
                }),
            )
        })
        .collect();
    Value::Object(partitions).into_ok_response()
}
