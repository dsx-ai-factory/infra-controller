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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use askama::Template;
use axum::extract::{OriginalUri, Path as AxumPath, Query, State as AxumState};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use carbide_api_core::Api;
use carbide_rpc_utils::managed_host_display::to_time;
use carbide_uuid::machine::{
    DpuMachineId, HostMachineId, MachineId, MachineInterfaceId, MachineType, StableHostMachineId,
};
use hyper::http::StatusCode;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine::network::ManagedHostQuarantineState;
use model::machine_boot_interface::canonical_redfish_boot_interface_id;
use model::network_segment::NetworkSegmentType;
use rpc::forge::forge_server::Forge;
use rpc::forge::get_machine_boot_interfaces_response::Reconciliation as BootInterfaceReconciliation;
use rpc::forge::get_machine_boot_interfaces_response::reconciliation::State as BootInterfaceReconciliationState;
use rpc::forge::{self as forgerpc, HealthReportApplyMode, MachineInventorySoftwareComponent};
use serde::Deserialize;

use super::pagination::{self, PageContext, PaginationParams};
use super::state_history::StateHistoryTable;
use super::{Base, filters, health};
use crate::action_status::{self, ActionStatus};

#[derive(Template)]
#[template(path = "machine_show.html")]
struct MachineShow {
    title: &'static str,
    machines: Vec<MachineRowDisplay>,
    page: PageContext,
}

#[derive(PartialEq, Eq)]
struct MachineRowDisplay {
    id: String,
    hostname: String,
    state_display: super::StateDisplay,
    associated_dpu_ids: Vec<String>,
    associated_host_id: String,
    sys_vendor: String,
    product_serial: String,
    ip_address: String,
    mac_address: String,
    is_host: bool,
    num_gpus: usize,
    num_ib_ifs: usize,
    health_probe_alerts: Vec<health_report::HealthProbeAlert>,
    override_mode_counts: String,
    metadata: rpc::forge::Metadata,
    instance_type_id: String,
    instance_type: String,
    num_nvlink_gpus: usize,
}

impl PartialOrd for MachineRowDisplay {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MachineRowDisplay {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Since Machine IDs are unique, we don't have to compare by anything else
        self.id.cmp(&other.id)
    }
}

impl MachineRowDisplay {
    fn new(m: forgerpc::Machine, instance_type: String) -> Self {
        let mut machine_interfaces = m
            .interfaces
            .into_iter()
            .filter(|x| x.primary_interface)
            .collect::<Vec<forgerpc::MachineInterface>>();
        let (hostname, ip_address, mac_address) = if machine_interfaces.is_empty() {
            ("None".to_string(), "None".to_string(), "None".to_string())
        } else {
            let mi = machine_interfaces.remove(0);
            (mi.hostname, mi.address.join(","), mi.mac_address)
        };

        let mut sys_vendor = String::new();
        let mut product_serial = String::new();
        let mut num_gpus = 0;
        let mut num_ib_ifs = 0;
        let mut num_nvlink_gpus = 0;
        if let Some(di) = m.discovery_info.as_ref() {
            if let Some(dmi) = di.dmi_data.as_ref() {
                sys_vendor = dmi.sys_vendor.clone();
                product_serial = dmi.product_serial.clone();
            }
            num_gpus = di.gpus.len();
            num_ib_ifs = di.infiniband_interfaces.len();
        }
        if let Some(nvlink_info) = m.nvlink_info.as_ref() {
            num_nvlink_gpus = nvlink_info.gpus.len();
        }
        let replace_count = m
            .health_sources
            .iter()
            .filter(|o| o.mode() == HealthReportApplyMode::Replace)
            .count();
        let merge_count = m
            .health_sources
            .iter()
            .filter(|o| o.mode() == HealthReportApplyMode::Merge)
            .count();

        let health = m
            .health
            .as_ref()
            .map(|h| {
                health_report::HealthReport::try_from(h.clone())
                    .unwrap_or_else(health_report::HealthReport::malformed_report)
            })
            .unwrap_or_else(health_report::HealthReport::missing_report);

        let time_in_state_above_sla = m
            .state_sla
            .as_ref()
            .map(|sla| sla.time_in_state_above_sla)
            .unwrap_or_default();
        let state_display = super::StateDisplay {
            state: m.state,
            time_in_state_above_sla,
        };

        MachineRowDisplay {
            hostname,
            id: m.id.map(|id| id.to_string()).unwrap_or_default(),
            state_display,
            ip_address,
            mac_address,
            is_host: m.machine_type == forgerpc::MachineType::Host as i32,
            associated_dpu_ids: m
                .associated_dpu_machine_ids
                .into_iter()
                .map(|i| i.to_string())
                .collect(),
            associated_host_id: m
                .associated_host_machine_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            sys_vendor,
            product_serial,
            num_gpus,
            num_ib_ifs,
            health_probe_alerts: health.alerts,
            override_mode_counts: format!(
                "{}",
                if replace_count > 0 {
                    replace_count
                } else {
                    merge_count
                }
            ),
            metadata: m.metadata.unwrap_or_default(),
            instance_type_id: m.instance_type_id.unwrap_or_default(),
            instance_type,
            num_nvlink_gpus,
        }
    }
}

pub(super) async fn show_hosts_html(
    state: AxumState<Arc<Api>>,
    query: Query<PaginationParams>,
    path: OriginalUri,
) -> impl IntoResponse {
    show(state, true, false, query, path).await
}

pub(super) async fn show_hosts_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let machines = match fetch_machines(state, false, true).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(error = %err, "fetch_machines");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading machines").into_response();
        }
    };
    (StatusCode::OK, Json(machines)).into_response()
}

pub(super) async fn show_dpus_html(
    state: AxumState<Arc<Api>>,
    query: Query<PaginationParams>,
    path: OriginalUri,
) -> impl IntoResponse {
    show(state, false, true, query, path).await
}

pub(super) async fn show_dpus_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let mut machines = match fetch_machines(state, true, true).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(error = %err, "fetch_machines");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading machines").into_response();
        }
    };
    machines
        .machines
        .retain(|m| m.machine_type == forgerpc::MachineType::Dpu as i32);
    (StatusCode::OK, Json(machines)).into_response()
}

/// List machines
pub(super) async fn show_all_html(
    state: AxumState<Arc<Api>>,
    query: Query<PaginationParams>,
    path: OriginalUri,
) -> impl IntoResponse {
    show(state, true, true, query, path).await
}

pub(super) async fn show_all_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let machines = match fetch_machines(state, true, true).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(error = %err, "fetch_machines");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading machines").into_response();
        }
    };
    (StatusCode::OK, Json(machines)).into_response()
}

