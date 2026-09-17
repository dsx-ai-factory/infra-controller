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

//! Wire-level tests.
//!
//! These drive the mock through a real gRPC client over a real socket,
//! rather than calling the trait methods directly, because the thing most
//! likely to break is the transport: codec, HTTP/2 framing, and the router
//! paths the services are mounted on. A test that called the impl directly
//! would pass even if nothing were reachable.

use std::net::IpAddr;
use std::sync::Arc;

use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager_v2::rack_manager_v2_client::RackManagerV2Client;
use mac_address::MacAddress;
use rms_mock::{RmsMock, RmsMockConfig, SimNode, SimNodeKind, StaticInventory};

/// An NVLink switch tray in rack unit 30 of rack-001.
fn a_switch() -> SimNode {
    SimNode {
        kind: Some(SimNodeKind::Switch),
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0x11, 0x11, 0x22, 0x22])),
        rack_id: Some("rack-001".to_string()),
        slot_number: Some(30),
        ..SimNode::default()
    }
}

/// A second switch tray in the same rack as [`a_switch`], lower in it.
fn a_second_switch() -> SimNode {
    SimNode {
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0x11, 0x11, 0x33, 0x33])),
        slot_number: Some(20),
        ..a_switch()
    }
}

/// A compute tray in slot 12 of rack-001, the third tray in its rack.
fn a_tray() -> SimNode {
    SimNode {
        kind: Some(SimNodeKind::Compute),
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0xab, 0xcd, 0x12, 0x34])),
        bmc_ip: Some(IpAddr::from([10, 233, 16, 20])),
        rack_id: Some("rack-001".to_string()),
        slot_number: Some(12),
        tray_index: Some(2),
        ..SimNode::default()
    }
}

/// Serve the mock on an ephemeral port and return its base URL.
async fn serve() -> String {
    serve_with(Vec::new()).await
}

async fn serve_with(nodes: Vec<SimNode>) -> String {
    let mock = Arc::new(RmsMock::new(
        Arc::new(StaticInventory::new(nodes.into())),
        RmsMockConfig::default(),
    ));
    let router = rms_mock::router(mock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn get_version_answers_an_unmodified_client() {
    let url = serve().await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .get_version(librms::protos::rack_manager::GetVersionRequest {})
        .await
        .expect("GetVersion is the client's connection probe and must succeed");

    assert!(
        response
            .into_inner()
            .version
            .starts_with("machine-a-tron-rms-mock/"),
        "version should identify the mock"
    );
}

#[tokio::test]
async fn out_of_scope_methods_report_unimplemented() {
    let url = serve().await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let status = client
        .list_racks(librms::protos::rack_manager::ListRacksRequest::default())
        .await
        .expect_err("list_racks is out of scope");

    assert_eq!(status.code(), tonic::Code::Unimplemented);
}

#[tokio::test]
async fn both_services_are_mounted() {
    // A missing service registration surfaces as a router 404, which tonic
    // also reports as `Unimplemented`, so each service is checked by a call
    // only its own handler could answer.
    let url = serve_with(vec![a_switch()]).await;

    let mut v1 = RackManagerClient::connect(url.clone()).await.unwrap();
    let status = v1
        .list_racks(librms::protos::rack_manager::ListRacksRequest::default())
        .await
        .expect_err("list_racks is out of scope");
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(
        status.message().contains("machine-a-tron RMS mock"),
        "expected the mock's own handler, not a router 404; got: {}",
        status.message()
    );

    let mut v2 = RackManagerV2Client::connect(url).await.unwrap();
    let response = v2
        .configure_scale_up_fabric_manager(
            librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
                }),
                primary_switch_node_id: None,
                domain: None,
                config: Some(fabric_config()),
            },
        )
        .await
        .expect("V2 is a separately registered service and must route");
    assert!(!response.into_inner().job_id.is_empty());
}

/// The fabric configuration NICo sends.
fn fabric_config() -> librms::protos::rack_manager_v2::ScaleUpFabricConfig {
    librms::protos::rack_manager_v2::ScaleUpFabricConfig {
        topology_type: "nvl72".to_string(),
        extra_static_configs: Vec::new(),
    }
}

