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

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{Callbacks, http};

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        "/redfish/v1/TaskService/Tasks/{task_id}",
        get(get_task::<C>),
    )
}

/// Return a task by ID: a live task or one from the BMC's bounded history of
/// finished tasks. Any other ID (never issued by this BMC instance, issued
/// before an unsaved simulator restart, or evicted from the history) is 404.
/// Saved DPU BFB tasks retain their IDs across a restart. A poll
/// loop cannot read completion of work the mock did not do.
async fn get_task<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(task_id): Path<String>,
) -> Response {
    state
        .update_service_state
        .find_task(&task_id)
        .map(|task_json| task_json.into_ok_response())
        .unwrap_or_else(http::not_found)
}
