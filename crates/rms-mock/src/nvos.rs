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

//! Switch system images (NVOS).
//!
//! An apply is a batch with a child per switch, matched or not: the caller
//! learns a switch's outcome from its job alone, so an unmatched switch gets
//! a child that fails. Nothing is downloaded or pushed.

use crate::envelope::{job_not_found, matched_or_not, node_batch, require_job_id};
use crate::jobs::{JobState, JobStatus};
use crate::{RmsMock, rms};

/// Begin applying a switch system image to the given switches.
pub(crate) async fn apply_switch_system_image(
    mock: &RmsMock,
    request: tonic::Request<rms::ApplySwitchSystemImageRequest>,
) -> Result<tonic::Response<rms::ApplySwitchSystemImageResponse>, tonic::Status> {
    let req = request.get_ref();
    let document = crate::firmware::sot_document(&req.config_json)?;
    let inventory = mock.inventory.nodes();
    let refs = crate::resolve::resolve_nodes(&inventory, req.nodes.as_ref());

    let batch = mock.jobs.start_batch(&refs, None);
    let jobs = batch
        .children
        .into_iter()
        .map(|(node_id, job_id)| rms::SwitchSystemImageUpdateJobInfo {
            node_id: node_id.to_owned(),
            job_id,
        })
        .collect();
    let object_id = crate::firmware::object_id(&mock.config.firmware_object_ids, &document);

    Ok(tonic::Response::new(rms::ApplySwitchSystemImageResponse {
        response: Some(node_batch(&matched_or_not(&refs), &batch.parent)),
        // Nothing is downloaded, so the name only has to be traceable.
        image_filename: format!("{object_id}-nvos.bin"),
        object_id,
        jobs,
    }))
}

/// Report progress of a switch system image job, parent or child. A job this
/// process never issued is `RETURN_CODE_FAILURE`, as the proto specifies.
pub(crate) async fn get_switch_system_image_job_status(
    mock: &RmsMock,
    request: tonic::Request<rms::GetSwitchSystemImageJobStatusRequest>,
) -> Result<tonic::Response<rms::GetSwitchSystemImageJobStatusResponse>, tonic::Status> {
    let job_id = &request.get_ref().job_id;
    require_job_id(job_id)?;
    if !mock.jobs.issued(job_id) {
        return Ok(tonic::Response::new(
            rms::GetSwitchSystemImageJobStatusResponse {
                status: rms::ReturnCode::Failure as i32,
                job_id: job_id.clone(),
                error_message: job_not_found(job_id),
                ..Default::default()
            },
        ));
    }
    let status = mock.observe_job(job_id);

    Ok(tonic::Response::new(
        rms::GetSwitchSystemImageJobStatusResponse {
            status: rms::ReturnCode::Success as i32,
            job_id: job_id.clone(),
            state: status.state.as_wire_str().to_owned(),
            message: progress(&status),
            rack_id: status.rack_id,
            node_id: status.node_id,
            error_message: status.error_message,
            result_json: String::new(),
            created_at: None,
            updated_at: None,
        },
    ))
}

/// The progress line: a batch summary for a parent, the state for a child.
fn progress(status: &JobStatus) -> String {
    if status.children.is_empty() {
        return format!("switch system image update {}", status.state.as_wire_str());
    }
    let count = |state: JobState| status.children.iter().filter(|c| c.state == state).count();
    let total = status.children.len();
    match count(JobState::Failed) {
        0 => format!(
            "{} of {total} switches completed",
            count(JobState::Completed)
        ),
        failed => format!(
            "{} of {total} switches completed, {failed} failed",
            count(JobState::Completed)
        ),
    }
}
