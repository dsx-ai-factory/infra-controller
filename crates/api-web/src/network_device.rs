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

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::State as AxumState;
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, filters};

#[derive(Template)]
#[template(path = "network_device_show.html")]
struct NetworkDeviceShow {
    devices: Vec<NetworkDeviceDisplay>,
}

struct NetworkDeviceDisplay {
    name: String,
    id: String,
    description: String,
    mgmt_ips: String,
    discovered_via: String,
    device_type: String,
    connected_dpus: Vec<ConnectedDPU>,
}

impl From<forgerpc::NetworkDevice> for NetworkDeviceDisplay {
    fn from(mut network_device: forgerpc::NetworkDevice) -> Self {
        let mut connected_dpus = Vec::new();
        for device in network_device.devices.drain(..) {
            connected_dpus.push(ConnectedDPU {
                id: device.id.unwrap_or_default().to_string(),
                local_port: device.local_port,
                remote_port: device
                    .remote_port
                    .split('=')
                    .next_back()
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Self {
            name: network_device.name,
            id: network_device.id,
            description: network_device.description.unwrap_or_default(),
            mgmt_ips: network_device.mgmt_ip.join(","),
            discovered_via: network_device.discovered_via,
            device_type: network_device.device_type,
            connected_dpus,
        }
    }
}

struct ConnectedDPU {
    id: String,
    local_port: String,
    remote_port: String,
}

/// List network devices
pub(super) async fn show_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let network_devices = match fetch_network_devices(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_devices");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network devices",
            )
                .into_response();
        }
    };
    let mut devices: Vec<NetworkDeviceDisplay> = Vec::new();
    for d in network_devices {
        devices.push(d.into());
    }
    let tmpl = NetworkDeviceShow { devices };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let network_devices = match fetch_network_devices(state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_network_devices");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading network devices",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(network_devices)).into_response()
}

async fn fetch_network_devices(
    api: Arc<Api>,
) -> Result<Vec<forgerpc::NetworkDevice>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::NetworkTopologyRequest { id: None });
    let mut topology = api
        .get_network_topology(request)
        .await
        .map(|response| response.into_inner())?;
    topology
        .network_devices
        .sort_unstable_by(|d1, d2| d1.name.cmp(&d2.name));
    Ok(topology.network_devices)
}

impl super::Base for NetworkDeviceShow {}
