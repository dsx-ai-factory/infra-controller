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

//! The power, lifecycle, firmware and switch image RPCs through `run`: node batches split by
//! rack owner, the firmware catalogue merged over every instance, and jobs polled through the
//! gateway ids the applies hand out.

use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager::{
    ApplyFirmwareObjectRequest, ApplySwitchSystemImageRequest, BatchGetPowerStateRequest,
    BatchResetSwitchFactoryDefaultRequest, BatchSetPowerStateRequest, FirmwareJobState,
    GetFirmwareJobStatusRequest, GetJobStatusRequest, GetSwitchSystemImageJobStatusRequest,
    JobError, JobExecutionState, ListFirmwareObjectsRequest, NodeInfo, PowerOperation, ReturnCode,
    UpdateSwitchSystemPasswordRequest,
};
use mat_protocol_gateway::ExitReason;
use rms_mock::{FaultConfig, RmsMockConfig};
use tokio_util::sync::CancellationToken;
use tonic::Code;
use tonic::transport::Channel;

use crate::common::{
    FakeController, FakeMachineATron, GUID_A, GUID_B, SWITCH_A, SWITCH_B, TRAY_A, counts,
    devices_a, devices_b, finished, free_loopback_address, gateway_config, node, nodes, rms_client,
    spawn_run, wait_until_ready,
};

fn s1() -> NodeInfo {
    node("s1", "rack-001", SWITCH_A)
}

fn t1() -> NodeInfo {
    node("t1", "rack-001", TRAY_A)
}

fn s2() -> NodeInfo {
    node("s2", "rack-002", SWITCH_B)
}

/// `(node id, pstate)` as `BatchGetPowerState` reports `targets`, in the order reported.
async fn power_states(
    rms: &mut RackManagerClient<Channel>,
    targets: Vec<NodeInfo>,
) -> Vec<(String, String)> {
    let response = rms
        .batch_get_power_state(BatchGetPowerStateRequest {
            nodes: nodes(targets),
        })
        .await
        .unwrap()
        .into_inner();
    let batch = response.response.unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert_eq!(batch.job_id, "", "reading power issues no job");
    response
        .node_power_states
        .into_iter()
        .map(|state| (state.node_id, state.pstate))
        .collect()
}

