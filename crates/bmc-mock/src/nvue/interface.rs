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

//! `nv show interface`: the NVLink ports, the fabric management ports, the
//! management interface and the loopback.
//!
//! NVUE renders a link state as an object keyed by the state, such as
//! `{"state": {"up": {}}}`, and link diagnostics as an object keyed by code,
//! each with a `status`. The mock carries the link fields a client reads
//! without the counters and PHY detail a switch adds.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::NvueState;
use crate::json::JsonExt;

/// The fabric management ports every NVLink switch tray has, `fnm1` and
/// `fnm2`.
pub(super) const FABRIC_MANAGEMENT_PORTS: u32 = 2;

/// `GET /nvue_v1/interface`. NVOS 25.02 ignores `filter_` on this tree, so
/// every interface is listed whatever the query.
pub(super) async fn list(State(state): State<Arc<NvueState>>) -> Response {
    let config = &state.config;
    let mut interfaces = Map::new();
    for port in 1..=config.nvl_ports {
        interfaces.insert(format!("acp{port}"), nvlink_port());
    }
    for port in 1..=FABRIC_MANAGEMENT_PORTS {
        interfaces.insert(format!("fnm{port}"), fabric_management_port());
    }
    let mut management = json!({
        "auto-negotiate": "on",
        "duplex": "full",
        "mtu": 1500,
        "speed": "1G",
        "state": { "up": {} },
    });
    if let Some(mac) = config.management_mac {
        management["mac"] = json!(mac.to_string().to_ascii_lowercase());
    }
    interfaces.insert(
        "eth0".to_string(),
        json!({ "ifindex": 2, "link": management, "type": "eth" }),
    );
    interfaces.insert(
        "lo".to_string(),
        json!({
            "ifindex": 1,
            "link": { "mac": "00:00:00:00:00:00", "mtu": 65536, "state": { "up": {} } },
            "type": "loopback",
        }),
    );
    Value::Object(interfaces).into_ok_response()
}

/// An NVLink port with its link up to a GPU.
fn nvlink_port() -> Value {
    json!({
        "link": {
            "auto-negotiate": "on",
            "connection-mode": "ndr",
            "diagnostics": { "0": { "status": "No issue was observed" } },
            "lanes": "2X",
            "logical-state": "Active",
            "max-supported-mtu": 256,
            "mtu": 256,
            "physical-state": "LinkUp",
            "speed": "400G",
            "state": { "up": {} },
            "supported-lanes": "2X",
            "supported-speed": "400G",
        },
        "type": "nvl",
    })
}

/// A fabric management port, administratively down as a tray ships.
fn fabric_management_port() -> Value {
    json!({
        "link": {
            "auto-negotiate": "on",
            "connection-mode": "ndr",
            "diagnostics": { "1": { "status": "Closed by command" } },
            "logical-state": "Down",
            "max-supported-mtu": 256,
            "physical-state": "Disabled",
            "state": { "down": {} },
            "supported-lanes": "2X",
            "supported-speed": "10G,200G",
        },
        "type": "fnm",
    })
}
