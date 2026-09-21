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

use std::str::FromStr;
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_uuid::switch::SwitchId;
use hyper::http::StatusCode;
use rpc::forge::forge_server::Forge;

use super::state_history::StateHistoryTable;
use super::{Base, filters};

#[derive(Template)]
#[template(path = "switch_show.html")]
struct SwitchShow {
    switches: Vec<SwitchRecord>,
}

#[derive(Debug)]
struct SwitchRecord {
    id: String,
    name: String,
    state_display: super::StateDisplay,
    slot_number: String,
    tray_index: String,
}

/// Show all switches
pub(super) async fn show_html(state: AxumState<Arc<Api>>) -> Response {
    let switches = match fetch_switches(&state).await {
        Ok(switches) => switches,
        Err(err) => {
            tracing::error!(error = %err, "fetch_switches");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading switches").into_response();
        }
    };

    let switches = switches
        .switches
        .into_iter()
        .map(|switch| {
            let status = switch.status.as_ref();
            let lifecycle = status.and_then(|status| status.lifecycle.as_ref());
            let state_display = super::StateDisplay::from_lifecycle(lifecycle);

            let config = switch.config.unwrap_or_default();
            SwitchRecord {
                id: switch.id.map(|id| id.to_string()).unwrap_or_default(),
                name: config.name,
                state_display,
                slot_number: switch
                    .placement_in_rack
                    .as_ref()
                    .and_then(|p| p.slot_number)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
                tray_index: switch
                    .placement_in_rack
                    .as_ref()
                    .and_then(|p| p.tray_index)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
            }
        })
        .collect();

    let display = SwitchShow { switches };
    (StatusCode::OK, Html(display.render().unwrap())).into_response()
}

/// Show all switches as JSON
pub(super) async fn show_json(state: AxumState<Arc<Api>>) -> Response {
    let switches = match fetch_switches(&state).await {
        Ok(switches) => switches,
        Err(err) => {
            tracing::error!(error = %err, "fetch_switches");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading switches").into_response();
        }
    };
    (StatusCode::OK, Json(switches)).into_response()
}

async fn fetch_switches(api: &Api) -> Result<rpc::forge::SwitchList, tonic::Status> {
    // Use find_switch_ids (which respects DeletedFilter::Exclude by default)
    // followed by find_switches_by_ids.
    let switch_ids = api
        .find_switch_ids(tonic::Request::new(
            rpc::forge::SwitchSearchFilter::default(),
        ))
        .await?
        .into_inner()
        .ids;

    if switch_ids.is_empty() {
        return Ok(Default::default());
    }

    let mut switches = Vec::new();
    let mut offset = 0;
    while offset < switch_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(switch_ids.len() - offset);
        let next_ids = &switch_ids[offset..offset + page_size];
        let page = api
            .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
                switch_ids: next_ids.to_vec(),
            }))
            .await?
            .into_inner();

        switches.extend(page.switches);
        offset += page_size;
    }

    Ok(rpc::forge::SwitchList { switches })
}

#[derive(Template)]
#[template(path = "switch_detail.html")]
struct SwitchDetail {
    id: String,
    rack_id: String,
    name: String,
    slot_number: String,
    tray_index: String,
    enable_nmxc: bool,
    lifecycle_detail: super::LifecycleDetail,
    power_state: Option<String>,
    health_status: Option<String>,
    bmc_info: Option<rpc::forge::BmcInfo>,
    nvos_ports: Vec<SwitchNvosPortRecord>,
    metadata_detail: super::MetadataDetail,
    health_detail: super::HealthDetail,
    history: StateHistoryTable,
}

#[derive(Debug, Eq, PartialEq)]
struct SwitchNvosPortRecord {
    mac: String,
    service_port: Option<u32>,
    addresses: Vec<SwitchNvosAddressRecord>,
}

#[derive(Debug, Eq, PartialEq)]
struct SwitchNvosAddressRecord {
    address_family: String,
    address: String,
}

impl From<&rpc::forge::SwitchNvosPortInfo> for SwitchNvosPortRecord {
    fn from(info: &rpc::forge::SwitchNvosPortInfo) -> Self {
        Self {
            mac: info.mac.clone().unwrap_or_else(|| "N/A".to_string()),
            service_port: info.service_port,
            addresses: info
                .addresses
                .iter()
                .map(|address| SwitchNvosAddressRecord {
                    address_family: match rpc::forge::AddressFamily::try_from(
                        address.address_family,
                    ) {
                        Ok(rpc::forge::AddressFamily::V4) => "IPv4",
                        Ok(rpc::forge::AddressFamily::V6) => "IPv6",
                        _ => "Unknown",
                    }
                    .to_string(),
                    address: address.address.clone(),
                })
                .collect(),
        }
    }
}

#[allow(deprecated)]
fn switch_nvos_port_records(
    nvos_ports: &[rpc::forge::SwitchNvosPortInfo],
    legacy_nvos_info: Option<&rpc::forge::SwitchNvosInfo>,
) -> Vec<SwitchNvosPortRecord> {
    if !nvos_ports.is_empty() {
        return nvos_ports.iter().map(Into::into).collect();
    }

    legacy_nvos_info
        .map(|info| SwitchNvosPortRecord {
            mac: info.mac.clone().unwrap_or_else(|| "N/A".to_string()),
            service_port: info.port,
            addresses: info
                .ip
                .as_ref()
                .map(|address| SwitchNvosAddressRecord {
                    address_family: address
                        .parse::<std::net::IpAddr>()
                        .map(|address| if address.is_ipv4() { "IPv4" } else { "IPv6" })
                        .unwrap_or("Unknown")
                        .to_string(),
                    address: address.clone(),
                })
                .into_iter()
                .collect(),
        })
        .into_iter()
        .collect()
}

