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

//! Fixtures shared by the wire tests. Each test binary compiles these on its
//! own and uses the subset it needs.

#![allow(dead_code)]

use std::net::IpAddr;
use std::sync::Arc;

use librms::protos::rack_manager as rms;
use mac_address::MacAddress;
use rms_mock::{FaultConfig, RmsMock, RmsMockConfig, SimNode, SimNodeKind, StaticInventory};

/// An NVLink switch tray in rack unit 30 of rack-001, reachable on its BMC
/// and on its NVOS address.
pub(crate) fn a_switch() -> SimNode {
    SimNode {
        kind: Some(SimNodeKind::Switch),
        bmc_mac: Some(MacAddress::new([0x02, 0x00, 0x11, 0x11, 0x22, 0x22])),
        host_ip: Some(IpAddr::from([10, 233, 32, 7])),
        rack_id: Some("rack-001".to_string()),
        slot_number: Some(30),
        ..SimNode::default()
    }
}

/// A compute tray in slot 12 of rack-001, the third tray in its rack.
pub(crate) fn a_tray() -> SimNode {
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

pub(crate) async fn serve_with(nodes: Vec<SimNode>) -> String {
    serve_with_config(nodes, RmsMockConfig::default()).await
}

/// Serve the mock on an ephemeral port and return its base URL.
pub(crate) async fn serve_with_config(nodes: Vec<SimNode>, config: RmsMockConfig) -> String {
    let mock = Arc::new(RmsMock::new(
        Arc::new(StaticInventory::new(nodes.into())),
        config,
    ));
    let router = rms_mock::router(mock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    format!("http://{addr}")
}

/// A node in rack-001 as NICo names it: an opaque `node_id` plus a BMC MAC.
pub(crate) fn node_info(node_id: &str, mac: &str) -> rms::NodeInfo {
    node_info_in_rack("rack-001", node_id, mac)
}

pub(crate) fn node_info_in_rack(rack_id: &str, node_id: &str, mac: &str) -> rms::NodeInfo {
    rms::NodeInfo {
        node_id: node_id.to_string(),
        rack_id: rack_id.to_string(),
        // Left unset: the mock matches on address, not on declared type.
        r#type: None,
        bmc_endpoint: Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
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

/// A configuration under which jobs for the named nodes fail.
pub(crate) fn failing_nodes(node_ids: &[&str]) -> RmsMockConfig {
    RmsMockConfig {
        faults: FaultConfig {
            fail_jobs_for_node_ids: node_ids.iter().map(|s| s.to_string()).collect(),
        },
        ..RmsMockConfig::default()
    }
}
