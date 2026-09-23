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

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::extract::{self, Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Redirect, Response};
use carbide_api_core::{Api, AuthContext};
use carbide_uuid::machine::MachineId;
use carbide_uuid::nvlink::NvLinkDomainId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use health_report::HealthReport;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    HealthReportApplyMode, InsertMachineHealthReportRequest, InsertNvLinkDomainHealthReportRequest,
    InsertPowerShelfHealthReportRequest, InsertRackHealthReportRequest,
    InsertSwitchHealthReportRequest, ListNvLinkDomainHealthReportsRequest,
    ListPowerShelfHealthReportsRequest, ListRackHealthReportsRequest,
    ListSwitchHealthReportsRequest, MachinesByIdsRequest, PowerShelvesByIdsRequest,
    RacksByIdsRequest, RemoveMachineHealthReportRequest, RemoveNvLinkDomainHealthReportRequest,
    RemovePowerShelfHealthReportRequest, RemoveRackHealthReportRequest,
    RemoveSwitchHealthReportRequest, SwitchesByIdsRequest,
};

use super::{Base, filters};

#[derive(Template)]
#[template(path = "health_reports_detail.html")]
struct HealthPage {
    id: String,
    object_label: String,
    object_detail_url: String,
    show_machine_templates: bool,
    entries: Vec<HealthReportEntry>,
    aggregate_health: LabeledHealthReport,
    health_contributors: Vec<LabeledHealthReport>,
    history_url: Option<String>,
    history: HealthHistoryTable,
}

