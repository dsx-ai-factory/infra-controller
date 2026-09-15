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

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use carbide_api_core::{Api, DefaultCredential};
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;
use rpc::forge::{self as forgerpc, BmcEndpointRequest, admin_power_control_request};
use rpc::site_explorer::{
    ExploredEndpoint, InternalLockdownStatus, LockdownStatus, MachineSetupStatus, SecureBootStatus,
    SiteExplorationReport,
};
use serde::Deserialize;

use super::pagination::{self, PageContext, PaginationParams};
use super::{Base, filters};
use crate::action_status::{self, ActionStatus};
use crate::site_explorer_run_status::SiteExplorerLastRunDisplay;

#[derive(Template)]
#[template(path = "explored_endpoints_show.html")]
struct ExploredEndpointsShow {
    last_run: Option<SiteExplorerLastRunDisplay>,
    vendors: Vec<String>,
    endpoints: Vec<ExploredEndpointDisplay>,
    filter_name: &'static str,
    active_vendor_filter: String,
    is_errors_only: bool,
    page: PageContext,
    missing_default_credentials: Vec<DefaultCredential>,
}

#[derive(Template)]
#[template(path = "explored_endpoints_show_paired.html")]
struct ExploredEndpointsShowPaired {
    last_run: Option<SiteExplorerLastRunDisplay>,
    managed_hosts: Vec<ExploredManagedHostDisplay>,
    page: PageContext,
    missing_default_credentials: Vec<DefaultCredential>,
}

fn last_run_from_report(report: &SiteExplorationReport) -> Option<SiteExplorerLastRunDisplay> {
    report.last_run.as_ref().map(Into::into)
}

