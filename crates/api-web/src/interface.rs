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

use std::collections::BTreeMap;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{OriginalUri, Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use chrono::{DateTime, Utc};
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::pagination::{self, PageContext, PaginationParams};
use super::{Base, filters};

#[derive(Template)]
#[template(path = "interface_show.html")]
struct InterfaceShow {
    interfaces: Vec<InterfaceRowDisplay>,
    page: PageContext,
}

struct InterfaceRowDisplay {
    id: String,
    interface_type: String,
    mac_address: String,
    ip_address: String,
    association_type: String,
    machine_id: String,
    switch_id: String,
    power_shelf_id: String,
    hostname: String,
    vendor: String,
    domain_name: String,
}

struct InterfaceAssociationDisplay {
    association_type: String,
    machine_id: String,
    switch_id: String,
    power_shelf_id: String,
}

impl From<&forgerpc::MachineInterface> for InterfaceAssociationDisplay {
    fn from(mi: &forgerpc::MachineInterface) -> Self {
        let machine_id = mi
            .machine_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let switch_id = mi
            .switch_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let power_shelf_id = mi
            .power_shelf_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let association_type = mi
            .association_type
            .and_then(|value| forgerpc::InterfaceAssociationType::try_from(value).ok());
        let association_type = association_type.or({
            if !machine_id.is_empty() {
                Some(forgerpc::InterfaceAssociationType::Machine)
            } else if !switch_id.is_empty() {
                Some(forgerpc::InterfaceAssociationType::Switch)
            } else if !power_shelf_id.is_empty() {
                Some(forgerpc::InterfaceAssociationType::Powershelf)
            } else {
                None
            }
        });
        let (association_type, machine_id, switch_id, power_shelf_id) = match association_type {
            Some(forgerpc::InterfaceAssociationType::Machine) => (
                "Machine".to_string(),
                machine_id,
                String::new(),
                String::new(),
            ),
            Some(forgerpc::InterfaceAssociationType::Switch) => (
                "Switch".to_string(),
                String::new(),
                switch_id,
                String::new(),
            ),
            Some(forgerpc::InterfaceAssociationType::Powershelf) => (
                "Powershelf".to_string(),
                String::new(),
                String::new(),
                power_shelf_id,
            ),
            Some(forgerpc::InterfaceAssociationType::None) | None => (
                "None".to_string(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };

        Self {
            association_type,
            machine_id,
            switch_id,
            power_shelf_id,
        }
    }
}

impl From<forgerpc::MachineInterface> for InterfaceRowDisplay {
    fn from(mi: forgerpc::MachineInterface) -> Self {
        let association = InterfaceAssociationDisplay::from(&mi);

        Self {
            id: mi.id.unwrap_or_default().to_string(),
            interface_type: if mi.interface_type == Some(forgerpc::InterfaceType::Bmc as i32) {
                "BMC".to_string()
            } else {
                "Data".to_string()
            },
            mac_address: mi.mac_address,
            ip_address: mi.address.join(","),
            association_type: association.association_type,
            machine_id: association.machine_id,
            switch_id: association.switch_id,
            power_shelf_id: association.power_shelf_id,
            hostname: mi.hostname,
            vendor: mi.vendor.unwrap_or_default(),
            domain_name: String::new(), // filled in later
        }
    }
}

/// List machine interfaces
pub(super) async fn show_html(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
    uri: OriginalUri,
) -> Response {
    let machine_interfaces = match fetch_machine_interfaces(state.clone()).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "find_interfaces");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading machine interfaces",
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(::rpc::protos::dns::DomainSearchQuery {
        id: None,
        name: None,
    });
    let domain_list = match state
        .find_domain(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "find_domain");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading domains").into_response();
        }
    };
    let domainlist_map = domain_list
        .domains
        .into_iter()
        .map(|x| (x.id.unwrap_or_default(), x.name))
        .collect::<BTreeMap<_, _>>();

    let mut interfaces = Vec::new();
    for iface in machine_interfaces {
        let domain_name = domainlist_map
            .get(&iface.domain_id.unwrap_or_default())
            .cloned()
            .unwrap_or_default();
        let mut display: InterfaceRowDisplay = iface.into();
        display.domain_name = domain_name;
        interfaces.push(display);
    }

    let (info, interfaces) = pagination::paginate_vec(interfaces, &params);

    let tmpl = InterfaceShow {
        interfaces,
        page: PageContext::new(info, uri.path()),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let machine_interfaces = match fetch_machine_interfaces(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "fetch_machine_interfaces");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading machine interfaces",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(machine_interfaces)).into_response()
}

async fn fetch_machine_interfaces(
    api: Arc<Api>,
) -> Result<Vec<forgerpc::MachineInterface>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::InterfaceSearchQuery { id: None, ip: None });
    let mut out = api
        .find_interfaces(request)
        .await
        .map(|response| response.into_inner())?;
    out.interfaces
        .sort_unstable_by(|iface1, iface2| iface1.hostname.cmp(&iface2.hostname));

    Ok(out.interfaces)
}