#[derive(Template)]
#[template(path = "health_history_table.html")]
pub(super) struct HealthHistoryTable {
    pub(super) records: Vec<HealthHistoryRecord>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct HealthHistoryRecord {
    timestamp: String,
    health: health_report::HealthReport,
}

impl HealthHistoryRecord {
    fn from_rpc_convert_invalid(record: ::rpc::forge::HealthHistoryRecord) -> Self {
        HealthHistoryRecord {
            timestamp: record.time.map(|time| time.to_string()).unwrap_or_default(),
            health: record
                .health
                .map(health_report_from_rpc_convert_invalid)
                .unwrap_or_else(health_report::HealthReport::missing_report),
        }
    }
}

#[derive(Template)]
#[template(path = "machine_health_component.html")]
struct LabeledHealthReport {
    label: String,
    report: Option<health_report::HealthReport>,
}

#[derive(Clone)]
enum HealthObject {
    Machine(MachineId),
    NvLinkDomain(NvLinkDomainId),
    PowerShelf(PowerShelfId),
    Rack(RackId),
    Switch(SwitchId),
}

impl HealthObject {
    fn id(&self) -> String {
        match self {
            HealthObject::Machine(id) => id.to_string(),
            HealthObject::NvLinkDomain(id) => id.to_string(),
            HealthObject::PowerShelf(id) => id.to_string(),
            HealthObject::Rack(id) => id.to_string(),
            HealthObject::Switch(id) => id.to_string(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            HealthObject::Machine(_) => "machine",
            HealthObject::NvLinkDomain(_) => "nvlink-domain",
            HealthObject::PowerShelf(_) => "power-shelf",
            HealthObject::Rack(_) => "rack",
            HealthObject::Switch(_) => "switch",
        }
    }

    fn label(&self) -> String {
        match self {
            HealthObject::Machine(id) => id.machine_type().to_string(),
            HealthObject::NvLinkDomain(_) => "NVLink Domain".to_string(),
            HealthObject::PowerShelf(_) => "Power Shelf".to_string(),
            HealthObject::Rack(_) => "Rack".to_string(),
            HealthObject::Switch(_) => "Switch".to_string(),
        }
    }

    fn detail_url(&self) -> String {
        format!("/admin/{}/{}", self.kind(), self.id())
    }

    /// Returns the admin health page URL for this object.
    fn health_url(&self) -> String {
        format!("{}/health", self.detail_url())
    }

    fn history_url(&self) -> Option<String> {
        match self {
            HealthObject::Machine(_)
            | HealthObject::PowerShelf(_)
            | HealthObject::Rack(_)
            | HealthObject::Switch(_) => Some(format!("{}/health-history", self.detail_url())),
            HealthObject::NvLinkDomain(_) => None,
        }
    }

    fn show_machine_templates(&self) -> bool {
        matches!(
            self,
            HealthObject::Machine(id)
                if id.machine_type().is_host() || id.machine_type().is_predicted_host()
        )
    }
}

/// Builds the admin health URL for an NVLink domain.
pub(super) fn nvlink_domain_health_url(domain_id: &NvLinkDomainId) -> String {
    HealthObject::NvLinkDomain(*domain_id).health_url()
}

struct HealthPageData {
    entries: Vec<::rpc::forge::HealthReportEntry>,
    aggregate_health: Option<HealthReport>,
    health_contributors: Vec<LabeledHealthReport>,
    history: HealthHistoryTable,
}

struct MachineHealthSnapshot {
    aggregate_health: Option<HealthReport>,
    associated_dpu_machine_ids: Vec<MachineId>,
}

/// View machine health.
pub(super) async fn machine_health(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
) -> Response {
    let Ok(machine_id) = MachineId::from_str(&machine_id) else {
        return (StatusCode::BAD_REQUEST, "invalid machine id").into_response();
    };

    let data = match fetch_machine_health_page_data(&state, &machine_id).await {
        Ok(data) => data,
        Err(response) => return response,
    };

    render_health(HealthObject::Machine(machine_id), data)
}

/// View rack health.
pub(super) async fn rack_health(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(rack_id): AxumPath<String>,
) -> Response {
    let Ok(rack_id) = RackId::from_str(&rack_id) else {
        return (StatusCode::BAD_REQUEST, "invalid rack id").into_response();
    };

    let data = match fetch_rack_health_page_data(&state, &rack_id).await {
        Ok(data) => data,
        Err(response) => return response,
    };

    render_health(HealthObject::Rack(rack_id), data)
}

/// View power shelf health.
pub(super) async fn power_shelf_health(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(power_shelf_id): AxumPath<String>,
) -> Response {
    let Ok(power_shelf_id) = PowerShelfId::from_str(&power_shelf_id) else {
        return (StatusCode::BAD_REQUEST, "invalid power shelf id").into_response();
    };

    let data = match fetch_power_shelf_health_page_data(&state, &power_shelf_id).await {
        Ok(data) => data,
        Err(response) => return response,
    };

    render_health(HealthObject::PowerShelf(power_shelf_id), data)
}

/// View switch health.
pub(super) async fn switch_health(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(switch_id): AxumPath<String>,
) -> Response {
    let Ok(switch_id) = SwitchId::from_str(&switch_id) else {
        return (StatusCode::BAD_REQUEST, "invalid switch id").into_response();
    };

    let data = match fetch_switch_health_page_data(&state, &switch_id).await {
        Ok(data) => data,
        Err(response) => return response,
    };

    render_health(HealthObject::Switch(switch_id), data)
}

/// View NVLink domain health.
pub(super) async fn nvlink_domain_health(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(domain_id): AxumPath<String>,
) -> Response {
    let Ok(domain_id) = NvLinkDomainId::from_str(&domain_id) else {
        return (StatusCode::BAD_REQUEST, "invalid NVLink domain id").into_response();
    };

    let data = match fetch_nvlink_domain_health_page_data(&state, &domain_id).await {
        Ok(data) => data,
        Err(response) => return response,
    };

    render_health(HealthObject::NvLinkDomain(domain_id), data)
}

/// Redirect NVLink domain details to the health page.
pub(super) async fn nvlink_domain_detail(AxumPath(domain_id): AxumPath<String>) -> Response {
    let Ok(domain_id) = NvLinkDomainId::from_str(&domain_id) else {
        return (StatusCode::BAD_REQUEST, "invalid NVLink domain id").into_response();
    };

    let health_url = nvlink_domain_health_url(&domain_id);

    Redirect::to(&health_url).into_response()
}

fn render_health(object: HealthObject, data: HealthPageData) -> Response {
    let mut entries = data.entries;
    // Sort by type first and source name second.
    entries.sort_by(|a, b| {
        if a.mode() == HealthReportApplyMode::Replace {
            return std::cmp::Ordering::Less;
        } else if b.mode() == HealthReportApplyMode::Replace {
            return std::cmp::Ordering::Greater;
        }
        a.report
            .as_ref()
            .map(|a| &a.source)
            .cmp(&b.report.as_ref().map(|b| &b.source))
    });

    let entries: Vec<HealthReportEntry> = entries
        .into_iter()
        .map(HealthReportEntry::from_rpc_convert_invalid)
        .collect();

    let display = HealthPage {
        id: object.id(),
        object_label: object.label(),
        object_detail_url: object.detail_url(),
        show_machine_templates: object.show_machine_templates(),
        aggregate_health: LabeledHealthReport {
            label: "Aggregate Health".to_string(),
            report: data.aggregate_health,
        },
        health_contributors: data.health_contributors,
        entries,
        history_url: object.history_url(),
        history: data.history,
    };

    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

fn not_found_response(kind: &str, id: &impl fmt::Display) -> Response {
    (StatusCode::NOT_FOUND, format!("{kind} not found: {id}")).into_response()
}

async fn fetch_machine_health_page_data(
    api: &Api,
    machine_id: &MachineId,
) -> Result<HealthPageData, Response> {
    let snapshot = fetch_machine_health_snapshot(api, machine_id).await?;
    let entries = match list_machine_health_report_entries(api, machine_id).await {
        Ok(entries) => entries,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "list_health_report_entries");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };
    let health_contributors =
        fetch_dpu_health_contributors(api, machine_id, snapshot.associated_dpu_machine_ids).await?;
    let history = match fetch_machine_health_history(api, machine_id).await {
        Ok(records) => HealthHistoryTable { records },
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "find_machine_health_histories");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(HealthPageData {
        entries,
        aggregate_health: snapshot.aggregate_health,
        health_contributors,
        history,
    })
}

async fn fetch_rack_health_page_data(
    api: &Api,
    rack_id: &RackId,
) -> Result<HealthPageData, Response> {
    let aggregate_health = fetch_rack_aggregate_health(api, rack_id).await?;
    let entries = match list_rack_health_report_entries(api, rack_id).await {
        Ok(entries) => entries,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %rack_id, "list_rack_health_report_overrides");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };
    let history = match fetch_rack_health_history(api, rack_id).await {
        Ok(records) => HealthHistoryTable { records },
        Err(err) => {
            tracing::error!(error = %err, %rack_id, "find_rack_health_histories");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(HealthPageData {
        entries,
        aggregate_health,
        health_contributors: Vec::new(),
        history,
    })
}

async fn fetch_power_shelf_health_page_data(
    api: &Api,
    power_shelf_id: &PowerShelfId,
) -> Result<HealthPageData, Response> {
    let aggregate_health = fetch_power_shelf_aggregate_health(api, power_shelf_id).await?;
    let entries = match list_power_shelf_health_report_entries(api, power_shelf_id).await {
        Ok(entries) => entries,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %power_shelf_id, "list_power_shelf_health_reports");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };
    let history = match fetch_power_shelf_health_history(api, power_shelf_id).await {
        Ok(records) => HealthHistoryTable { records },
        Err(err) => {
            tracing::error!(error = %err, %power_shelf_id, "find_power_shelf_health_histories");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(HealthPageData {
        entries,
        aggregate_health,
        health_contributors: Vec::new(),
        history,
    })
}

async fn fetch_switch_health_page_data(
    api: &Api,
    switch_id: &SwitchId,
) -> Result<HealthPageData, Response> {
    let aggregate_health = fetch_switch_aggregate_health(api, switch_id).await?;
    let entries = match list_switch_health_report_entries(api, switch_id).await {
        Ok(entries) => entries,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %switch_id, "list_switch_health_reports");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };
    let history = match fetch_switch_health_history(api, switch_id).await {
        Ok(records) => HealthHistoryTable { records },
        Err(err) => {
            tracing::error!(error = %err, %switch_id, "find_switch_health_histories");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(HealthPageData {
        entries,
        aggregate_health,
        health_contributors: Vec::new(),
        history,
    })
}

async fn fetch_nvlink_domain_health_page_data(
    api: &Api,
    domain_id: &NvLinkDomainId,
) -> Result<HealthPageData, Response> {
    match db::nvlink_domain_health_report::find(api.db_reader().as_mut(), domain_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(not_found_response("NVLink domain", domain_id)),
        Err(err) => {
            tracing::error!(error = %err, %domain_id, "find_nvlink_domain_health_reports");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    }

    let entries = match list_nvlink_domain_health_report_entries(api, domain_id).await {
        Ok(entries) => entries,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %domain_id, "list_nvlink_domain_health_reports");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };
    let aggregate_health = aggregate_health_report_entries(&entries);

    Ok(HealthPageData {
        entries,
        aggregate_health,
        health_contributors: Vec::new(),
        history: HealthHistoryTable {
            records: Vec::new(),
        },
    })
}

async fn fetch_dpu_health_contributors(
    api: &Api,
    host_machine_id: &MachineId,
    dpu_machine_ids: Vec<MachineId>,
) -> Result<Vec<LabeledHealthReport>, Response> {
    if dpu_machine_ids.is_empty() {
        return Ok(Vec::new());
    }

    let request = tonic::Request::new(MachinesByIdsRequest {
        machine_ids: dpu_machine_ids,
        include_history: false,
    });
    let dpus = match api
        .find_machines_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) => m.machines,
        Err(err) if err.code() == tonic::Code::NotFound => Vec::new(),
        Err(err) => {
            tracing::error!(error = %err, %host_machine_id, "find_dpu_machines_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(dpus
        .into_iter()
        .map(|dpu| LabeledHealthReport {
            label: format!(
                "DPU Health {}",
                dpu.id.map(|id| id.to_string()).unwrap_or_default()
            ),
            report: dpu.health.map(health_report_from_rpc_convert_invalid),
        })
        .collect())
}

async fn list_machine_health_report_entries(
    api: &Api,
    machine_id: &MachineId,
) -> Result<Vec<::rpc::forge::HealthReportEntry>, tonic::Status> {
    Ok(api
        .list_machine_health_reports(tonic::Request::new(*machine_id))
        .await?
        .into_inner()
        .health_report_entries)
}

async fn list_rack_health_report_entries(
    api: &Api,
    rack_id: &RackId,
) -> Result<Vec<::rpc::forge::HealthReportEntry>, tonic::Status> {
    Ok(api
        .list_rack_health_reports(tonic::Request::new(ListRackHealthReportsRequest {
            rack_id: Some(rack_id.clone()),
        }))
        .await?
        .into_inner()
        .health_report_entries)
}

async fn list_power_shelf_health_report_entries(
    api: &Api,
    power_shelf_id: &PowerShelfId,
) -> Result<Vec<::rpc::forge::HealthReportEntry>, tonic::Status> {
    Ok(api
        .list_power_shelf_health_reports(tonic::Request::new(ListPowerShelfHealthReportsRequest {
            power_shelf_id: Some(*power_shelf_id),
        }))
        .await?
        .into_inner()
        .health_report_entries)
}

async fn list_switch_health_report_entries(
    api: &Api,
    switch_id: &SwitchId,
) -> Result<Vec<::rpc::forge::HealthReportEntry>, tonic::Status> {
    Ok(api
        .list_switch_health_reports(tonic::Request::new(ListSwitchHealthReportsRequest {
            switch_id: Some(*switch_id),
        }))
        .await?
        .into_inner()
        .health_report_entries)
}

async fn list_nvlink_domain_health_report_entries(
    api: &Api,
    domain_id: &NvLinkDomainId,
) -> Result<Vec<::rpc::forge::HealthReportEntry>, tonic::Status> {
    Ok(api
        .list_nv_link_domain_health_reports(tonic::Request::new(
            ListNvLinkDomainHealthReportsRequest {
                domain_id: Some(*domain_id),
            },
        ))
        .await?
        .into_inner()
        .health_report_entries)
}

async fn fetch_machine_health_snapshot(
    api: &Api,
    machine_id: &MachineId,
) -> Result<MachineHealthSnapshot, Response> {
    let machine = match api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![*machine_id],
            include_history: false,
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) if m.machines.is_empty() => {
            return Err(not_found_response("Machine", machine_id));
        }
        Ok(m) if m.machines.len() != 1 => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Machine list for {machine_id} returned {} machines",
                    m.machines.len()
                ),
            )
                .into_response());
        }
        Ok(mut m) => m.machines.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return Err(not_found_response("Machine", machine_id));
        }
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "find_machines_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(MachineHealthSnapshot {
        aggregate_health: machine
            .health
            .as_ref()
            .map(|health| health_report_from_rpc_convert_invalid(health.clone())),
        associated_dpu_machine_ids: machine
            .associated_dpu_machine_ids
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

