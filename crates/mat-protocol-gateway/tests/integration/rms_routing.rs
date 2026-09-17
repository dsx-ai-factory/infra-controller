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

//! RMS routing through `run`: fake machine-a-tron instances declare their racks on
//! `/racks/status` and serve the RMS mock, and an unmodified `librms` client talks to the gateway.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::StatusCode;
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager::{
    BatchGetNodeDeviceInfoRequest, BatchGetScaleUpFabricServiceStatusRequest,
    ConfigureSwitchCertificateRequest, GetConfigureSwitchCertificateJobStatusRequest,
    GetJobStatusRequest, GetScaleUpFabricStatusRequest, GetVersionRequest, JobExecutionState,
    JobStatus, ListRacksRequest, NodeInfo, ReturnCode,
};
use librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest;
use librms::protos::rack_manager_v2::rack_manager_v2_client::RackManagerV2Client;
use mat_protocol_gateway::{ExitReason, RMS_VERSION};
use tokio_util::sync::CancellationToken;
use tonic::Code;
use tonic::transport::Channel;

use crate::common::{
    FakeController, FakeMachineATron, GUID_A, GUID_B, SWITCH_A, SWITCH_B, TRAY_A, counts,
    devices_a, devices_b, finished, free_loopback_address, gateway_config, node, nodes, probe,
    rms_client, spawn_run, wait_until, wait_until_ready,
};

/// Status and body of `/readyz`; `None` while nothing answers.
async fn readyz(client: &reqwest::Client, gateway: SocketAddr) -> Option<(StatusCode, String)> {
    let response = client
        .get(format!("http://{gateway}/readyz"))
        .send()
        .await
        .ok()?;
    let status = response.status();
    Some((status, response.text().await.unwrap()))
}

/// `GetScaleUpFabricStatus` for one switch of `rack_id`, the rack-scoped call NICo makes.
async fn fabric_status(
    rms: &mut RackManagerClient<Channel>,
    rack_id: &str,
    mac: [u8; 6],
) -> Result<String, tonic::Status> {
    let response = rms
        .get_scale_up_fabric_status(GetScaleUpFabricStatusRequest {
            nodes: nodes(vec![node("switch", rack_id, mac)]),
            ..GetScaleUpFabricStatusRequest::default()
        })
        .await?
        .into_inner();
    assert_eq!(response.status, ReturnCode::Success as i32);
    let switches = response.fabric_status.expect("fabric status").switches;
    assert_eq!(switches.len(), 1);
    assert_eq!(switches[0].node_id, "switch");
    Ok(switches[0].error_message.clone())
}

/// The certificate batch NICo sends for the switches of both instances.
fn certificate_batch(switches: Vec<NodeInfo>) -> ConfigureSwitchCertificateRequest {
    ConfigureSwitchCertificateRequest {
        nodes: nodes(switches),
        services: Vec::new(),
        test_hello: false,
        domain: Some("site.example.com".to_string()),
    }
}

/// `(job id, parent job id, child job ids)` of every entry, in the order reported.
fn job_tree(states: &[JobStatus]) -> Vec<(&str, Option<&str>, Vec<&str>)> {
    states
        .iter()
        .map(|state| {
            (
                state.job_id.as_str(),
                state.parent_job_id.as_deref(),
                state.child_job_ids.iter().map(String::as_str).collect(),
            )
        })
        .collect()
}