/// Build a request for one node, identified the way carbide identifies nodes:
/// an opaque `node_id` plus a BMC MAC.
fn node_info(node_id: &str, mac: &str) -> librms::protos::rack_manager::NodeInfo {
    node_info_in_rack("rack-001", node_id, mac)
}

fn node_info_in_rack(
    rack_id: &str,
    node_id: &str,
    mac: &str,
) -> librms::protos::rack_manager::NodeInfo {
    librms::protos::rack_manager::NodeInfo {
        node_id: node_id.to_string(),
        rack_id: rack_id.to_string(),
        // Left unset: the mock matches on address, not on declared type.
        r#type: None,
        bmc_endpoint: Some(librms::protos::rack_manager::Endpoint {
            interface: Some(librms::protos::rack_manager::NetworkInterface {
                ip_address: String::new(),
                mac_address: mac.to_string(),
                host_name: None,
            }),
            port: 443,
            credentials: None,
        }),
        host_endpoint: None,
        node_descriptor: None,
    }
}

/// Placement is whatever the inventory holds for the node: the caller keys
/// the answer by its own node id and stores the two numbers as they come.
#[tokio::test]
async fn device_info_reports_placement_from_the_inventory() {
    let url = serve_with(vec![a_tray()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    // A different separator and case than the inventory holds, because callers
    // send whatever their own records contain.
    let response = client
        .batch_get_node_device_info(
            librms::protos::rack_manager::BatchGetNodeDeviceInfoRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("carbide-row-77", "02:00:AB:CD:12:34")],
                }),
            },
        )
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        response.status,
        librms::protos::rack_manager::ReturnCode::Success as i32
    );
    let details = &response.node_device_details;
    assert_eq!(details.len(), 1);
    assert_eq!(
        details[0].node_id, "carbide-row-77",
        "node_id must be echoed verbatim"
    );
    assert_eq!(details[0].slot_number, Some(12));
    assert_eq!(details[0].tray_index, Some(2));
    let stats = response
        .stats
        .expect("stats drive the caller's success check");
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (1, 1, 0)
    );
}

/// One known node and one the inventory does not have. The proto says the
/// batch fails when any node does and lists only the nodes that were found;
/// the caller reads the batch message as the unknown node's error, and a
/// placeholder entry with every field unset would be stored as a placement.
#[tokio::test]
async fn device_info_fails_the_batch_for_an_unmatched_node() {
    let url = serve_with(vec![a_tray()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .batch_get_node_device_info(
            librms::protos::rack_manager::BatchGetNodeDeviceInfoRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![
                        node_info("known", "02:00:AB:CD:12:34"),
                        node_info("stranger", "02:00:00:00:00:99"),
                    ],
                }),
            },
        )
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        response.status,
        librms::protos::rack_manager::ReturnCode::Failure as i32
    );
    assert!(
        response.message.contains("stranger"),
        "the batch message names the node that was not found: {:?}",
        response.message
    );
    let known: Vec<&str> = response
        .node_device_details
        .iter()
        .map(|d| d.node_id.as_str())
        .collect();
    assert_eq!(known, ["known"], "only found nodes are listed");
    assert_eq!(response.node_device_details[0].slot_number, Some(12));
    let stats = response.stats.unwrap();
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (2, 1, 1)
    );
}

/// A certificate job NICo can poll: a per-node job id and a batch whose
/// status, node results and stats agree.
#[tokio::test]
async fn configure_switch_certificate_returns_a_usable_job() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let response = client
        .configure_switch_certificate(
            librms::protos::rack_manager::ConfigureSwitchCertificateRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
                }),
                services: Vec::new(),
                test_hello: false,
                domain: Some("site.example.com".to_string()),
            },
        )
        .await
        .unwrap()
        .into_inner();

    let batch = response
        .response
        .expect("a missing batch is read as failure");
    assert_eq!(
        batch.status,
        librms::protos::rack_manager::ReturnCode::Success as i32
    );
    assert_eq!(batch.stats.as_ref().unwrap().failed_nodes, 0);
    assert!(
        batch
            .node_results
            .iter()
            .all(|r| r.status == librms::protos::rack_manager::ReturnCode::Success as i32),
        "a single non-success node result fails the whole batch"
    );

    let job = response
        .jobs
        .iter()
        .find(|j| j.node_id == "switch-7")
        .expect("NodeId must match the request or NICo cannot find its job");
    assert!(
        !job.job_id.is_empty(),
        "an empty job id aborts configuration"
    );
}

