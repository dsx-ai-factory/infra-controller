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

//! Wire-level tests for the firmware RPCs, driven through a real gRPC client
//! like `wire_grpc.rs`: the shape of what comes back is what NICo's rack
//! firmware upgrade reads.

mod common;

use common::{a_switch, a_tray, failing_nodes, node_info, serve_with, serve_with_config};
use librms::protos::rack_manager as rms;
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use rms_mock::RmsMockConfig;

type Client = RackManagerClient<tonic::transport::Channel>;

/// The apply request NICo's rack maintenance flow sends.
fn apply_request(nodes: Vec<rms::NodeInfo>, config_json: &str) -> rms::ApplyFirmwareObjectRequest {
    rms::ApplyFirmwareObjectRequest {
        rack_id: "rack-001".to_string(),
        config_json: config_json.to_string(),
        access_token: Some("NOAUTH".to_string()),
        firmware_type: "prod".to_string(),
        hardware_type: "any".to_string(),
        nodes: Some(rms::NodeSet { nodes }),
        force_update: false,
        component_filters: Default::default(),
        node_descriptor_component_filters: Vec::new(),
    }
}

fn state_of(response: &rms::GetFirmwareJobStatusResponse) -> rms::FirmwareJobState {
    rms::FirmwareJobState::try_from(response.job_state)
        .expect("a state outside the enum is read as an unknown outcome")
}

