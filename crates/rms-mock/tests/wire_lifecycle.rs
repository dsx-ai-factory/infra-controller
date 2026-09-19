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

//! Wire-level tests for the switch lifecycle RPCs, password rotation and
//! factory reset, driven through a real gRPC client like `wire_grpc.rs`.
//! They assert the shapes NICo reads: a parent job id in the batch, one child
//! per switch when the parent is polled with children, a failed child that
//! names its switch, and per-node results aligned with the request.

mod common;

use std::net::IpAddr;

use common::{a_switch, failing_nodes, node_info, serve_with, serve_with_config};
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager_v2::rack_manager_v2_client::RackManagerV2Client;
use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
use mac_address::MacAddress;
use rms_mock::SimNode;

type Client = RackManagerClient<tonic::transport::Channel>;

/// A second switch in the same rack, at the same rack position.
fn another_switch() -> SimNode {
    SimNode {
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0x11, 0x11, 0x33, 0x33])),
        host_ip: Some(IpAddr::from([10, 233, 32, 8])),
        ..a_switch()
    }
}

/// A node as NICo names it for a password rotation: the NVOS host endpoint
/// with the current credentials and no BMC endpoint, so only the host
/// address can match.
fn host_only_node_info(node_id: &str, host_ip: &str) -> rms::NodeInfo {
    rms::NodeInfo {
        node_id: node_id.to_string(),
        rack_id: "rack-001".to_string(),
        r#type: None,
        bmc_endpoint: None,
        host_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: host_ip.to_string(),
                mac_address: String::new(),
                host_name: None,
            }),
            port: 0,
            credentials: Some(rms::Credentials {
                auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                    username: "admin".to_string(),
                    password: "current-password".to_string(),
                })),
            }),
        }),
        node_descriptor: None,
    }
}

fn password_request(nodes: Vec<rms::NodeInfo>) -> rms::UpdateSwitchSystemPasswordRequest {
    rms::UpdateSwitchSystemPasswordRequest {
        nodes: Some(rms::NodeSet { nodes }),
        username: "admin".to_string(),
        password: "next-password".to_string(),
    }
}

fn reset_request(
    nodes: Vec<rms::NodeInfo>,
    domain: Option<&str>,
) -> rms::BatchResetSwitchFactoryDefaultRequest {
    rms::BatchResetSwitchFactoryDefaultRequest {
        nodes: Some(rms::NodeSet { nodes }),
        domain: domain.map(str::to_owned),
    }
}

fn is_terminal(state: i32) -> bool {
    matches!(
        rms::JobExecutionState::try_from(state),
        Ok(rms::JobExecutionState::Completed | rms::JobExecutionState::Failed)
    )
}

/// Poll a parent through `GetJobStatus` with its children, the way both
/// lifecycle callers do, until it is terminal. Returns the final response, in
/// which the parent comes first.
async fn poll_parent(client: &mut Client, parent: &str) -> rms::GetJobStatusResponse {
    for _ in 0..5 {
        let response = client
            .get_job_status(rms::GetJobStatusRequest {
                job_id: parent.to_string(),
                include_child_job_states: true,
            })
            .await
            .unwrap()
            .into_inner();
        let job = &response.job_states[0];
        assert_eq!(job.job_id, parent, "the parent comes first");
        assert_ne!(
            job.execution_state,
            rms::JobExecutionState::Unspecified as i32,
            "an unset execution state is read as an unknown outcome"
        );
        if is_terminal(job.execution_state) {
            return response;
        }
    }
    panic!("job {parent} never reached a terminal state");
}

/// The children of a polled parent, as the callers correlate them: by
/// `parent_job_id`, cross-checked against the parent's `child_job_ids`.
fn children_of<'a>(
    response: &'a rms::GetJobStatusResponse,
    parent: &str,
) -> Vec<&'a rms::JobStatus> {
    let parent_job = &response.job_states[0];
    let children: Vec<&rms::JobStatus> = response
        .job_states
        .iter()
        .filter(|j| j.parent_job_id.as_deref() == Some(parent))
        .collect();
    for child in &children {
        assert!(
            parent_job.child_job_ids.contains(&child.job_id),
            "a child naming the parent must be listed by it, or the reset stays pending"
        );
    }
    assert_eq!(
        children.len(),
        parent_job.child_job_ids.len(),
        "every child the parent lists must be present, or the reset stays pending"
    );
    children
}