/// Certificate jobs reach `completed`, spelled as the caller's vocabulary
/// expects.
#[tokio::test]
async fn certificate_jobs_progress_to_completed() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let started = client
        .configure_switch_certificate(
            librms::protos::rack_manager::ConfigureSwitchCertificateRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
                }),
                services: Vec::new(),
                test_hello: false,
                domain: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    let job_id = started.jobs[0].job_id.clone();

    let mut seen = Vec::new();
    for _ in 0..5 {
        let status = client
            .get_configure_switch_certificate_job_status(
                librms::protos::rack_manager::GetConfigureSwitchCertificateJobStatusRequest {
                    job_id: job_id.clone(),
                },
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status.job_id, job_id);
        seen.push(status.state.clone());
        if status.state == "completed" {
            break;
        }
    }

    assert!(
        seen.iter().any(|s| s == "completed"),
        "job never completed; polled states were {seen:?}"
    );
    // Every state reported must be one the caller actually maps.
    for state in &seen {
        assert!(
            matches!(state.as_str(), "running" | "completed"),
            "{state:?} is outside the vocabulary NICo maps, so it would poll forever"
        );
    }
}

/// A poll for a job this process never issued reports it completed on both
/// job-status RPCs.
#[tokio::test]
async fn an_unknown_job_is_reported_completed() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();
    let job_id = "rms-mock-from-a-past-life".to_string();

    let certificate = client
        .get_configure_switch_certificate_job_status(
            librms::protos::rack_manager::GetConfigureSwitchCertificateJobStatusRequest {
                job_id: job_id.clone(),
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(certificate.job_id, job_id);
    assert_eq!(
        certificate.status,
        librms::protos::rack_manager::ReturnCode::Success as i32
    );
    assert_eq!(certificate.state, "completed");

    let fabric = client
        .get_job_status(librms::protos::rack_manager::GetJobStatusRequest {
            job_id: job_id.clone(),
            include_child_job_states: false,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fabric.job_states[0].job_id, job_id);
    assert_eq!(
        fabric.job_states[0].execution_state,
        librms::protos::rack_manager::JobExecutionState::Completed as i32
    );
}

/// A request naming no switches is `INVALID_ARGUMENT`, and no job is started
/// for it.
#[tokio::test]
async fn a_fabric_request_naming_no_switches_is_an_invalid_argument() {
    let url = serve_with(vec![a_switch()]).await;
    let mut v2 = RackManagerV2Client::connect(url.clone()).await.unwrap();
    let mut v1 = RackManagerClient::connect(url).await.unwrap();

    let switches = || librms::protos::rack_manager::NodeSet {
        nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
    };
    let request = |nodes| librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
        nodes,
        primary_switch_node_id: None,
        domain: None,
        config: Some(fabric_config()),
    };

    for (what, bad) in [
        ("no nodes", request(None)),
        (
            "empty node set",
            request(Some(librms::protos::rack_manager::NodeSet {
                nodes: vec![],
            })),
        ),
    ] {
        let status = v2
            .configure_scale_up_fabric_manager(bad)
            .await
            .expect_err(what);
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{what}");
    }

    // Nothing was issued: the first accepted request gets the first id.
    let job_id = v2
        .configure_scale_up_fabric_manager(request(Some(switches())))
        .await
        .unwrap()
        .into_inner()
        .job_id;
    assert_eq!(job_id, "rms-mock-1");
    let status = v1
        .get_job_status(librms::protos::rack_manager::GetJobStatusRequest {
            job_id: job_id.clone(),
            include_child_job_states: false,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(status.job_states[0].job_id, job_id);
}

/// The fabric job must be pollable through the V1 job-status RPC, echo the id
/// that was asked for, and never report the proto3 default execution state.
#[tokio::test]
async fn fabric_jobs_are_pollable_and_reach_completed() {
    let url = serve_with(vec![a_switch()]).await;
    let mut v2 = RackManagerV2Client::connect(url.clone()).await.unwrap();
    let mut v1 = RackManagerClient::connect(url).await.unwrap();

    let job_id = v2
        .configure_scale_up_fabric_manager(
            librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
                }),
                primary_switch_node_id: None,
                domain: None,
                config: Some(fabric_config()),
            },
        )
        .await
        .unwrap()
        .into_inner()
        .job_id;

    let mut final_state = 0;
    for _ in 0..5 {
        let response = v1
            .get_job_status(librms::protos::rack_manager::GetJobStatusRequest {
                job_id: job_id.clone(),
                include_child_job_states: true,
            })
            .await
            .unwrap()
            .into_inner();

        let job = response
            .job_states
            .iter()
            .find(|j| j.job_id == job_id)
            .expect("the caller finds its job by id and gives up if it is absent");

        assert_ne!(
            job.execution_state,
            librms::protos::rack_manager::JobExecutionState::Unspecified as i32,
            "an unset execution state is read as an unknown outcome"
        );
        final_state = job.execution_state;
        if final_state == librms::protos::rack_manager::JobExecutionState::Completed as i32 {
            break;
        }
    }

    assert_eq!(
        final_state,
        librms::protos::rack_manager::JobExecutionState::Completed as i32,
        "the fabric job never completed, so the rack would never reach ready"
    );
}