async fn show(
    AxumState(state): AxumState<Arc<Api>>,
    include_hosts: bool,
    include_dpus: bool,
    Query(params): Query<PaginationParams>,
    uri: OriginalUri,
) -> Response {
    let all_machines = match fetch_machines(state.clone(), include_dpus, false).await {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "find_machines");
            return (StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response();
        }
    };

    let should_show_machine =
        |m: &forgerpc::Machine| match forgerpc::MachineType::try_from(m.machine_type) {
            Ok(forgerpc::MachineType::Host) => include_hosts,
            Ok(forgerpc::MachineType::Dpu) => include_dpus,
            _ => false,
        };

    let instance_type_ids: Vec<String> = all_machines
        .machines
        .iter()
        .filter(|m| should_show_machine(m))
        .filter_map(|m| {
            m.instance_type_id
                .as_ref()
                .filter(|id| !id.is_empty())
                .cloned()
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let instance_types = match fetch_instance_type_names(&state, instance_type_ids).await {
        Ok(instance_types) => instance_types,
        Err(e) => return e,
    };

    let all_display: Vec<MachineRowDisplay> = all_machines
        .machines
        .into_iter()
        .filter(should_show_machine)
        .map(|m| {
            let instance_type = m
                .instance_type_id
                .as_ref()
                .and_then(|id| instance_types.get(id.as_str()))
                .cloned()
                .unwrap_or_default();
            MachineRowDisplay::new(m, instance_type)
        })
        .sorted()
        .collect();

    let (info, machines) = pagination::paginate_vec(all_display, &params);

    let tmpl = MachineShow {
        machines,
        title: if include_hosts && include_dpus {
            "Machines"
        } else if include_hosts {
            "Hosts"
        } else {
            "DPUs"
        },
        page: PageContext::new(info, uri.path()),
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

async fn fetch_machine(
    api: &Api,
    machine_id: MachineId,
) -> Result<::rpc::forge::Machine, Response> {
    let request = tonic::Request::new(rpc::forge::MachinesByIdsRequest {
        machine_ids: vec![machine_id],
        include_history: true,
    });

    let machine = match api
        .find_machines_by_ids(request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(m) if m.machines.is_empty() => {
            return Err(super::not_found_response(format!(
                "{machine_id}\n<a href=\"/admin/machine/{machine_id}/state-history\">View State history for Machine</a>"
            )));
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
            return Err(super::not_found_response(machine_id.to_string()));
        }
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "find_machines_by_ids");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response());
        }
    };

    Ok(machine)
}

/// Fetches Instance Type Names for the given Instance Type IDs
async fn fetch_instance_type_names(
    api: &Api,
    instance_type_ids: Vec<String>,
) -> Result<HashMap<String, String>, Response> {
    if instance_type_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let request = tonic::Request::new(rpc::forge::FindInstanceTypesByIdsRequest {
        instance_type_ids,
        tenant_organization_id: None,
        include_allocation_stats: false,
    });

    let instance_types = api
        .find_instance_types_by_ids(request)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "find_instance_types_by_ids");
            (StatusCode::INTERNAL_SERVER_ERROR, Html(err.to_string())).into_response()
        })?
        .into_inner()
        .instance_types;

    let mut result = HashMap::new();
    for instance_type in instance_types {
        if let Some(name) = instance_type.metadata.as_ref().map(|m| m.name.clone()) {
            result.insert(instance_type.id, name);
        }
    }

    Ok(result)
}

pub(super) async fn fetch_machines(
    api: Arc<Api>,
    include_dpus: bool,
    include_history: bool,
) -> Result<forgerpc::MachineList, tonic::Status> {
    let request = tonic::Request::new(forgerpc::MachineSearchConfig {
        include_dpus,
        include_predicted_host: true,
        ..Default::default()
    });

    let machine_ids = api
        .find_machine_ids(request)
        .await?
        .into_inner()
        .machine_ids;

    let mut machines = Vec::new();
    let mut offset = 0;
    while offset != machine_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(machine_ids.len() - offset);
        let next_ids = &machine_ids[offset..offset + page_size];
        let next_machines = api
            .find_machines_by_ids(tonic::Request::new(forgerpc::MachinesByIdsRequest {
                machine_ids: next_ids.to_vec(),
                include_history,
            }))
            .await?
            .into_inner();

        machines.extend(next_machines.machines);
        offset += page_size;
    }

    Ok(forgerpc::MachineList { machines })
}

#[derive(Template)]
#[template(path = "machine_detail.html")]
struct MachineDetail<'a> {
    id: String,
    host_id: String,
    rack_id: String,
    lifecycle_detail: super::LifecycleDetail,
    last_reboot: String,
    machine_type: String,
    is_host: bool,
    network_config: String,
    history: StateHistoryTable,
    bios_version: String,
    board_version: String,
    product_name: String,
    product_serial: String,
    board_serial: String,
    chassis_serial: String,
    sys_vendor: String,
    maintenance_reference_is_link: bool,
    maintenance_reference: String,
    maintenance_start_time: String,
    interfaces: Vec<MachineInterfaceDisplay>,
    ib_interfaces: Vec<MachineIbInterfaceDisplay>,
    inventory: Vec<MachineInventorySoftwareComponent>,
    health_detail: super::HealthDetail,
    bmc_info: Option<rpc::forge::BmcInfo>,
    has_complete_bmc_info: bool,
    discovery_info_json: String,
    metadata_detail: super::MetadataDetail,
    capabilities: Vec<MachineCapability>,
    capabilities_json: String,
    validation_runs: Vec<ValidationRun>,
    hw_sku: String,
    available_skus: Vec<String>,
    quarantine_state: Option<ManagedHostQuarantineState>,
    quarantine_state_is_link: bool,
    instance_type_id: String,
    instance_type: String,
    has_instance_type: bool,
    nvlink_gpus: Vec<MachineNvLinkGpuDisplay>,
    desired_boot_interface: Option<DesiredBootInterfaceDisplay>,
    action_status: Option<ActionStatus<'a>>,
}

/// Template projection of the managed host's boot-interface lookup. Outer
/// `None` means the lookup failed; `reconciliation: None` means the lookup
/// succeeded before Site Explorer initialized a desired target.
struct DesiredBootInterfaceDisplay {
    reconciliation: Option<BootInterfaceReconciliationDisplay>,
    candidates: Vec<BootInterfaceCandidateDisplay>,
    predicted_candidates: Vec<PredictedBootInterfaceDisplay>,
    default_machine_interface_id: Option<String>,
    has_selectable_candidates: bool,
}

/// Template projection of the targeted boot-interface reconciliation status.
struct BootInterfaceReconciliationDisplay {
    reconciliation_state: &'static str,
    desired_mac_address: String,
    desired_redfish_interface_id: String,
    selection_source: String,
    selection_updated_at: String,
    desired_version: String,
    verified_version: String,
    observed_at: String,
    observation_type: &'static str,
    machine_state: String,
    reconciling_version: String,
    failure: Option<String>,
}

