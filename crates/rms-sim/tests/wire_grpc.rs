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
//! These drive the simulator through a real gRPC client over a real socket,
//! rather than calling the trait methods directly, because the thing most
//! likely to break is the transport: codec, HTTP/2 framing, and the router
//! paths the services are mounted on. A test that called the impl directly
//! would pass even if nothing were reachable.

use std::sync::Arc;

use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager_v2::rack_manager_v2_client::RackManagerV2Client;
use rms_sim::{RmsSimConfig, RmsSimulator, SimNode, SimNodeKind, StaticInventory};

/// A compute tray in slot 12 of rack-001, the third tray in its rack.
fn a_tray() -> SimNode {
    SimNode {
        kind: Some(SimNodeKind::Compute),
        bmc_mac: Some("0200abcd1234".to_string()),
        bmc_ip: Some("10.233.16.20".to_string()),
        rack_id: Some("rack-001".to_string()),
        slot_number: Some(12),
        tray_index: Some(2),
        ..SimNode::default()
    }
}

/// Serve the simulator on an ephemeral port and return its base URL.
async fn serve() -> String {
    serve_with(Vec::new()).await
}

async fn serve_with(nodes: Vec<SimNode>) -> String {
    let simulator = Arc::new(RmsSimulator::new(
        Arc::new(StaticInventory(nodes.into())),
        RmsSimConfig::default(),
    ));
    let router = rms_sim::router(simulator);

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
            .starts_with("machine-a-tron-rms-sim/"),
        "version should identify the simulator"
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
    // A missing V2 registration would surface as a router 404, which tonic
    // reports as `Unimplemented` too -- so assert on the message, which only
    // the simulator's own handler produces.
    let url = serve().await;
    let mut client = RackManagerV2Client::connect(url).await.unwrap();

    let status = client
        .configure_scale_up_fabric_manager(
            librms::protos::rack_manager_v2::ConfigureScaleUpFabricManagerRequest::default(),
        )
        .await
        .expect_err("not implemented yet at this stage");

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(
        status.message().contains("machine-a-tron RMS simulator"),
        "expected the simulator's own handler to answer, not a router 404; got: {}",
        status.message()
    );
}

/// Build a request for one node, identified the way carbide identifies nodes:
/// an opaque `node_id` plus a BMC MAC.
fn node_info(node_id: &str, mac: &str) -> librms::protos::rack_manager::NodeInfo {
    librms::protos::rack_manager::NodeInfo {
        node_id: node_id.to_string(),
        rack_id: "rack-001".to_string(),
        // Left unset: the simulator matches on address, not on declared type.
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
