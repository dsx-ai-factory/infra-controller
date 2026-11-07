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

//! `nv show cluster apps`: the NMX applications the switch hosts.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::NvueState;
use crate::json::JsonExt;

/// `GET /nvue_v1/cluster/apps`. Every configured application runs.
pub(super) async fn apps(State(state): State<Arc<NvueState>>) -> Response {
    let apps: Map<String, Value> = state
        .config
        .cluster_apps
        .iter()
        .map(|app| {
            (
                app.name.to_string(),
                json!({
                    "addition-info": app.addition_info,
                    "app-id": app.app_id,
                    "app-ver": app.app_version,
                    "capabilities": app.capabilities,
                    "components-ver": app.components_version,
                    "reason": "",
                    "status": "ok",
                }),
            )
        })
        .collect();
    Value::Object(apps).into_ok_response()
}