/// A rack-scoped call reaches the instance owning the rack, a batch spanning two instances is
/// split and merged back in request order with the node nobody owns failed in place, and jobs
/// poll to completion through gateway ids: a single instance's job as is, a batch split across
/// instances through one aggregate parent listing the per-instance parents.
#[tokio::test]
async fn rms_requests_are_routed_by_rack_and_batches_split_across_instances_are_merged() {
    let mat_a =
        FakeMachineATron::start_with_racks("mat-a", &[GUID_A], &["rack-001"], devices_a()).await;
    let mat_b =
        FakeMachineATron::start_with_racks("mat-b", &[GUID_B], &["rack-002"], devices_b()).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let config = gateway_config(controller.serve().await, listen);
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());
    wait_until_ready(&http, listen).await;
    let mut rms = rms_client(listen).await;

    let version = rms.get_version(GetVersionRequest {}).await.unwrap();
    assert_eq!(version.into_inner().version, RMS_VERSION);

    // Rack-scoped: the mock on mat-b knows the switch, the one on mat-a would report it unmatched.
    assert_eq!(
        fabric_status(&mut rms, "rack-002", SWITCH_B).await.unwrap(),
        ""
    );
    let status = rms
        .get_scale_up_fabric_status(GetScaleUpFabricStatusRequest {
            nodes: nodes(vec![
                node("s1", "rack-001", SWITCH_A),
                node("s2", "rack-002", SWITCH_B),
            ]),
            ..GetScaleUpFabricStatusRequest::default()
        })
        .await
        .expect_err("a rack-scoped call over two racks is refused");
    assert_eq!(status.code(), Code::InvalidArgument);

    // RackManagerV2 is routed the same way; the job it returns is a gateway id that
    // `GetJobStatus` resolves to the one instance and echoes.
    let mut rms_v2 = RackManagerV2Client::connect(format!("http://{listen}"))
        .await
        .unwrap();
    let fabric_job = rms_v2
        .configure_scale_up_fabric_manager(ConfigureScaleUpFabricManagerRequest {
            nodes: nodes(vec![node("s1", "rack-001", SWITCH_A)]),
            ..ConfigureScaleUpFabricManagerRequest::default()
        })
        .await
        .unwrap()
        .into_inner()
        .job_id;
    assert!(fabric_job.starts_with("gw-"), "{fabric_job}");
    for expected in [JobExecutionState::Running, JobExecutionState::Completed] {
        let states = rms
            .get_job_status(GetJobStatusRequest {
                job_id: fabric_job.clone(),
                include_child_job_states: false,
            })
            .await
            .unwrap()
            .into_inner()
            .job_states;
        assert_eq!(
            job_tree(&states),
            vec![(fabric_job.as_str(), None, Vec::new())]
        );
        assert_eq!(states[0].execution_state, expected as i32, "{states:?}");
    }
    let status = rms_v2
        .configure_scale_up_fabric_manager(ConfigureScaleUpFabricManagerRequest {
            nodes: nodes(vec![
                node("s1", "rack-001", SWITCH_A),
                node("s2", "rack-002", SWITCH_B),
            ]),
            ..ConfigureScaleUpFabricManagerRequest::default()
        })
        .await
        .expect_err("a rack-scoped call over two racks is refused");
    assert_eq!(status.code(), Code::InvalidArgument);

    // Batch across both instances, interleaved so request order differs from part order, plus a
    // node whose rack nobody simulates.
    let response = rms
        .batch_get_node_device_info(BatchGetNodeDeviceInfoRequest {
            nodes: nodes(vec![
                node("t1", "rack-001", TRAY_A),
                node("s2", "rack-002", SWITCH_B),
                node("s1", "rack-001", SWITCH_A),
                node("x", "rack-999", [0x02, 0, 0, 0, 0, 0x99]),
            ]),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.status, ReturnCode::Failure as i32);
    let details: Vec<(&str, Option<u32>, Option<u32>)> = response
        .node_device_details
        .iter()
        .map(|detail| {
            (
                detail.node_id.as_str(),
                detail.slot_number,
                detail.tray_index,
            )
        })
        .collect();
    assert_eq!(
        details,
        vec![
            ("t1", Some(12), Some(2)),
            ("s2", Some(30), None),
            ("s1", Some(30), None)
        ],
        "details come back in request order, only for nodes an instance knows"
    );
    assert_eq!(counts(response.stats.as_ref()), (4, 3, 1));
    assert!(
        response
            .message
            .contains("node x: no machine-a-tron instance owns rack \"rack-999\""),
        "{}",
        response.message
    );

    // A batch answered as a map keyed by node id: the unowned node's entry carries the reason.
    let response = rms
        .batch_get_scale_up_fabric_service_status(BatchGetScaleUpFabricServiceStatusRequest {
            nodes: nodes(vec![
                node("s1", "rack-001", SWITCH_A),
                node("s2", "rack-002", SWITCH_B),
                node("x", "rack-999", [0x02, 0, 0, 0, 0, 0x99]),
            ]),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.status, ReturnCode::Failure as i32);
    let mut entries: Vec<(&str, bool, &str)> = response
        .service_statuses
        .iter()
        .map(|(node_id, entry)| {
            (
                node_id.as_str(),
                !entry.status_json.is_empty(),
                entry.error_message.as_str(),
            )
        })
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![
            ("s1", true, ""),
            ("s2", true, ""),
            (
                "x",
                false,
                "no machine-a-tron instance owns rack \"rack-999\""
            ),
        ]
    );
    assert_eq!(counts(response.stats.as_ref()), (3, 2, 1));

    // A batch with jobs: one per switch and one aggregate parent for the two instances' parents.
    let response = rms
        .configure_switch_certificate(certificate_batch(vec![
            node("s1", "rack-001", SWITCH_A),
            node("s2", "rack-002", SWITCH_B),
        ]))
        .await
        .unwrap()
        .into_inner();
    let batch = response.response.unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32);
    assert_eq!(counts(batch.stats.as_ref()), (2, 2, 0));
    let job_ids: Vec<&str> = response
        .jobs
        .iter()
        .map(|job| job.job_id.as_str())
        .collect();
    assert_eq!(
        response
            .jobs
            .iter()
            .map(|job| job.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s1", "s2"]
    );
    assert!(
        job_ids
            .iter()
            .chain([&batch.job_id.as_str()])
            .all(|id| id.starts_with("gw-")),
        "every id handed out is a gateway id: {job_ids:?}, {}",
        batch.job_id
    );
    assert_ne!(
        job_ids[0], job_ids[1],
        "each instance's switch job gets a gateway id of its own"
    );
    assert!(
        !job_ids.contains(&batch.job_id.as_str()),
        "the batch id is the aggregate"
    );

    // The aggregate parent is running while any part is and complete once every part is (the
    // mock completes a job on its second poll). Its children are the per-instance parents,
    // gateway ids of their own; with child states each parent follows with its switch's job.
    for expected in [JobExecutionState::Running, JobExecutionState::Completed] {
        let states = rms
            .get_job_status(GetJobStatusRequest {
                job_id: batch.job_id.clone(),
                include_child_job_states: true,
            })
            .await
            .unwrap()
            .into_inner()
            .job_states;
        let parents: Vec<&str> = states[0].child_job_ids.iter().map(String::as_str).collect();
        assert_eq!(parents.len(), 2, "{states:?}");
        assert!(
            parents
                .iter()
                .all(|id| id.starts_with("gw-") && !job_ids.contains(id)),
            "the per-instance parents are gateway ids distinct from the switch jobs: {states:?}"
        );
        assert_eq!(
            job_tree(&states),
            vec![
                (batch.job_id.as_str(), None, parents.clone()),
                (parents[0], Some(batch.job_id.as_str()), vec![job_ids[0]]),
                (job_ids[0], Some(parents[0]), Vec::new()),
                (parents[1], Some(batch.job_id.as_str()), vec![job_ids[1]]),
                (job_ids[1], Some(parents[1]), Vec::new()),
            ]
        );
        assert!(
            states
                .iter()
                .all(|state| state.execution_state == expected as i32),
            "{states:?}"
        );
    }
    let states = rms
        .get_job_status(GetJobStatusRequest {
            job_id: batch.job_id.clone(),
            include_child_job_states: false,
        })
        .await
        .unwrap()
        .into_inner()
        .job_states;
    assert_eq!(states.len(), 1, "no child states unless asked: {states:?}");
    assert_eq!(states[0].job_id, batch.job_id);
    let response = rms
        .get_configure_switch_certificate_job_status(
            GetConfigureSwitchCertificateJobStatusRequest {
                job_id: job_ids[0].to_string(),
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        response.job_id, job_ids[0],
        "the gateway id is echoed, not the mock's"
    );
    assert_eq!(response.state, "completed");

    // The certificate status RPC resolves an aggregate too: running while any part is, then
    // completed, with the RPC itself succeeding throughout.
    let aggregate = rms
        .configure_switch_certificate(certificate_batch(vec![
            node("s1", "rack-001", SWITCH_A),
            node("s2", "rack-002", SWITCH_B),
        ]))
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap()
        .job_id;
    for expected in ["running", "completed"] {
        let response = rms
            .get_configure_switch_certificate_job_status(
                GetConfigureSwitchCertificateJobStatusRequest {
                    job_id: aggregate.clone(),
                },
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.status, ReturnCode::Success as i32, "{response:?}");
        assert_eq!(response.job_id, aggregate);
        assert_eq!(response.state, expected, "{response:?}");
        assert_eq!(
            (response.rack_id.as_str(), response.node_id.as_str()),
            ("", ""),
            "an aggregate spans racks: {response:?}"
        );
    }

    // An id from before a restart is unknown to this process and reported complete by both
    // status RPCs, as the mock does; a blank id names no job.
    let states = rms
        .get_job_status(GetJobStatusRequest {
            job_id: "gw-0-999".to_string(),
            include_child_job_states: true,
        })
        .await
        .unwrap()
        .into_inner()
        .job_states;
    assert_eq!(job_tree(&states), vec![("gw-0-999", None, Vec::new())]);
    assert_eq!(
        states[0].execution_state,
        JobExecutionState::Completed as i32
    );
    let response = rms
        .get_configure_switch_certificate_job_status(
            GetConfigureSwitchCertificateJobStatusRequest {
                job_id: "gw-0-999".to_string(),
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (
            response.status,
            response.job_id.as_str(),
            response.state.as_str()
        ),
        (ReturnCode::Success as i32, "gw-0-999", "completed")
    );
    let status = rms
        .get_job_status(GetJobStatusRequest {
            job_id: " ".to_string(),
            include_child_job_states: false,
        })
        .await
        .expect_err("a blank id names no job");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(status.message(), "job_id is required");
    let status = rms
        .get_configure_switch_certificate_job_status(
            GetConfigureSwitchCertificateJobStatusRequest {
                job_id: String::new(),
            },
        )
        .await
        .expect_err("a blank id names no job");
    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(status.message(), "job_id is required");

    // An RPC the mock does not implement is not routed.
    let status = rms
        .list_racks(ListRacksRequest::default())
        .await
        .expect_err("not routed");
    assert_eq!(status.code(), Code::Unimplemented);
    assert!(
        status.message().contains("does not route list_racks"),
        "{status}"
    );

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}

/// Two instances reporting one rack is a conflict: `/readyz` names it, routed RPCs are refused
/// rather than sent to either instance, and both clear once one instance stops reporting it.
#[tokio::test]
async fn a_rack_reported_by_two_instances_blocks_readiness_and_routing_until_one_withdraws() {
    let mat_a = FakeMachineATron::start_with_racks(
        "mat-a",
        &[GUID_A],
        &["rack-001", "rack-shared"],
        devices_a(),
    )
    .await;
    let mat_b = FakeMachineATron::start_with_racks(
        "mat-b",
        &[GUID_B],
        &["rack-002", "rack-shared"],
        devices_b(),
    )
    .await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let config = gateway_config(controller.serve().await, listen);
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());

    wait_until("the gateway explains the conflict", || async {
        readyz(&http, listen)
            .await
            .is_some_and(|(_, body)| body.starts_with("rack "))
    })
    .await;
    assert_eq!(
        readyz(&http, listen).await,
        Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "rack rack-shared is reported by mat-a, mat-b".to_string()
        ))
    );
    let mut rms = rms_client(listen).await;
    let status = fabric_status(&mut rms, "rack-001", SWITCH_A)
        .await
        .expect_err("nothing is routed while ownership is conflicted");
    assert_eq!(status.code(), Code::Unavailable);

    mat_b.set_racks(&["rack-002"]);
    wait_until_ready(&http, listen).await;
    assert_eq!(
        fabric_status(&mut rms, "rack-shared", SWITCH_A)
            .await
            .unwrap(),
        ""
    );
    assert_eq!(
        fabric_status(&mut rms, "rack-002", SWITCH_B).await.unwrap(),
        ""
    );

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}

