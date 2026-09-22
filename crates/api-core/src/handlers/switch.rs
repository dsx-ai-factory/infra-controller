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
use std::net::IpAddr;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge::{self as rpc, HealthReportEntry};
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::switch::SwitchId;
use db::db_read::DbReader;
use db::{ObjectColumnFilter, switch as db_switch};
use health_report::HealthReportApplyMode;
use mac_address::MacAddress;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::metadata::Metadata;
use model::switch::{Switch, SwitchControllerState};
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::auth::AuthContext;

/// BMC and declared NVOS MACs associated with a switch, used for optional
/// force-delete cleanup of interfaces and suppressions.
async fn associated_switch_macs(
    txn: &mut PgConnection,
    switch: &Switch,
) -> Result<Vec<MacAddress>, CarbideError> {
    let Some(bmc_mac) = switch.bmc_mac_address else {
        return Ok(Vec::new());
    };

    let mut macs = vec![bmc_mac];
    if let Some(expected) = db::expected_switch::find_by_bmc_mac_address(txn, bmc_mac).await? {
        macs.extend(expected.nvos_mac_addresses);
    }
    Ok(macs)
}

fn switch_nvos_address(ip: IpAddr) -> rpc::IpAddress {
    let address_family = match ip {
        IpAddr::V4(_) => rpc::AddressFamily::V4,
        IpAddr::V6(_) => rpc::AddressFamily::V6,
    };

    rpc::IpAddress {
        address_family: address_family.into(),
        address: ip.to_string(),
    }
}

/// Groups NVOS endpoint rows into one port entry per MAC address.
///
/// This relies on [`db_switch::find_switch_nvos_endpoints_by_ids`] ordering rows
/// by switch ID and MAC address so that rows for each MAC are contiguous.
/// Noncontiguous rows for one MAC would produce duplicate port entries.
async fn load_switch_nvos_info(
    db: impl DbReader<'_>,
    switch_ids: &[SwitchId],
) -> Result<HashMap<SwitchId, Vec<rpc::SwitchNvosPortInfo>>, CarbideError> {
    let rows = db_switch::find_switch_nvos_endpoints_by_ids(db, switch_ids).await?;
    let mut nvos_info_by_switch: HashMap<SwitchId, Vec<rpc::SwitchNvosPortInfo>> = HashMap::new();

    for row in rows {
        let Some(mac) = row.nvos_mac.map(|mac| mac.to_string()) else {
            continue;
        };

        let nvos_ports = nvos_info_by_switch.entry(row.switch_id).or_default();
        if nvos_ports.last().and_then(|port| port.mac.as_ref()) != Some(&mac) {
            nvos_ports.push(rpc::SwitchNvosPortInfo {
                mac: Some(mac),
                service_port: None,
                addresses: Vec::new(),
            });
        }

        if let Some(ip) = row.nvos_ip {
            nvos_ports
                .last_mut()
                .expect("an NVOS port was added for the current MAC")
                .addresses
                .push(switch_nvos_address(ip));
        }
    }

    for nvos_ports in nvos_info_by_switch.values_mut() {
        for nvos_port in nvos_ports {
            nvos_port
                .addresses
                .sort_by_key(|address| address.address_family);
        }
    }

    Ok(nvos_info_by_switch)
}

fn legacy_switch_nvos_info(nvos_ports: &[rpc::SwitchNvosPortInfo]) -> Option<rpc::SwitchNvosInfo> {
    let nvos_port = nvos_ports
        .iter()
        .find(|port| {
            port.addresses
                .iter()
                .any(|address| address.address_family == i32::from(rpc::AddressFamily::V4))
        })
        .or_else(|| nvos_ports.iter().find(|port| !port.addresses.is_empty()))
        .or_else(|| nvos_ports.first())?;
    let address = nvos_port
        .addresses
        .iter()
        .find(|address| address.address_family == i32::from(rpc::AddressFamily::V4))
        .or_else(|| nvos_port.addresses.first());

    Some(rpc::SwitchNvosInfo {
        ip: address.map(|address| address.address.clone()),
        mac: nvos_port.mac.clone(),
        port: nvos_port.service_port,
    })
}

