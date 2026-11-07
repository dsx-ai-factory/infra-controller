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

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
//use axum::extract::{Path as AxumPath, State as AxumState};
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse, Redirect, Response};
use carbide_api_core::Api;
use carbide_uuid::machine::{MachineId, MachineType};
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::IdentifySerialResponse;
use rpc::forge::forge_server::Forge;
use uuid::Uuid;

use super::Base;

pub(super) async fn find(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(query) = params.get("q").map(|v| v.trim()) else {
        return (StatusCode::BAD_REQUEST, "Missing query").into_response();
    };

    if let Some(url) = shortcodes(query) {
        return Redirect::to(url).into_response();
    }

    if let Ok(machine_id) = MachineId::from_str(query) {
        return find_machine_id(machine_id).into_response();
    }

    if IpAddr::from_str(query).is_ok() {
        return find_ip(state, query).await.into_response();
    }

    if let Ok(u) = Uuid::parse_str(query) {
        return find_by_uuid(state, u).await.into_response();
    }

    if let Ok(mac) = mac_address::MacAddress::from_str(query) {
        return find_by_mac(state, mac).await.into_response();
    }

    if let Some(machine_id) = find_by_serial(state, query).await {
        return Redirect::to(&format!("/admin/machine/{machine_id}")).into_response();
    }

    (StatusCode::NOT_FOUND, "No matches").into_response()
}

/// Quick jumps for keyboard navigation
fn shortcodes(q: &str) -> Option<&'static str> {
    match q {
        "i" => Some("/admin/instance"),
        "d" => Some("/admin/dpu"),
        "h" => Some("/admin/host"),
        "mh" => Some("/admin/managed-host"),
        "ns" => Some("/admin/network-segment"),
        "nd" => Some("/admin/network-device"),
        "rp" => Some("/admin/resource-pool"),
        "mi" => Some("/admin/interface"),
        "ib" => Some("/admin/ib-partition"),
        "vp" => Some("/admin/vpc"),
        "ca" => Some("/admin/compute-allocation"),
        "ee" => Some("/admin/explored-endpoint"),
        _ => None,
    }
}

fn find_machine_id(machine_id: MachineId) -> impl IntoResponse {
    match machine_id.machine_type() {
        MachineType::Dpu => Redirect::to(&format!("/admin/machine/{machine_id}")),
        MachineType::Host | MachineType::PredictedHost => {
            Redirect::to(&format!("/admin/managed-host/{machine_id}"))
        }
    }
}

