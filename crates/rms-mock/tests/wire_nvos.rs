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

//! Wire-level tests for the switch system image (NVOS) RPCs, driven through
//! a real gRPC client like `wire_grpc.rs`: the parent and per-switch child
//! jobs an apply returns, the state vocabulary a poll reports, and the
//! failure path NICo's NVOS update reads.

mod common;

use common::{a_switch, failing_nodes, node_info, serve_with, serve_with_config};
use librms::protos::rack_manager as rms;
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use mac_address::MacAddress;
use rms_mock::SimNode;

type Client = RackManagerClient<tonic::transport::Channel>;

/// The SOT JSON NICo's own fixtures send: an object named by `Id`.
const SOT_JSON: &str = r#"{"Id":"fw-nvos"}"#;

/// A second switch in the same rack.
fn another_switch() -> SimNode {
    SimNode {
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0x11, 0x11, 0x33, 0x33])),
        ..a_switch()
    }
}

/// The two switches of `a_switch()` and `another_switch()`, as NICo names them.
fn two_switches() -> Vec<rms::NodeInfo> {
    vec![
        node_info("switch-1", "02:00:11:11:22:22"),
        node_info("switch-2", "02:00:11:11:33:33"),
    ]
}

fn apply_request(
    config_json: &str,
    nodes: Vec<rms::NodeInfo>,
) -> rms::ApplySwitchSystemImageRequest {
    rms::ApplySwitchSystemImageRequest {
        rack_id: "rack-001".to_string(),
        config_json: config_json.to_string(),
        access_token: Some("NOAUTH".to_string()),
        software_type: "prod".to_string(),
        hardware_type: "gb200".to_string(),
        nodes: Some(rms::NodeSet { nodes }),
    }
}

async fn apply_to_two_switches(client: &mut Client) -> rms::ApplySwitchSystemImageResponse {
    client
        .apply_switch_system_image(apply_request(SOT_JSON, two_switches()))
        .await
        .unwrap()
        .into_inner()
}