#[allow(deprecated)]
fn populate_switch_nvos_info(
    rpc_switch: &mut rpc::Switch,
    nvos_ports: Vec<rpc::SwitchNvosPortInfo>,
) {
    rpc_switch.nvos_info = legacy_switch_nvos_info(&nvos_ports);
    if let Some(status) = rpc_switch.status.as_mut() {
        status.nvos_ports = nvos_ports;
    }
}

pub(crate) async fn find_switch(
    api: &Api,
    request: Request<rpc::SwitchQuery>,
) -> Result<Response<rpc::SwitchList>, Status> {
    let query = request.into_inner();
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    // Handle ID search (takes precedence)
    let switch_list = if let Some(id) = query.switch_id {
        db_switch::find_by(
            &mut txn,
            db::ObjectColumnFilter::One(db_switch::IdColumn, &id),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to find switch: {}", e),
        })?
    } else if let Some(name) = query.name {
        // Handle name search
        db_switch::find_by(
            &mut txn,
            db::ObjectColumnFilter::One(db_switch::NameColumn, &name),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to find switch: {}", e),
        })?
    } else {
        // No filter - return all
        db_switch::find_by(&mut txn, db::ObjectColumnFilter::<db_switch::IdColumn>::All)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("Failed to find switch: {}", e),
            })?
    };

    let switch_ids: Vec<_> = switch_list.iter().map(|switch| switch.id).collect();
    let mut nvos_info_by_switch = if switch_ids.is_empty() {
        HashMap::new()
    } else {
        load_switch_nvos_info(&mut *txn, &switch_ids)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("Failed to get switch NVOS endpoint info: {}", e),
            })?
    };

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let switches: Vec<rpc::Switch> = switch_list
        .into_iter()
        .map(|s| {
            let id = s.id;
            let nvos_ports = nvos_info_by_switch.remove(&id).unwrap_or_default();

            rpc::Switch::try_from(s).map(|mut rpc_switch| {
                populate_switch_nvos_info(&mut rpc_switch, nvos_ports);
                rpc_switch
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to convert switch: {}", e),
        })?;

    Ok(Response::new(rpc::SwitchList { switches }))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::SwitchSearchFilter>,
) -> Result<Response<rpc::SwitchIdList>, Status> {
    log_request_data(&request);

    let filter: model::switch::SwitchSearchFilter = request.into_inner().into();

    let switch_ids = db_switch::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::SwitchIdList { ids: switch_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::SwitchesByIdsRequest>,
) -> Result<Response<rpc::SwitchList>, Status> {
    log_request_data(&request);

    let switch_ids = request.into_inner().switch_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if switch_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if switch_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let switch_list = db_switch::find_by(
        &mut txn,
        ObjectColumnFilter::List(db_switch::IdColumn, &switch_ids),
    )
    .await?;

    let mut nvos_info_by_switch = load_switch_nvos_info(txn.as_pgconn(), &switch_ids)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to get switch NVOS endpoint info: {}", e),
        })?;

    txn.rollback_or_log("read-only load of switches by id")
        .await;

    let switches: Vec<rpc::Switch> = switch_list
        .into_iter()
        .map(|s| {
            let id = s.id;
            let nvos_ports = nvos_info_by_switch.remove(&id).unwrap_or_default();

            rpc::Switch::try_from(s).map(|mut rpc_switch| {
                populate_switch_nvos_info(&mut rpc_switch, nvos_ports);
                rpc_switch
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to convert switch: {}", e),
        })?;

    Ok(Response::new(rpc::SwitchList { switches }))
}

pub(crate) async fn find_switch_state_histories(
    api: &Api,
    request: Request<rpc::SwitchStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let switch_ids = request.switch_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if switch_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if switch_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::Switch,
        &switch_ids,
    )
    .await
    .map_err(CarbideError::from)?;

    let mut response = rpc::StateHistories::default();
    for (switch_id, records) in results {
        response.histories.insert(
            switch_id,
            ::rpc::forge::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;

    Ok(tonic::Response::new(response))
}

pub(crate) async fn decommission_switch(
    api: &Api,
    request: Request<rpc::DecommissionSwitchRequest>,
) -> Result<Response<rpc::DecommissionSwitchResponse>, Status> {
    log_request_data(&request);
    let switch_id = request
        .into_inner()
        .switch_id
        .ok_or_else(|| CarbideError::InvalidArgument("switch_id is required".to_string()))?;

    let component_manager = api.component_manager.as_ref().ok_or_else(|| {
        CarbideError::FailedPrecondition(
            "managed-switch decommissioning requires the RMS component-manager backend".to_string(),
        )
    })?;
    if component_manager.nv_switch.name() != "rms" {
        return Err(CarbideError::FailedPrecondition(format!(
            "managed-switch decommissioning requires the RMS component-manager backend (configured backend: {})",
            component_manager.nv_switch.name()
        ))
        .into());
    }

    let mut txn = api.txn_begin().await?;
    let switch = db::switch::find_by_id(&mut txn, &switch_id)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;
    if !matches!(switch.controller_state.value, SwitchControllerState::Ready) {
        return Err(CarbideError::FailedPrecondition(format!(
            "switch {switch_id} must be in the ready state to be decommissioned (current state: {})",
            serde_json::to_string(&switch.controller_state.value).unwrap_or_default()
        ))
        .into());
    }

    if let Some(rack_id) = switch.rack_id.as_ref() {
        let assigned_hosts =
            db::managed_host::find_assigned_hosts_in_rack(&mut txn, rack_id).await?;
        if !assigned_hosts.is_empty() {
            let assignments = assigned_hosts
                .iter()
                .map(|(machine_id, instance_id)| format!("{machine_id} ({instance_id})"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CarbideError::FailedPrecondition(format!(
                "switch {switch_id} cannot be decommissioned while managed hosts in rack {rack_id} are assigned to instances: {assignments}"
            ))
            .into());
        }
    }

    db_switch::set_decommission_requested(&mut txn, switch_id).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::DecommissionSwitchResponse {}))
}

pub(crate) async fn find_switch_health_histories(
    api: &Api,
    request: Request<rpc::SwitchHealthHistoriesRequest>,
) -> Result<Response<rpc::HealthHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    crate::handlers::health::find_health_histories(
        api,
        request.switch_ids,
        db::health_history::HealthHistoryTableId::Switch,
        request.start_time,
        request.end_time,
    )
    .await
}

// TODO: block if switch is in use (firmware update, etc.)
pub(crate) async fn delete_switch(
    api: &Api,
    request: Request<rpc::SwitchDeletionRequest>,
) -> Result<Response<rpc::SwitchDeletionResult>, Status> {
    let req = request.into_inner();

    let switch_id = match req.id {
        Some(id) => id,
        None => {
            return Err(CarbideError::InvalidArgument("switch ID is required".to_string()).into());
        }
    };

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let mut switch_list = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("Failed to find switch: {}", e),
    })?;

    if switch_list.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        }
        .into());
    }

    let switch = switch_list.first_mut().unwrap();
    db_switch::mark_as_deleted(switch, &mut txn)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to delete switch: {}", e),
        })?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(rpc::SwitchDeletionResult {}))
}

