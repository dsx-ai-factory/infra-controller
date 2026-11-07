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
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Query, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;

use super::Base;
use super::pagination::{self, PageContext, PaginationParams};
use crate::filters;
use crate::site_explorer_run_status::SiteExplorerLastRunDisplay;

#[derive(Template)]
#[template(path = "expected_machine_show.html")]
struct ExpectedMachines {
    last_run: Option<SiteExplorerLastRunDisplay>,
    expected_rows: Vec<ExpectedMachineRow>,
    unexpected_rows: Vec<UnexpectedMachineRow>,
    all_count: usize,
    completed_count: usize,
    unseen_count: usize,
    unexplored_count: usize,
    unlinked_count: usize,
    unexpected_count: usize,
    active_tab: String,
    page: PageContext,
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, serde::Serialize)]
struct ExpectedMachineRow {
    bmc_mac_address: String,
    interface_id: String,
    serial_number: String,
    address: String,    // The explored endpoint
    machine_id: String, // The machine
}

impl From<rpc::forge::LinkedExpectedMachine> for ExpectedMachineRow {
    fn from(l: rpc::forge::LinkedExpectedMachine) -> ExpectedMachineRow {
        ExpectedMachineRow {
            bmc_mac_address: l.bmc_mac_address,
            interface_id: l.interface_id.unwrap_or_default(),
            serial_number: l.chassis_serial_number,
            address: l.explored_endpoint_address.unwrap_or_default(),
            machine_id: l.machine_id.map(|m| m.to_string()).unwrap_or_default(),
        }
    }
}

/// Row in the Unexpected tab: an explored host BMC endpoint whose MAC address is
/// not listed in any of `expected_machines`, `expected_power_shelf`, or
/// `expected_switch`.
#[derive(Ord, PartialOrd, Eq, PartialEq, serde::Serialize)]
struct UnexpectedMachineRow {
    address: String,
    bmc_mac: String,
    machine_id: String,
}

impl From<rpc::forge::UnexpectedMachine> for UnexpectedMachineRow {
    fn from(m: rpc::forge::UnexpectedMachine) -> UnexpectedMachineRow {
        UnexpectedMachineRow {
            address: m.address,
            bmc_mac: m.bmc_mac_address,
            machine_id: m.machine_id.map(|id| id.to_string()).unwrap_or_default(),
        }
    }
}

const TABS: &[&str] = &[
    "all",
    "completed",
    "unseen",
    "unexplored",
    "unlinked",
    "unexpected",
];

struct ExpectedMachineTabs {
    all_machines: Vec<ExpectedMachineRow>,
    completed_machines: Vec<ExpectedMachineRow>,
    unseen_machines: Vec<ExpectedMachineRow>,
    unexplored_machines: Vec<ExpectedMachineRow>,
    unlinked_machines: Vec<ExpectedMachineRow>,
}

impl ExpectedMachineTabs {
    fn from_linked(machines: Vec<rpc::forge::LinkedExpectedMachine>) -> Self {
        let mut all_machines: Vec<ExpectedMachineRow> = Vec::with_capacity(machines.len());
        let mut completed_machines: Vec<ExpectedMachineRow> = Vec::new();
        let mut unseen_machines: Vec<ExpectedMachineRow> = Vec::new();
        let mut unexplored_machines: Vec<ExpectedMachineRow> = Vec::new();
        let mut unlinked_machines: Vec<ExpectedMachineRow> = Vec::new();

        for em in machines {
            let no_dhcp = em.interface_id.is_none();
            let is_unexplored = em.explored_endpoint_address.is_none();
            let is_unlinked = em.machine_id.is_none();
            let row: ExpectedMachineRow = em.into();

            all_machines.push(row.clone());
            if !is_unlinked && !is_unexplored {
                completed_machines.push(row.clone());
            }
            if no_dhcp {
                unseen_machines.push(row.clone());
            }
            if is_unexplored {
                unexplored_machines.push(row.clone());
            }
            if is_unlinked {
                unlinked_machines.push(row);
            }
        }

        all_machines.sort_unstable();
        completed_machines.sort_unstable();
        unseen_machines.sort_unstable();
        unexplored_machines.sort_unstable();
        unlinked_machines.sort_unstable();

        Self {
            all_machines,
            completed_machines,
            unseen_machines,
            unexplored_machines,
            unlinked_machines,
        }
    }
}

