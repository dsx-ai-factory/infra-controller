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
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::Api;
use carbide_rpc_utils::dhcp::DhcpConfig;
use chrono::{DateTime, Utc};
use hyper::http::StatusCode;
use rpc::forge as forgerpc;
use rpc::forge::forge_server::Forge;

use super::{Base, LifecycleDetail, filters};

#[derive(Template)]
#[template(path = "ipam_dhcp.html")]
struct IpamDhcp {
    entries: Vec<DhcpEntryDisplay>,
    lease_duration_secs: i64,
}

struct DhcpEntryDisplay {
    ip_address: String,
    mac_address: String,
    machine_id: String,
    hostname: String,
    created: String,
    last_dhcp: String,
    last_dhcp_rfc3339: String,
}

impl DhcpEntryDisplay {
    fn from_interface(mi: forgerpc::MachineInterface) -> Vec<Self> {
        let created: DateTime<Utc> = mi
            .created
            .and_then(|t| t.try_into().ok())
            .unwrap_or_default();
        let last_dhcp: Option<DateTime<Utc>> = mi.last_dhcp.and_then(|t| t.try_into().ok());

        let machine_id = mi
            .machine_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default();

        if mi.address.is_empty() {
            return Vec::new();
        }

        mi.address
            .into_iter()
            .map(|addr| DhcpEntryDisplay {
                ip_address: addr,
                mac_address: mi.mac_address.clone(),
                machine_id: machine_id.clone(),
                hostname: mi.hostname.clone(),
                created: created.format("%F %T %Z").to_string(),
                last_dhcp: last_dhcp
                    .map(|d| d.format("%F %T %Z").to_string())
                    .unwrap_or_default(),
                last_dhcp_rfc3339: last_dhcp.map(|d| d.to_rfc3339()).unwrap_or_default(),
            })
            .collect()
    }
}

/// DHCP allocations page
pub(super) async fn dhcp_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let interfaces = match fetch_interfaces(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "find_interfaces for DHCP");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading DHCP allocations",
            )
                .into_response();
        }
    };

    let mut entries: Vec<DhcpEntryDisplay> = interfaces
        .into_iter()
        .flat_map(DhcpEntryDisplay::from_interface)
        .collect();
    entries.sort_by(|a, b| a.ip_address.cmp(&b.ip_address));

    let tmpl = IpamDhcp {
        entries,
        lease_duration_secs: DhcpConfig::default().lease_time_secs as i64,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

pub(super) async fn dhcp_json(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let interfaces = match fetch_interfaces(state).await {
        Ok(n) => n,
        Err(err) => {
            tracing::error!(error = %err, "find_interfaces for DHCP JSON");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading DHCP allocations",
            )
                .into_response();
        }
    };
    (StatusCode::OK, Json(interfaces)).into_response()
}

async fn fetch_interfaces(api: Arc<Api>) -> Result<Vec<forgerpc::MachineInterface>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::InterfaceSearchQuery { id: None, ip: None });
    let mut out = api
        .find_interfaces(request)
        .await
        .map(|response| response.into_inner())?;
    out.interfaces
        .sort_unstable_by(|a, b| a.hostname.cmp(&b.hostname));
    Ok(out.interfaces)
}

#[derive(Template)]
#[template(path = "ipam_dns.html")]
struct IpamDns {
    zones: Vec<DnsZoneDisplay>,
    records: Vec<DnsRecordDisplay>,
}

struct DnsZoneDisplay {
    name: String,
    soa_serial: String,
    record_count: usize,
}

struct DnsRecordDisplay {
    q_name: String,
    q_type: String,
    value: String,
    ttl: i32,
    zone: String,
}