async fn fetch_rack_aggregate_health(
    api: &Api,
    rack_id: &RackId,
) -> Result<Option<HealthReport>, Response> {
    let rack = match api
        .find_racks_by_ids(tonic::Request::new(RacksByIdsRequest {
            rack_ids: vec![rack_id.clone()],
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(r) if r.racks.is_empty() => return Err(not_found_response("Rack", rack_id)),
        Ok(r) if r.racks.len() != 1 => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Rack list for {rack_id} returned {} racks", r.racks.len()),
            )
                .into_response());
        }
        Ok(mut r) => r.racks.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return Err(not_found_response("Rack", rack_id));
        }
        Err(err) => {
            tracing::error!(error = %err, %rack_id, "find_racks_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(rack
        .status
        .as_ref()
        .and_then(|status| status.health.as_ref())
        .map(|health| health_report_from_rpc_convert_invalid(health.clone())))
}

async fn fetch_power_shelf_aggregate_health(
    api: &Api,
    power_shelf_id: &PowerShelfId,
) -> Result<Option<HealthReport>, Response> {
    let power_shelf = match api
        .find_power_shelves_by_ids(tonic::Request::new(PowerShelvesByIdsRequest {
            power_shelf_ids: vec![*power_shelf_id],
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(r) if r.power_shelves.is_empty() => {
            return Err(not_found_response("Power shelf", power_shelf_id));
        }
        Ok(r) if r.power_shelves.len() != 1 => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Power shelf list for {power_shelf_id} returned {} power shelves",
                    r.power_shelves.len()
                ),
            )
                .into_response());
        }
        Ok(mut r) => r.power_shelves.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return Err(not_found_response("Power shelf", power_shelf_id));
        }
        Err(err) => {
            tracing::error!(error = %err, %power_shelf_id, "find_power_shelves_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(power_shelf
        .status
        .as_ref()
        .and_then(|status| status.health.as_ref())
        .map(|health| health_report_from_rpc_convert_invalid(health.clone())))
}