#[derive(Template)]
#[template(path = "interface_detail.html")]
struct InterfaceDetail {
    id: String,
    interface_type: String,
    dpu_machine_id: String,
    association_type: String,
    machine_id: String,
    switch_id: String,
    power_shelf_id: String,
    segment_id: String,
    mac_address: String,
    ip_address: String,
    hostname: String,
    vendor: String,
    domain_id: String,
    domain_name: String,
    is_primary: bool,
    created: String,
    last_dhcp: String,
}

impl From<forgerpc::MachineInterface> for InterfaceDetail {
    fn from(mi: forgerpc::MachineInterface) -> Self {
        let association = InterfaceAssociationDisplay::from(&mi);
        let created: DateTime<Utc> = mi
            .created
            .expect("machine_interfaces.created is NOT NULL in DB, should exist")
            .try_into()
            .unwrap_or_default();
        let last_dhcp: Option<DateTime<Utc>> = match mi.last_dhcp {
            None => None,
            Some(d) => d.try_into().ok(),
        };
        Self {
            id: mi.id.unwrap_or_default().to_string(),
            interface_type: if mi.interface_type == Some(forgerpc::InterfaceType::Bmc as i32) {
                "BMC".to_string()
            } else {
                "Data".to_string()
            },
            dpu_machine_id: mi
                .attached_dpu_machine_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
            association_type: association.association_type,
            machine_id: association.machine_id,
            switch_id: association.switch_id,
            power_shelf_id: association.power_shelf_id,
            segment_id: mi.segment_id.unwrap_or_default().to_string(),
            mac_address: mi.mac_address,
            ip_address: mi.address.join(","),
            hostname: mi.hostname,
            vendor: mi.vendor.unwrap_or_default(),
            is_primary: mi.primary_interface,
            domain_id: mi.domain_id.unwrap_or_default().to_string(),
            // filled in later
            domain_name: String::new(),
            // e.g "2001-07-08 00:34:60	UTC"
            created: created.format("%F %T %Z").to_string(),
            last_dhcp: last_dhcp
                .map(|d| d.format("%F %T %Z").to_string())
                .unwrap_or_default(),
        }
    }
}

/// View machine interface details
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(interface_id): AxumPath<String>,
) -> Response {
    let (show_json, interface_id_string) = match interface_id.strip_suffix(".json") {
        Some(interface_id) => (true, interface_id.to_string()),
        None => (false, interface_id),
    };

    let interface_id = match interface_id_string.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid interface ID: {e}"),
            )
                .into_response();
        }
    };

    let request = tonic::Request::new(forgerpc::InterfaceSearchQuery {
        id: Some(interface_id),
        ip: None,
    });
    let mut machine_interfaces = match state
        .find_interfaces(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(n) => n,
        Err(err) if err.code() == tonic::Code::NotFound => {
            return super::not_found_response(interface_id_string);
        }
        Err(err) => {
            tracing::error!(error = %err, "find_interfaces");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading machine interface",
            )
                .into_response();
        }
    };

    if machine_interfaces.interfaces.len() != 1 {
        tracing::error!(
            machine_interface_id = %interface_id,
            expected_interface_count = 1,
            matching_interface_count = machine_interfaces.interfaces.len(),
            "Unexpected number of matching interfaces",
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Expected exactly one interface to match",
        )
            .into_response();
    }
    let interface = machine_interfaces.interfaces.pop().unwrap(); // safe, we check above

    if show_json {
        return (StatusCode::OK, Json(interface)).into_response();
    }

    let tmpl: InterfaceDetail = interface.into();
    // TODO tmpl.domain_name = domain_name;
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for InterfaceShow {}
impl super::Base for InterfaceDetail {}
