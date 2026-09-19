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

//! Switch lifecycle: password rotation and factory reset.
//!
//! Both answer with a batch whose `job_id` is a parent with one child per
//! switch, matched or not, which the caller polls through `GetJobStatus`
//! with its children: it learns a switch's outcome from the job alone, so an
//! unmatched switch gets a child that fails. A simulated switch keeps no
//! credentials, and a factory reset clears the one thing the mock holds for
//! a switch, its fabric primary role, when the switch's job completes.

use crate::envelope::{matched_or_not, node_batch};
use crate::jobs::Effect;
use crate::{RmsMock, rms};

/// Begin a password change on the given switches.
pub(crate) async fn update_switch_system_password(
    mock: &RmsMock,
    request: tonic::Request<rms::UpdateSwitchSystemPasswordRequest>,
) -> Result<tonic::Response<rms::UpdateSwitchSystemPasswordResponse>, tonic::Status> {
    let req = request.get_ref();
    if req.username.is_empty() {
        return Err(tonic::Status::invalid_argument("username is required"));
    }
    if req.password.is_empty() {
        return Err(tonic::Status::invalid_argument("password is required"));
    }

    let inventory = mock.inventory.nodes();
    let refs = crate::resolve::resolve_nodes(&inventory, req.nodes.as_ref());
    let batch = mock.jobs.start_batch(&refs, None);

    Ok(tonic::Response::new(
        rms::UpdateSwitchSystemPasswordResponse {
            response: Some(node_batch(&matched_or_not(&refs), &batch.parent)),
        },
    ))
}

/// Begin a factory reset of the given switches. `domain` names TLS material
/// a simulated switch does not have, so it is accepted and ignored.
pub(crate) async fn batch_reset_switch_factory_default(
    mock: &RmsMock,
    request: tonic::Request<rms::BatchResetSwitchFactoryDefaultRequest>,
) -> Result<tonic::Response<rms::BatchResetSwitchFactoryDefaultResponse>, tonic::Status> {
    let inventory = mock.inventory.nodes();
    let refs = crate::resolve::resolve_nodes(&inventory, request.get_ref().nodes.as_ref());
    let batch = mock.jobs.start_batch(&refs, Some(Effect::ResetFabricRole));

    Ok(tonic::Response::new(
        rms::BatchResetSwitchFactoryDefaultResponse {
            response: Some(node_batch(&matched_or_not(&refs), &batch.parent)),
        },
    ))
}