/// DNS records page
pub(super) async fn dns_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    // Fetch domains.
    let domains = match db::dns::domain::find_by(
        &state.database_connection,
        db::ObjectColumnFilter::<db::dns::domain::IdColumn>::All,
    )
    .await
    {
        Ok(d) => d,
        Err(err) => {
            tracing::error!(error = %err, "fetch domains for DNS");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading DNS zones").into_response();
        }
    };

    // Fetch all DNS records.
    let db_records =
        match db::dns::resource_record::get_all_records_all_domains(&state.database_connection)
            .await
        {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(error = %err, "fetch DNS records");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error loading DNS records",
                )
                    .into_response();
            }
        };

    // Build domain ID -> name map, and count records per zone.
    let domain_name_map: HashMap<String, String> = domains
        .iter()
        .map(|d| (d.id.to_string(), d.name.clone()))
        .collect();

    let mut record_counts: HashMap<String, usize> = HashMap::new();
    for r in &db_records {
        *record_counts.entry(r.domain_id.to_string()).or_default() += 1;
    }

    let zones: Vec<DnsZoneDisplay> = domains
        .iter()
        .map(|d| {
            let soa_serial = d
                .soa
                .as_ref()
                .map(|s| s.0.serial.to_string())
                .unwrap_or_else(|| "N/A".to_string());
            DnsZoneDisplay {
                name: d.name.clone(),
                soa_serial,
                record_count: record_counts.get(&d.id.to_string()).copied().unwrap_or(0),
            }
        })
        .collect();

    let records: Vec<DnsRecordDisplay> = db_records
        .into_iter()
        .map(|r| {
            let zone = domain_name_map
                .get(&r.domain_id.to_string())
                .cloned()
                .unwrap_or_default();
            DnsRecordDisplay {
                q_name: r.q_name,
                q_type: r.q_type,
                value: r.record,
                ttl: r.ttl,
                zone,
            }
        })
        .collect();

    let tmpl = IpamDns { zones, records };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

#[derive(Template)]
#[template(path = "ipam_underlay.html")]
struct IpamUnderlay {
    segments: Vec<UnderlaySegmentDisplay>,
    segment_count: usize,
}

struct UnderlaySegmentDisplay {
    id: String,
    name: String,
    segment_type: String,
    prefix: String,
    gateway: String,
    mtu: String,
    state: String,
    ip_count: i64,
}