/// A power batch is read across both instances in request order and changed on each node's own
/// instance, and an operation both instances refuse is refused; a lifecycle batch split across
/// instances gets one aggregate parent that polls through `GetJobStatus` to the failure of the
/// one part that fails, naming its instance, and one instance's batch gets that instance's
/// parent, whose children are translated too.
#[tokio::test]
async fn power_and_lifecycle_batches_are_split_by_rack_and_their_parents_poll_through_gateway_ids()
{
    let mat_a =
        FakeMachineATron::start_with_racks("mat-a", &[GUID_A], &["rack-001"], devices_a()).await;
    // s2's jobs fail on their instance, so a batch spanning both instances has one failed part.
    let mat_b = FakeMachineATron::start_with_rms(
        "mat-b",
        &[GUID_B],
        &["rack-002"],
        devices_b(),
        RmsMockConfig {
            faults: FaultConfig {
                fail_jobs_for_node_ids: vec!["s2".to_string()],
            },
            ..RmsMockConfig::default()
        },
    )
    .await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let config = gateway_config(controller.serve().await, listen);
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());
    wait_until_ready(&http, listen).await;
    let mut rms = rms_client(listen).await;

    let on = |node_id: &str| (node_id.to_string(), "ON".to_string());
    let off = |node_id: &str| (node_id.to_string(), "OFF".to_string());
    assert_eq!(
        power_states(&mut rms, vec![t1(), s2(), s1()]).await,
        vec![on("t1"), on("s2"), on("s1")],
        "states come back in request order, not grouped by instance"
    );
    let batch = rms
        .batch_set_power_state(BatchSetPowerStateRequest {
            nodes: nodes(vec![s2(), t1()]),
            operation: PowerOperation::Off as i32,
        })
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert_eq!(counts(batch.stats.as_ref()), (2, 2, 0));
    assert_eq!(
        power_states(&mut rms, vec![t1(), s2(), s1()]).await,
        vec![off("t1"), off("s2"), on("s1")],
        "each instance powered off its own node and nothing else"
    );
    let refused = rms
        .batch_set_power_state(BatchSetPowerStateRequest {
            nodes: nodes(vec![s2(), t1()]),
            operation: PowerOperation::Unspecified as i32,
        })
        .await
        .map(drop)
        .unwrap_err();
    assert_eq!(
        (refused.code(), refused.message()),
        (
            Code::InvalidArgument,
            "machine-a-tron mat-b: power operation is unspecified"
        ),
        "an operation every instance refuses is refused, not failed per node"
    );

    // A factory reset across both instances: the batch id is an aggregate of the two parents,
    // and s2's part fails on its second poll.
    let batch = rms
        .batch_reset_switch_factory_default(BatchResetSwitchFactoryDefaultRequest {
            nodes: nodes(vec![s1(), s2()]),
            domain: None,
        })
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert_eq!(counts(batch.stats.as_ref()), (2, 2, 0));
    assert!(batch.job_id.starts_with("gw-"), "{}", batch.job_id);
    for expected in [JobExecutionState::Running, JobExecutionState::Failed] {
        let states = rms
            .get_job_status(GetJobStatusRequest {
                job_id: batch.job_id.clone(),
                include_child_job_states: false,
            })
            .await
            .unwrap()
            .into_inner()
            .job_states;
        assert_eq!(states.len(), 1, "{states:?}");
        assert_eq!(states[0].job_id, batch.job_id);
        assert_eq!(states[0].child_job_ids.len(), 2, "{states:?}");
        assert_eq!(states[0].execution_state, expected as i32, "{states:?}");
        assert_eq!(
            (states[0].rack_id.as_deref(), states[0].node_id.as_deref()),
            (None, None),
            "an aggregate spans racks: {states:?}"
        );
        if expected == JobExecutionState::Failed {
            assert_eq!(states[0].error_code, JobError::Other as i32, "{states:?}");
            assert!(
                states[0]
                    .error_message
                    .starts_with("machine-a-tron mat-b: ")
                    && states[0].error_message.contains("s2"),
                "the failed part decides the aggregate and its instance is named: {states:?}"
            );
        } else {
            assert_eq!(states[0].error_message, "", "{states:?}");
        }
    }

    // A password change on one instance's switch: the batch id is that instance's parent, and
    // its child is reported under a gateway id too.
    let batch = rms
        .update_switch_system_password(UpdateSwitchSystemPasswordRequest {
            nodes: nodes(vec![s1()]),
            username: "admin".to_string(),
            password: "rotated".to_string(),
        })
        .await
        .unwrap()
        .into_inner()
        .response
        .unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert!(batch.job_id.starts_with("gw-"), "{}", batch.job_id);
    let states = rms
        .get_job_status(GetJobStatusRequest {
            job_id: batch.job_id.clone(),
            include_child_job_states: true,
        })
        .await
        .unwrap()
        .into_inner()
        .job_states;
    assert_eq!(states.len(), 2, "the parent and its one child: {states:?}");
    assert_eq!(states[0].job_id, batch.job_id);
    assert_eq!(states[0].child_job_ids, vec![states[1].job_id.clone()]);
    assert!(states[1].job_id.starts_with("gw-"), "{states:?}");
    assert_eq!(
        states[1].parent_job_id.as_deref(),
        Some(batch.job_id.as_str()),
        "{states:?}"
    );
    assert_eq!(states[1].node_id.as_deref(), Some("s1"), "{states:?}");

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}