/// Force deletes a switch and optionally its associated interfaces from the database.
/// Unlike `delete_switch` (soft delete), this immediately hard-deletes the switch
/// while retaining its state history.
pub(crate) async fn admin_force_delete_switch(
    api: &Api,
    request: Request<rpc::AdminForceDeleteSwitchRequest>,
) -> Result<Response<rpc::AdminForceDeleteSwitchResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let switch_id = request
        .switch_id
        .ok_or_else(|| CarbideError::InvalidArgument("switch_id is required".to_string()))?;

    let mut txn = api.txn_begin().await?;

    // Verify the switch exists.
    let switch_list = db_switch::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await
    .map_err(CarbideError::from)?;

    let switch = switch_list
        .into_iter()
        .next()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;

    let needs_mac_cleanup = request.delete_interfaces || request.delete_bmc_suppressions;
    let macs = if needs_mac_cleanup {
        associated_switch_macs(&mut txn, &switch).await?
    } else {
        Vec::new()
    };

    // Optionally delete associated machine interfaces.
    let mut interfaces_deleted: u32 = 0;
    if request.delete_interfaces {
        let mut interface_ids: HashSet<MachineInterfaceId> =
            db::machine_interface::find_ids_by_switch_id(&mut txn, &switch_id)
                .await
                .map_err(CarbideError::from)?
                .into_iter()
                .collect();
        for &mac in &macs {
            for interface in db::machine_interface::find_by_mac_address(&mut txn, mac)
                .await
                .map_err(CarbideError::from)?
            {
                interface_ids.insert(interface.id);
            }
        }
        for interface_id in &interface_ids {
            db::machine_interface::delete(interface_id, &mut txn)
                .await
                .map_err(CarbideError::from)?;
        }
        interfaces_deleted = interface_ids.len() as u32;
    }

    if request.delete_bmc_suppressions {
        db::bmc_suppression::delete_many(&mut txn, &macs, BmcSuppressionSubsystem::Dhcp)
            .await
            .map_err(CarbideError::from)?;
        db::bmc_suppression::delete_many(&mut txn, &macs, BmcSuppressionSubsystem::SiteExplorer)
            .await
            .map_err(CarbideError::from)?;
    }

    // Hard-delete the switch.
    db_switch::final_delete(switch_id, &mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::AdminForceDeleteSwitchResponse {
        switch_id: switch_id.to_string(),
        interfaces_deleted,
    }))
}