fn managed_hosts_from_report(report: &SiteExplorationReport) -> Vec<ExploredManagedHostDisplay> {
    let mut managed_hosts = Vec::new();
    for mh in &report.managed_hosts {
        let host = match report
            .endpoints
            .binary_search_by(|ep| ep.address.cmp(&mh.host_bmc_ip))
        {
            Ok(idx) => Some(&report.endpoints[idx]),
            Err(_) => None,
        };
        let host = host.and_then(|h| h.report.as_ref());

        managed_hosts.push(ExploredManagedHostDisplay {
            host_bmc_ip: mh.host_bmc_ip.clone(),
            host_vendor: host
                .map(|report| report.vendor().to_string())
                .unwrap_or_default(),
            host_serial_numbers: host
                .map(|report| {
                    report
                        .systems
                        .iter()
                        .filter_map(|sys| sys.serial_number.clone())
                        .collect()
                })
                .unwrap_or_default(),
            dpus: mh
                .dpus
                .iter()
                .map(|d| {
                    let report = report
                        .endpoints
                        .iter()
                        .find(|ep| ep.address == d.bmc_ip)
                        .and_then(|dpu| dpu.report.as_ref());
                    ExploredDpuDisplay {
                        dpu_bmc_ip: d.bmc_ip.clone(),
                        dpu_machine_id: report
                            .map(|r| r.machine_id().to_string())
                            .unwrap_or_default(),
                        dpu_serial_numbers: report
                            .map(|r| {
                                r.systems
                                    .iter()
                                    .filter_map(|sys| sys.serial_number.clone())
                                    .collect()
                            })
                            .unwrap_or_default(),
                        host_pf_mac: d.host_pf_mac_address.clone().unwrap_or_default(),
                        dpu_oob_mac: report
                            .and_then(|r| r.systems.first())
                            .and_then(|sys| sys.ethernet_interfaces.first())
                            .and_then(|iface| iface.mac_address.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect(),
        });
    }
    managed_hosts
}

struct ExploredDpuDisplay {
    dpu_bmc_ip: String,
    dpu_machine_id: String,
    dpu_serial_numbers: Vec<String>,
    host_pf_mac: String,
    dpu_oob_mac: String,
}

struct ExploredManagedHostDisplay {
    host_bmc_ip: String,
    host_serial_numbers: Vec<String>,
    host_vendor: String,
    dpus: Vec<ExploredDpuDisplay>,
}

struct ExploredEndpointDisplay {
    address: String,
    endpoint_type: String,
    last_exploration_latency: Option<i64>,
    last_exploration_error: String,
    has_exploration_error: bool,
    vendor: String,
    bmc_mac_addrs: Vec<String>,
    power_states: Vec<String>,
    machine_id: String,
    preingestion_state: String,
    serial_numbers: Vec<String>,
}

impl From<&ExploredEndpoint> for ExploredEndpointDisplay {
    fn from(ep: &ExploredEndpoint) -> Self {
        let report_ref = ep.report.as_ref();
        Self {
            address: ep.address.clone(),
            endpoint_type: report_ref
                .map(|report| report.endpoint_type.clone())
                .unwrap_or_default(),
            last_exploration_error: report_ref
                .map(|report| report.last_exploration_error_display())
                .unwrap_or_default(),
            last_exploration_latency: report_ref
                .and_then(|report| report.last_exploration_latency.as_ref())
                .map(|latency| latency.seconds),
            has_exploration_error: report_ref
                .map(|report| report.has_last_exploration_error())
                .unwrap_or_default(),
            bmc_mac_addrs: report_ref
                .map(|report| {
                    report
                        .managers
                        .iter()
                        .flat_map(|m| {
                            m.ethernet_interfaces
                                .iter()
                                .filter_map(|iface| iface.mac_address.clone())
                        })
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default(),
            vendor: report_ref
                .and_then(|report| report.vendor.clone())
                .unwrap_or_default(),
            machine_id: report_ref
                .and_then(|report| report.machine_id.clone())
                .unwrap_or_default(),
            preingestion_state: ep.preingestion_state.clone(),
            serial_numbers: report_ref
                .map(|report| {
                    report
                        .systems
                        .iter()
                        .map(|s| s.serial_number().to_string())
                        .collect()
                })
                .unwrap_or_default(),
            power_states: report_ref
                .map(|report| {
                    report
                        .systems
                        .iter()
                        .map(|s| format!("{:?}", s.power_state()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// List explored endpoints
pub(super) async fn show_html_all(
    AxumState(state): AxumState<Arc<Api>>,
    Query(mut params): Query<HashMap<String, String>>,
) -> Response {
    let report = match fetch_explored_endpoints(&state).await {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(error = %err, "fetch_explored_endpoints");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading site exploration report",
            )
                .into_response();
        }
    };

    let pagination_params = PaginationParams {
        current_page: params.remove("current_page").and_then(|s| s.parse().ok()),
        limit: params.remove("limit").and_then(|s| s.parse().ok()),
    };

    let endpoints: Vec<ExploredEndpointDisplay> = report.endpoints.iter().map(Into::into).collect();
    let vendors = vendors(&endpoints);
    let vendor_filter = params
        .get("vendor-filter")
        .cloned()
        .unwrap_or("all".to_string());
    let is_errors_only = params
        .get("errors-only")
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);

    let mut extra_query_params = String::new();
    if vendor_filter != "all" {
        extra_query_params.push_str(&format!("&vendor-filter={vendor_filter}"));
    }
    if is_errors_only {
        extra_query_params.push_str("&errors-only=true");
    }

    let query_filter = query_filter_for(params);
    let filtered: Vec<_> = endpoints.into_iter().filter(|x| query_filter(x)).collect();
    let (info, endpoints) = pagination::paginate_vec(filtered, &pagination_params);

    let tmpl = ExploredEndpointsShow {
        last_run: last_run_from_report(&report),
        filter_name: "All",
        vendors,
        endpoints,
        active_vendor_filter: vendor_filter,
        is_errors_only,
        page: PageContext::new(info, "/admin/explored-endpoint")
            .with_extra_params(extra_query_params),
        missing_default_credentials: state.missing_default_credentials().await,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_html_paired(
    AxumState(state): AxumState<Arc<Api>>,
    Query(params): Query<PaginationParams>,
) -> Response {
    let report = match fetch_explored_endpoints(&state).await {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(error = %err, "fetch_explored_endpoints");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading site exploration report",
            )
                .into_response();
        }
    };

    let all_hosts = managed_hosts_from_report(&report);
    let (info, managed_hosts) = pagination::paginate_vec(all_hosts, &params);

    let tmpl = ExploredEndpointsShowPaired {
        last_run: last_run_from_report(&report),
        managed_hosts,
        page: PageContext::new(info, "/admin/explored-endpoint/paired"),
        missing_default_credentials: state.missing_default_credentials().await,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_html_unpaired(
    AxumState(state): AxumState<Arc<Api>>,
    Query(mut params): Query<HashMap<String, String>>,
) -> Response {
    let report = match fetch_explored_endpoints(&state).await {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(error = %err, "fetch_explored_endpoints");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading site exploration report",
            )
                .into_response();
        }
    };

    let pagination_params = PaginationParams {
        current_page: params.remove("current_page").and_then(|s| s.parse().ok()),
        limit: params.remove("limit").and_then(|s| s.parse().ok()),
    };

    let paired_bmcs: HashSet<&str> = report
        .managed_hosts
        .iter()
        .flat_map(|mh| [mh.host_bmc_ip.as_str(), mh.dpu_bmc_ip.as_str()])
        .collect();
    let endpoints: Vec<ExploredEndpointDisplay> = report
        .endpoints
        .iter()
        .filter(|ep| !paired_bmcs.contains(ep.address.as_str()))
        .map(Into::into)
        .collect();

    // We have filtered out the ones Site Explorer paired. Now filter the pre-site-explorer ones.
    // Once we are 100% site explorer everywhere we can remove this part
    let bmc_ips: Vec<String> = endpoints.iter().map(|ep| ep.address.clone()).collect();
    let req = tonic::Request::new(forgerpc::BmcIpList { bmc_ips });
    let legacy_paired_bmcs: HashSet<String> = match state.find_machine_ids_by_bmc_ips(req).await {
        Ok(res) => res
            .into_inner()
            .pairs
            .into_iter()
            .map(|pair| pair.bmc_ip)
            .collect(),
        Err(err) => {
            tracing::error!(error = %err, "find_machine_ids_by_bmc_ips");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error find_machine_ids_by_bmc_ips",
            )
                .into_response();
        }
    };
    let endpoints: Vec<_> = endpoints
        .into_iter()
        .filter(|ep| !legacy_paired_bmcs.contains(ep.address.as_str()))
        .collect();

    let vendors = vendors(&endpoints);

    let vendor_filter = params
        .get("vendor-filter")
        .cloned()
        .unwrap_or("all".to_string());
    let is_errors_only = params
        .get("errors-only")
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false);

    let mut extra_query_params = String::new();
    if vendor_filter != "all" {
        extra_query_params.push_str(&format!("&vendor-filter={vendor_filter}"));
    }
    if is_errors_only {
        extra_query_params.push_str("&errors-only=true");
    }

    let query_filter = query_filter_for(params);
    let filtered: Vec<_> = endpoints.into_iter().filter(|x| query_filter(x)).collect();
    let (info, endpoints) = pagination::paginate_vec(filtered, &pagination_params);

    let tmpl = ExploredEndpointsShow {
        last_run: last_run_from_report(&report),
        filter_name: "Unpaired",
        vendors,
        endpoints,
        active_vendor_filter: vendor_filter,
        is_errors_only,
        page: PageContext::new(info, "/admin/explored-endpoint/unpaired")
            .with_extra_params(extra_query_params),
        missing_default_credentials: state.missing_default_credentials().await,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let report = match fetch_explored_endpoints(&state).await {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(error = %err, "fetch_explored_endpoints");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading site exploration report",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(report)).into_response()
}

async fn fetch_explored_endpoints(api: &Api) -> Result<SiteExplorationReport, tonic::Status> {
    let request = tonic::Request::new(forgerpc::GetSiteExplorationRequest {});
    api.get_site_exploration_report(request)
        .await
        .map(|response| response.into_inner())
        .map(|mut report| {
            // Sort everything for a a pretter display
            report.endpoints.sort_by(|a, b| a.address.cmp(&b.address));
            report
                .managed_hosts
                .sort_by(|a, b| a.host_bmc_ip.cmp(&b.host_bmc_ip));
            report
        })
}

#[derive(Template)]
#[template(path = "explored_endpoint_detail.html")]
struct ExploredEndpointDetail<'a> {
    endpoint: ExploredEndpoint,
    has_exploration_error: bool,
    last_exploration_error: String,
    machine_setup_status: String,
    credentials_set: String,
    has_machine: bool,
    secure_boot_status: String,
    lockdown_status: String,
    is_dell_endpoint: bool,
    report_age: String,
    pause_remediation: bool,
    bmc_action_redirect_to: Option<String>,
    action_status: Option<ActionStatus<'a>>,
}

struct ExploredEndpointInfo {
    endpoint: ExploredEndpoint,
    credentials_set: String,
    has_machine: bool,
}

impl From<ExploredEndpointInfo> for ExploredEndpointDetail<'_> {
    fn from(endpoint_info: ExploredEndpointInfo) -> Self {
        let report_ref = endpoint_info.endpoint.report.as_ref();

        // Check if this is a Dell endpoint
        let is_dell_endpoint = report_ref
            .and_then(|report| report.vendor.as_ref())
            .map(|vendor| vendor.to_lowercase() == "dell")
            .unwrap_or(false);

        let report_age =
            config_version::ConfigVersion::from_str(&endpoint_info.endpoint.report_version)
                .ok()
                .map(|v| {
                    let elapsed = v.since_state_change();
                    if elapsed.num_seconds() <= 0 {
                        "just now".to_string()
                    } else {
                        format!("{} ago", v.since_state_change_humanized())
                    }
                })
                .unwrap_or_else(|| "unknown".to_string());

        let pause_remediation = endpoint_info.endpoint.pause_remediation;

        Self {
            last_exploration_error: report_ref
                .map(|report| report.last_exploration_error_display())
                .unwrap_or_default(),
            has_exploration_error: report_ref
                .map(|report| report.has_last_exploration_error())
                .unwrap_or_default(),
            machine_setup_status: machine_setup_status_to_string(
                report_ref.and_then(|report| report.machine_setup_status.as_ref()),
            ),
            secure_boot_status: secure_boot_status_to_string(
                report_ref.and_then(|report| report.secure_boot_status.as_ref()),
            ),
            lockdown_status: lockdown_status_to_string(
                report_ref.and_then(|report| report.lockdown_status.as_ref()),
            ),
            endpoint: endpoint_info.endpoint,
            credentials_set: endpoint_info.credentials_set,
            has_machine: endpoint_info.has_machine,
            is_dell_endpoint,
            report_age,
            pause_remediation,
            bmc_action_redirect_to: None,
            action_status: None,
        }
    }
}

/// Fetch a single explored endpoint
/// TODO: The API is rather inefficient since it loads all of them and filters client side
async fn fetch_explored_endpoint(
    api: &Api,
    endpoint_ip: String,
) -> Result<ExploredEndpoint, Response> {
    let report = match fetch_explored_endpoints(api).await {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(error = %err, "fetch_explored_endpoints");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading site exploration report",
            )
                .into_response());
        }
    };

    let endpoint: ExploredEndpoint = match report
        .endpoints
        .into_iter()
        .find(|ep| ep.address.trim() == endpoint_ip.trim())
    {
        Some(ep) => ep,
        None => {
            return Err(super::not_found_response(endpoint_ip).into_response());
        }
    };

    Ok(endpoint)
}
/// View details of an explored endpoint
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (show_json, endpoint_ip) = match endpoint_ip.strip_suffix(".json") {
        Some(endpoint_ip) => (true, endpoint_ip.to_string()),
        None => (false, endpoint_ip),
    };

    let mut endpoint = match fetch_explored_endpoint(&state, endpoint_ip.clone()).await {
        Ok(endpoint) => endpoint,
        Err(response) => return response,
    };

    if show_json {
        return (StatusCode::OK, Json(endpoint)).into_response();
    }

    // Check if this endpoint has a machine
    let has_machine = match state
        .is_bmc_in_managed_host(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            ip_address: endpoint_ip.clone(),
            mac_address: None,
        }))
        .await
    {
        Ok(response) => response.into_inner().in_managed_host,
        Err(err) => {
            tracing::error!(error = %err, "is_bmc_in_managed_host check failed");
            // Default to true if we can't determine the status so we can't delete the endpoint
            true
        }
    };

    // Site Explorer doesn't link Host Explored Endpoints with their machine, only DPUs.
    // So do it here.
    if let Some(ref mut report) = endpoint.report
        && report.machine_id.is_none()
    {
        let req = tonic::Request::new(forgerpc::BmcIpList {
            bmc_ips: vec![endpoint.address.clone()],
        });
        match state.find_machine_ids_by_bmc_ips(req).await {
            Ok(res) => {
                if let Some(pair) = res.into_inner().pairs.first() {
                    // we found a matching machine
                    report.machine_id = pair
                        .machine_id
                        .as_ref()
                        .map(|machine_id| machine_id.to_string());
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "find_machine_ids_by_bmc_ips");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error find_machine_ids_by_bmc_ips",
                )
                    .into_response();
            }
        }
    }

    let req = tonic::Request::new(forgerpc::BmcIp {
        bmc_ip: endpoint_ip.clone(),
    });
    let mac_address = state
        .find_mac_address_by_bmc_ip(req)
        .await
        .map(|res| res.into_inner().mac_address)
        .unwrap_or_default();

    let credentials_set = if mac_address.is_empty() {
        "Not Configured".to_string()
    } else {
        match state
            .bmc_credential_status(tonic::Request::new(rpc::forge::BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: Some(mac_address),
            }))
            .await
            .map(|response| response.into_inner())
        {
            Ok(response) => {
                if response.have_credentials {
                    "Configured".to_string()
                } else {
                    "Not Configured".to_string()
                }
            }
            Err(err) => {
                tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "bmc_credential_status");
                "Not Configured".to_string()
            }
        }
    };

    let endpoint_info = ExploredEndpointInfo {
        endpoint,
        credentials_set,
        has_machine,
    };

    let mut display = ExploredEndpointDetail::from(endpoint_info);

    display.action_status = ActionStatus::from_query(&params);

    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