/// The firmware catalogue is the union over the instances, each id once; a firmware apply on
/// one rack hands out gateway ids for the parent and every node, and a node's job polls to
/// completion through `GetFirmwareJobStatus` with the gateway id echoed; a switch image apply
/// does the same through its own status RPC.
#[tokio::test]
async fn firmware_and_switch_image_applies_hand_out_gateway_jobs_and_the_catalogue_is_the_union() {
    let catalogue = |ids: &[&str]| RmsMockConfig {
        firmware_object_ids: ids.iter().map(ToString::to_string).collect(),
        ..RmsMockConfig::default()
    };
    let mat_a = FakeMachineATron::start_with_rms(
        "mat-a",
        &[GUID_A],
        &["rack-001"],
        devices_a(),
        catalogue(&["fw-shared", "fw-a"]),
    )
    .await;
    let mat_b = FakeMachineATron::start_with_rms(
        "mat-b",
        &[GUID_B],
        &["rack-002"],
        devices_b(),
        catalogue(&["fw-b", "fw-shared"]),
    )
    .await;
    let controller = FakeController::new(vec![mat_a.source(), mat_b.source()], 1);
    let listen = free_loopback_address().await;
    let config = gateway_config(controller.serve().await, listen);
    let http = reqwest::Client::new();
    let shutdown = CancellationToken::new();
    let running = spawn_run(config, shutdown.clone());
    wait_until_ready(&http, listen).await;
    let mut rms = rms_client(listen).await;

    let objects = rms
        .list_firmware_objects(ListFirmwareObjectsRequest::default())
        .await
        .unwrap()
        .into_inner()
        .objects;
    assert_eq!(
        objects.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["fw-shared", "fw-a", "fw-b"],
        "instances in name order, an id seen again kept once"
    );

    // A firmware apply on rack-001 for both of its nodes.
    let response = rms
        .apply_firmware_object(ApplyFirmwareObjectRequest {
            rack_id: "rack-001".to_string(),
            config_json: r#"{"Artifacts":[]}"#.to_string(),
            nodes: nodes(vec![t1(), s1()]),
            ..ApplyFirmwareObjectRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let batch = response.response.unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert_eq!(counts(batch.stats.as_ref()), (2, 2, 0));
    assert!(batch.job_id.starts_with("gw-"), "{}", batch.job_id);
    assert_eq!(
        response.object_id, "fw-shared",
        "the owning instance's first configured object when the document names none"
    );
    let jobs: Vec<(&str, &str)> = response
        .jobs
        .iter()
        .map(|job| (job.node_id.as_str(), job.job_id.as_str()))
        .collect();
    assert_eq!(
        jobs.iter().map(|(node_id, _)| *node_id).collect::<Vec<_>>(),
        ["t1", "s1"]
    );
    assert!(
        jobs.iter()
            .all(|(_, job_id)| job_id.starts_with("gw-") && *job_id != batch.job_id),
        "every node job is a gateway id of its own: {jobs:?}"
    );
    let node_job = jobs[0].1.to_string();
    for expected in [FirmwareJobState::Running, FirmwareJobState::Completed] {
        let status = rms
            .get_firmware_job_status(GetFirmwareJobStatusRequest {
                job_id: node_job.clone(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.status, ReturnCode::Success as i32, "{status:?}");
        assert_eq!(status.job_id, node_job, "the gateway id is echoed");
        assert_eq!(status.job_state, expected as i32, "{status:?}");
        assert_eq!(
            (status.rack_id.as_str(), status.node_id.as_str()),
            ("rack-001", "t1")
        );
    }
    let status = rms
        .get_firmware_job_status(GetFirmwareJobStatusRequest {
            job_id: "gw-0-999".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (
            status.status,
            status.job_id.as_str(),
            status.error_message.as_str()
        ),
        (
            ReturnCode::Failure as i32,
            "gw-0-999",
            "job gw-0-999 not found"
        ),
        "an id from before a restart is answered as the mock answers an id it never issued"
    );
    // The instance restarts and forgets the job; the gateway still knows the id and answers the
    // instance's not-found with the gateway id, never the instance's.
    mat_a.restart(&[GUID_A]);
    let status = rms
        .get_firmware_job_status(GetFirmwareJobStatusRequest {
            job_id: node_job.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (
            status.status,
            status.job_id.as_str(),
            status.error_message.as_str()
        ),
        (
            ReturnCode::Failure as i32,
            node_job.as_str(),
            format!("job {node_job} not found").as_str()
        ),
        "{status:?}"
    );

    // A switch image apply on rack-002, whose object the document names.
    let response = rms
        .apply_switch_system_image(ApplySwitchSystemImageRequest {
            rack_id: "rack-002".to_string(),
            config_json: r#"{"Id":"nvos-1"}"#.to_string(),
            nodes: nodes(vec![s2()]),
            ..ApplySwitchSystemImageRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let batch = response.response.unwrap();
    assert_eq!(batch.status, ReturnCode::Success as i32, "{batch:?}");
    assert!(batch.job_id.starts_with("gw-"), "{}", batch.job_id);
    assert_eq!(
        (
            response.object_id.as_str(),
            response.image_filename.as_str()
        ),
        ("nvos-1", "nvos-1-nvos.bin")
    );
    assert_eq!(response.jobs.len(), 1, "{:?}", response.jobs);
    let switch_job = &response.jobs[0];
    assert_eq!(switch_job.node_id, "s2");
    assert!(switch_job.job_id.starts_with("gw-"), "{switch_job:?}");
    let status = rms
        .get_switch_system_image_job_status(GetSwitchSystemImageJobStatusRequest {
            job_id: switch_job.job_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.status, ReturnCode::Success as i32, "{status:?}");
    assert_eq!(status.job_id, switch_job.job_id, "the gateway id is echoed");
    assert_eq!(
        (status.state.as_str(), status.node_id.as_str()),
        ("running", "s2"),
        "{status:?}"
    );

    shutdown.cancel();
    assert_eq!(finished(running).await, ExitReason::Shutdown);
}