impl SwitchDetail {
    #[allow(deprecated)]
    fn new(switch: rpc::forge::Switch, history: StateHistoryTable) -> Self {
        let id = switch
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let config = switch.config.unwrap_or_default();
        let status = switch.status.as_ref();
        let nvos_ports = switch_nvos_port_records(
            status
                .map(|status| status.nvos_ports.as_slice())
                .unwrap_or_default(),
            switch.nvos_info.as_ref(),
        );
        let lifecycle = status.and_then(|s| s.lifecycle.clone()).unwrap_or_default();
        let power_state = status.and_then(|s| s.power_state.clone());
        let health_status = status.and_then(|s| s.health_status.clone());
        let health_detail = super::HealthDetail::new(
            format!("/admin/switch/{id}/health"),
            "Go to Switch health reports",
            status.and_then(|s| s.health.clone()),
            status.map(|s| s.health_sources.clone()).unwrap_or_default(),
        );
        let metadata_detail = super::MetadataDetail {
            metadata: switch.metadata.unwrap_or_default(),
            metadata_version: switch.version,
        };
        Self {
            id,
            rack_id: switch.rack_id.map(|id| id.to_string()).unwrap_or_default(),
            name: config.name,
            slot_number: switch
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.slot_number)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            tray_index: switch
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.tray_index)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            enable_nmxc: config.enable_nmxc,
            lifecycle_detail: lifecycle.into(),
            power_state,
            health_status,
            bmc_info: switch.bmc_info,
            nvos_ports,
            metadata_detail,
            health_detail,
            history,
        }
    }
}

/// View details about a Switch.
pub(super) async fn detail(
    AxumState(api): AxumState<Arc<Api>>,
    AxumPath(switch_id): AxumPath<String>,
) -> Response {
    let (show_json, switch_id) = match switch_id.strip_suffix(".json") {
        Some(id) => (true, id.to_string()),
        None => (false, switch_id),
    };

    let switch = match fetch_switch(&api, &switch_id).await {
        Ok(Some(switch)) => switch,
        Ok(None) => {
            return super::not_found_response(switch_id);
        }
        Err(response) => return response,
    };

    if show_json {
        return (StatusCode::OK, Json(switch)).into_response();
    }

    let history = match super::state_history::fetch_switch_state_history_records(&api, &switch_id)
        .await
    {
        Ok((_switch_id, records)) => StateHistoryTable {
            records: records.into_iter().map(Into::into).collect(),
        },
        Err((code, err)) => {
            tracing::error!(http_status = %code, error = %err, %switch_id, "fetch_switch_state_history_records");
            StateHistoryTable { records: vec![] }
        }
    };

    let detail = SwitchDetail::new(switch, history);
    (StatusCode::OK, Html(detail.render().unwrap())).into_response()
}

async fn fetch_switch(api: &Api, switch_id: &str) -> Result<Option<rpc::forge::Switch>, Response> {
    let switch_id_parsed = match SwitchId::from_str(switch_id) {
        Ok(id) => id,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid switch ID").into_response()),
    };

    let response = match api
        .find_switches(tonic::Request::new(rpc::forge::SwitchQuery {
            name: None,
            switch_id: Some(switch_id_parsed),
        }))
        .await
    {
        Ok(response) => response.into_inner(),
        Err(err) if err.code() == tonic::Code::NotFound => return Ok(None),
        Err(err) => {
            tracing::error!(error = %err, %switch_id, "fetch_switch");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response());
        }
    };

    Ok(response.switches.into_iter().next())
}

impl super::Base for SwitchShow {}
impl super::Base for SwitchDetail {}

#[cfg(test)]
mod tests {
    use super::switch_nvos_port_records;
    use rpc::forge;

    fn address(address_family: forge::AddressFamily, address: &str) -> forge::IpAddress {
        forge::IpAddress {
            address_family: address_family.into(),
            address: address.to_string(),
        }
    }

    #[test]
    fn converts_every_nvos_port_and_address() {
        let ports = vec![
            forge::SwitchNvosPortInfo {
                mac: Some("44:44:33:33:01:00".to_string()),
                service_port: Some(8443),
                addresses: vec![
                    address(forge::AddressFamily::V4, "10.2.14.52"),
                    address(forge::AddressFamily::V6, "2001:db8::10"),
                ],
            },
            forge::SwitchNvosPortInfo {
                mac: Some("44:44:33:33:01:01".to_string()),
                service_port: None,
                addresses: vec![address(forge::AddressFamily::V4, "10.2.14.53")],
            },
        ];

        let records = switch_nvos_port_records(&ports, None);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].mac, "44:44:33:33:01:00");
        assert_eq!(records[0].service_port, Some(8443));
        assert_eq!(records[0].addresses[0].address_family, "IPv4");
        assert_eq!(records[0].addresses[0].address, "10.2.14.52");
        assert_eq!(records[0].addresses[1].address_family, "IPv6");
        assert_eq!(records[0].addresses[1].address, "2001:db8::10");
        assert_eq!(records[1].mac, "44:44:33:33:01:01");
        assert_eq!(records[1].addresses[0].address, "10.2.14.53");
    }

    #[test]
    #[allow(deprecated)]
    fn falls_back_to_legacy_nvos_info() {
        let legacy = forge::SwitchNvosInfo {
            ip: Some("2001:db8::10".to_string()),
            mac: Some("44:44:33:33:01:00".to_string()),
            port: Some(8443),
        };

        let records = switch_nvos_port_records(&[], Some(&legacy));

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].mac, "44:44:33:33:01:00");
        assert_eq!(records[0].service_port, Some(8443));
        assert_eq!(records[0].addresses[0].address_family, "IPv6");
        assert_eq!(records[0].addresses[0].address, "2001:db8::10");
    }
}