async fn find_by_uuid(state: Arc<Api>, u: uuid::Uuid) -> Response {
    let req = forgerpc::IdentifyUuidRequest {
        uuid: Some(u.into()),
    };
    let request = tonic::Request::new(req);
    let out = match state
        .identify_uuid(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) => m,
        Err(status) if status.code() == tonic::Code::NotFound => {
            return (StatusCode::NOT_FOUND, "UUID does not match anything").into_response();
        }
        Err(err) => {
            tracing::error!(error = %err, "find_by_uuid error calling grpc identify_uuid");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Ok(object_type) = forgerpc::UuidType::try_from(out.object_type) else {
        tracing::error!(
            object_type = out.object_type,
            "Invalid UuidType from carbide api",
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    use forgerpc::UuidType::*;
    match object_type {
        NetworkSegment => Redirect::to(&format!("/admin/network-segment/{u}")).into_response(),
        Instance => Redirect::to(&format!("/admin/instance/{u}")).into_response(),
        MachineInterface => Redirect::to(&format!("/admin/interface/{u}")).into_response(),
        Vpc => Redirect::to(&format!("/admin/vpc/{u}")).into_response(),
        Domain => {
            // Domains don't have individual URLs
            Redirect::to("/admin/domain").into_response()
        }
        DpaInterfaceId => Redirect::to(&format!("/admin/dpa/{u}")).into_response(),
        ComputeAllocationId => {
            Redirect::to(&format!("/admin/compute-allocation/{u}")).into_response()
        }
    }
}

async fn find_by_mac(state: Arc<Api>, mac: mac_address::MacAddress) -> impl IntoResponse {
    let req = forgerpc::IdentifyMacRequest {
        mac_address: mac.to_string(),
    };
    let request = tonic::Request::new(req);
    let out = match state
        .identify_mac(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) => m,
        Err(status) if status.code() == tonic::Code::NotFound => {
            return (StatusCode::NOT_FOUND, "MAC does not match anything").into_response();
        }
        Err(err) => {
            tracing::error!(error = %err, "find_by_mac error calling grpc identify_mac");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Ok(object_type) = forgerpc::MacOwner::try_from(out.object_type) else {
        tracing::error!(
            object_type = out.object_type,
            "Invalid MacOwner from carbide api",
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    use forgerpc::MacOwner::*;
    match object_type {
        MachineInterface => {
            Redirect::to(&format!("/admin/interface/{}", out.primary_key)).into_response()
        }
        ExploredEndpoint => {
            Redirect::to(&format!("/admin/explored-endpoint/{}", out.primary_key)).into_response()
        }
        // If the search got this far it doesn't have an machine_interface, so it's Unseen
        ExpectedMachine => Redirect::to("/admin/expected-machine?tab=unseen").into_response(),

        DpaInterface => Redirect::to(&format!("/admin/dpa/{}", out.primary_key)).into_response(),
    }
}

async fn find_by_serial(state: Arc<Api>, serial_number: &str) -> Option<MachineId> {
    let req = forgerpc::IdentifySerialRequest {
        serial_number: serial_number.to_string(),
        exact: false,
    };
    let request = tonic::Request::new(req);
    match state
        .identify_serial(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(IdentifySerialResponse { machine_id, .. }) => machine_id,
        Err(status) if status.code() == tonic::Code::NotFound => {
            // don't log it
            None
        }
        Err(err) => {
            tracing::info!(error = %err, serial_number, "find_by_serial error");
            None
        }
    }
}

#[derive(Template)]
#[template(path = "ip_finder.html")]
struct IpFinder {
    ip: String,
    found: Vec<IpMatch>,
}

struct IpMatch {
    name: &'static str,
    url: String,
    message: String,
}

/// Renders the IP search page via the API `find_ip_address` handler, including `StaticBmcIp`
/// matches for operator-configured BMC addresses.
async fn find_ip(state: Arc<Api>, ip: &str) -> impl IntoResponse {
    let mut found = Vec::new();
    let req = forgerpc::FindIpAddressRequest { ip: ip.to_string() };
    let request = tonic::Request::new(req);
    let out = match state
        .find_ip_address(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) => m,
        Err(_) => {
            let tmpl = IpFinder {
                ip: ip.to_string(),
                found: Vec::new(),
            };
            return (StatusCode::OK, Html(tmpl.render().unwrap()));
        }
    };
    for m in out.matches {
        let ip_type = match forgerpc::IpType::try_from(m.ip_type) {
            Ok(t) => t,
            Err(err) => {
                tracing::error!(ip_type = m.ip_type, error = %err, "Invalid IpType");
                continue;
            }
        };
        use forgerpc::IpType::*;
        let owner = m.owner_id.unwrap_or_default();
        let (name, url) = match ip_type {
            StaticDataDhcpServer => ("DHCP Server", "".to_string()),
            StaticDataRouteServer => ("Route Server", "".to_string()),
            RouteServerFromConfigFile => ("Route Server from Carbide Config", "".to_string()),
            RouteServerFromAdminApi => ("Route Server from Admin API", "".to_string()),
            ResourcePool => ("Resource Pool", "/admin/resource-pool".to_string()),
            InstanceAddress => ("Instance", format!("/admin/instance/{owner}")),
            MachineAddress => ("Machine", format!("/admin/machine/{owner}")),
            BmcIp => ("BMC IP", format!("/admin/machine/{owner}")),
            // Operator/reserved BMC (static allocation or static-assignments segment); see finder.
            StaticBmcIp => ("Static BMC IP", format!("/admin/machine/{owner}")),
            ExploredEndpoint => (
                "Explored Endpoint",
                format!("/admin/explored-endpoint/{owner}"),
            ),
            LoopbackIp => ("Loopback IP", format!("/admin/machine/{owner}")),
            NetworkSegment => ("Network Segment", format!("/admin/network-segment/{owner}")),
            DpaInterface => ("Dpa Interface", format!("/admin/dpa/{owner}")),
        };
        found.push(IpMatch {
            name,
            url,
            message: m.message,
        });
    }

    let tmpl = IpFinder {
        ip: ip.to_string(),
        found,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap()))
}

impl super::Base for IpFinder {}