/// One owned `machine_interfaces` row that an operator can select by its exact
/// UUID. The annotations come from the server's effective and default picks.
struct BootInterfaceCandidateDisplay {
    machine_interface_id: Option<String>,
    mac_address: String,
    redfish_interface_id: String,
    network_segment_type: String,
    is_selectable: bool,
    is_current: bool,
    is_desired: bool,
    is_default: bool,
}

/// One pre-lease boot candidate. Predictions do not have an owned interface
/// UUID yet, so the managed-host page keeps them informational.
struct PredictedBootInterfaceDisplay {
    mac_address: String,
    redfish_interface_id: String,
    network_segment_type: String,
    is_primary: bool,
    is_default: bool,
}

impl DesiredBootInterfaceDisplay {
    /// Builds the operator projection with the same primary-interface policy
    /// enforced by `SetPrimaryInterface`.
    fn new(
        response: forgerpc::GetMachineBootInterfacesResponse,
        dpu_backed_machine_interface_ids: &HashSet<MachineInterfaceId>,
    ) -> Self {
        let forgerpc::GetMachineBootInterfacesResponse {
            machine_interfaces,
            predicted_interfaces,
            effective_boot_interface_mac,
            effective_boot_interface_id,
            default_boot_interface,
            predicted_boot_interface,
            reconciliation,
            ..
        } = response;

        let effective_boot_interface =
            effective_boot_interface_mac.map(|mac_address| forgerpc::MachineBootInterface {
                mac_address,
                interface_id: effective_boot_interface_id,
            });
        let effective_machine_interface_id = effective_boot_interface
            .as_ref()
            .and_then(|target| unique_managed_interface_id(target, &machine_interfaces));
        let current_machine_interface_id = match machine_interfaces
            .iter()
            .find(|candidate| candidate.primary_interface)
        {
            Some(candidate) => candidate.interface_id,
            None => effective_machine_interface_id,
        };
        let desired_boot_interface = reconciliation
            .as_ref()
            .and_then(|status| status.desired_boot_interface.as_ref());
        let host_has_dpu_backed_admin_interface = machine_interfaces.iter().any(|candidate| {
            candidate.interface_id.is_some_and(|interface_id| {
                dpu_backed_machine_interface_ids.contains(&interface_id)
            }) && candidate
                .network_segment_type
                .as_deref()
                .and_then(|segment_type| segment_type.parse().ok())
                == Some(NetworkSegmentType::Admin)
        });
        let selectable_machine_interface_ids = machine_interfaces
            .iter()
            .filter(|candidate| {
                managed_interface_is_selectable(candidate, host_has_dpu_backed_admin_interface)
            })
            .filter_map(|candidate| candidate.interface_id)
            .collect::<HashSet<_>>();
        let default_machine_interface_id = default_boot_interface
            .as_ref()
            .and_then(|target| unique_managed_interface_id(target, &machine_interfaces))
            .filter(|interface_id| selectable_machine_interface_ids.contains(interface_id));
        let has_selectable_candidates = !selectable_machine_interface_ids.is_empty();

        let mut candidates = machine_interfaces
            .into_iter()
            .map(|candidate| {
                let interface_id = candidate.interface_id;
                BootInterfaceCandidateDisplay {
                    is_current: interface_id.is_some()
                        && interface_id == current_machine_interface_id,
                    is_desired: desired_boot_interface.as_ref().is_some_and(|target| {
                        boot_interface_target_matches(
                            target,
                            &candidate.mac_address,
                            candidate.boot_interface_id.as_deref(),
                        )
                    }),
                    is_default: default_boot_interface.as_ref().is_some_and(|target| {
                        boot_interface_target_matches(
                            target,
                            &candidate.mac_address,
                            candidate.boot_interface_id.as_deref(),
                        )
                    }),
                    machine_interface_id: interface_id
                        .map(|machine_interface_id| machine_interface_id.to_string()),
                    is_selectable: interface_id.is_some_and(|interface_id| {
                        selectable_machine_interface_ids.contains(&interface_id)
                    }),
                    mac_address: candidate.mac_address,
                    redfish_interface_id: candidate
                        .boot_interface_id
                        .unwrap_or_else(|| "-".to_string()),
                    network_segment_type: candidate
                        .network_segment_type
                        .unwrap_or_else(|| "-".to_string()),
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            (&left.mac_address, &left.machine_interface_id)
                .cmp(&(&right.mac_address, &right.machine_interface_id))
        });

        let mut predicted_candidates = predicted_interfaces
            .into_iter()
            .map(|candidate| PredictedBootInterfaceDisplay {
                is_default: predicted_boot_interface.as_ref().is_some_and(|target| {
                    boot_interface_target_matches(
                        target,
                        &candidate.mac_address,
                        candidate.boot_interface_id.as_deref(),
                    )
                }),
                mac_address: candidate.mac_address,
                redfish_interface_id: candidate
                    .boot_interface_id
                    .unwrap_or_else(|| "-".to_string()),
                network_segment_type: candidate
                    .network_segment_type
                    .unwrap_or_else(|| "-".to_string()),
                is_primary: candidate.primary_interface,
            })
            .collect::<Vec<_>>();
        predicted_candidates.sort_by(|left, right| left.mac_address.cmp(&right.mac_address));

        Self {
            reconciliation: reconciliation.map(Into::into),
            candidates,
            predicted_candidates,
            default_machine_interface_id: default_machine_interface_id
                .map(|machine_interface_id| machine_interface_id.to_string()),
            has_selectable_candidates,
        }
    }
}

/// Mirrors the eligibility guard in `SetPrimaryInterface`. Hosts with a
/// DPU-backed Admin link must keep an Admin primary; other hosts may select any
/// owned row.
fn managed_interface_is_selectable(
    candidate: &forgerpc::MachineInterfaceBootInterface,
    host_has_dpu_backed_admin_interface: bool,
) -> bool {
    candidate.interface_id.is_some()
        && (!host_has_dpu_backed_admin_interface
            || candidate
                .network_segment_type
                .as_deref()
                .and_then(|segment_type| segment_type.parse().ok())
                == Some(NetworkSegmentType::Admin))
}

/// Finds the one owned interface row represented by a server-selected target.
/// A MAC-only target that matches several rows is deliberately not selectable.
fn unique_managed_interface_id(
    target: &forgerpc::MachineBootInterface,
    candidates: &[forgerpc::MachineInterfaceBootInterface],
) -> Option<MachineInterfaceId> {
    candidates
        .iter()
        .filter(|candidate| {
            boot_interface_target_matches(
                target,
                &candidate.mac_address,
                candidate.boot_interface_id.as_deref(),
            )
        })
        .exactly_one()
        .ok()?
        .interface_id
}

/// Compares a server-selected target with one candidate. Redfish IDs use the
/// model's boundary-whitespace normalization; a MAC-only target matches by MAC.
fn boot_interface_target_matches(
    target: &forgerpc::MachineBootInterface,
    candidate_mac_address: &str,
    candidate_redfish_interface_id: Option<&str>,
) -> bool {
    let Ok(target_mac_address) = target.mac_address.parse::<MacAddress>() else {
        return false;
    };
    let Ok(candidate_mac_address) = candidate_mac_address.parse::<MacAddress>() else {
        return false;
    };
    if target_mac_address != candidate_mac_address {
        return false;
    }

    match target.interface_id.as_deref() {
        Some(target_redfish_interface_id) => canonical_redfish_boot_interface_id(
            target_redfish_interface_id,
        )
        .is_some_and(|target_redfish_interface_id| {
            candidate_redfish_interface_id.and_then(canonical_redfish_boot_interface_id)
                == Some(target_redfish_interface_id)
        }),
        None => true,
    }
}

impl From<BootInterfaceReconciliation> for BootInterfaceReconciliationDisplay {
    fn from(status: BootInterfaceReconciliation) -> Self {
        let selection_source = status.selection_source().as_str().to_string();
        let desired_boot_interface = status.desired_boot_interface.unwrap_or_default();
        let reconciliation_state =
            match BootInterfaceReconciliationState::try_from(status.reconciliation_state)
                .unwrap_or_default()
            {
                BootInterfaceReconciliationState::Unspecified => "Unspecified",
                BootInterfaceReconciliationState::Pending => "Pending",
                BootInterfaceReconciliationState::Converging => "Converging",
                BootInterfaceReconciliationState::Converged => "Converged",
                BootInterfaceReconciliationState::Failed => "Failed",
            };
        let observation_type = match (&status.verified_version, status.is_compatibility_baseline) {
            (None, _) => "None",
            (Some(_), true) => "Compatibility baseline",
            (Some(_), false) => "Redfish verified",
        };

        Self {
            reconciliation_state,
            desired_mac_address: if desired_boot_interface.mac_address.is_empty() {
                "-".to_string()
            } else {
                desired_boot_interface.mac_address
            },
            desired_redfish_interface_id: desired_boot_interface
                .interface_id
                .unwrap_or_else(|| "-".to_string()),
            selection_source,
            selection_updated_at: to_time(status.selection_updated_at, None::<&str>)
                .unwrap_or_else(|| "-".to_string()),
            desired_version: status.desired_version,
            verified_version: status.verified_version.unwrap_or_else(|| "-".to_string()),
            observed_at: to_time(status.observed_at, None::<&str>)
                .unwrap_or_else(|| "-".to_string()),
            observation_type,
            machine_state: status.machine_state,
            reconciling_version: status
                .reconciling_version
                .unwrap_or_else(|| "-".to_string()),
            failure: status.failure,
        }
    }
}

struct MachineCapability {
    ty: &'static str,
    name: String,
    count: u32,
}

struct MachineInterfaceDisplay {
    index: usize,
    id: String,
    dpu_id: String,
    segment_id: String,
    domain_id: String,
    hostname: String,
    primary: String,
    mac_address: String,
    addresses: String,
}

#[derive(Debug, Default)]
struct MachineIbInterfaceDisplay {
    guid: String,
    device: String,
    vendor: String,
    slot: String,
    lid: String,
    fabric_id: String,
    associated_pkeys: Option<Vec<String>>,
    associated_partitions: Option<Vec<String>>,
    observed_at: String,
}

#[derive(Debug, Default)]
struct MachineNvLinkGpuDisplay {
    domain_uuid: String,
    domain_health_url: String,
    tray_index: i32,
    slot_id: i32,
    device_instance: i32,
    guid: u64,
}

pub(super) struct ValidationRun {
    pub(super) status: String,
    pub(super) context: String,
    pub(super) validation_id: String,
    pub(super) start_time: String,
    pub(super) end_time: String,
    pub(super) machine_id: String,
}

impl From<forgerpc::Machine> for MachineDetail<'_> {
    fn from(m: forgerpc::Machine) -> Self {
        let machine_id = m.id.map(|id| id.to_string()).unwrap_or_default();

        let history = StateHistoryTable {
            records: m.events.into_iter().rev().map(Into::into).collect(),
        };

        let interfaces: Vec<_> = m
            .interfaces
            .into_iter()
            .enumerate()
            .map(|(i, interface)| MachineInterfaceDisplay {
                index: i,
                id: interface.id.unwrap_or_default().to_string(),
                dpu_id: interface
                    .attached_dpu_machine_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(super::invalid_machine_id),
                segment_id: interface.segment_id.unwrap_or_default().to_string(),
                domain_id: interface.domain_id.unwrap_or_default().to_string(),
                hostname: interface.hostname,
                primary: interface.primary_interface.to_string(),
                mac_address: interface.mac_address,
                addresses: interface.address.join(","),
            })
            .collect();

        let mut bios_version = String::new();
        let mut board_version = String::new();
        let mut product_name = String::new();
        let mut product_serial = String::new();
        let mut board_serial = String::new();
        let mut chassis_serial = String::new();
        let mut sys_vendor = String::new();
        let mut ib_interfaces = Vec::new();
        let mut inventory = Vec::new();
        let mut nvlink_gpus = Vec::new();

        let discovery_info_json = m
            .discovery_info
            .as_ref()
            .map(|info| {
                serde_json::to_string_pretty(info)
                    .unwrap_or_else(|e| format!("Formatting error: {e}"))
            })
            .unwrap_or_else(|| "null".to_string());

        if let Some(di) = m.discovery_info {
            if let Some(dmi) = di.dmi_data {
                product_name = dmi.product_name;
                product_serial = dmi.product_serial;
                board_serial = dmi.board_serial;
                chassis_serial = dmi.chassis_serial;
                sys_vendor = dmi.sys_vendor;
                bios_version = dmi.bios_version;
                board_version = dmi.board_version;
            }

            for iface in di.infiniband_interfaces.into_iter() {
                let mut iface_display = MachineIbInterfaceDisplay {
                    guid: iface.guid,
                    ..Default::default()
                };
                if let Some(props) = iface.pci_properties {
                    iface_display.device = props.device;
                    iface_display.vendor = props.vendor;
                    iface_display.slot = props.slot.clone().unwrap_or_default();
                }
                if let Some(ib_status) = m.ib_status.as_ref() {
                    iface_display.observed_at =
                        to_time(ib_status.observed_at, Some(&machine_id)).unwrap_or_default();

                    for iter_status in ib_status.ib_interfaces.iter() {
                        if Some(&iface_display.guid) == iter_status.guid.as_ref() {
                            iface_display.fabric_id =
                                iter_status.fabric_id.clone().unwrap_or_default();
                            iface_display.lid =
                                format!("0x{:x}", iter_status.lid.unwrap_or_default());

                            iface_display.associated_pkeys =
                                iter_status.associated_pkeys.clone().map(|list| list.items);
                            iface_display.associated_partitions = iter_status
                                .associated_partition_ids
                                .clone()
                                .map(|ids| ids.items);
                            break;
                        }
                    }
                }

                ib_interfaces.push(iface_display);
            }
            // Sort the IB interfaces in the same way the Instance allocation API
            // would sort them
            ib_interfaces.sort_by_key(|iface| iface.slot.clone());
        }
        if let Some(inv) = m.inventory {
            inventory.extend(inv.components);
        }

        if let Some(nvlink_info) = m.nvlink_info {
            let domain_id = nvlink_info.domain_uuid.unwrap_or_default();
            let domain_uuid = domain_id.to_string();
            let domain_health_url = health::nvlink_domain_health_url(&domain_id);

            nvlink_gpus = nvlink_info
                .gpus
                .into_iter()
                .map(|gpu| MachineNvLinkGpuDisplay {
                    domain_uuid: domain_uuid.clone(),
                    domain_health_url: domain_health_url.clone(),
                    tray_index: gpu.tray_index,
                    slot_id: gpu.slot_id,
                    guid: gpu.guid,
                    device_instance: gpu.device_id,
                })
                .collect();
        }

        let quarantine_state = m
            .quarantine_state
            .and_then(|q| ManagedHostQuarantineState::try_from(q).ok());
        let is_host = m.machine_type == forgerpc::MachineType::Host as i32;
        let host_id = m
            .associated_host_machine_id
            .map_or_else(String::default, |id| id.to_string());
        let health_reports_link_text = if is_host {
            "Go to Host health reports"
        } else {
            "Go to DPU health reports"
        };
        let health_detail = super::HealthDetail::new(
            format!("/admin/machine/{machine_id}/health"),
            health_reports_link_text,
            m.health,
            m.health_sources,
        );
        let has_complete_bmc_info = m
            .bmc_info
            .as_ref()
            .is_some_and(|bmc_info| bmc_info.ip.is_some() && bmc_info.mac.is_some());

        MachineDetail {
            id: machine_id.clone(),
            rack_id: m.rack_id.map(|id| id.to_string()).unwrap_or_default(),
            lifecycle_detail: super::LifecycleDetail::new(
                m.state,
                m.state_version,
                m.state_reason,
                m.state_sla,
            ),
            last_reboot: to_time(m.last_reboot_time, Some(&machine_id))
                .unwrap_or("N/A".to_string()),
            metadata_detail: super::MetadataDetail {
                metadata: m.metadata.unwrap_or_default(),
                metadata_version: m.version,
            },
            machine_type: get_machine_type(&machine_id),
            is_host,
            network_config: String::new(), // filled in later
            bmc_info: m.bmc_info,
            has_complete_bmc_info,
            history,
            bios_version,
            board_version,
            product_serial,
            chassis_serial,
            board_serial,
            sys_vendor,
            product_name,
            ib_interfaces,
            interfaces,
            inventory,
            maintenance_reference_is_link: m
                .maintenance_reference
                .as_ref()
                .map(|r| r.starts_with("http"))
                .unwrap_or_default(),
            maintenance_reference: m.maintenance_reference.unwrap_or_default(),
            maintenance_start_time: to_time(m.maintenance_start_time, Some(&machine_id))
                .unwrap_or_default(),
            host_id,
            health_detail,
            discovery_info_json,
            capabilities_json: m
                .capabilities
                .as_ref()
                .map(|set| serde_json::to_string_pretty(set).unwrap_or("Invalid JSON".to_string()))
                .unwrap_or_else(|| "{}".to_string()),
            capabilities: m
                .capabilities
                .map(|s| {
                    let mut caps = Vec::new();
                    for item in s.cpu {
                        caps.push(MachineCapability {
                            ty: "cpu",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.gpu {
                        caps.push(MachineCapability {
                            ty: "gpu",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.memory {
                        caps.push(MachineCapability {
                            ty: "memory",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.storage {
                        caps.push(MachineCapability {
                            ty: "storage",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.network {
                        caps.push(MachineCapability {
                            ty: "network",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.infiniband {
                        caps.push(MachineCapability {
                            ty: "infiniband",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    for item in s.dpu {
                        caps.push(MachineCapability {
                            ty: "dpu",
                            name: item.name,
                            count: item.count,
                        })
                    }
                    caps
                })
                .unwrap_or_default(),
            validation_runs: Vec::new(),
            hw_sku: m.hw_sku.unwrap_or_default(),
            available_skus: Vec::new(), // filled in later
            quarantine_state_is_link: quarantine_state
                .as_ref()
                .is_some_and(|r| r.reason_str().starts_with("http")),
            quarantine_state,
            has_instance_type: m.instance_type_id.is_some(),
            instance_type_id: m.instance_type_id.unwrap_or_default(),
            instance_type: "".to_string(),
            nvlink_gpus,
            desired_boot_interface: None,
            action_status: None,
        }
    }
}

/// View machine
pub(super) async fn detail(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (show_json, machine_id) = match machine_id.strip_suffix(".json") {
        Some(machine_id) => (true, machine_id.to_string()),
        None => (false, machine_id),
    };

    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(machine_id) => machine_id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let machine = match fetch_machine(&state, machine_id).await {
        Ok(machine) => machine,
        Err(response) => return response,
    };

    if show_json {
        return (StatusCode::OK, Json(machine)).into_response();
    }

    // DPU attachment lives on the machine response, while segment type lives
    // on the boot-interface response. Preserve the exact row IDs to join those
    // two facts when projecting the API's primary-interface policy.
    let dpu_backed_machine_interface_ids = machine
        .interfaces
        .iter()
        .filter(|interface| interface.attached_dpu_machine_id.is_some())
        .filter_map(|interface| interface.id)
        .collect::<HashSet<_>>();
    let mut display: MachineDetail = machine.into();

    if display.is_host {
        match state
            .find_instance_by_machine_id(tonic::Request::new(machine_id))
            .await
            .map(|response| response.into_inner())
        {
            Ok(instances) => {
                if let Some(instance_id) = instances
                    .instances
                    .first()
                    .and_then(|instance| instance.id.as_ref())
                {
                    display.lifecycle_detail.associated_instance_id = Some(instance_id.to_string());
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, %machine_id, "find_instance_by_machine_id failed");
            }
        }

        match state
            .get_machine_boot_interfaces(tonic::Request::new(
                forgerpc::GetMachineBootInterfacesRequest {
                    machine_id: Some(machine_id),
                },
            ))
            .await
            .map(|response| response.into_inner())
        {
            Ok(boot_interfaces) => {
                display.desired_boot_interface = Some(DesiredBootInterfaceDisplay::new(
                    boot_interfaces,
                    &dpu_backed_machine_interface_ids,
                ));
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    %machine_id,
                    "get_machine_boot_interfaces failed",
                );
            }
        }
    }

    if display.has_instance_type {
        match fetch_instance_type_names(&state, vec![display.instance_type_id.clone()]).await {
            Ok(mut instance_types) => {
                display.instance_type = instance_types
                    .remove(&display.instance_type_id)
                    .unwrap_or_default()
            }
            Err(e) => return e,
        };
    }

    // Populate the list of SKUs available for assignment (hosts only).
    if display.is_host {
        match state
            .get_all_sku_ids(tonic::Request::new(()))
            .await
            .map(|response| response.into_inner())
        {
            Ok(mut sku_ids) => {
                sku_ids.ids.sort_unstable();
                display.available_skus = sku_ids.ids;
            }
            Err(err) => {
                tracing::warn!(error = %err, %machine_id, "get_all_sku_ids failed");
            }
        }
    }

    // Get validation results
    let validation_request = tonic::Request::new(rpc::forge::MachineValidationRunListGetRequest {
        machine_id: Some(machine_id),
        include_history: false,
    });

    let validation_runs = match state
        .get_machine_validation_runs(validation_request)
        .await
        .map(|response| response.into_inner())
    {
        Ok(results) => results
            .runs
            .into_iter()
            .rev() // Show the most recent run first
            .map(|vr| ValidationRun {
                machine_id: vr.machine_id.map(|id| id.to_string()).unwrap_or_default(),
                status:format!("{:?}", vr.status.unwrap_or_default().machine_validation_state.unwrap_or(
                    rpc::forge::machine_validation_status::MachineValidationState::Completed(
                        rpc::forge::machine_validation_status::MachineValidationCompleted::Success.into(),
                    ),
                )),
                context: vr.context.unwrap_or_default(),
                validation_id: vr.validation_id.unwrap_or_default().to_string(),
                start_time: vr.start_time.unwrap_or_default().to_string(),
                end_time: vr.end_time.unwrap_or_default().to_string(),
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, %machine_id, "get_machine_validation_runs failed");
            Vec::new() // Empty validation results on error
        }
    };

    display.validation_runs = validation_runs;
    display.action_status = ActionStatus::from_query(&params);

    if let Ok(dpu_machine_id) = DpuMachineId::try_from(machine_id) {
        let request = tonic::Request::new(forgerpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        });
        if let Ok(netconf) = state
            .get_managed_host_network_config(request)
            .await
            .map(|response| response.into_inner())
        {
            display.network_config = serde_json::to_string_pretty(&netconf)
                .unwrap_or_else(|_| "\"Invalid\"".to_string());
        }
    }
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

fn get_machine_type(machine_id: &str) -> String {
    MachineType::from_id_string(machine_id)
        .map(|t| t.to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[derive(Deserialize, Debug)]
pub(super) struct MaintenanceAction {
    action: String,
    reference: Option<String>,
}

/// Enter / Exit maintenance mode
pub(super) async fn maintenance(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    Form(form): Form<MaintenanceAction>,
) -> Response {
    let view_url = format!("/admin/machine/{machine_id}");

    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(machine_id) => machine_id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let req = if form.action == "enter" {
        forgerpc::MaintenanceRequest {
            operation: forgerpc::MaintenanceOperation::Enable.into(),
            host_id: Some(machine_id),
            reference: form.reference,
        }
    } else if form.action == "exit" {
        forgerpc::MaintenanceRequest {
            operation: forgerpc::MaintenanceOperation::Disable.into(),
            host_id: Some(machine_id),
            reference: None,
        }
    } else {
        tracing::error!("Expected action to be 'enter' or 'exit' but got neither");
        return Redirect::to(&view_url).into_response();
    };

    if machine_id.machine_type().is_dpu() {
        tracing::error!("Maintenance Mode can not be set on DPUs");
        return Redirect::to(&view_url).into_response();
    }

    if let Err(err) = state
        .set_maintenance(tonic::Request::new(req))
        .await
        .map(|response| response.into_inner())
    {
        tracing::error!(error = %err, %machine_id, "set_maintenance");
        return Redirect::to(&view_url).into_response();
    }

    Redirect::to(&view_url).into_response()
}

#[derive(Deserialize, Debug)]
pub(super) struct QuarantineAction {
    action: String,
    mode: Option<String>,
    reason: Option<String>,
}

/// Enter / Exit quarantine
pub(super) async fn quarantine(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    Form(form): Form<QuarantineAction>,
) -> Response {
    let view_url = format!("/admin/machine/{machine_id}");
    let machine_id = match machine_id.parse::<HostMachineId>() {
        Ok(machine_id) => machine_id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let err = match form.action.as_str() {
        "enable" => {
            let mode = form
                .mode
                .as_deref()
                .and_then(forgerpc::ManagedHostQuarantineMode::from_str_name)
                .unwrap_or(forgerpc::ManagedHostQuarantineMode::BlockAllTraffic);
            state
                .set_managed_host_quarantine_state(tonic::Request::new(
                    forgerpc::SetManagedHostQuarantineStateRequest {
                        machine_id: Some(machine_id),
                        quarantine_state: Some(forgerpc::ManagedHostQuarantineState {
                            mode: mode as i32,
                            reason: form.reason,
                        }),
                    },
                ))
                .await
                .map(|_| ())
        }
        "disable" => state
            .clear_managed_host_quarantine_state(tonic::Request::new(machine_id.into()))
            .await
            .map(|_| ()),
        unknown => {
            tracing::error!(action = unknown, "Unexpected quarantine action",);
            return Redirect::to(&view_url).into_response();
        }
    };

    if let Err(error) = err {
        tracing::error!(%error, %machine_id, "quarantine");
    }

    Redirect::to(&view_url).into_response()
}

#[derive(Deserialize, Debug)]
pub(super) struct SkuAction {
    action: String,
    sku_id: Option<String>,
    force: Option<String>,
}

/// Assign / Remove a SKU on a machine
pub(super) async fn sku(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    Form(form): Form<SkuAction>,
) -> Response {
    let view_url = format!("/admin/machine/{machine_id}#sku_view");

    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(machine_id) => machine_id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    // Checkboxes submit "on" when checked and are omitted otherwise.
    let force = form.force.as_deref() == Some("on");

    let result = match form.action.as_str() {
        "assign" => {
            let sku_id = form.sku_id.unwrap_or_default();
            if sku_id.is_empty() {
                let redirect_url = ActionStatus {
                    action: action_status::Type::Sku,
                    class: action_status::Class::Error,
                    message: "No SKU selected".into(),
                }
                .update_redirect_url(&view_url);
                return Redirect::to(&redirect_url).into_response();
            }
            state
                .assign_sku_to_machine(tonic::Request::new(forgerpc::SkuMachinePair {
                    sku_id: sku_id.clone(),
                    machine_id: Some(machine_id),
                    force,
                }))
                .await
                .map(|_| format!("SKU {sku_id} assigned successfully"))
        }
        "remove" => state
            .remove_sku_association(tonic::Request::new(forgerpc::RemoveSkuRequest {
                machine_id: Some(machine_id),
                force,
            }))
            .await
            .map(|_| "SKU association removed successfully".to_string()),
        unknown => {
            tracing::error!(action = unknown, "Unexpected SKU action",);
            return Redirect::to(&view_url).into_response();
        }
    };

    let redirect_url = match result {
        Ok(message) => ActionStatus {
            action: action_status::Type::Sku,
            class: action_status::Class::Success,
            message: message.into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "sku action failed");
            ActionStatus {
                action: action_status::Type::Sku,
                class: action_status::Class::Error,
                message: err.message().to_string().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

/// Form fields for selecting one exact owned interface as the host's desired
/// boot interface.
#[derive(Deserialize, Debug)]
pub(super) struct SetDesiredBootInterfaceAction {
    machine_interface_id: MachineInterfaceId,
}

/// Updates the host's primary and desired boot interface together. The API
/// records the intent and leaves Redfish convergence to machine-controller.
pub(super) async fn set_desired_boot_interface(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
    Form(form): Form<SetDesiredBootInterfaceAction>,
) -> Response {
    let view_url = format!("/admin/machine/{machine_id}#desired_boot_interface");

    let machine_id = match machine_id.parse::<StableHostMachineId>() {
        Ok(machine_id) => machine_id,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    let redirect_url = match state
        .set_primary_interface(tonic::Request::new(forgerpc::SetPrimaryInterfaceRequest {
            host_machine_id: Some(machine_id),
            interface_id: Some(form.machine_interface_id),
            force_reconcile: true,
            ..Default::default()
        }))
        .await
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::DesiredBootInterface,
            class: action_status::Class::Success,
            message: "Desired boot interface updated".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(
                error = %err,
                %machine_id,
                machine_interface_id = %form.machine_interface_id,
                "set_primary_interface failed",
            );
            ActionStatus {
                action: action_status::Type::DesiredBootInterface,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

/// Requests another machine-controller pass for the persisted desired target
/// without replacing it with the primary interface shown by the page.
pub(super) async fn reconcile_boot_interface(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
) -> Response {
    let view_url = format!("/admin/machine/{machine_id}#desired_boot_interface");

    let machine_id = match machine_id.parse::<MachineId>() {
        Ok(machine_id) => machine_id,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    if !machine_id.machine_type().is_host() {
        return (StatusCode::BAD_REQUEST, "machine must be a host").into_response();
    }

    let boot_interfaces = match state
        .get_machine_boot_interfaces(tonic::Request::new(
            forgerpc::GetMachineBootInterfacesRequest {
                machine_id: Some(machine_id),
            },
        ))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "get_machine_boot_interfaces failed");
            let redirect_url = ActionStatus {
                action: action_status::Type::DesiredBootInterface,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url);
            return Redirect::to(&redirect_url).into_response();
        }
    };
    if boot_interfaces.reconciliation.is_none() {
        let redirect_url = ActionStatus {
            action: action_status::Type::DesiredBootInterface,
            class: action_status::Class::Error,
            message: "Desired boot interface has not been initialized".into(),
        }
        .update_redirect_url(&view_url);
        return Redirect::to(&redirect_url).into_response();
    }

    let redirect_url = match state
        .set_dpu_first_boot_order(tonic::Request::new(forgerpc::SetDpuFirstBootOrderRequest {
            machine_id: Some(machine_id.to_string()),
            bmc_endpoint_request: None,
            boot_interface_mac: None,
        }))
        .await
    {
        Ok(_) => ActionStatus {
            action: action_status::Type::DesiredBootInterface,
            class: action_status::Class::Success,
            message: "Boot-interface reconciliation requested".into(),
        }
        .update_redirect_url(&view_url),
        Err(err) => {
            tracing::error!(error = %err, %machine_id, "set_dpu_first_boot_order failed");
            ActionStatus {
                action: action_status::Type::DesiredBootInterface,
                class: action_status::Class::Error,
                message: err.message().into(),
            }
            .update_redirect_url(&view_url)
        }
    };

    Redirect::to(&redirect_url).into_response()
}

impl super::Base for MachineShow {}
impl<'a> super::Base for MachineDetail<'a> {}

#[cfg(test)]
mod boot_interface_display_tests {
    use std::time::{Duration, UNIX_EPOCH};

    use carbide_test_support::value_scenarios;

    use super::*;

    /// Builds one managed candidate for the target-to-row resolution table.
    fn managed_candidate(
        machine_interface_id: &str,
        mac_address: &str,
        redfish_interface_id: Option<&str>,
    ) -> forgerpc::MachineInterfaceBootInterface {
        forgerpc::MachineInterfaceBootInterface {
            mac_address: mac_address.to_string(),
            primary_interface: false,
            boot_interface_id: redfish_interface_id.map(str::to_string),
            network_segment_type: None,
            interface_id: Some(machine_interface_id.parse().unwrap()),
        }
    }

    /// Builds the server-selected target used by the same resolution table.
    fn boot_interface_target(
        mac_address: &str,
        redfish_interface_id: Option<&str>,
    ) -> forgerpc::MachineBootInterface {
        forgerpc::MachineBootInterface {
            mac_address: mac_address.to_string(),
            interface_id: redfish_interface_id.map(str::to_string),
        }
    }

    #[test]
    fn managed_targets_resolve_only_to_one_exact_row() {
        const FIRST_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
        const SECOND_ID: &str = "abcdef01-2345-6789-abcd-ef0123456789";

        value_scenarios!(run = |(target, candidates)| {
            unique_managed_interface_id(&target, &candidates)
        };
            "canonical Redfish id pair" {
                (
                    boot_interface_target("00:11:22:33:44:55", Some("NIC.Slot.1")),
                    vec![
                        managed_candidate(FIRST_ID, "00:11:22:33:44:55", Some(" NIC.Slot.1 ")),
                        managed_candidate(SECOND_ID, "00:11:22:33:44:55", Some("NIC.Slot.2")),
                    ],
                ) => Some(FIRST_ID.parse().unwrap()),
            }

            "unique MAC-only target" {
                (
                    boot_interface_target("00:11:22:33:44:55", None),
                    vec![managed_candidate(FIRST_ID, "00:11:22:33:44:55", None)],
                ) => Some(FIRST_ID.parse().unwrap()),
            }

            "ambiguous MAC-only target" {
                (
                    boot_interface_target("00:11:22:33:44:55", None),
                    vec![
                        managed_candidate(FIRST_ID, "00:11:22:33:44:55", None),
                        managed_candidate(SECOND_ID, "00:11:22:33:44:55", None),
                    ],
                ) => None,
            }

            "unknown target" {
                (
                    boot_interface_target("00:11:22:33:44:66", Some("NIC.Slot.1")),
                    vec![managed_candidate(
                        FIRST_ID,
                        "00:11:22:33:44:55",
                        Some("NIC.Slot.1"),
                    )],
                ) => None,
            }
        );
    }

    #[test]
    fn selection_source_display_uses_typed_enum_names() {
        value_scenarios!(run = |selection_source| {
            BootInterfaceReconciliationDisplay::from(BootInterfaceReconciliation {
                selection_source,
                ..Default::default()
            })
            .selection_source
        };
            "wire sentinel" {
                forgerpc::BootInterfaceSelectionSource::Unspecified as i32 => "Unspecified".to_string(),
            }
            "known selection source" {
                forgerpc::BootInterfaceSelectionSource::RedfishChassisId as i32 => "RedfishChassisId".to_string(),
            }
            "persisted unknown source" {
                forgerpc::BootInterfaceSelectionSource::LegacyUnknown as i32 => "LegacyUnknown".to_string(),
            }
        );
    }

    #[test]
    fn selection_updated_at_formats_present_values_and_marks_absence() {
        value_scenarios!(run = |selection_updated_at| {
            BootInterfaceReconciliationDisplay::from(BootInterfaceReconciliation {
                selection_updated_at,
                ..Default::default()
            })
            .selection_updated_at
        };
            "epoch" {
                Some(UNIX_EPOCH.into()) => "1970-01-01 00:00:00 UTC".to_string(),
            }
            "nonzero timestamp" {
                Some((UNIX_EPOCH + Duration::from_secs(1_700_000_000)).into())
                    => "2023-11-14 22:13:20 UTC".to_string(),
            }
            "absent timestamp" {
                None => "-".to_string(),
            }
        );
    }

    #[test]
    fn first_primary_row_remains_current_when_the_effective_target_is_ambiguous() {
        let mut primary = managed_candidate(
            "12345678-1234-5678-90ab-cdef01234567",
            "00:11:22:33:44:55",
            None,
        );
        primary.primary_interface = true;
        let mut duplicate = managed_candidate(
            "abcdef01-2345-6789-abcd-ef0123456789",
            "00:11:22:33:44:55",
            None,
        );
        duplicate.primary_interface = true;
        let display = DesiredBootInterfaceDisplay::new(
            forgerpc::GetMachineBootInterfacesResponse {
                machine_interfaces: vec![primary, duplicate],
                effective_boot_interface_mac: Some("00:11:22:33:44:55".to_string()),
                default_boot_interface: Some(boot_interface_target("00:11:22:33:44:55", None)),
                ..Default::default()
            },
            &HashSet::new(),
        );

        let current_candidates = display
            .candidates
            .iter()
            .filter(|candidate| candidate.is_current)
            .collect::<Vec<_>>();
        assert_eq!(current_candidates.len(), 1);
        assert_eq!(
            current_candidates[0].machine_interface_id.as_deref(),
            Some("12345678-1234-5678-90ab-cdef01234567"),
        );
        assert!(display.default_machine_interface_id.is_none());
        assert!(
            display
                .candidates
                .iter()
                .all(|candidate| candidate.is_default),
            "matching rows stay annotated even when no default action is safe",
        );
    }

    #[test]
    fn unresolved_effective_target_does_not_mark_idless_rows_current() {
        let mut first = managed_candidate(
            "12345678-1234-5678-90ab-cdef01234567",
            "00:11:22:33:44:55",
            None,
        );
        first.interface_id = None;
        let mut duplicate = managed_candidate(
            "abcdef01-2345-6789-abcd-ef0123456789",
            "00:11:22:33:44:55",
            None,
        );
        duplicate.interface_id = None;

        let display = DesiredBootInterfaceDisplay::new(
            forgerpc::GetMachineBootInterfacesResponse {
                machine_interfaces: vec![first, duplicate],
                effective_boot_interface_mac: Some("00:11:22:33:44:55".to_string()),
                ..Default::default()
            },
            &HashSet::new(),
        );

        assert!(
            display
                .candidates
                .iter()
                .all(|candidate| !candidate.is_current),
        );
    }

    #[test]
    fn selectable_rows_follow_primary_interface_policy() {
        value_scenarios!(run = |(network_segment_type, requires_admin, has_interface_id)| {
            let mut candidate = managed_candidate(
                "12345678-1234-5678-90ab-cdef01234567",
                "00:11:22:33:44:55",
                Some("NIC.Slot.1"),
            );
            candidate.network_segment_type = network_segment_type.map(|value| value.to_string());
            if !has_interface_id {
                candidate.interface_id = None;
            }
            managed_interface_is_selectable(&candidate, requires_admin)
        };
            "DPU-managed Admin row" {
                (Some(NetworkSegmentType::Admin), true, true) => true,
            }
            "DPU-managed HostInband row" {
                (Some(NetworkSegmentType::HostInband), true, true) => false,
            }
            "zero-DPU HostInband row" {
                (Some(NetworkSegmentType::HostInband), false, true) => true,
            }
            "zero-DPU Underlay row follows the RPC" {
                (Some(NetworkSegmentType::Underlay), false, true) => true,
            }
            "row without an exact UUID" {
                (Some(NetworkSegmentType::Admin), false, false) => false,
            }
        );
    }

    #[test]
    fn ineligible_default_is_not_actionable() {
        const ADMIN_ID: &str = "12345678-1234-5678-90ab-cdef01234567";
        let mut admin = managed_candidate(ADMIN_ID, "00:11:22:33:44:55", Some("Admin"));
        admin.network_segment_type = Some(NetworkSegmentType::Admin.to_string());
        let mut host_inband = managed_candidate(
            "abcdef01-2345-6789-abcd-ef0123456789",
            "00:11:22:33:44:66",
            Some("HostInband"),
        );
        host_inband.network_segment_type = Some(NetworkSegmentType::HostInband.to_string());

        let display = DesiredBootInterfaceDisplay::new(
            forgerpc::GetMachineBootInterfacesResponse {
                machine_interfaces: vec![admin, host_inband.clone()],
                default_boot_interface: Some(boot_interface_target(
                    &host_inband.mac_address,
                    host_inband.boot_interface_id.as_deref(),
                )),
                ..Default::default()
            },
            &HashSet::from([ADMIN_ID.parse().unwrap()]),
        );

        assert!(display.default_machine_interface_id.is_none());
    }
}