fn stats_of(batch: &rms::NodeBatchResponse) -> (u32, u32, u32) {
    let stats = batch
        .stats
        .as_ref()
        .expect("stats drive the caller's checks");
    (
        stats.total_nodes,
        stats.successful_nodes,
        stats.failed_nodes,
    )
}

fn node_result<'a>(
    batch: &'a rms::NodeBatchResponse,
    node_id: &str,
) -> &'a rms::NodeOperationResult {
    batch
        .node_results
        .iter()
        .find(|r| r.node_id == node_id)
        .unwrap_or_else(|| panic!("no result for {node_id}"))
}

fn child_nodes<'a>(children: &[&'a rms::JobStatus]) -> Vec<&'a str> {
    let mut nodes: Vec<&str> = children
        .iter()
        .map(|c| c.node_id.as_deref().unwrap_or_default())
        .collect();
    nodes.sort_unstable();
    nodes
}

/// Credentials are mandatory. The caller reads `INVALID_ARGUMENT` as a
/// rejection before dispatch, which is only true if nothing was issued.
#[tokio::test]
async fn password_rotation_rejects_empty_credentials() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();
    let node = || host_only_node_info("switch-7", "10.233.32.7");

    for (what, request) in [
        (
            "no username",
            rms::UpdateSwitchSystemPasswordRequest {
                username: String::new(),
                ..password_request(vec![node()])
            },
        ),
        (
            "no password",
            rms::UpdateSwitchSystemPasswordRequest {
                password: String::new(),
                ..password_request(vec![node()])
            },
        ),
    ] {
        let status = client
            .update_switch_system_password(request)
            .await
            .expect_err(what);
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{what}");
    }

    // Nothing was issued: the first accepted request gets the first id.
    let batch = client
        .update_switch_system_password(password_request(vec![node()]))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert!(
        batch.job_id.starts_with("rms-mock-") && batch.job_id.ends_with("-1"),
        "expected the first id of the run, got {}",
        batch.job_id
    );
}

/// The rotation caller names one switch by its NVOS host endpoint alone,
/// keeps the batch's `job_id` whatever the batch status says, and polls it
/// as a parent with children until every related job is complete.
#[tokio::test]
async fn password_rotation_issues_a_parent_that_completes_with_its_child() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .update_switch_system_password(password_request(vec![host_only_node_info(
            "switch-7",
            "10.233.32.7",
        )]))
        .await
        .unwrap()
        .into_inner();

    let batch = response
        .response
        .expect("a missing batch is read as an unknown outcome");
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);
    assert_eq!(stats_of(&batch), (1, 1, 0));
    assert!(
        !batch.job_id.is_empty(),
        "an empty job id makes the caller give up with an unknown outcome"
    );
    assert_eq!(
        node_result(&batch, "switch-7").status,
        rms::ReturnCode::Success as i32,
        "the host-only node matches the switch by its NVOS address"
    );

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    let parent = &final_poll.job_states[0];
    assert_eq!(
        parent.execution_state,
        rms::JobExecutionState::Completed as i32
    );
    assert!(parent.error_message.is_empty());

    let children = children_of(&final_poll, &batch.job_id);
    assert_eq!(child_nodes(&children), ["switch-7"]);
    assert_eq!(
        children[0].execution_state,
        rms::JobExecutionState::Completed as i32,
        "the caller needs every related job complete, not just the parent"
    );
}

/// A rotation selected to fail fails its parent and carries a failed child,
/// since the caller prefers the child's device-specific message over the
/// parent's summary.
#[tokio::test]
async fn a_faulted_password_rotation_fails_through_a_child_that_names_the_switch() {
    let url = serve_with_config(vec![a_switch()], failing_nodes(&["switch-7"])).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let batch = client
        .update_switch_system_password(password_request(vec![host_only_node_info(
            "switch-7",
            "10.233.32.7",
        )]))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    // Submission succeeds: the failure is the job's outcome, not a rejection.
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    assert_eq!(
        final_poll.job_states[0].execution_state,
        rms::JobExecutionState::Failed as i32
    );
    let children = children_of(&final_poll, &batch.job_id);
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].execution_state,
        rms::JobExecutionState::Failed as i32
    );
    assert!(
        children[0].error_message.contains("switch-7"),
        "the child's message names the switch: {:?}",
        children[0].error_message
    );
}