pub(super) async fn re_explore(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<ReExploreEndpointAction>,
) -> impl IntoResponse {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    if let Err(err) = state
        .re_explore_endpoint(tonic::Request::new(rpc::forge::ReExploreEndpointRequest {
            ip_address: endpoint_ip.clone(),
            if_version_match: form.if_version_match,
        }))
        .await
        .map(|response| response.into_inner())
    {
        tracing::error!(error = %err, bmc_ip_address = endpoint_ip, "re_explore_endpoint");
        return Redirect::to(&view_url);
    }

    Redirect::to(&view_url)
}

pub(super) async fn refresh_endpoint(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> impl IntoResponse {
    match state
        .refresh_endpoint_report(tonic::Request::new(
            rpc::forge::RefreshEndpointReportRequest {
                ip_address: endpoint_ip.clone(),
            },
        ))
        .await
    {
        Ok(response) => {
            let ep = response.into_inner();
            let has_error = ep
                .report
                .as_ref()
                .map(|report| report.has_last_exploration_error())
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(serde_json::json!({ "has_exploration_error": has_error })),
            )
                .into_response()
        }
        Err(err) => {
            let status_code = match err.code() {
                tonic::Code::AlreadyExists => StatusCode::CONFLICT,
                tonic::Code::NotFound => StatusCode::NOT_FOUND,
                tonic::Code::FailedPrecondition => StatusCode::PRECONDITION_FAILED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            tracing::error!(error = %err, bmc_ip_address = endpoint_ip, "refresh_endpoint");
            (
                status_code,
                Json(serde_json::json!({ "error": err.message() })),
            )
                .into_response()
        }
    }
}

pub(super) async fn pause_remediation(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<PauseRemediationAction>,
) -> impl IntoResponse {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    if let Err(err) = state
        .pause_explored_endpoint_remediation(tonic::Request::new(
            rpc::forge::PauseExploredEndpointRemediationRequest {
                ip_address: endpoint_ip.clone(),
                pause: form.pause,
            },
        ))
        .await
        .map(|response| response.into_inner())
    {
        tracing::error!(error = %err, bmc_ip_address = endpoint_ip, "pause_explored_endpoint_remediation");
        return Redirect::to(&view_url);
    }

    Redirect::to(&view_url)
}

#[derive(Deserialize, Debug)]
pub(super) struct ReExploreEndpointAction {
    if_version_match: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct PauseRemediationAction {
    pause: bool,
}

fn vendors(endpoints: &[ExploredEndpointDisplay]) -> Vec<String> {
    let vendors: HashSet<String> = endpoints
        .iter()
        .map(|ep| ep.vendor.clone())
        .filter(|v| !v.is_empty())
        .collect();
    let mut vendors: Vec<String> = vendors.into_iter().collect();
    vendors.sort();
    vendors
}

fn query_filter_for(
    mut params: HashMap<String, String>,
) -> Box<dyn Fn(&ExploredEndpointDisplay) -> bool + Send> {
    let vf: Box<dyn Fn(&ExploredEndpointDisplay) -> bool + Send> =
        match params.remove("vendor-filter").map(|v| v.trim().to_string()) {
            Some(v) if v != "all" => Box::new(move |ep: &ExploredEndpointDisplay| {
                ep.vendor.to_lowercase() == v || v == "none" && ep.vendor.is_empty()
            }),
            _ => Box::new(|_| true),
        };
    let ef: Box<dyn Fn(&ExploredEndpointDisplay) -> bool + Send> = if params
        .get("errors-only")
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false)
    {
        Box::new(|ep: &ExploredEndpointDisplay| ep.has_exploration_error)
    } else {
        Box::new(|_| true)
    };
    Box::new(move |x| vf(x) && ef(x))
}

pub(super) async fn power_control(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<PowerControlEndpointAction>,
) -> Response {
    let view_url = form
        .redirect_to
        .unwrap_or_else(|| format!("/admin/explored-endpoint/{endpoint_ip}"));

    let Some(action) = form.action else {
        return Redirect::to(&view_url).into_response();
    };

    let Some(act) = admin_power_control_request::SystemPowerControl::from_str_name(&action) else {
        tracing::error!(bmc_ip_address = %endpoint_ip, action = %action, "power_control_endpoint invalid action");
        let redirect_url = ActionStatus {
            action: action_status::Type::Power,
            class: action_status::Class::Error,
            message: "invalid action requested".into(),
        }
        .update_redirect_url(&view_url);
        return Redirect::to(&redirect_url).into_response();
    };

    match state
        .admin_power_control(tonic::Request::new(rpc::forge::AdminPowerControlRequest {
            machine_id: None,
            bmc_endpoint_request: Some(BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: None,
            }),
            action: act.into(),
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(response) => {
            let raw_msg = response
                .msg
                .unwrap_or_else(|| "completed successfully".to_string());
            let class = if raw_msg.to_lowercase().contains("warning") {
                action_status::Class::Warning
            } else {
                action_status::Class::Success
            };
            let friendly_action = match action.as_str() {
                "On" => "Power On",
                "GracefulShutdown" => "Graceful Shutdown",
                "ForceOff" => "Force Off",
                "GracefulRestart" => "Graceful Restart",
                "ForceRestart" => "Force Restart",
                "ACPowercycle" => "AC Powercycle",
                other => other,
            };
            let message = format!("{friendly_action} {raw_msg}");
            let redirect_url = ActionStatus {
                action: action_status::Type::Power,
                class,
                message: message.into(),
            }
            .update_redirect_url(&view_url);
            Redirect::to(&redirect_url).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, action = %action, "power_control_endpoint");
            let redirect_url = ActionStatus {
                action: action_status::Type::Power,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url);
            Redirect::to(&redirect_url).into_response()
        }
    }
}

#[derive(Deserialize, Debug)]
pub(super) struct PowerControlEndpointAction {
    action: Option<String>,
    redirect_to: Option<String>,
}

pub(super) async fn bmc_reset(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<BmcResetEndpointAction>,
) -> Response {
    let view_url = form
        .redirect_to
        .unwrap_or_else(|| format!("/admin/explored-endpoint/{endpoint_ip}"));

    let use_ipmi = match form.use_ipmi {
        Some(i) => i.parse::<bool>().unwrap_or_default(),
        _ => false,
    };

    match state
        .admin_bmc_reset(tonic::Request::new(rpc::forge::AdminBmcResetRequest {
            bmc_endpoint_request: Some(BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: None,
            }),
            use_ipmitool: use_ipmi,
            ..Default::default()
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_response) => {
            let method = if use_ipmi { "IPMI" } else { "Redfish" };
            let redirect_url = ActionStatus {
                action: action_status::Type::ResetBmc,
                class: action_status::Class::Success,
                message: format!("BMC Reset ({method}) initiated successfully").into(),
            }
            .update_redirect_url(&view_url);
            Redirect::to(&redirect_url).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, use_ipmi = %use_ipmi, "bmc_reset_endpoint");
            let redirect_url = ActionStatus {
                action: action_status::Type::ResetBmc,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url);
            Redirect::to(&redirect_url).into_response()
        }
    }
}