/// An instance whose status routes stop answering keeps its racks for `stale_after` and then
/// loses them: rack-scoped calls are `UNAVAILABLE` naming it, batch nodes fail in place, nothing
/// is redirected, `/readyz` stays 200, and the racks return with the instance.
#[tokio::test]
async fn a_source_that_stops_answering_keeps_its_racks_until_stale_after_then_answers_unavailable()
{
    let mat_a =
        FakeMachineATron::start_with_racks("mat-a", &[GUID_A], &["rack-001"], devices_a()).await;
    let mat_b =
        FakeMachineATron::start_with_racks("mat-b", &[GUID_B], &["rack-002"], devices_b()).await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let mut config = gateway_config(controller.serve().await, listen);
    // The stale state is asserted before it expires, so give the assertion room on a loaded host.
    config.ownership.stale_after = Duration::from_secs(2);
    config.validate().unwrap();
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());
    wait_until_ready(&http, listen).await;
    let mut rms = rms_client(listen).await;

    mat_b.set_available(false);
    assert_eq!(
        fabric_status(&mut rms, "rack-002", SWITCH_B).await.unwrap(),
        "",
        "the last answer keeps routing while the source is merely stale"
    );
    wait_until("mat-b is dropped from routing", || async {
        fabric_status(&mut rms_client(listen).await, "rack-002", SWITCH_B)
            .await
            .is_err()
    })
    .await;
    let status = fabric_status(&mut rms, "rack-002", SWITCH_B)
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert!(
        status
            .message()
            .contains("machine-a-tron mat-b owns rack \"rack-002\""),
        "{status}"
    );
    assert_eq!(probe(&http, listen, "/readyz").await, Some(StatusCode::OK));

    let response = rms
        .batch_get_node_device_info(BatchGetNodeDeviceInfoRequest {
            nodes: nodes(vec![
                node("t1", "rack-001", TRAY_A),
                node("s2", "rack-002", SWITCH_B),
            ]),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        response
            .node_device_details
            .iter()
            .map(|detail| detail.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t1"]
    );
    assert_eq!(counts(response.stats.as_ref()), (2, 1, 1));
    assert!(
        response
            .message
            .contains("node s2: machine-a-tron mat-b owns rack"),
        "{}",
        response.message
    );

    mat_b.set_available(true);
    wait_until("mat-b routes again", || async {
        fabric_status(&mut rms_client(listen).await, "rack-002", SWITCH_B)
            .await
            .is_ok()
    })
    .await;

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}

/// An instance that is down when the gateway starts holds `/readyz` and routing, naming itself,
/// only until `stale_after`; then the rest of the fleet is served and its racks are unknown until
/// it answers.
#[tokio::test]
async fn a_source_down_at_startup_blocks_readiness_only_until_stale_after() {
    let mat_a =
        FakeMachineATron::start_with_racks("mat-a", &[GUID_A], &["rack-001"], devices_a()).await;
    let mat_b =
        FakeMachineATron::start_with_racks("mat-b", &[GUID_B], &["rack-002"], devices_b()).await;
    mat_b.set_available(false);
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let mut config = gateway_config(controller.serve().await, listen);
    // The pending state is asserted before it expires, so give the assertion room on a loaded host.
    config.ownership.stale_after = Duration::from_secs(2);
    config.validate().unwrap();
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());

    wait_until("the gateway names the pending source", || async {
        readyz(&http, listen)
            .await
            .is_some_and(|(_, body)| body.starts_with("no rack status"))
    })
    .await;
    let (status, body) = readyz(&http, listen).await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.starts_with("no rack status from source mat-b yet: GET http://"),
        "{body}"
    );
    let mut rms = rms_client(listen).await;
    assert_eq!(
        fabric_status(&mut rms, "rack-001", SWITCH_A)
            .await
            .unwrap_err()
            .code(),
        Code::Unavailable
    );

    wait_until_ready(&http, listen).await;
    assert_eq!(
        fabric_status(&mut rms, "rack-001", SWITCH_A).await.unwrap(),
        ""
    );
    let status = fabric_status(&mut rms, "rack-002", SWITCH_B)
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::NotFound, "{status}");

    mat_b.set_available(true);
    wait_until("mat-b's rack is owned", || async {
        fabric_status(&mut rms_client(listen).await, "rack-002", SWITCH_B)
            .await
            .is_ok()
    })
    .await;

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}
