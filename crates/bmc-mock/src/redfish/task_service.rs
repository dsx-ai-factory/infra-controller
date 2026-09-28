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

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::Callbacks;
use crate::bmc_state::BmcState;
use crate::json::JsonExt;

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        "/redfish/v1/TaskService/Tasks/{task_id}",
        get(get_task::<C>),
    )
}

/// Return a task by ID.
///
/// Live tasks (created by upload handlers) are looked up in `UpdateServiceState`.
/// For any unknown ID (pruned tasks, legacy hard-coded IDs, DPU BFB task IDs that
/// get pruned on PowerOn) return a synthetic completed task.  Treating unknown IDs
/// as Completed is safe: carbide poll loops interpret Completed as "proceed" and
/// only hard-fail on explicit error responses.
async fn get_task<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(task_id): Path<String>,
) -> Response {
    if let Some(task_json) = state.update_service_state.find_task(&task_id) {
        return task_json.into_ok_response();
    }

    // Return synthetic Completed for any unknown ID — pruned firmware tasks and
    // DPU BFB task IDs both end up here; treating them as Completed is safe.
    json!({
        "@odata.id": format!("/redfish/v1/TaskService/Tasks/{task_id}"),
        "@odata.type": "#Task.v1_4_3.Task",
        "Id": task_id,
        "PercentComplete": 100,
        "StartTime": "2024-01-30T09:00:52+00:00",
        "TaskMonitor": format!("/redfish/v1/TaskService/Tasks/{task_id}/Monitor"),
        "TaskState": "Completed",
        "TaskStatus": "OK",
        "Messages": [{
            "MessageId": "Update.1.0.OperationTransitionedToJob",
            "Message": "Firmware staged; version will be applied after the next power-cycle.",
            "Severity": "OK"
        }]
    })
    .into_ok_response()
}
