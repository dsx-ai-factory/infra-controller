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
// Flat `rpc::forge::Machine` fields are deprecated in favour of `status`/`config`
// sub-messages, but this module must still read them until the REST API is migrated.
// See https://github.com/NVIDIA/infra-controller/issues/2793
#![allow(deprecated)]

use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge as forgerpc;

use super::pagination::{self, PageContext, PaginationParams};
use super::{Base, filters};
use crate::machine;

#[derive(Template)]
#[template(path = "dpu_versions.html")]
struct DpuVersions {
    machines: Vec<Row>,
    page: PageContext,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
struct Row {
    host_machine_id: String,
    dpu_machine_id: String,
    state: String,
    dpu_type: String,
    dpu_agent_version: String,
    firmware_version: String,
    bmc_version: String,
    bios_version: String,
    hbn_version: String,
}

impl From<forgerpc::Machine> for Row {
    fn from(machine: forgerpc::Machine) -> Self {
        let state = match machine.state.split_once(' ') {
            Some((state, _)) => state.to_owned(),
            None => machine.state,
        };

        Row {
            dpu_machine_id: machine.id.map(|id| id.to_string()).unwrap_or_default(),
            host_machine_id: machine
                .associated_host_machine_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            dpu_type: machine
                .discovery_info
                .as_ref()
                .and_then(|di| di.dmi_data.as_ref())
                .map(|dmi_data| dmi_data.product_name.clone())
                .unwrap_or_default(),
            state,
            dpu_agent_version: machine
                .inventory
                .as_ref()
                .and_then(|inventory| {
                    inventory
                        .components
                        .iter()
                        .find(|c| c.name == "carbide-dpu-agent" || c.name == "forge-dpu-agent")
                        .map(|c| c.version.clone())
                })
                .unwrap_or_default(),
            firmware_version: machine
                .discovery_info
                .as_ref()
                .and_then(|di| di.dpu_info.as_ref())
                .map(|dpu| dpu.firmware_version.clone())
                .unwrap_or_default(),
            bmc_version: machine
                .bmc_info
                .as_ref()
                .and_then(|bmc| bmc.firmware_version.clone())
                .unwrap_or_default(),
            bios_version: machine
                .discovery_info
                .as_ref()
                .and_then(|di| di.dmi_data.as_ref())
                .map(|dmi_data| dmi_data.bios_version.clone())
                .unwrap_or_default(),
            hbn_version: machine
                .inventory
                .and_then(|inv| {
                    inv.components
                        .iter()
                        .find(|c| c.name.contains("hbn"))
                        .map(|c| c.version.clone())
                })
                .unwrap_or_default(),
        }
    }
}

async fn fetch_dpus(api: &Arc<Api>) -> Result<Vec<Row>, tonic::Status> {
    let mut machines = machine::fetch_machines(api.clone(), true, false).await?;
    machines
        .machines
        .retain(|m| m.machine_type == forgerpc::MachineType::Dpu as i32);

    let machines = machines.machines.into_iter().map(Row::from).collect();

    Ok(machines)
}

pub(super) async fn list_html(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let machines = match fetch_dpus(&state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_dpus");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading DPUs").into_response();
        }
    };

    let (info, machines) = pagination::paginate_vec(machines, &params);

    let tmpl = DpuVersions {
        machines,
        page: PageContext::new(info, "/admin/dpu/versions"),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn list_json(AxumState(state): AxumState<Arc<Api>>) -> impl IntoResponse {
    let machines = match fetch_dpus(&state).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "fetch_dpus");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading DPUs").into_response();
        }
    };
    (StatusCode::OK, Json(machines)).into_response()
}

impl super::Base for DpuVersions {}