/// A factory reset is a parent with one child per switch, and the caller
/// reads it as complete only once at least one child is present and every
/// child the parent lists is complete.
#[tokio::test]
async fn factory_reset_issues_a_parent_with_a_child_per_switch_and_completes() {
    let url = serve_with(vec![a_switch(), another_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .batch_reset_switch_factory_default(reset_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("switch-8", "02:00:11:11:33:33"),
            ],
            Some("site.example.com"),
        ))
        .await
        .unwrap()
        .into_inner();

    let batch = response
        .response
        .expect("a missing batch is read as an unknown outcome");
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);
    assert_eq!(stats_of(&batch), (2, 2, 0));
    assert!(
        !batch.job_id.trim().is_empty(),
        "an empty job id makes the caller report the outcome unknown"
    );

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    let parent = &final_poll.job_states[0];
    assert_eq!(
        parent.execution_state,
        rms::JobExecutionState::Completed as i32
    );
    assert_eq!(parent.node_id, None, "a parent is not tied to one node");
    assert_eq!(parent.rack_id.as_deref(), Some("rack-001"));

    let children = children_of(&final_poll, &batch.job_id);
    assert_eq!(
        child_nodes(&children),
        ["switch-7", "switch-8"],
        "one child per switch, or the reset stays pending"
    );
    for child in children {
        assert_eq!(
            child.execution_state,
            rms::JobExecutionState::Completed as i32
        );
        assert_eq!(child.rack_id.as_deref(), Some("rack-001"));
    }
}

/// One failed switch fails the reset, and the caller reports the failed
/// child, its node, message, and error code, rather than the parent.
#[tokio::test]
async fn a_faulted_factory_reset_reports_the_failed_switch() {
    let url = serve_with_config(
        vec![a_switch(), another_switch()],
        failing_nodes(&["switch-8"]),
    )
    .await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let batch = client
        .batch_reset_switch_factory_default(reset_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("switch-8", "02:00:11:11:33:33"),
            ],
            None,
        ))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    let parent = &final_poll.job_states[0];
    assert_eq!(
        parent.execution_state,
        rms::JobExecutionState::Failed as i32
    );
    assert!(
        parent.error_message.contains("switch-8"),
        "the parent's summary names the failed switch: {:?}",
        parent.error_message
    );

    let children = children_of(&final_poll, &batch.job_id);
    let failed = children
        .iter()
        .find(|c| c.execution_state == rms::JobExecutionState::Failed as i32)
        .expect("the caller prefers a failed child for its diagnostics");
    assert_eq!(failed.node_id.as_deref(), Some("switch-8"));
    assert!(
        failed.error_message.contains("switch-8"),
        "the proto guarantees a message on failure: {:?}",
        failed.error_message
    );
    assert_ne!(
        failed.error_code,
        rms::JobError::Unspecified as i32,
        "the caller renders the error code by name"
    );
    let healthy = children
        .iter()
        .find(|c| c.node_id.as_deref() == Some("switch-7"))
        .expect("the healthy switch still has its child");
    assert_eq!(
        healthy.execution_state,
        rms::JobExecutionState::Completed as i32
    );
}

/// A node no device matches is a per-node failure in all three places the
/// caller checks, while the batch still carries a job id and the children
/// still line up with the request; the caller ignores the batch status once
/// it has a job id, so the parent fails too, through the ghost's child.
#[tokio::test]
async fn factory_reset_of_an_unmatched_node_is_a_per_node_failure() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let batch = client
        .batch_reset_switch_factory_default(reset_request(
            vec![
                node_info("switch-7", "02:00:11:11:22:22"),
                node_info("ghost", "02:00:00:00:00:99"),
            ],
            None,
        ))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();

    assert_eq!(batch.status, rms::ReturnCode::Failure as i32);
    assert!(batch.message.contains("ghost"), "{:?}", batch.message);
    assert_eq!(stats_of(&batch), (2, 1, 1));
    assert_eq!(
        node_result(&batch, "switch-7").status,
        rms::ReturnCode::Success as i32
    );
    let ghost = node_result(&batch, "ghost");
    assert_eq!(ghost.status, rms::ReturnCode::Failure as i32);
    assert!(!ghost.error_message.is_empty());
    assert!(
        !batch.job_id.is_empty(),
        "the caller keeps the job id whatever the batch says"
    );

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    let parent = &final_poll.job_states[0];
    assert_eq!(
        parent.execution_state,
        rms::JobExecutionState::Failed as i32
    );
    assert!(
        parent.error_message.contains("ghost"),
        "{:?}",
        parent.error_message
    );
    let children = children_of(&final_poll, &batch.job_id);
    assert_eq!(
        child_nodes(&children),
        ["ghost", "switch-7"],
        "children stay aligned with the request, matched or not"
    );
    let child = |node: &str| {
        children
            .iter()
            .find(|c| c.node_id.as_deref() == Some(node))
            .unwrap()
    };
    assert_eq!(
        child("switch-7").execution_state,
        rms::JobExecutionState::Completed as i32
    );
    assert_eq!(
        child("ghost").execution_state,
        rms::JobExecutionState::Failed as i32
    );
    assert_eq!(child("ghost").error_message, ghost.error_message);
}