async fn fetch_switch_aggregate_health(
    api: &Api,
    switch_id: &SwitchId,
) -> Result<Option<HealthReport>, Response> {
    let switch = match api
        .find_switches_by_ids(tonic::Request::new(SwitchesByIdsRequest {
            switch_ids: vec![*switch_id],
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(r) if r.switches.is_empty() => return Err(not_found_response("Switch", switch_id)),
        Ok(r) if r.switches.len() != 1 => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Switch list for {switch_id} returned {} switches",
                    r.switches.len()
                ),
            )
                .into_response());
        }
        Ok(mut r) => r.switches.remove(0),
        Err(err) if err.code() == tonic::Code::NotFound => {
            return Err(not_found_response("Switch", switch_id));
        }
        Err(err) => {
            tracing::error!(error = %err, %switch_id, "find_switches_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(switch
        .status
        .as_ref()
        .and_then(|status| status.health.as_ref())
        .map(|health| health_report_from_rpc_convert_invalid(health.clone())))
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct HealthReportEntry {
    mode: String,
    health_report: HealthReport,
}

impl HealthReportEntry {
    fn from_rpc_convert_invalid(o: ::rpc::forge::HealthReportEntry) -> Self {
        let mode = match o.mode() {
            HealthReportApplyMode::Merge => "Merge",
            HealthReportApplyMode::Replace => "Replace",
        }
        .to_string();

        let health_report = o
            .report
            .map(health_report_from_rpc_convert_invalid)
            .unwrap_or_else(HealthReport::missing_report);

        HealthReportEntry {
            mode,
            health_report,
        }
    }
}

impl TryFrom<HealthReportEntry> for ::rpc::forge::HealthReportEntry {
    type Error = String;

    fn try_from(value: HealthReportEntry) -> Result<Self, Self::Error> {
        let mode = match value.mode.as_str() {
            "Replace" => HealthReportApplyMode::Replace,
            "Merge" => HealthReportApplyMode::Merge,
            m => {
                return Err(format!(
                    "Apply mode must be \"Replace\" or \"Merge\", but was \"{m}\""
                ));
            }
        };

        Ok(::rpc::forge::HealthReportEntry {
            mode: mode as i32,
            report: Some(::rpc::health::HealthReport::from(value.health_report)),
        })
    }
}

#[derive(serde::Deserialize)]
pub(super) struct RemoveHealthReport {
    source: String,
}

pub(super) async fn add_machine_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    auth_context: Option<axum::Extension<AuthContext>>,
    extract::Json(payload): extract::Json<HealthReportEntry>,
) -> Response {
    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    add_health_report_for(
        state,
        HealthObject::Machine(machine_id),
        auth_context,
        payload,
    )
    .await
}