pub(crate) async fn update_switch_metadata(
    api: &Api,
    request: Request<rpc::SwitchMetadataUpdateRequest>,
) -> std::result::Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let switch_id = request
        .switch_id
        .ok_or_else(|| CarbideError::from(RpcDataConversionError::MissingArgument("switch_id")))?;

    let metadata = match request.metadata {
        Some(m) => Metadata::try_from(m).map_err(CarbideError::from)?,
        _ => {
            return Err(
                CarbideError::from(RpcDataConversionError::MissingArgument("metadata")).into(),
            );
        }
    };
    metadata.validate(true).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    let switches = db_switch::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_switch::IdColumn, &switch_id),
    )
    .await
    .map_err(CarbideError::from)?;

    let switch = switches
        .into_iter()
        .next()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;

    let expected_version: config_version::ConfigVersion = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => switch.version,
    };

    db_switch::update_metadata(&mut txn, &switch_id, expected_version, metadata).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

pub(crate) async fn list_switch_health_reports(
    api: &Api,
    request: Request<rpc::ListSwitchHealthReportsRequest>,
) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let switch_id = req
        .switch_id
        .ok_or_else(|| CarbideError::MissingArgument("switch_id"))?;

    let mut conn = api
        .database_connection
        .acquire()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let switch = db_switch::find_by_id(&mut conn, &switch_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;

    Ok(Response::new(rpc::ListHealthReportResponse {
        health_report_entries: switch
            .health_reports
            .into_iter()
            .map(|o| HealthReportEntry {
                report: Some(o.0.into()),
                mode: o.1 as i32,
            })
            .collect(),
    }))
}

pub(crate) async fn insert_switch_health_report(
    api: &Api,
    request: Request<rpc::InsertSwitchHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let rpc::InsertSwitchHealthReportRequest {
        switch_id,
        health_report_entry: Some(rpc::HealthReportEntry { report, mode }),
    } = request.into_inner()
    else {
        return Err(CarbideError::MissingArgument("override").into());
    };
    let switch_id = switch_id.ok_or_else(|| CarbideError::MissingArgument("switch_id"))?;

    let Some(report) = report else {
        return Err(CarbideError::MissingArgument("report").into());
    };
    let Ok(mode) = rpc::HealthReportApplyMode::try_from(mode) else {
        return Err(CarbideError::InvalidArgument("mode".to_string()).into());
    };
    let mode: HealthReportApplyMode = mode.into();

    let mut txn = api.txn_begin().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;

    let mut report = health_report::HealthReport::try_from(report.clone())
        .map_err(|e| CarbideError::internal(e.to_string()))?;
    if report.observed_at.is_none() {
        report.observed_at = Some(chrono::Utc::now());
    }
    report.triggered_by = triggered_by;
    report.update_in_alert_since(switch.health_reports.by_source(&report.source));

    match remove_switch_health_report_by_source(&switch, &mut txn, report.source.clone()).await {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    }

    db_switch::insert_health_report(&mut txn, &switch_id, mode, &report).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn remove_switch_health_report(
    api: &Api,
    request: Request<rpc::RemoveSwitchHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let rpc::RemoveSwitchHealthReportRequest { switch_id, source } = request.into_inner();
    let switch_id = switch_id.ok_or_else(|| CarbideError::MissingArgument("switch_id"))?;

    let mut txn = api.txn_begin().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "switch",
            id: switch_id.to_string(),
        })?;

    remove_switch_health_report_by_source(&switch, &mut txn, source).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