/// Underlay Networks top-level.
pub(super) async fn underlay_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    let query = r#"
        SELECT ns.id as segment_id, ns.name as segment_name,
               ns.network_segment_type::text as segment_type,
               np.prefix as segment_prefix, np.gateway,
               ns.mtu,
               COALESCE(
                   (SELECT count(*) FROM machine_interface_addresses mia
                    JOIN machine_interfaces mi ON mi.id = mia.interface_id
                    WHERE mi.segment_id = ns.id), 0
               ) as ip_count,
               ns.controller_state
        FROM network_segments ns
        LEFT JOIN network_prefixes np ON np.segment_id = ns.id
        WHERE ns.network_segment_type IN ('underlay', 'host_inband')
          AND ns.deleted IS NULL
        ORDER BY ns.network_segment_type, ns.name
    "#;

    let segments: Vec<UnderlaySegmentDisplay> = match sqlx::query_as::<_, UnderlaySegmentRow>(query)
        .fetch_all(&state.database_connection)
        .await
    {
        Ok(rows) => rows.into_iter().map(Into::into).collect(),
        Err(err) => {
            tracing::error!(error = %err, "fetch underlay segments");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading underlay segments",
            )
                .into_response();
        }
    };

    // The overview has one row per prefix, but the summary counts segments.
    let segment_count = segments
        .iter()
        .map(|segment| &segment.id)
        .collect::<HashSet<_>>()
        .len();
    let tmpl = IpamUnderlay {
        segments,
        segment_count,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

#[derive(sqlx::FromRow)]
struct UnderlaySegmentRow {
    segment_id: carbide_uuid::network::NetworkSegmentId,
    segment_name: String,
    segment_type: String,
    segment_prefix: Option<ipnetwork::IpNetwork>,
    gateway: Option<std::net::IpAddr>,
    mtu: Option<i32>,
    ip_count: Option<i64>,
    controller_state: Option<sqlx::types::Json<serde_json::Value>>,
}

impl From<UnderlaySegmentRow> for UnderlaySegmentDisplay {
    fn from(row: UnderlaySegmentRow) -> Self {
        let state = row
            .controller_state
            .and_then(|cs| cs.get("state").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_else(|| "N/A".to_string());
        Self {
            id: row.segment_id.to_string(),
            name: row.segment_name,
            segment_type: row.segment_type,
            prefix: row
                .segment_prefix
                .map(|p| p.to_string())
                .unwrap_or_default(),
            gateway: row
                .gateway
                .map(|g| g.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            state,
            mtu: row.mtu.map(|m| m.to_string()).unwrap_or_default(),
            ip_count: row.ip_count.unwrap_or(0),
        }
    }
}

#[derive(Template)]
#[template(path = "ipam_underlay_segment.html")]
struct IpamUnderlaySegment {
    segment_id: String,
    segment_name: String,
    segment_type: String,
    segment_prefixes: Vec<String>,
    addresses: Vec<UnderlayAddressDisplay>,
}

struct UnderlayAddressDisplay {
    address: String,
    mac_address: String,
    machine_id: String,
    hostname: String,
}

/// Underlay segment detail, which includes machine IPs
/// allocated in a segment.
pub(super) async fn underlay_segment_html(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(segment_id): AxumPath<String>,
) -> Response {
    let segment_uuid: carbide_uuid::network::NetworkSegmentId = match segment_id.parse() {
        Ok(id) => id,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid segment ID: {e}")).into_response();
        }
    };

    // Fetch the segment for metadata.
    let segment = match state
        .find_network_segments_by_ids(tonic::Request::new(forgerpc::NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_uuid],
            include_history: false,
            include_num_free_ips: false,
        }))
        .await
        .map(|r| r.into_inner().network_segments)
    {
        Ok(mut s) if s.len() == 1 => s.remove(0),
        Ok(_) => return super::not_found_response(segment_id),
        Err(err) => {
            tracing::error!(error = %err, "find_network_segments_by_ids for underlay");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading segment").into_response();
        }
    };

    let segment_name = segment
        .metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_default();

    let Some(config) = segment.config else {
        tracing::error!("underlay segment missing config");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Segment data incomplete").into_response();
    };
    let segment_prefixes = config.prefixes.into_iter().map(|p| p.prefix).collect();
    let segment_type = format!(
        "{:?}",
        forgerpc::NetworkSegmentType::try_from(config.segment_type).unwrap_or_default()
    );

    // Fetch machine interface addresses in this segment.
    let addr_query = r#"
        SELECT mia.address, mi.mac_address, mi.machine_id, mi.hostname
        FROM machine_interface_addresses mia
        JOIN machine_interfaces mi ON mi.id = mia.interface_id
        WHERE mi.segment_id = $1::uuid
        ORDER BY mia.address
    "#;

    let addresses: Vec<UnderlayAddressDisplay> =
        match sqlx::query_as::<_, UnderlayAddressRow>(addr_query)
            .bind(segment_uuid)
            .fetch_all(&state.database_connection)
            .await
        {
            Ok(rows) => rows.into_iter().map(Into::into).collect(),
            Err(err) => {
                tracing::error!(error = %err, "fetch underlay segment addresses");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error loading segment addresses",
                )
                    .into_response();
            }
        };

    let tmpl = IpamUnderlaySegment {
        segment_id,
        segment_name,
        segment_type,
        segment_prefixes,
        addresses,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

#[derive(sqlx::FromRow)]
struct UnderlayAddressRow {
    address: std::net::IpAddr,
    mac_address: mac_address::MacAddress,
    machine_id: Option<String>,
    hostname: String,
}

impl From<UnderlayAddressRow> for UnderlayAddressDisplay {
    fn from(row: UnderlayAddressRow) -> Self {
        Self {
            address: row.address.to_string(),
            mac_address: row.mac_address.to_string(),
            machine_id: row.machine_id.unwrap_or_default(),
            hostname: row.hostname,
        }
    }
}

#[derive(Template)]
#[template(path = "ipam_overlay.html")]
struct IpamOverlay {
    vpcs: Vec<OverlayVpcDisplay>,
}

struct OverlayVpcDisplay {
    id: String,
    name: String,
    vni: String,
    tenant: String,
    prefixes: Vec<OverlayVpcPrefixDisplay>,
}

struct OverlayVpcPrefixDisplay {
    id: String,
    prefix: String,
    name: String,
    lifecycle_state: String,
}

/// Overlay Networks -- lists VNIs and their prefixes.
pub(super) async fn overlay_html(AxumState(state): AxumState<Arc<Api>>) -> Response {
    // Fetch all VPCs.
    let rpc_vpcs = match fetch_vpcs(state.clone()).await {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(error = %err, "fetch_vpcs for overlay");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading VPCs").into_response();
        }
    };

    // Fetch all VPC prefix IDs, and then the prefixes themselves.
    let prefix_request = tonic::Request::new(forgerpc::VpcPrefixSearchQuery {
        deleted: forgerpc::DeletedFilter::Include as i32,
        ..Default::default()
    });
    let prefix_ids = match state
        .search_vpc_prefixes(prefix_request)
        .await
        .map(|r| r.into_inner().vpc_prefix_ids)
    {
        Ok(ids) => ids,
        Err(err) => {
            tracing::error!(error = %err, "search_vpc_prefixes for overlay");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading VPC prefixes",
            )
                .into_response();
        }
    };

    let rpc_prefixes = if prefix_ids.is_empty() {
        Vec::new()
    } else {
        match state
            .get_vpc_prefixes(tonic::Request::new(forgerpc::VpcPrefixGetRequest {
                vpc_prefix_ids: prefix_ids,
                deleted: forgerpc::DeletedFilter::Include as i32,
            }))
            .await
            .map(|r| r.into_inner().vpc_prefixes)
        {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(error = %err, "get_vpc_prefixes for overlay");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error loading VPC prefixes",
                )
                    .into_response();
            }
        }
    };

    // Group prefixes by VPC ID.
    let mut prefixes_by_vpc: HashMap<String, Vec<OverlayVpcPrefixDisplay>> = HashMap::new();
    for p in rpc_prefixes {
        let vpc_id = p.vpc_id.map(|id| id.to_string()).unwrap_or_default();
        let prefix_str = p
            .config
            .as_ref()
            .map(|c| c.prefix.clone())
            .or_else(|| {
                // Fall back to "deprecated" field.
                if p.prefix.is_empty() {
                    None
                } else {
                    Some(p.prefix.clone())
                }
            })
            .unwrap_or_default();
        let name = p
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        prefixes_by_vpc
            .entry(vpc_id)
            .or_default()
            .push(OverlayVpcPrefixDisplay {
                id: p.id.map(|id| id.to_string()).unwrap_or_default(),
                prefix: prefix_str,
                name,
                lifecycle_state: lifecycle_state_label(&p),
            });
    }

    let vpcs: Vec<OverlayVpcDisplay> = rpc_vpcs
        .into_iter()
        .map(|vpc| {
            let id = vpc.id.map(|id| id.to_string()).unwrap_or_default();
            let prefixes = prefixes_by_vpc.remove(&id).unwrap_or_default();
            let tenant = vpc
                .config
                .as_ref()
                .map(|config| config.tenant_organization_id.clone())
                .unwrap_or_default();
            OverlayVpcDisplay {
                id,
                name: vpc
                    .metadata
                    .as_ref()
                    .map(|m| m.name.clone())
                    .unwrap_or_default(),
                vni: vpc
                    .status
                    .as_ref()
                    .and_then(|status| status.vni)
                    .map(|vni| vni.to_string())
                    .unwrap_or_default(),
                tenant,
                prefixes,
            }
        })
        .collect();

    let tmpl = IpamOverlay { vpcs };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