/// A fabric request whose switches all miss the inventory is accepted and its
/// job fails naming them; one matched switch is enough to complete.
#[tokio::test]
async fn a_fabric_request_matching_no_switch_returns_a_job_that_fails() {
    use librms::protos::rack_manager::JobExecutionState;

    struct Case {
        scenario: &'static str,
        nodes: Vec<librms::protos::rack_manager::NodeInfo>,
        terminal: JobExecutionState,
        /// Node ids the failure must name; empty for a job that completes.
        named: &'static [&'static str],
    }
    let cases = [
        Case {
            scenario: "no switch matched",
            nodes: vec![
                node_info("switch-1", "02:00:00:00:00:98"),
                node_info("switch-2", "02:00:00:00:00:99"),
            ],
            terminal: JobExecutionState::Failed,
            named: &["switch-1", "switch-2"],
        },
        Case {
            scenario: "one switch matched",
            nodes: vec![
                node_info("switch-1", "02:00:00:00:00:98"),
                node_info("switch-7", "02:00:11:11:22:22"),
            ],
            terminal: JobExecutionState::Completed,
            named: &[],
        },
    ];

    for case in cases {
        let url = serve_with(vec![a_switch()]).await;
        let mut v2 = RackManagerV2Client::connect(url.clone()).await.unwrap();
        let mut v1 = RackManagerClient::connect(url).await.unwrap();

        let job_id = v2
            .configure_scale_up_fabric_manager(
                librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
                    nodes: Some(librms::protos::rack_manager::NodeSet { nodes: case.nodes }),
                    primary_switch_node_id: None,
                    domain: None,
                    config: Some(fabric_config()),
                },
            )
            .await
            .expect(case.scenario)
            .into_inner()
            .job_id;
        assert!(!job_id.is_empty(), "{}", case.scenario);

        let mut polls = Vec::new();
        for _ in 0..3 {
            let response = v1
                .get_job_status(librms::protos::rack_manager::GetJobStatusRequest {
                    job_id: job_id.clone(),
                    include_child_job_states: false,
                })
                .await
                .unwrap()
                .into_inner();
            let job = response
                .job_states
                .into_iter()
                .find(|j| j.job_id == job_id)
                .expect(case.scenario);
            polls.push((job.execution_state, job.error_message));
        }

        // Paced like a completing job; the reason appears only once failed.
        let states: Vec<i32> = polls.iter().map(|(state, _)| *state).collect();
        assert_eq!(
            states,
            [
                JobExecutionState::Running as i32,
                case.terminal as i32,
                case.terminal as i32,
            ],
            "{}",
            case.scenario
        );
        assert!(
            polls[0].1.is_empty(),
            "{}: a job that has not ended has no error",
            case.scenario
        );
        for (_, error) in &polls[1..] {
            assert_eq!(
                error.is_empty(),
                case.named.is_empty(),
                "{}: {error:?}",
                case.scenario
            );
            for name in case.named {
                assert!(
                    error.contains(name),
                    "{}: {error:?} does not name {name}",
                    case.scenario
                );
            }
        }
    }
}