async fn remove_switch_health_report_by_source(
    switch: &model::switch::Switch,
    txn: &mut db::Transaction<'_>,
    source: String,
) -> Result<(), CarbideError> {
    let mode = if switch.health_reports.replace.as_ref().map(|o| &o.source) == Some(&source) {
        HealthReportApplyMode::Replace
    } else if switch.health_reports.merges.contains_key(&source) {
        HealthReportApplyMode::Merge
    } else {
        return Err(CarbideError::NotFoundError {
            kind: "switch health report with source",
            id: source,
        });
    };

    db_switch::remove_health_report(&mut *txn, &switch.id, mode, &source).await?;

    Ok(())
}

#[cfg(test)]
mod switch_nvos_info_tests {
    use ::rpc::forge as rpc;

    use super::legacy_switch_nvos_info;

    fn address(address_family: rpc::AddressFamily, address: &str) -> rpc::IpAddress {
        rpc::IpAddress {
            address_family: address_family.into(),
            address: address.to_string(),
        }
    }

    fn port(mac: &str, addresses: Vec<rpc::IpAddress>) -> rpc::SwitchNvosPortInfo {
        rpc::SwitchNvosPortInfo {
            mac: Some(mac.to_string()),
            service_port: None,
            addresses,
        }
    }

    #[test]
    fn returns_none_without_declared_ports() {
        assert!(legacy_switch_nvos_info(&[]).is_none());
    }

    #[test]
    fn skips_an_unresolved_port_when_a_later_port_has_an_address() {
        let ports = vec![
            port("44:44:33:33:01:00", Vec::new()),
            port(
                "44:44:33:33:01:01",
                vec![address(rpc::AddressFamily::V6, "2001:db8::10")],
            ),
        ];

        let info = legacy_switch_nvos_info(&ports).expect("NVOS info");
        assert_eq!(info.mac.as_deref(), Some("44:44:33:33:01:01"));
        assert_eq!(info.ip.as_deref(), Some("2001:db8::10"));
    }

    #[test]
    fn prefers_ipv4_across_resolved_ports() {
        let ports = vec![
            port(
                "44:44:33:33:01:00",
                vec![address(rpc::AddressFamily::V6, "2001:db8::10")],
            ),
            port(
                "44:44:33:33:01:01",
                vec![address(rpc::AddressFamily::V4, "10.2.14.52")],
            ),
        ];

        let info = legacy_switch_nvos_info(&ports).expect("NVOS info");
        assert_eq!(info.mac.as_deref(), Some("44:44:33:33:01:01"));
        assert_eq!(info.ip.as_deref(), Some("10.2.14.52"));
    }

    #[test]
    fn copies_service_port_to_legacy_info() {
        let mut nvos_port = port(
            "44:44:33:33:01:00",
            vec![address(rpc::AddressFamily::V4, "10.2.14.52")],
        );
        nvos_port.service_port = Some(8443);

        let info = legacy_switch_nvos_info(&[nvos_port]).expect("NVOS info");
        assert_eq!(info.port, Some(8443));
    }

    #[test]
    fn falls_back_to_the_first_declared_port_when_all_are_unresolved() {
        let ports = vec![
            port("44:44:33:33:01:00", Vec::new()),
            port("44:44:33:33:01:01", Vec::new()),
        ];

        let info = legacy_switch_nvos_info(&ports).expect("NVOS info");
        assert_eq!(info.mac.as_deref(), Some("44:44:33:33:01:00"));
        assert!(info.ip.is_none());
    }
}