#[derive(Deserialize, Debug)]
pub(super) struct BmcResetEndpointAction {
    use_ipmi: Option<String>,
    redirect_to: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct MachineSetupAction {
    boot_interface_mac: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct DpuFirstBootOrderAction {
    boot_interface_mac: Option<String>,
}

pub(super) async fn clear_last_exploration_error(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    if let Err(err) = state
        .clear_site_exploration_error(tonic::Request::new(
            rpc::forge::ClearSiteExplorationErrorRequest {
                ip_address: endpoint_ip.clone(),
            },
        ))
        .await
        .map(|response| response.into_inner())
    {
        tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "clear_last_exploration_error_endpoint");
        return (StatusCode::INTERNAL_SERVER_ERROR, err.message().to_owned()).into_response();
    }

    Redirect::to(&view_url).into_response()
}

pub(super) async fn clear_bmc_credentials(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let req = tonic::Request::new(forgerpc::BmcIp {
        bmc_ip: endpoint_ip.clone(),
    });
    let mac_address = match state.find_mac_address_by_bmc_ip(req).await {
        Ok(res) => res.into_inner().mac_address,
        Err(err) => {
            tracing::error!(error = %err, "find_mac_address_by_bmc_ip");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error find_mac_address_by_bmc_ip",
            )
                .into_response();
        }
    };