pub(super) async fn add_rack_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(rack_id): AxumPath<String>,
    auth_context: Option<axum::Extension<AuthContext>>,
    extract::Json(payload): extract::Json<HealthReportEntry>,
) -> Response {
    let rack_id = match rack_id.parse::<RackId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    add_health_report_for(state, HealthObject::Rack(rack_id), auth_context, payload).await
}

pub(super) async fn add_power_shelf_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(power_shelf_id): AxumPath<String>,
    auth_context: Option<axum::Extension<AuthContext>>,
    extract::Json(payload): extract::Json<HealthReportEntry>,
) -> Response {
    let power_shelf_id = match power_shelf_id.parse::<PowerShelfId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    add_health_report_for(
        state,
        HealthObject::PowerShelf(power_shelf_id),
        auth_context,
        payload,
    )
    .await
}

pub(super) async fn add_switch_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(switch_id): AxumPath<String>,
    auth_context: Option<axum::Extension<AuthContext>>,
    extract::Json(payload): extract::Json<HealthReportEntry>,
) -> Response {
    let switch_id = match switch_id.parse::<SwitchId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    add_health_report_for(
        state,
        HealthObject::Switch(switch_id),
        auth_context,
        payload,
    )
    .await
}

/// Insert an NVLink domain health report from the admin web UI.
pub(super) async fn add_nvlink_domain_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(domain_id): AxumPath<String>,
    auth_context: Option<axum::Extension<AuthContext>>,
    extract::Json(payload): extract::Json<HealthReportEntry>,
) -> Response {
    let domain_id = match domain_id.parse::<NvLinkDomainId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    add_health_report_for(
        state,
        HealthObject::NvLinkDomain(domain_id),
        auth_context,
        payload,
    )
    .await
}