/// A default request is an empty batch: a job id with nothing under it,
/// which completes on its first poll.
#[tokio::test]
async fn factory_reset_of_no_nodes_is_an_empty_batch() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let batch = client
        .batch_reset_switch_factory_default(rms::BatchResetSwitchFactoryDefaultRequest::default())
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert_eq!(batch.status, rms::ReturnCode::Success as i32);
    assert_eq!(stats_of(&batch), (0, 0, 0));
    assert!(!batch.job_id.is_empty());

    let final_poll = poll_parent(&mut client, &batch.job_id).await;
    assert_eq!(
        final_poll.job_states[0].execution_state,
        rms::JobExecutionState::Completed as i32
    );
    assert!(children_of(&final_poll, &batch.job_id).is_empty());
}

/// A factory reset wipes the switch's configuration, and the one piece of it
/// the mock holds is its fabric primary role. The role is held until the
/// switch's reset job completes; then the switch stops reporting a configured
/// control plane, and the rack still reads back exactly one primary.
#[tokio::test]
async fn factory_reset_clears_the_switch_fabric_role_once_the_job_completes() {
    let url = serve_with(vec![a_switch(), another_switch()]).await;
    let mut v1 = RackManagerClient::connect(url.clone()).await.unwrap();
    let mut v2 = RackManagerV2Client::connect(url).await.unwrap();

    let node_set = || rms::NodeSet {
        nodes: vec![
            node_info("switch-7", "02:00:11:11:22:22"),
            node_info("switch-8", "02:00:11:11:33:33"),
        ],
    };
    let enabled_switches = |response: rms::GetScaleUpFabricStatusResponse| {
        response
            .fabric_status
            .unwrap()
            .switches
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| s.node_id)
            .collect::<Vec<_>>()
    };

    // switch-8 is made the rack's primary, and reads back as such.
    v2.configure_scale_up_fabric_manager(rms_v2::ConfigureScaleUpFabricManagerRequest {
        nodes: Some(node_set()),
        primary_switch_node_id: Some("switch-8".to_string()),
        domain: None,
        config: Some(rms_v2::ScaleUpFabricConfig {
            topology_type: "nvl72".to_string(),
            extra_static_configs: Vec::new(),
        }),
    })
    .await
    .unwrap();
    let before = v1
        .get_scale_up_fabric_status(rms::GetScaleUpFabricStatusRequest {
            nodes: Some(node_set()),
            domain: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(enabled_switches(before), ["switch-8"]);

    let reset = v1
        .batch_reset_switch_factory_default(reset_request(
            vec![node_info("switch-8", "02:00:11:11:33:33")],
            None,
        ))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    let submitted = v1
        .get_scale_up_fabric_status(rms::GetScaleUpFabricStatusRequest {
            nodes: Some(node_set()),
            domain: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        enabled_switches(submitted),
        ["switch-8"],
        "a reset that has not completed has not changed the switch"
    );
    poll_parent(&mut v1, &reset.job_id).await;

    // The reset switch no longer runs the fabric manager; the rack still has
    // exactly one primary, elected afresh.
    let services = v1
        .batch_get_scale_up_fabric_service_status(rms::BatchGetScaleUpFabricServiceStatusRequest {
            nodes: Some(node_set()),
        })
        .await
        .unwrap()
        .into_inner();
    let configured: Vec<&str> = services
        .service_statuses
        .iter()
        .filter(|(_, entry)| entry.status_json.contains("CONTROL_PLANE_STATE_CONFIGURED"))
        .map(|(node_id, _)| node_id.as_str())
        .collect();
    assert_eq!(configured, ["switch-7"]);

    let after = v1
        .get_scale_up_fabric_status(rms::GetScaleUpFabricStatusRequest {
            nodes: Some(node_set()),
            domain: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        enabled_switches(after),
        ["switch-7"],
        "a reset primary must not stay primary, and the rack must not be left without one"
    );
}