/// Poll a firmware job until it is terminal, returning every state seen and
/// the final response. Every poll must report a real state: an unspecified
/// one is read as an unknown outcome.
async fn poll_firmware_job(
    client: &mut Client,
    job_id: &str,
) -> (
    Vec<rms::FirmwareJobState>,
    rms::GetFirmwareJobStatusResponse,
) {
    let mut seen = Vec::new();
    for _ in 0..5 {
        let status = client
            .get_firmware_job_status(rms::GetFirmwareJobStatusRequest {
                job_id: job_id.to_string(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.status, rms::ReturnCode::Success as i32);
        assert_eq!(status.job_id, job_id, "the caller correlates by echoed id");
        let state = state_of(&status);
        assert_ne!(state, rms::FirmwareJobState::Unspecified);
        seen.push(state);
        if matches!(
            state,
            rms::FirmwareJobState::Completed | rms::FirmwareJobState::Failed
        ) {
            return (seen, status);
        }
    }
    panic!("job {job_id} never reached a terminal state; polled states were {seen:?}");
}

/// NICo lists the catalog unfiltered and offers the ids as the versions a
/// device can be brought to: the default catalog has one, and a configured
/// catalog is listed as given.
#[tokio::test]
async fn firmware_object_ids_are_listed_from_configuration() {
    let listed = |url: String| async move {
        RackManagerClient::connect(url)
            .await
            .unwrap()
            .list_firmware_objects(rms::ListFirmwareObjectsRequest {
                only_available: false,
                hardware_type: String::new(),
            })
            .await
            .unwrap()
            .into_inner()
            .objects
            .into_iter()
            .map(|o| o.id)
            .collect::<Vec<_>>()
    };

    let default = listed(serve_with(Vec::new()).await).await;
    assert_eq!(default.len(), 1, "the default catalog has one entry");
    assert!(!default[0].is_empty());

    let configured = RmsMockConfig {
        firmware_object_ids: vec!["fw-1.1.0".to_string(), "fw-1.2.0".to_string()],
        ..RmsMockConfig::default()
    };
    assert_eq!(
        listed(serve_with_config(Vec::new(), configured).await).await,
        ["fw-1.1.0", "fw-1.2.0"]
    );
}

/// The response the rack firmware upgrade needs before it polls: a batch
/// whose status, node results, and stats agree, the parent job in the batch,
/// a child per node in `jobs` keyed by the caller's node id, and the object
/// id it stores as the rack's firmware. The children are then polled to
/// completion, and the parent alone drives them.
#[tokio::test]
async fn apply_issues_a_parent_and_per_node_children_that_complete() {
    let url = serve_with(vec![a_switch(), a_tray()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let listed = client
        .list_firmware_objects(rms::ListFirmwareObjectsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .objects;

    let started = client
        .apply_firmware_object(apply_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("tray-12", "02:00:AB:CD:12:34"),
            ],
            &format!(r#"{{"Id":"{}"}}"#, listed[0].id),
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        started.object_id, listed[0].id,
        "stored as the rack's firmware id"
    );
    let batch = started
        .response
        .expect("a missing batch is read as failure");
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);
    let stats = batch.stats.as_ref().unwrap();
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (2, 2, 0)
    );
    assert!(
        batch
            .node_results
            .iter()
            .all(|r| r.status == rms::ReturnCode::Success as i32 && r.error_message.is_empty()),
        "a non-success node result, or a message on one, is read as a failed device"
    );
    let parent = batch.job_id;
    assert!(!parent.is_empty(), "the parent id is the rack's job id");

    let child = |node_id: &str| {
        started
            .jobs
            .iter()
            .find(|j| j.node_id == node_id)
            .unwrap_or_else(|| panic!("no child job for {node_id}: a device without one is failed"))
            .job_id
            .clone()
    };
    let (switch_job, tray_job) = (child("switch-7"), child("tray-12"));
    assert_ne!(switch_job, tray_job);
    assert!(
        switch_job != parent && tray_job != parent,
        "children are jobs of their own"
    );

    // A child reports its own node and rack, through states the caller maps.
    let (seen, last) = poll_firmware_job(&mut client, &switch_job).await;
    assert_eq!(
        seen,
        [
            rms::FirmwareJobState::Running,
            rms::FirmwareJobState::Completed
        ]
    );
    assert_eq!(
        (last.node_id.as_str(), last.rack_id.as_str()),
        ("switch-7", "rack-001")
    );
    assert!(last.error_message.is_empty());
    assert!(!last.state_description.is_empty());

    // The same batch is visible through the generic job-status RPC, the
    // completed child included.
    let generic = client
        .get_job_status(rms::GetJobStatusRequest {
            job_id: parent.clone(),
            include_child_job_states: true,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(generic.job_states.len(), 3, "parent plus two children");
    assert_eq!(generic.job_states[0].job_id, parent);
    assert_eq!(generic.job_states[0].child_job_ids, [switch_job, tray_job]);

    // Polling only the parent drives the remaining child to completion; the
    // parent reports no node of its own.
    let (_, parent_status) = poll_firmware_job(&mut client, &parent).await;
    assert_eq!(state_of(&parent_status), rms::FirmwareJobState::Completed);
    assert_eq!(
        (
            parent_status.node_id.as_str(),
            parent_status.rack_id.as_str()
        ),
        ("", "rack-001")
    );
}

/// A job selected to fail runs like any other, then reports failed with a
/// reason naming the node and an error code, and its parent fails with it.
#[tokio::test]
async fn a_faulted_firmware_child_runs_then_fails_and_fails_its_parent() {
    let url = serve_with_config(vec![a_switch(), a_tray()], failing_nodes(&["tray-12"])).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let started = client
        .apply_firmware_object(apply_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("tray-12", "02:00:AB:CD:12:34"),
            ],
            r#"{"Id":"fw-sot"}"#,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(started.object_id, "fw-sot", "the document's id wins");
    let batch = started.response.unwrap();
    assert_eq!(
        batch.status,
        rms::ReturnCode::Success as i32,
        "the failure is the job's outcome"
    );
    let job_of = |node_id: &str| {
        started
            .jobs
            .iter()
            .find(|j| j.node_id == node_id)
            .unwrap()
            .job_id
            .clone()
    };

    let (seen, failed) = poll_firmware_job(&mut client, &job_of("tray-12")).await;
    assert_eq!(
        seen,
        [
            rms::FirmwareJobState::Running,
            rms::FirmwareJobState::Failed
        ]
    );
    assert!(
        failed.error_message.contains("tray-12"),
        "the failure names the node: {:?}",
        failed.error_message
    );
    assert_eq!(
        failed.error_code,
        rms::FirmwareUpdateError::TaskFailed as i32
    );
    assert_eq!(failed.node_id, "tray-12");

    let (_, parent) = poll_firmware_job(&mut client, &batch.job_id).await;
    assert_eq!(state_of(&parent), rms::FirmwareJobState::Failed);
    assert!(parent.error_message.contains("tray-12"));

    let (_, healthy) = poll_firmware_job(&mut client, &job_of("switch-7")).await;
    assert_eq!(state_of(&healthy), rms::FirmwareJobState::Completed);
    assert!(healthy.error_message.is_empty());
}

/// A node no device matches is a per-node failure at submission, in all
/// three places the caller checks, and gets no child job; the matched nodes
/// are unaffected and their parent still completes.
#[tokio::test]
async fn an_unmatched_node_is_a_per_node_failure_without_a_child_job() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let started = client
        .apply_firmware_object(apply_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("ghost", "02:00:00:00:00:99"),
            ],
            r#"{"Id":"fw-sot"}"#,
        ))
        .await
        .unwrap()
        .into_inner();

    let batch = started.response.unwrap();
    assert_eq!(batch.status, rms::ReturnCode::Failure as i32);
    assert!(batch.message.contains("ghost"), "{:?}", batch.message);
    let stats = batch.stats.as_ref().unwrap();
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (2, 1, 1)
    );
    let result = |node: &str| {
        batch
            .node_results
            .iter()
            .find(|r| r.node_id == node)
            .unwrap_or_else(|| panic!("no result for {node}"))
    };
    assert_eq!(result("switch-7").status, rms::ReturnCode::Success as i32);
    assert_eq!(result("ghost").status, rms::ReturnCode::Failure as i32);
    assert!(!result("ghost").error_message.is_empty());

    let job_nodes: Vec<&str> = started.jobs.iter().map(|j| j.node_id.as_str()).collect();
    assert_eq!(job_nodes, ["switch-7"], "no job for a node that failed");

    // A failed batch still carries the accepted work's handle.
    assert!(!batch.job_id.is_empty());
    let (_, parent) = poll_firmware_job(&mut client, &batch.job_id).await;
    assert_eq!(state_of(&parent), rms::FirmwareJobState::Completed);
}

/// A document that is not a JSON object is `INVALID_ARGUMENT`, which the
/// caller reads as a rejection before dispatch, so no job may be started.
#[tokio::test]
async fn a_request_without_sot_json_is_an_invalid_argument() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();
    let switch = || vec![node_info("switch-7", "02:00:11:11:22:22")];

    let status = client
        .apply_firmware_object(apply_request(switch(), ""))
        .await
        .expect_err("a request without a document names nothing to apply");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status.message().contains("config_json"),
        "{}",
        status.message()
    );

    // Nothing was issued: the first accepted request gets the first id.
    let started = client
        .apply_firmware_object(apply_request(switch(), r#"{"Id":"fw-sot"}"#))
        .await
        .unwrap()
        .into_inner();
    let job_id = started.response.unwrap().job_id;
    assert!(
        job_id.starts_with("rms-mock-") && job_id.ends_with("-1"),
        "expected the first id of the run, got {job_id}"
    );
}