async fn add_health_report_for(
    state: Arc<Api>,
    object: HealthObject,
    auth_context: Option<axum::Extension<AuthContext>>,
    payload: HealthReportEntry,
) -> Response {
    let entry = match ::rpc::forge::HealthReportEntry::try_from(payload) {
        Ok(entry) => entry,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let object_id = object.id();
    let object_kind = object.kind();
    let result = match object {
        HealthObject::Machine(machine_id) => {
            let mut request = tonic::Request::new(InsertMachineHealthReportRequest {
                machine_id: Some(machine_id),
                health_report_entry: Some(entry),
            });
            if let Some(axum::Extension(auth_context)) = auth_context {
                request.extensions_mut().insert(auth_context);
            }
            state
                .insert_machine_health_report(request)
                .await
                .map(|response| response.into_inner())
        }
        HealthObject::NvLinkDomain(domain_id) => {
            let mut request = tonic::Request::new(InsertNvLinkDomainHealthReportRequest {
                domain_id: Some(domain_id),
                health_report_entry: Some(entry),
            });
            if let Some(axum::Extension(auth_context)) = auth_context {
                request.extensions_mut().insert(auth_context);
            }
            state
                .insert_nv_link_domain_health_report(request)
                .await
                .map(|response| response.into_inner())
        }
        HealthObject::PowerShelf(power_shelf_id) => {
            let mut request = tonic::Request::new(InsertPowerShelfHealthReportRequest {
                power_shelf_id: Some(power_shelf_id),
                health_report_entry: Some(entry),
            });
            if let Some(axum::Extension(auth_context)) = auth_context {
                request.extensions_mut().insert(auth_context);
            }
            state
                .insert_power_shelf_health_report(request)
                .await
                .map(|response| response.into_inner())
        }
        HealthObject::Rack(rack_id) => {
            let mut request = tonic::Request::new(InsertRackHealthReportRequest {
                rack_id: Some(rack_id),
                health_report_entry: Some(entry),
            });
            if let Some(axum::Extension(auth_context)) = auth_context {
                request.extensions_mut().insert(auth_context);
            }
            state
                .insert_rack_health_report(request)
                .await
                .map(|response| response.into_inner())
        }
        HealthObject::Switch(switch_id) => {
            let mut request = tonic::Request::new(InsertSwitchHealthReportRequest {
                switch_id: Some(switch_id),
                health_report_entry: Some(entry),
            });
            if let Some(axum::Extension(auth_context)) = auth_context {
                request.extensions_mut().insert(auth_context);
            }
            state
                .insert_switch_health_report(request)
                .await
                .map(|response| response.into_inner())
        }
    };

    match result {
        Err(err) if err.code() == tonic::Code::NotFound => {
            (StatusCode::NOT_FOUND, format!("Not found: {object_id}")).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, %object_id, object_kind, "insert_health_report");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(_) => (StatusCode::OK, String::new()).into_response(),
    }
}

pub(super) async fn remove_machine_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    extract::Json(payload): extract::Json<RemoveHealthReport>,
) -> Response {
    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    remove_health_report_for(state, HealthObject::Machine(machine_id), payload).await
}