/// Reading the fabric back after configuration finds exactly one enabled
/// switch per rack.
#[tokio::test]
async fn fabric_status_reports_exactly_one_primary_per_configured_rack() {
    let url = serve_with(vec![a_switch(), a_second_switch()]).await;
    let mut v2 = RackManagerV2Client::connect(url.clone()).await.unwrap();
    let mut v1 = RackManagerClient::connect(url).await.unwrap();

    let node_set = || librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info("switch-7", "02:00:11:11:22:22"),
            node_info("switch-8", "02:00:11:11:33:33"),
        ],
    };
    let enabled_switches =
        |response: librms::protos::rack_manager::GetScaleUpFabricStatusResponse| {
            response
                .fabric_status
                .unwrap()
                .switches
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| s.node_id)
                .collect::<Vec<_>>()
        };

    // No primary requested: the switch lowest in the rack is elected.
    v2.configure_scale_up_fabric_manager(
        librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
            nodes: Some(node_set()),
            primary_switch_node_id: None,
            domain: None,
            config: Some(fabric_config()),
        },
    )
    .await
    .unwrap();
    let status = v1
        .get_scale_up_fabric_status(
            librms::protos::rack_manager::GetScaleUpFabricStatusRequest {
                nodes: Some(node_set()),
                domain: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        enabled_switches(status),
        vec!["switch-8".to_string()],
        "more or fewer than one enabled switch is read as no/multiple primaries"
    );

    // A requested primary in the rack replaces the elected one.
    v2.configure_scale_up_fabric_manager(
        librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
            nodes: Some(node_set()),
            primary_switch_node_id: Some("switch-7".to_string()),
            domain: None,
            config: Some(fabric_config()),
        },
    )
    .await
    .unwrap();
    let status = v1
        .get_scale_up_fabric_status(
            librms::protos::rack_manager::GetScaleUpFabricStatusRequest {
                nodes: Some(node_set()),
                domain: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(enabled_switches(status), vec!["switch-7".to_string()]);
}

/// A switch the mock has no device for is never elected primary, however it
/// sorts or is requested.
#[tokio::test]
async fn an_unmatched_switch_is_never_elected_primary() {
    // "switch-1" sorts before "switch-7" but matches no simulated device.
    let node_set = || librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info("switch-1", "02:00:00:00:00:99"),
            node_info("switch-7", "02:00:11:11:22:22"),
        ],
    };
    let read = |url: String| async move {
        RackManagerClient::connect(url)
            .await
            .unwrap()
            .get_scale_up_fabric_status(
                librms::protos::rack_manager::GetScaleUpFabricStatusRequest {
                    nodes: Some(node_set()),
                    domain: None,
                },
            )
            .await
            .unwrap()
            .into_inner()
            .fabric_status
            .unwrap()
            .switches
    };
    let enabled = |switches: &[librms::protos::rack_manager::ScaleUpFabricSwitchStatus]| {
        switches
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.node_id.clone())
            .collect::<Vec<_>>()
    };

    // Through configuration, with and without the unmatched switch requested.
    let url = serve_with(vec![a_switch()]).await;
    let mut v2 = RackManagerV2Client::connect(url.clone()).await.unwrap();
    for requested in [None, Some("switch-1".to_string())] {
        v2.configure_scale_up_fabric_manager(
            librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
                nodes: Some(node_set()),
                primary_switch_node_id: requested.clone(),
                domain: None,
                config: Some(fabric_config()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            enabled(&read(url.clone()).await),
            vec!["switch-7".to_string()],
            "requested primary {requested:?}"
        );
    }

    // Through a status read that elects for a rack never configured here.
    let switches = read(serve_with(vec![a_switch()]).await).await;
    assert_eq!(enabled(&switches), vec!["switch-7".to_string()]);
    let stranger = switches.iter().find(|s| s.node_id == "switch-1").unwrap();
    assert!(
        !stranger.error_message.is_empty(),
        "a switch that could not be inspected carries the reason"
    );
    assert!(stranger.fabric_manager_status.is_empty());
}

