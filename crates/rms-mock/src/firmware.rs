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

//! Firmware objects and firmware jobs.
//!
//! The catalog is the configured object ids, and nothing is installed from
//! it. An apply is a batch with a child per matched node; an unmatched node
//! is a per-node failure with no child, which the caller reads as a failed
//! device.

use serde_json::{Map, Value};

use crate::envelope::{job_not_found, matched_or_not, node_batch, require_job_id};
use crate::jobs::JobState;
use crate::{RmsMock, rms};

/// The SOT firmware object document a request carries.
pub(crate) type SotDocument = Map<String, Value>;

/// Report the catalog. NICo lists it unfiltered and reads only the ids, so
/// the request's filters are not applied.
pub(crate) async fn list_firmware_objects(
    mock: &RmsMock,
    _request: tonic::Request<rms::ListFirmwareObjectsRequest>,
) -> Result<tonic::Response<rms::ListFirmwareObjectsResponse>, tonic::Status> {
    let objects = mock
        .config
        .firmware_object_ids
        .iter()
        .map(|id| rms::FirmwareObject {
            id: id.clone(),
            ..Default::default()
        })
        .collect();

    Ok(tonic::Response::new(rms::ListFirmwareObjectsResponse {
        objects,
    }))
}

/// Begin applying a firmware object to the given nodes.
///
/// The document is checked before any job is issued. Each matched node gets
/// a child job keyed by its own node id; an unmatched node is a per-node
/// failure with no child.
pub(crate) async fn apply_firmware_object(
    mock: &RmsMock,
    request: tonic::Request<rms::ApplyFirmwareObjectRequest>,
) -> Result<tonic::Response<rms::ApplyFirmwareObjectResponse>, tonic::Status> {
    let req = request.get_ref();
    let document = sot_document(&req.config_json)?;
    let inventory = mock.inventory.nodes();
    let refs = crate::resolve::resolve_nodes(&inventory, req.nodes.as_ref());

    let batch = mock
        .jobs
        .start_batch(refs.iter().filter(|r| r.matched()), None);
    let jobs = batch
        .children
        .into_iter()
        .map(|(node_id, job_id)| rms::NodeFirmwareJobInfo {
            node_id: node_id.to_owned(),
            job_id,
        })
        .collect();

    Ok(tonic::Response::new(rms::ApplyFirmwareObjectResponse {
        response: Some(node_batch(&matched_or_not(&refs), &batch.parent)),
        object_id: object_id(&mock.config.firmware_object_ids, &document),
        jobs,
    }))
}

/// Report progress of a firmware job, parent or child. A job this process
/// never issued is `RETURN_CODE_FAILURE`, as the proto specifies.
pub(crate) async fn get_firmware_job_status(
    mock: &RmsMock,
    request: tonic::Request<rms::GetFirmwareJobStatusRequest>,
) -> Result<tonic::Response<rms::GetFirmwareJobStatusResponse>, tonic::Status> {
    let job_id = &request.get_ref().job_id;
    require_job_id(job_id)?;
    if !mock.jobs.issued(job_id) {
        return Ok(tonic::Response::new(rms::GetFirmwareJobStatusResponse {
            status: rms::ReturnCode::Failure as i32,
            job_id: job_id.clone(),
            error_message: job_not_found(job_id),
            ..Default::default()
        }));
    }
    let status = mock.observe_job(job_id);

    Ok(tonic::Response::new(rms::GetFirmwareJobStatusResponse {
        status: rms::ReturnCode::Success as i32,
        job_id: job_id.clone(),
        job_state: job_state(status.state) as i32,
        state_description: describe(status.state).to_owned(),
        rack_id: status.rack_id,
        node_id: status.node_id,
        error_code: error_code(status.state) as i32,
        error_message: status.error_message,
        result_json: String::new(),
        created_at: None,
        updated_at: None,
    }))
}

/// The SOT document of a request: a JSON object, or `INVALID_ARGUMENT`.
/// Any object is accepted, since nothing is installed from it.
pub(crate) fn sot_document(config_json: &str) -> Result<SotDocument, tonic::Status> {
    if config_json.trim().is_empty() {
        return Err(tonic::Status::invalid_argument(
            "config_json is required: the SOT JSON names the firmware object to apply",
        ));
    }
    match serde_json::from_str(config_json) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(tonic::Status::invalid_argument(
            "config_json must be a JSON object describing one firmware object",
        )),
        Err(error) => Err(tonic::Status::invalid_argument(format!(
            "config_json is not valid JSON: {error}"
        ))),
    }
}

/// The object an apply is attributed to: the document's `Id`, or the first
/// configured object when the document names none.
pub(crate) fn object_id(catalog: &[String], document: &SotDocument) -> String {
    document
        .get("Id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| catalog.first().map(String::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// The `FirmwareJobState` for a job state; never `Unspecified`.
fn job_state(state: JobState) -> rms::FirmwareJobState {
    match state {
        JobState::Running => rms::FirmwareJobState::Running,
        JobState::Completed => rms::FirmwareJobState::Completed,
        JobState::Failed => rms::FirmwareJobState::Failed,
    }
}

fn describe(state: JobState) -> &'static str {
    match state {
        JobState::Running => "installing firmware",
        JobState::Completed => "firmware update completed",
        JobState::Failed => "firmware update failed",
    }
}

/// Set only once the job has an outcome.
fn error_code(state: JobState) -> rms::FirmwareUpdateError {
    match state {
        JobState::Running => rms::FirmwareUpdateError::Unspecified,
        JobState::Completed => rms::FirmwareUpdateError::Success,
        JobState::Failed => rms::FirmwareUpdateError::TaskFailed,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::{object_id, sot_document};

    #[test]
    fn the_object_id_comes_from_the_document_when_it_names_one() {
        let catalog = vec!["fw-first".to_string(), "fw-second".to_string()];
        check_values(
            [
                Check {
                    scenario: "the document's id",
                    input: r#"{"Id":"fw-from-sot","Artifacts":[]}"#,
                    expect: "fw-from-sot".to_string(),
                },
                Check {
                    scenario: "no id falls back to the first configured object",
                    input: r#"{"Artifacts":[]}"#,
                    expect: "fw-first".to_string(),
                },
                Check {
                    scenario: "a blank id falls back too",
                    input: r#"{"Id":"  "}"#,
                    expect: "fw-first".to_string(),
                },
            ],
            |config_json| object_id(&catalog, &sot_document(config_json).unwrap()),
        );
        assert_eq!(object_id(&[], &sot_document("{}").unwrap()), "");
    }

    #[test]
    fn absent_or_malformed_documents_are_invalid_arguments() {
        for bad in ["", "   ", "not json", "[]", "\"fw\"", "42"] {
            let status = sot_document(bad).expect_err(bad);
            assert_eq!(status.code(), tonic::Code::InvalidArgument, "{bad:?}");
            assert!(
                status.message().contains("config_json"),
                "{bad:?}: {}",
                status.message()
            );
        }
    }
}