pub(super) async fn remove_rack_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(rack_id): AxumPath<String>,
    extract::Json(payload): extract::Json<RemoveHealthReport>,
) -> Response {
    let rack_id = match rack_id.parse::<RackId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    remove_health_report_for(state, HealthObject::Rack(rack_id), payload).await
}

pub(super) async fn remove_power_shelf_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(power_shelf_id): AxumPath<String>,
    extract::Json(payload): extract::Json<RemoveHealthReport>,
) -> Response {
    let power_shelf_id = match power_shelf_id.parse::<PowerShelfId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    remove_health_report_for(state, HealthObject::PowerShelf(power_shelf_id), payload).await
}

pub(super) async fn remove_switch_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(switch_id): AxumPath<String>,
    extract::Json(payload): extract::Json<RemoveHealthReport>,
) -> Response {
    let switch_id = match switch_id.parse::<SwitchId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    remove_health_report_for(state, HealthObject::Switch(switch_id), payload).await
}

/// Remove an NVLink domain health report from the admin web UI.
pub(super) async fn remove_nvlink_domain_health_report(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(domain_id): AxumPath<String>,
    extract::Json(payload): extract::Json<RemoveHealthReport>,
) -> Response {
    let domain_id = match domain_id.parse::<NvLinkDomainId>() {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    remove_health_report_for(state, HealthObject::NvLinkDomain(domain_id), payload).await
}

async fn remove_health_report_for(
    state: Arc<Api>,
    object: HealthObject,
    payload: RemoveHealthReport,
) -> Response {
    let object_id = object.id();
    let object_kind = object.kind();
    let result = match object {
        HealthObject::Machine(machine_id) => state
            .remove_machine_health_report(tonic::Request::new(RemoveMachineHealthReportRequest {
                machine_id: Some(machine_id),
                source: payload.source,
            }))
            .await
            .map(|response| response.into_inner()),
        HealthObject::NvLinkDomain(domain_id) => state
            .remove_nv_link_domain_health_report(tonic::Request::new(
                RemoveNvLinkDomainHealthReportRequest {
                    domain_id: Some(domain_id),
                    source: payload.source,
                },
            ))
            .await
            .map(|response| response.into_inner()),
        HealthObject::PowerShelf(power_shelf_id) => state
            .remove_power_shelf_health_report(tonic::Request::new(
                RemovePowerShelfHealthReportRequest {
                    power_shelf_id: Some(power_shelf_id),
                    source: payload.source,
                },
            ))
            .await
            .map(|response| response.into_inner()),
        HealthObject::Rack(rack_id) => state
            .remove_rack_health_report(tonic::Request::new(RemoveRackHealthReportRequest {
                rack_id: Some(rack_id),
                source: payload.source,
            }))
            .await
            .map(|response| response.into_inner()),
        HealthObject::Switch(switch_id) => state
            .remove_switch_health_report(tonic::Request::new(RemoveSwitchHealthReportRequest {
                switch_id: Some(switch_id),
                source: payload.source,
            }))
            .await
            .map(|response| response.into_inner()),
    };

    match result {
        Err(err) if err.code() == tonic::Code::NotFound => {
            (StatusCode::NOT_FOUND, format!("Not found: {object_id}")).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, %object_id, object_kind, "remove_health_report");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
        Ok(_) => (StatusCode::OK, String::new()).into_response(),
    }
}

fn health_report_from_rpc_convert_invalid(
    report: rpc::health::HealthReport,
) -> health_report::HealthReport {
    health_report::HealthReport::try_from(report)
        .unwrap_or_else(health_report::HealthReport::malformed_report)
}

fn aggregate_health_report_entries(
    entries: &[::rpc::forge::HealthReportEntry],
) -> Option<HealthReport> {
    let mut aggregate = HealthReport::empty("aggregate-nvlink-domain-health".to_string());
    let mut has_merge = false;
    let mut replace = None;

    for entry in entries {
        let Some(report) = entry.report.as_ref() else {
            continue;
        };
        let report = health_report_from_rpc_convert_invalid(report.clone());

        if entry.mode() == HealthReportApplyMode::Replace {
            replace = Some(report);
        } else {
            aggregate.merge(&report);
            has_merge = true;
        }
    }

    replace.or_else(|| has_merge.then_some(aggregate))
}

/// Extracts and converts the records for `object_id` from a `HealthHistories`
/// response map, dropping entries for any other object id.
fn health_history_records_for(
    mut histories: std::collections::HashMap<String, ::rpc::forge::HealthHistoryRecords>,
    object_id: &str,
) -> Vec<HealthHistoryRecord> {
    histories
        .remove(object_id)
        .unwrap_or_default()
        .records
        .into_iter()
        .map(HealthHistoryRecord::from_rpc_convert_invalid)
        .collect()
}

pub(super) async fn fetch_machine_health_history(
    api: &Api,
    machine_id: &MachineId,
) -> Result<Vec<HealthHistoryRecord>, tonic::Status> {
    let histories = api
        .find_machine_health_histories(tonic::Request::new(
            ::rpc::forge::MachineHealthHistoriesRequest {
                machine_ids: vec![*machine_id],
                start_time: None,
                end_time: None,
            },
        ))
        .await
        .map(|response| response.into_inner())?
        .histories;

    Ok(health_history_records_for(
        histories,
        &machine_id.to_string(),
    ))
}

pub(super) async fn fetch_switch_health_history(
    api: &Api,
    switch_id: &SwitchId,
) -> Result<Vec<HealthHistoryRecord>, tonic::Status> {
    let histories = api
        .find_switch_health_histories(tonic::Request::new(
            ::rpc::forge::SwitchHealthHistoriesRequest {
                switch_ids: vec![*switch_id],
                start_time: None,
                end_time: None,
            },
        ))
        .await
        .map(|response| response.into_inner())?
        .histories;

    Ok(health_history_records_for(
        histories,
        &switch_id.to_string(),
    ))
}

pub(super) async fn fetch_rack_health_history(
    api: &Api,
    rack_id: &RackId,
) -> Result<Vec<HealthHistoryRecord>, tonic::Status> {
    let histories = api
        .find_rack_health_histories(tonic::Request::new(
            ::rpc::forge::RackHealthHistoriesRequest {
                rack_ids: vec![rack_id.clone()],
                start_time: None,
                end_time: None,
            },
        ))
        .await
        .map(|response| response.into_inner())?
        .histories;

    Ok(health_history_records_for(histories, rack_id.as_ref()))
}

pub(super) async fn fetch_power_shelf_health_history(
    api: &Api,
    power_shelf_id: &PowerShelfId,
) -> Result<Vec<HealthHistoryRecord>, tonic::Status> {
    let histories = api
        .find_power_shelf_health_histories(tonic::Request::new(
            ::rpc::forge::PowerShelfHealthHistoriesRequest {
                power_shelf_ids: vec![*power_shelf_id],
                start_time: None,
                end_time: None,
            },
        ))
        .await
        .map(|response| response.into_inner())?
        .histories;

    Ok(health_history_records_for(
        histories,
        &power_shelf_id.to_string(),
    ))
}

impl super::Base for HealthPage {}