pub(super) async fn show_all_html(
    AxumState(api): AxumState<Arc<Api>>,
    Query(mut params): Query<HashMap<String, String>>,
) -> Response {
    let active_tab = params.remove("tab").unwrap_or_else(|| "all".to_string());
    if !TABS.contains(&active_tab.as_str()) {
        return (StatusCode::BAD_REQUEST, "Unknown tab").into_response();
    }

    let pagination_params = PaginationParams {
        current_page: params.remove("current_page").and_then(|s| s.parse().ok()),
        limit: params.remove("limit").and_then(|s| s.parse().ok()),
    };

    let result = match api
        .get_all_expected_machines_linked(tonic::Request::new(()))
        .await
        .map(|response| response.into_inner())
    {
        Ok(machines) => machines,
        Err(err) => {
            tracing::error!(error = %err, "get_all_expected_machines_linked");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading expected machines from carbide-api",
            )
                .into_response();
        }
    };

    let expected_tabs = ExpectedMachineTabs::from_linked(result.expected_machines);
    let all_count = expected_tabs.all_machines.len();
    let completed_count = expected_tabs.completed_machines.len();
    let unseen_count = expected_tabs.unseen_machines.len();
    let unexplored_count = expected_tabs.unexplored_machines.len();
    let unlinked_count = expected_tabs.unlinked_machines.len();

    let unexpected_response = match api
        .get_all_unexpected_machines(tonic::Request::new(()))
        .await
        .map(|response| response.into_inner())
    {
        Ok(list) => list,
        Err(err) => {
            tracing::error!(error = %err, "get_all_unexpected_machines");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading unexpected machines from carbide-api",
            )
                .into_response();
        }
    };
    let mut unexpected_machines: Vec<UnexpectedMachineRow> = unexpected_response
        .unexpected_machines
        .into_iter()
        .map(Into::into)
        .collect();
    unexpected_machines.sort_unstable();
    let unexpected_count = unexpected_machines.len();
    let last_run = match api
        .get_site_explorer_last_run(tonic::Request::new(()))
        .await
        .map(|response| response.into_inner().last_run)
    {
        Ok(last_run) => last_run.as_ref().map(Into::into),
        Err(err) => {
            tracing::error!(error = %err, "get_site_explorer_last_run");
            None
        }
    };

    let (expected_rows, unexpected_rows, info) = match active_tab.as_str() {
        "all" => {
            let (info, rows) =
                pagination::paginate_vec(expected_tabs.all_machines, &pagination_params);
            (rows, Vec::new(), info)
        }
        "completed" => {
            let (info, rows) =
                pagination::paginate_vec(expected_tabs.completed_machines, &pagination_params);
            (rows, Vec::new(), info)
        }
        "unseen" => {
            let (info, rows) =
                pagination::paginate_vec(expected_tabs.unseen_machines, &pagination_params);
            (rows, Vec::new(), info)
        }
        "unexplored" => {
            let (info, rows) =
                pagination::paginate_vec(expected_tabs.unexplored_machines, &pagination_params);
            (rows, Vec::new(), info)
        }
        "unlinked" => {
            let (info, rows) =
                pagination::paginate_vec(expected_tabs.unlinked_machines, &pagination_params);
            (rows, Vec::new(), info)
        }
        "unexpected" => {
            let (info, rows) = pagination::paginate_vec(unexpected_machines, &pagination_params);
            (Vec::new(), rows, info)
        }
        _ => unreachable!(),
    };

    let extra_query_params = if active_tab != "all" {
        format!("&tab={active_tab}")
    } else {
        String::new()
    };

    let tmpl = ExpectedMachines {
        last_run,
        expected_rows,
        unexpected_rows,
        all_count,
        completed_count,
        unseen_count,
        unexplored_count,
        unlinked_count,
        unexpected_count,
        active_tab,
        page: PageContext::new(info, "/admin/expected-machine")
            .with_extra_params(extra_query_params),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_expected_machine_raw_json(
    AxumState(api): AxumState<Arc<Api>>,
) -> Response {
    let result = match api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .map(|response| response.into_inner())
    {
        Ok(machines) => machines,
        Err(err) => {
            tracing::error!(error = %err, "show_expected_machine_raw_json");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading expected machines from carbide-api",
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(result)).into_response()
}

impl super::Base for ExpectedMachines {}