/// Poll a switch system image job until it is terminal, returning every
/// state seen and the final response.
async fn poll_image_job(
    client: &mut Client,
    job_id: &str,
) -> (Vec<String>, rms::GetSwitchSystemImageJobStatusResponse) {
    let mut seen = Vec::new();
    for _ in 0..5 {
        let status = client
            .get_switch_system_image_job_status(rms::GetSwitchSystemImageJobStatusRequest {
                job_id: job_id.to_string(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.status, rms::ReturnCode::Success as i32);
        assert_eq!(status.job_id, job_id, "the job id is echoed");
        seen.push(status.state.clone());
        if matches!(status.state.as_str(), "completed" | "failed") {
            return (seen, status);
        }
    }
    panic!("job {job_id} never reached a terminal state; polled states were {seen:?}");
}

/// The caller needs a parent id in the batch and a child job per switch
/// keyed by the node id it sent; a switch without a child falls back to the
/// parent and loses per-switch status. Status, node results, and stats are
/// checked independently, so the batch must agree with itself.
#[tokio::test]
async fn apply_issues_a_parent_and_a_child_per_switch() {
    let url = serve_with(vec![a_switch(), another_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = apply_to_two_switches(&mut client).await;

    let batch = response.response.expect("a batch response is required");
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);
    assert!(!batch.job_id.is_empty(), "the parent id goes in the batch");
    let stats = batch.stats.expect("stats are required");
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (2, 2, 0)
    );
    let node_ids: Vec<&str> = batch
        .node_results
        .iter()
        .map(|r| r.node_id.as_str())
        .collect();
    assert_eq!(node_ids, ["switch-1", "switch-2"], "in request order");
    assert!(
        batch
            .node_results
            .iter()
            .all(|r| r.status == rms::ReturnCode::Success as i32)
    );

    let children: Vec<(&str, &str)> = response
        .jobs
        .iter()
        .map(|j| (j.node_id.as_str(), j.job_id.as_str()))
        .collect();
    assert_eq!(children.len(), 2, "one child per switch");
    assert_eq!((children[0].0, children[1].0), ("switch-1", "switch-2"));
    assert!(children.iter().all(|(_, id)| !id.is_empty()));
    assert_ne!(children[0].1, children[1].1, "children are distinct jobs");
    assert!(
        children.iter().all(|(_, id)| *id != batch.job_id),
        "children are not the parent"
    );

    assert_eq!(response.object_id, "fw-nvos", "the object the SOT names");
    assert!(
        !response.image_filename.is_empty(),
        "the caller records an image filename per switch"
    );
}

/// Each child reports the state vocabulary the caller maps, ends completed,
/// and names its own switch; polling only the parent drives the children too
/// and summarizes them.
#[tokio::test]
async fn child_jobs_progress_to_completed_and_the_parent_summarizes_them() {
    let url = serve_with(vec![a_switch(), another_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = apply_to_two_switches(&mut client).await;
    let parent_id = response.response.unwrap().job_id;

    for child in &response.jobs {
        let (seen, last) = poll_image_job(&mut client, &child.job_id).await;
        assert_eq!(seen, ["running", "completed"]);
        assert_eq!(last.node_id, child.node_id, "a child names its switch");
        assert_eq!(last.rack_id, "rack-001");
        assert!(last.error_message.is_empty());
    }

    // With every child reported complete the parent is done with, and reads
    // complete on its first poll.
    let (seen, _) = poll_image_job(&mut client, &parent_id).await;
    assert_eq!(seen, ["completed"]);

    // A caller that has only the parent id sees the batch finish, summarized.
    let second = apply_to_two_switches(&mut client).await;
    let parent_id = second.response.unwrap().job_id;
    let (seen, parent) = poll_image_job(&mut client, &parent_id).await;
    assert_eq!(seen, ["running", "completed"]);
    assert_eq!(parent.node_id, "", "a parent is not tied to one switch");
    assert_eq!(parent.rack_id, "rack-001");
    assert_eq!(parent.message, "2 of 2 switches completed");
    for child in &second.jobs {
        let (seen, _) = poll_image_job(&mut client, &child.job_id).await;
        assert_eq!(
            seen,
            ["completed"],
            "child {} was driven by its parent",
            child.node_id
        );
    }
}

/// A switch selected to fail reports `failed` after `running`, with a reason
/// the caller surfaces as the switch's failure cause; the parent fails with
/// it and names the switch.
#[tokio::test]
async fn a_faulted_switch_job_runs_and_then_fails_and_so_does_its_parent() {
    let url = serve_with_config(
        vec![a_switch(), another_switch()],
        failing_nodes(&["switch-2"]),
    )
    .await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = apply_to_two_switches(&mut client).await;
    let parent_id = response.response.unwrap().job_id;
    let child_of = |node: &str| {
        response
            .jobs
            .iter()
            .find(|j| j.node_id == node)
            .map(|j| j.job_id.clone())
            .unwrap_or_else(|| panic!("no child for {node}"))
    };

    let (seen, failed) = poll_image_job(&mut client, &child_of("switch-2")).await;
    assert_eq!(seen, ["running", "failed"]);
    assert_eq!(failed.node_id, "switch-2");
    assert!(
        failed.error_message.contains("switch-2"),
        "the reason names the switch: {:?}",
        failed.error_message
    );

    let (_, healthy) = poll_image_job(&mut client, &child_of("switch-1")).await;
    assert_eq!(healthy.state, "completed");
    assert!(healthy.error_message.is_empty());

    let (_, parent) = poll_image_job(&mut client, &parent_id).await;
    assert_eq!(parent.state, "failed", "one failed switch fails the batch");
    assert!(
        parent.error_message.contains("switch-2"),
        "the parent's reason names the failed switch: {:?}",
        parent.error_message
    );
    assert_eq!(parent.message, "1 of 2 switches completed, 1 failed");
}

/// A switch no device matches is a per-node failure at submission, and the
/// child it is still given fails for the same reason: the caller keeps the
/// job ids it was handed and learns the outcome from the poll.
#[tokio::test]
async fn an_unmatched_switch_fails_at_submission_and_in_its_child_job() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .apply_switch_system_image(apply_request(
            SOT_JSON,
            vec![
                node_info("switch-1", "02:00:11:11:22:22"),
                node_info("switch-9", "02:00:99:99:99:99"),
            ],
        ))
        .await
        .unwrap()
        .into_inner();

    let batch = response.response.unwrap();
    assert_eq!(batch.status, rms::ReturnCode::Failure as i32);
    assert!(batch.message.contains("switch-9"), "{:?}", batch.message);
    let stats = batch.stats.unwrap();
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
    assert_eq!(result("switch-1").status, rms::ReturnCode::Success as i32);
    assert_eq!(result("switch-9").status, rms::ReturnCode::Failure as i32);
    assert!(!result("switch-9").error_message.is_empty());
    assert!(!batch.job_id.is_empty(), "accepted work keeps its handle");

    assert_eq!(
        response.jobs.len(),
        2,
        "children stay aligned with the request"
    );
    let child = |node: &str| {
        response
            .jobs
            .iter()
            .find(|j| j.node_id == node)
            .unwrap_or_else(|| panic!("no child for {node}"))
            .job_id
            .clone()
    };
    let (_, real) = poll_image_job(&mut client, &child("switch-1")).await;
    assert_eq!(real.state, "completed");
    let (_, ghost) = poll_image_job(&mut client, &child("switch-9")).await;
    assert_eq!(ghost.state, "failed");
    assert_eq!(ghost.node_id, "switch-9");
    assert_eq!(ghost.error_message, result("switch-9").error_message);
}

/// The caller treats `INVALID_ARGUMENT` as terminal for the request, so a
/// request that names no image is refused up front and starts no job.
#[tokio::test]
async fn a_request_without_an_image_is_an_invalid_argument() {
    let url = serve_with(vec![a_switch(), another_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let no_image = client
        .apply_switch_system_image(apply_request("", two_switches()))
        .await
        .expect_err("a request without an image names nothing to apply");
    assert_eq!(no_image.code(), tonic::Code::InvalidArgument);
    assert!(
        no_image.message().contains("config_json"),
        "{}",
        no_image.message()
    );

    // Nothing was issued: the first accepted request gets the first id.
    let accepted = apply_to_two_switches(&mut client).await;
    let job_id = accepted.response.unwrap().job_id;
    assert!(
        job_id.starts_with("rms-mock-") && job_id.ends_with("-1"),
        "expected the first id of the run, got {job_id}"
    );
}