/// An unmatched node is a per-node failure on every batch RPC: the batch
/// fails, the result says why, the stats count it, and no job is issued.
#[tokio::test]
async fn an_unmatched_node_fails_per_node_on_every_batch_rpc() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();
    let node_set = || librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info("switch-7", "02:00:11:11:22:22"),
            node_info("stranger", "02:00:00:00:00:99"),
        ],
    };
    fn per_node(batch: &librms::protos::rack_manager::NodeBatchResponse) -> Vec<(&str, i32, bool)> {
        batch
            .node_results
            .iter()
            .map(|r| (r.node_id.as_str(), r.status, r.error_message.is_empty()))
            .collect()
    }
    let success = librms::protos::rack_manager::ReturnCode::Success as i32;
    let failure = librms::protos::rack_manager::ReturnCode::Failure as i32;

    let certificate = client
        .configure_switch_certificate(
            librms::protos::rack_manager::ConfigureSwitchCertificateRequest {
                nodes: Some(node_set()),
                services: Vec::new(),
                test_hello: false,
                domain: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    let job_nodes: Vec<&str> = certificate
        .jobs
        .iter()
        .map(|j| j.node_id.as_str())
        .collect();
    assert_eq!(
        job_nodes,
        ["switch-7"],
        "no job is issued for a node without a device"
    );

    let batch = certificate.response.unwrap();
    assert_eq!(batch.status, failure, "one failed node fails the batch");
    assert!(batch.message.contains("stranger"), "{:?}", batch.message);
    assert_eq!(
        per_node(&batch),
        [("switch-7", success, true), ("stranger", failure, false)],
        "results stay aligned with the request and the failure says why"
    );
    let stats = batch.stats.unwrap();
    assert_eq!(
        (
            stats.total_nodes,
            stats.successful_nodes,
            stats.failed_nodes
        ),
        (2, 1, 1)
    );

    let services = client
        .batch_get_scale_up_fabric_service_status(
            librms::protos::rack_manager::BatchGetScaleUpFabricServiceStatusRequest {
                nodes: Some(node_set()),
            },
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(services.status, failure);
    assert_eq!(services.stats.unwrap().failed_nodes, 1);
    let stranger = &services.service_statuses["stranger"];
    assert!(stranger.status_json.is_empty() && !stranger.error_message.is_empty());
    assert!(
        services.service_statuses["switch-7"]
            .error_message
            .is_empty()
    );
}

/// Reading a rack never configured here elects a primary, and only the
/// primary reports a configured control plane.
#[tokio::test]
async fn fabric_status_elects_a_primary_without_prior_configuration() {
    let url = serve_with(vec![a_switch(), a_second_switch()]).await;
    let mut v1 = RackManagerClient::connect(url).await.unwrap();

    let node_set = || librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info("switch-7", "02:00:11:11:22:22"),
            node_info("switch-8", "02:00:11:11:33:33"),
        ],
    };

    let status = v1
        .get_scale_up_fabric_status(
            librms::protos::rack_manager::GetScaleUpFabricStatusRequest {
                nodes: Some(node_set()),
                domain: None,
            },
        )
        .await
        .unwrap()
        .into_inner();
    let enabled: Vec<String> = status
        .fabric_status
        .unwrap()
        .switches
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| s.node_id)
        .collect();
    assert_eq!(
        enabled.len(),
        1,
        "a rack read before any configuration must still show one primary"
    );

    let services = v1
        .batch_get_scale_up_fabric_service_status(
            librms::protos::rack_manager::BatchGetScaleUpFabricServiceStatusRequest {
                nodes: Some(node_set()),
            },
        )
        .await
        .unwrap()
        .into_inner();
    let configured: Vec<String> = services
        .service_statuses
        .into_iter()
        .filter(|(_, entry)| entry.status_json.contains("CONTROL_PLANE_STATE_CONFIGURED"))
        .map(|(node_id, _)| node_id)
        .collect();
    assert_eq!(
        configured, enabled,
        "the switch reporting a configured control plane must be the enabled primary"
    );
}