    if let Err(err) = state
        .delete_credential(tonic::Request::new(rpc::forge::CredentialDeletionRequest {
            credential_type: rpc::CredentialType::RootBmcByMacAddress.into(),
            username: None,
            mac_address: Some(mac_address),
            credential_name: None,
        }))
        .await
        .map(|response| response.into_inner())
    {
        tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "clear_bmc_credentials");
        return (StatusCode::INTERNAL_SERVER_ERROR, err.message().to_owned()).into_response();
    }

    Redirect::to(&view_url).into_response()
}

pub(super) async fn disable_secure_boot(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let redirect_url = match state
        .disable_secure_boot(tonic::Request::new(rpc::forge::BmcEndpointRequest {
            ip_address: endpoint_ip.clone(),
            mac_address: None,
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::DisableSecureBoot,
            class: action_status::Class::Success,
            message: "Secure boot disabled successfully".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "disable_secure_boot");
            ActionStatus {
                action: action_status::Type::DisableSecureBoot,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

pub(super) async fn disable_lockdown(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let redirect_url = match state
        .lockdown(tonic::Request::new(rpc::forge::LockdownRequest {
            bmc_endpoint_request: Some(BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: None,
            }),
            machine_id: None,
            action: Some(rpc::forge::LockdownAction::Disable as i32),
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::DisableLockdown,
            class: action_status::Class::Success,
            message: "Lockdown disabled successfully".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "disable_lockdown");
            ActionStatus {
                action: action_status::Type::DisableLockdown,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

pub(super) async fn enable_lockdown(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let redirect_url = match state
        .lockdown(tonic::Request::new(rpc::forge::LockdownRequest {
            bmc_endpoint_request: Some(BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: None,
            }),
            machine_id: None,
            action: Some(rpc::forge::LockdownAction::Enable as i32),
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::EnableLockdown,
            class: action_status::Class::Success,
            message: "Lockdown enabled successfully".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "enable_lockdown");
            ActionStatus {
                action: action_status::Type::EnableLockdown,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

pub(super) async fn machine_setup(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<MachineSetupAction>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let boot_interface_mac = form
        .boot_interface_mac
        .as_ref()
        .filter(|mac| !mac.trim().is_empty())
        .map(|mac| mac.trim().to_string());

    if let Some(ref mac) = boot_interface_mac
        && mac.parse::<mac_address::MacAddress>().is_err()
    {
        tracing::error!(bmc_ip_address = %endpoint_ip, mac_address = %mac, "Invalid MAC address format");
        let status = ActionStatus {
            action: action_status::Type::MachineSetup,
            class: action_status::Class::Error,
            message: "Invalid MAC address format. Expected format: 00:11:22:33:44:55".into(),
        };
        return Redirect::to(&status.update_redirect_url(&view_url)).into_response();
    }

    let redirect_url = match state
        .machine_setup(tonic::Request::new(rpc::forge::MachineSetupRequest {
            machine_id: None,
            bmc_endpoint_request: Some(BmcEndpointRequest {
                ip_address: endpoint_ip.clone(),
                mac_address: None,
            }),
            boot_interface_mac,
        }))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::MachineSetup,
            class: action_status::Class::Success,
            message: "Machine setup request accepted".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "bmc_machine_setup");
            ActionStatus {
                action: action_status::Type::MachineSetup,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

pub(super) async fn set_dpu_first_boot_order(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
    Form(form): Form<DpuFirstBootOrderAction>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let boot_interface_mac = form
        .boot_interface_mac
        .as_ref()
        .filter(|mac| !mac.trim().is_empty())
        .map(|mac| mac.trim().to_string());

    // Validate MAC address format if provided
    if let Some(ref mac) = boot_interface_mac
        && mac.parse::<mac_address::MacAddress>().is_err()
    {
        tracing::error!(bmc_ip_address = %endpoint_ip, mac_address = %mac, "Invalid MAC address format");
        let redirect_url = ActionStatus {
            action: action_status::Type::SetFirstBootOrder,
            class: action_status::Class::Error,
            message: "Invalid MAC address format. Expected format - 00:11:22:33:44:55".into(),
        }
        .update_redirect_url(&view_url);
        return Redirect::to(&redirect_url).into_response();
    }

    let redirect_url = match state
        .set_dpu_first_boot_order(tonic::Request::new(
            rpc::forge::SetDpuFirstBootOrderRequest {
                machine_id: None,
                bmc_endpoint_request: Some(BmcEndpointRequest {
                    ip_address: endpoint_ip.clone(),
                    mac_address: None,
                }),
                boot_interface_mac,
            },
        ))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::SetFirstBootOrder,
            class: action_status::Class::Success,
            message: "Boot-interface request accepted".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "set_dpu_first_boot_order");
            ActionStatus {
                action: action_status::Type::SetFirstBootOrder,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

/// Requests another pass for the host's resolved boot interface.
///
/// This takes no MAC from the operator: it reuses `set_dpu_first_boot_order`
/// with `boot_interface_mac: None`, which makes the backend resolve the boot
/// interface the same way every other flow does -- the owning machine's
/// designated interface (`primary_interface` + its captured Redfish interface
/// id) once a machine owns this endpoint, or site-explorer's automatic default
/// for a not-yet-managed endpoint. Managed hosts persist a fresh generation
/// for machine-controller; unowned endpoints still apply it directly.
pub(super) async fn restore_boot_interface(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/explored-endpoint/{endpoint_ip}");

    let redirect_url = match state
        .set_dpu_first_boot_order(tonic::Request::new(
            rpc::forge::SetDpuFirstBootOrderRequest {
                machine_id: None,
                bmc_endpoint_request: Some(BmcEndpointRequest {
                    ip_address: endpoint_ip.clone(),
                    mac_address: None,
                }),
                boot_interface_mac: None,
            },
        ))
        .await
        .map(|response| response.into_inner())
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::RestoreBootInterface,
            class: action_status::Class::Success,
            message: "Boot-interface reconciliation request accepted".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "restore_boot_interface");
            ActionStatus {
                action: action_status::Type::RestoreBootInterface,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

pub(super) async fn delete_endpoint(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(endpoint_ip): AxumPath<String>,
) -> Response {
    let list_url = "/admin/explored-endpoint";

    match state
        .delete_explored_endpoint(tonic::Request::new(
            rpc::forge::DeleteExploredEndpointRequest {
                ip_address: endpoint_ip.clone(),
            },
        ))
        .await
        .map(|response| response.into_inner())
    {
        Ok(response) => {
            if response.deleted {
                tracing::info!(bmc_ip_address = %endpoint_ip, "Successfully deleted explored endpoint");
            } else {
                tracing::warn!(bmc_ip_address = %endpoint_ip, reason = ?response.message, "Failed to delete explored endpoint");
            }
        }
        Err(err) => {
            tracing::error!(error = %err, bmc_ip_address = %endpoint_ip, "delete_explored_endpoint");
            return (StatusCode::INTERNAL_SERVER_ERROR, err.message().to_owned()).into_response();
        }
    }

    // Redirect to the list page after deletion
    Redirect::to(list_url).into_response()
}

fn machine_setup_status_to_string(status: Option<&MachineSetupStatus>) -> String {
    match status {
        None => "Unable to fetch Machine Setup Status".to_string(),
        Some(s) if s.is_done => "OK".to_string(),
        Some(s) => {
            let diffs_string = s
                .diffs
                .iter()
                .map(|diff| {
                    format!(
                        "{} is '{}' expected '{}'",
                        diff.key, diff.actual, diff.expected
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("Mismatch: {diffs_string}")
        }
    }
}

fn secure_boot_status_to_string(status: Option<&SecureBootStatus>) -> String {
    match status {
        None => "Unknown".to_string(),
        Some(s) => {
            if s.is_enabled {
                "Enabled".to_string()
            } else {
                "Disabled".to_string()
            }
        }
    }
}

fn lockdown_status_to_string(status: Option<&LockdownStatus>) -> String {
    match status {
        None => "Unknown".to_string(),
        Some(s) => match s.status {
            x if x == InternalLockdownStatus::Enabled as i32 => "Enabled".to_string(),
            x if x == InternalLockdownStatus::Disabled as i32 => "Disabled".to_string(),
            x if x == InternalLockdownStatus::Partial as i32 => "Partial".to_string(),
            _ => "Unknown".to_string(),
        },
    }
}

impl super::Base for ExploredEndpointsShow {}
impl super::Base for ExploredEndpointsShowPaired {}
impl<'a> super::Base for ExploredEndpointDetail<'a> {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use askama::Template;
    use rpc::site_explorer::{EndpointExplorationReport, ExploredEndpoint, OperatorErrorSchema};

    use super::{ExploredEndpointDisplay, ExploredEndpointsShow, query_filter_for};
    use crate::pagination::PageContext;

    fn endpoint(address: &str, schema: Option<OperatorErrorSchema>) -> ExploredEndpoint {
        ExploredEndpoint {
            address: address.to_string(),
            report: Some(EndpointExplorationReport {
                last_exploration_error_schema: schema,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn endpoint_error_html_and_filter_render_structured_and_clear_reports() {
        let error = endpoint(
            "192.0.2.20",
            Some(OperatorErrorSchema {
                error_code: "NICO-SITEEXPLORER-122".to_string(),
                mitigation: Some("Check the HCL".to_string()),
                text: "BMC vendor missing".to_string(),
            }),
        );
        let clear = endpoint("192.0.2.21", None);
        let error = ExploredEndpointDisplay::from(&error);
        let clear = ExploredEndpointDisplay::from(&clear);

        let errors_only = query_filter_for(HashMap::from([(
            "errors-only".to_string(),
            "true".to_string(),
        )]));
        assert!(errors_only(&error));
        assert!(!errors_only(&clear));

        let rendered = ExploredEndpointsShow {
            last_run: None,
            vendors: Vec::new(),
            endpoints: vec![error, clear],
            filter_name: "All",
            active_vendor_filter: "all".to_string(),
            is_errors_only: false,
            page: PageContext::all(2, "/admin/explored-endpoint"),
            missing_default_credentials: Vec::new(),
        }
        .render()
        .expect("template renders");

        assert!(rendered.contains("NICO-SITEEXPLORER-122"));
        assert!(rendered.contains("192.0.2.20"));
        assert!(rendered.contains("192.0.2.21"));
        assert_eq!(rendered.matches("clearlasterror_action").count(), 1);
    }
}