fn lifecycle_state_label(prefix: &forgerpc::VpcPrefix) -> String {
    let Some(lifecycle) = prefix
        .status
        .as_ref()
        .and_then(|status| status.lifecycle.as_ref())
    else {
        return "Unavailable".to_string();
    };

    serde_json::from_str::<serde_json::Value>(&lifecycle.state)
        .ok()
        .and_then(|state| {
            state
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| lifecycle.state.clone())
}

async fn fetch_vpcs(api: Arc<Api>) -> Result<Vec<forgerpc::Vpc>, tonic::Status> {
    let request = tonic::Request::new(forgerpc::VpcSearchFilter::default());
    let vpc_ids = api.find_vpc_ids(request).await?.into_inner().vpc_ids;

    let mut vpcs = Vec::new();
    let mut offset = 0;
    while offset != vpc_ids.len() {
        const PAGE_SIZE: usize = 100;
        let page_size = PAGE_SIZE.min(vpc_ids.len() - offset);
        let next_ids = &vpc_ids[offset..offset + page_size];
        let next = api
            .find_vpcs_by_ids(tonic::Request::new(forgerpc::VpcsByIdsRequest {
                vpc_ids: next_ids.to_vec(),
            }))
            .await?
            .into_inner();
        vpcs.extend(next.vpcs);
        offset += page_size;
    }

    vpcs.sort_unstable_by(|a, b| {
        let vpc1_name = a
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or("<no name>");
        let vpc2_name = b
            .metadata
            .as_ref()
            .map(|x| x.name.as_str())
            .unwrap_or("<no name>");
        vpc1_name.cmp(vpc2_name)
    });
    Ok(vpcs)
}

#[derive(Template)]
#[template(path = "ipam_overlay_prefix.html")]
struct IpamOverlayPrefix {
    vpc_prefix: String,
    vpc_prefix_name: String,
    vpc_name: String,
    vpc_id: String,
    vni: String,
    lifecycle_detail: Option<LifecycleDetail>,
    state_history: Vec<forgerpc::StateHistoryRecord>,
    state_history_load_error: Option<String>,
    segments: Vec<OverlaySegmentDisplay>,
}

struct OverlaySegmentDisplay {
    id: String,
    name: String,
    segment_type: String,
    prefix: String,
    gateway: String,
    state: String,
    mtu: String,
    ip_count: i64,
}

/// Overlay prefix detail, which includes segments carved
/// from a VPC prefix.
pub(super) async fn overlay_prefix_html(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(vpc_prefix_id): AxumPath<String>,
) -> Response {
    let vpc_prefix_uuid = match vpc_prefix_id.parse() {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid VPC prefix ID: {e}"),
            )
                .into_response();
        }
    };

    // Fetch the VPC prefix.
    let prefix = match state
        .get_vpc_prefixes(tonic::Request::new(forgerpc::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![vpc_prefix_uuid],
            deleted: forgerpc::DeletedFilter::Include as i32,
        }))
        .await
        .map(|r| r.into_inner().vpc_prefixes)
    {
        Ok(mut p) if p.len() == 1 => p.remove(0),
        Ok(_) => return super::not_found_response(vpc_prefix_id),
        Err(err) => {
            tracing::error!(error = %err, "get_vpc_prefixes");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error loading VPC prefix",
            )
                .into_response();
        }
    };

    let prefix_str = prefix
        .config
        .as_ref()
        .map(|c| c.prefix.clone())
        .unwrap_or_else(|| prefix.prefix.clone());
    let prefix_name = prefix
        .metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "<no name>".to_string());
    let vpc_id = prefix.vpc_id.map(|id| id.to_string()).unwrap_or_default();
    let lifecycle_detail = prefix
        .status
        .as_ref()
        .and_then(|status| status.lifecycle.clone())
        .map(Into::into);

    // Fetch the controller state history for this prefix.
    let (mut state_history, state_history_load_error) = match state
        .find_vpc_prefix_state_histories(tonic::Request::new(
            forgerpc::VpcPrefixStateHistoriesRequest {
                vpc_prefix_ids: vec![vpc_prefix_uuid],
            },
        ))
        .await
        .map(|r| r.into_inner().histories)
    {
        Ok(mut histories) => (
            histories
                .remove(&vpc_prefix_uuid.to_string())
                .map(|history| history.records)
                .unwrap_or_default(),
            None,
        ),
        Err(err) => {
            tracing::error!(error = %err, "find_vpc_prefix_state_histories");
            (Vec::new(), Some(err.to_string()))
        }
    };
    state_history.reverse();

    // Fetch the parent VPC for name/VNI.
    let (vpc_name, vni) = if let Ok(vpc_uuid) = vpc_id.parse() {
        match state
            .find_vpcs_by_ids(tonic::Request::new(forgerpc::VpcsByIdsRequest {
                vpc_ids: vec![vpc_uuid],
            }))
            .await
            .map(|r| r.into_inner().vpcs)
        {
            Ok(mut v) if !v.is_empty() => {
                let vpc = v.remove(0);
                (
                    vpc.metadata
                        .as_ref()
                        .map(|m| m.name.clone())
                        .unwrap_or_default(),
                    vpc.status
                        .as_ref()
                        .and_then(|status| status.vni)
                        .map(|vni| vni.to_string())
                        .unwrap_or_default(),
                )
            }
            _ => (String::new(), String::new()),
        }
    } else {
        (String::new(), String::new())
    };

    // Query segments linked to this VPC prefix via network_prefixes table.
    let query = r#"
        SELECT ns.id as segment_id, ns.name as segment_name,
               ns.network_segment_type::text as segment_type,
               np.prefix as segment_prefix, np.gateway,
               ns.mtu,
               COALESCE(
                   CASE WHEN ns.network_segment_type = 'tenant'
                        THEN (SELECT count(*) FROM instance_addresses ia WHERE ia.segment_id = ns.id)
                        ELSE (SELECT count(*) FROM machine_interface_addresses mia
                              JOIN machine_interfaces mi ON mi.id = mia.interface_id
                              WHERE mi.segment_id = ns.id)
                   END, 0
               ) as ip_count,
               ns.controller_state
        FROM network_segments ns
        JOIN network_prefixes np ON np.segment_id = ns.id
        WHERE np.vpc_prefix_id = $1::uuid AND ns.deleted IS NULL
        ORDER BY np.prefix
    "#;

    let segments: Vec<OverlaySegmentDisplay> = match sqlx::query_as::<_, OverlaySegmentRow>(query)
        .bind(vpc_prefix_uuid)
        .fetch_all(&state.database_connection)
        .await
    {
        Ok(rows) => rows.into_iter().map(Into::into).collect(),
        Err(err) => {
            tracing::error!(error = %err, "fetch segments for VPC prefix");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading segments").into_response();
        }
    };

    let tmpl = IpamOverlayPrefix {
        vpc_prefix: prefix_str,
        vpc_prefix_name: prefix_name,
        vpc_name,
        vpc_id,
        vni,
        lifecycle_detail,
        state_history,
        state_history_load_error,
        segments,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

#[derive(sqlx::FromRow)]
struct OverlaySegmentRow {
    segment_id: carbide_uuid::network::NetworkSegmentId,
    segment_name: String,
    segment_type: String,
    segment_prefix: ipnetwork::IpNetwork,
    gateway: Option<std::net::IpAddr>,
    mtu: Option<i32>,
    ip_count: Option<i64>,
    controller_state: Option<sqlx::types::Json<serde_json::Value>>,
}

impl From<OverlaySegmentRow> for OverlaySegmentDisplay {
    fn from(row: OverlaySegmentRow) -> Self {
        let state = row
            .controller_state
            .and_then(|cs| cs.get("state").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_else(|| "Unknown".to_string());
        Self {
            id: row.segment_id.to_string(),
            name: row.segment_name,
            segment_type: row.segment_type,
            prefix: row.segment_prefix.to_string(),
            gateway: row
                .gateway
                .map(|g| g.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
            state,
            mtu: row.mtu.map(|m| m.to_string()).unwrap_or_default(),
            ip_count: row.ip_count.unwrap_or(0),
        }
    }
}

#[derive(Template)]
#[template(path = "ipam_overlay_segment.html")]
struct IpamOverlaySegment {
    segment_id: String,
    segment_name: String,
    segment_prefixes: Vec<String>,
    vpc_name: String,
    addresses: Vec<OverlayAddressDisplay>,
}

struct OverlayAddressDisplay {
    address: String,
    instance_id: String,
}

/// Overlay segment detail (IPs allocated in a segment).
pub(super) async fn overlay_segment_html(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(segment_id): AxumPath<String>,
) -> Response {
    let segment_uuid: carbide_uuid::network::NetworkSegmentId = match segment_id.parse() {
        Ok(id) => id,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid segment ID: {e}")).into_response();
        }
    };

    // Fetch the segment for metadata.
    let segment = match state
        .find_network_segments_by_ids(tonic::Request::new(forgerpc::NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_uuid],
            include_history: false,
            include_num_free_ips: false,
        }))
        .await
        .map(|r| r.into_inner().network_segments)
    {
        Ok(mut s) if s.len() == 1 => s.remove(0),
        Ok(_) => return super::not_found_response(segment_id),
        Err(err) => {
            tracing::error!(error = %err, "find_network_segments_by_ids");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Error loading segment").into_response();
        }
    };

    let segment_name = segment
        .metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_default();

    let Some(config) = segment.config else {
        tracing::error!("overlay segment missing config");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Segment data incomplete").into_response();
    };
    let segment_prefixes = config.prefixes.into_iter().map(|p| p.prefix).collect();

    // Fetch VPC name if available.
    let vpc_name = if let Some(vpc_id) = config.vpc_id {
        match state
            .find_vpcs_by_ids(tonic::Request::new(forgerpc::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
            .await
            .map(|r| r.into_inner().vpcs)
        {
            Ok(mut v) if !v.is_empty() => v.remove(0).metadata.map(|m| m.name).unwrap_or_default(),
            _ => String::new(),
        }
    } else {
        String::new()
    };

    // Fetch instance addresses allocated in this segment.
    let instance_addresses =
        match db::instance_address::find_by_segment_id(&state.database_connection, &segment_uuid)
            .await
        {
            Ok(addrs) => addrs,
            Err(err) => {
                tracing::error!(error = %err, "find_by_segment_id");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error loading segment addresses",
                )
                    .into_response();
            }
        };

    let addresses: Vec<OverlayAddressDisplay> = instance_addresses
        .into_iter()
        .map(|ia| OverlayAddressDisplay {
            address: ia.address.to_string(),
            instance_id: ia.instance_id.to_string(),
        })
        .collect();

    let tmpl = IpamOverlaySegment {
        segment_id,
        segment_name,
        segment_prefixes,
        vpc_name,
        addresses,
    };
    (StatusCode::OK, Html(tmpl.render().unwrap())).into_response()
}

impl super::Base for IpamDhcp {}
impl super::Base for IpamDns {}
impl super::Base for IpamUnderlay {}
impl super::Base for IpamUnderlaySegment {}
impl super::Base for IpamOverlay {}
impl super::Base for IpamOverlayPrefix {}
impl super::Base for IpamOverlaySegment {}