/// The per-switch health JSON carries a `status` the caller recognises.
#[tokio::test]
async fn fabric_service_health_is_a_status_the_caller_recognises() {
    let url = serve_with(vec![a_switch()]).await;
    let mut client = RackManagerClient::connect(url).await.unwrap();

    let services = client
        .batch_get_scale_up_fabric_service_status(
            librms::protos::rack_manager::BatchGetScaleUpFabricServiceStatusRequest {
                nodes: Some(librms::protos::rack_manager::NodeSet {
                    nodes: vec![node_info("switch-7", "02:00:11:11:22:22")],
                }),
            },
        )
        .await
        .unwrap()
        .into_inner();
    let entry = &services.service_statuses["switch-7"];
    let parsed: serde_json::Value = serde_json::from_str(&entry.status_json)
        .expect("an unparseable body is read as an unknown state");
    assert_eq!(
        parsed["status"], "ok",
        "the caller matches this against a two-word vocabulary"
    );
}

/// Racks configured concurrently each end up with exactly one primary of
/// their own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fabric_election_is_independent_per_rack() {
    let in_rack_2 = |mac: [u8; 6]| SimNode {
        bmc_mac: Some(MacAddress::new(mac)),
        rack_id: Some("rack-002".to_string()),
        ..a_switch()
    };
    let url = serve_with(vec![
        a_switch(),
        a_second_switch(),
        in_rack_2([0x02, 0x00, 0x22, 0x22, 0x44, 0x44]),
        in_rack_2([0x02, 0x00, 0x22, 0x22, 0x55, 0x55]),
    ])
    .await;

    let rack_1 = librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info_in_rack("rack-001", "switch-7", "02:00:11:11:22:22"),
            node_info_in_rack("rack-001", "switch-8", "02:00:11:11:33:33"),
        ],
    };
    let rack_2 = librms::protos::rack_manager::NodeSet {
        nodes: vec![
            node_info_in_rack("rack-002", "switch-9", "02:00:22:22:44:44"),
            node_info_in_rack("rack-002", "switch-3", "02:00:22:22:55:55"),
        ],
    };

    // One request per rack, each from its own task.
    let configure =
        |url: String, nodes: librms::protos::rack_manager::NodeSet, primary: Option<&str>| {
            let primary = primary.map(str::to_owned);
            tokio::spawn(async move {
                let mut v2 = RackManagerV2Client::connect(url).await.unwrap();
                v2.configure_scale_up_fabric_manager(
                    librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest {
                        nodes: Some(nodes),
                        primary_switch_node_id: primary,
                        domain: None,
                        config: Some(fabric_config()),
                    },
                )
                .await
                .unwrap()
                .into_inner()
                .job_id
            })
        };
    let (job_1, job_2) = tokio::join!(
        configure(url.clone(), rack_1.clone(), Some("switch-8")),
        configure(url.clone(), rack_2.clone(), None),
    );
    let (job_1, job_2) = (job_1.unwrap(), job_2.unwrap());
    assert_ne!(job_1, job_2, "each rack gets a job of its own");

    let mut v1 = RackManagerClient::connect(url).await.unwrap();
    let enabled = |response: librms::protos::rack_manager::GetScaleUpFabricStatusResponse| {
        response
            .fabric_status
            .unwrap()
            .switches
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| s.node_id)
            .collect::<Vec<_>>()
    };
    for (rack, expected) in [(rack_1, "switch-8"), (rack_2, "switch-3")] {
        let status = v1
            .get_scale_up_fabric_status(
                librms::protos::rack_manager::GetScaleUpFabricStatusRequest {
                    nodes: Some(rack),
                    domain: None,
                },
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            enabled(status),
            vec![expected.to_string()],
            "the requested primary is honoured in one rack and the lowest node id elected in the other"
        );
    }
}
