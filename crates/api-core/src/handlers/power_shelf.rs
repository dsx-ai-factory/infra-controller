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

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge::{self as rpc, HealthReportEntry};
use db::{ObjectColumnFilter, power_shelf as db_power_shelf};
use health_report::HealthReportApplyMode;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::metadata::Metadata;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::auth::AuthContext;

pub(crate) async fn find_power_shelf(
    api: &Api,
    request: Request<rpc::PowerShelfQuery>,
) -> Result<Response<rpc::PowerShelfList>, Status> {
    let query = request.into_inner();
    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    // Handle ID search (takes precedence)
    let power_shelf_list = if let Some(id) = query.power_shelf_id {
        db_power_shelf::find_by(
            &mut txn,
            db::ObjectColumnFilter::One(db_power_shelf::IdColumn, &id),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to find power shelf: {}", e),
        })?
    } else if let Some(name) = query.name {
        // Handle name search
        db_power_shelf::find_by(
            &mut txn,
            db::ObjectColumnFilter::One(db_power_shelf::NameColumn, &name),
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to find power shelf: {}", e),
        })?
    } else {
        // No filter - return all
        db_power_shelf::find_by(
            &mut txn,
            db::ObjectColumnFilter::<db_power_shelf::IdColumn>::All,
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to find power shelf: {}", e),
        })?
    };

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    let power_shelves = convert_power_shelves(power_shelf_list)?;

    Ok(Response::new(rpc::PowerShelfList { power_shelves }))
}

/// Convert DB power shelves into their RPC representation. `bmc_info` is
/// populated by the power-shelf load query and carried through the model->rpc
/// conversion, so no extra resolution is needed here.
fn convert_power_shelves(
    power_shelf_list: Vec<model::power_shelf::PowerShelf>,
) -> Result<Vec<rpc::PowerShelf>, CarbideError> {
    power_shelf_list
        .into_iter()
        .map(rpc::PowerShelf::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to convert power shelf: {}", e),
        })
}

pub(crate) async fn decommission_power_shelf(
    api: &Api,
    request: Request<rpc::DecommissionPowerShelfRequest>,
) -> Result<Response<rpc::DecommissionPowerShelfResponse>, Status> {
    log_request_data(&request);
    let power_shelf_id = request
        .into_inner()
        .power_shelf_id
        .ok_or_else(|| CarbideError::InvalidArgument("power_shelf_id is required".to_string()))?;
    let mut txn = api.txn_begin().await?;
    let power_shelf = db_power_shelf::find_by_id(&mut txn, &power_shelf_id)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        })?;

    if !matches!(
        power_shelf.controller_state.value,
        model::power_shelf::PowerShelfControllerState::Ready
    ) {
        return Err(CarbideError::FailedPrecondition(format!(
            "power shelf {power_shelf_id} must be in the ready state to be decommissioned (current state: {:?})",
            power_shelf.controller_state.value
        ))
        .into());
    }

    if let Some(rack_id) = power_shelf.rack_id.as_ref() {
        let assigned_hosts =
            db::managed_host::find_assigned_hosts_in_rack(&mut txn, rack_id).await?;
        if !assigned_hosts.is_empty() {
            let assignments = assigned_hosts
                .iter()
                .map(|(machine_id, instance_id)| format!("{machine_id} ({instance_id})"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CarbideError::FailedPrecondition(format!(
                "power shelf {power_shelf_id} cannot be decommissioned while managed hosts in rack {rack_id} are assigned to instances: {assignments}"
            ))
            .into());
        }
    }

    db_power_shelf::set_decommission_requested(&mut txn, power_shelf_id).await?;
    txn.commit().await?;
    Ok(Response::new(rpc::DecommissionPowerShelfResponse {}))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::PowerShelfSearchFilter>,
) -> Result<Response<rpc::PowerShelfIdList>, Status> {
    log_request_data(&request);

    let filter: model::power_shelf::PowerShelfSearchFilter = request.into_inner().into();

    let power_shelf_ids = db_power_shelf::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::PowerShelfIdList {
        ids: power_shelf_ids,
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::PowerShelvesByIdsRequest>,
) -> Result<Response<rpc::PowerShelfList>, Status> {
    log_request_data(&request);

    let power_shelf_ids = request.into_inner().power_shelf_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if power_shelf_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if power_shelf_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let power_shelf_list = db_power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::List(db_power_shelf::IdColumn, &power_shelf_ids),
    )
    .await?;

    txn.rollback_or_log("read-only load of power shelves by id")
        .await;

    let power_shelves = convert_power_shelves(power_shelf_list)?;

    Ok(Response::new(rpc::PowerShelfList { power_shelves }))
}

pub(crate) async fn delete_power_shelf(
    api: &Api,
    request: Request<rpc::PowerShelfDeletionRequest>,
) -> Result<Response<rpc::PowerShelfDeletionResult>, Status> {
    let req = request.into_inner();

    let power_shelf_id = match req.id {
        Some(id) => id,
        None => {
            return Err(
                CarbideError::InvalidArgument("power shelf ID is required".to_string()).into(),
            );
        }
    };

    let mut txn = api
        .database_connection
        .begin()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let mut power_shelf_list = db_power_shelf::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_power_shelf::IdColumn, &power_shelf_id),
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("Failed to find power shelf: {}", e),
    })?;

    if power_shelf_list.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        }
        .into());
    }

    let power_shelf = power_shelf_list.first_mut().unwrap();
    db_power_shelf::mark_as_deleted(power_shelf, &mut txn)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Failed to delete power shelf: {}", e),
        })?;

    txn.commit().await.map_err(|e| CarbideError::Internal {
        message: format!("Failed to commit transaction: {}", e),
    })?;

    Ok(Response::new(rpc::PowerShelfDeletionResult {}))
}

/// Force deletes a power shelf and optionally its associated interfaces
/// and BMC suppressions from the database.
/// Unlike `delete_power_shelf` (soft delete), this immediately hard-deletes the power shelf
/// while retaining its state history.
pub(crate) async fn admin_force_delete_power_shelf(
    api: &Api,
    request: Request<rpc::AdminForceDeletePowerShelfRequest>,
) -> Result<Response<rpc::AdminForceDeletePowerShelfResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let power_shelf_id = request
        .power_shelf_id
        .ok_or_else(|| CarbideError::InvalidArgument("power_shelf_id is required".to_string()))?;

    let mut txn = api.txn_begin().await?;

    // Verify the power shelf exists.
    let power_shelf = db_power_shelf::find_by_id(&mut txn, &power_shelf_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        })?;

    let mut interfaces_deleted: u32 = 0;
    if request.delete_interfaces {
        let interface_ids =
            db::machine_interface::find_ids_by_power_shelf_id(&mut txn, &power_shelf_id)
                .await
                .map_err(CarbideError::from)?;
        for interface_id in &interface_ids {
            db::machine_interface::delete(interface_id, &mut txn)
                .await
                .map_err(CarbideError::from)?;
        }
        interfaces_deleted = interface_ids.len() as u32;
    }

    if request.delete_bmc_suppressions {
        let bmc_mac = power_shelf
            .bmc_info
            .as_ref()
            .and_then(|info| info.mac)
            .or(power_shelf.bmc_mac_address)
            .ok_or_else(|| {
                CarbideError::FailedPrecondition(format!(
                    "power shelf {power_shelf_id} has no BMC MAC address; cannot delete BMC suppressions"
                ))
            })?;

        db::bmc_suppression::delete_many(
            &mut txn,
            &[bmc_mac],
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await
        .map_err(CarbideError::from)?;
        db::bmc_suppression::delete_many(&mut txn, &[bmc_mac], BmcSuppressionSubsystem::Dhcp)
            .await
            .map_err(CarbideError::from)?;
    }

    // Hard-delete the power shelf.
    db_power_shelf::final_delete(power_shelf_id, &mut txn)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::AdminForceDeletePowerShelfResponse {
        power_shelf_id: power_shelf_id.to_string(),
        interfaces_deleted,
    }))
}

pub(crate) async fn set_power_shelf_maintenance(
    api: &Api,
    request: Request<rpc::PowerShelfMaintenanceRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let req = request.into_inner();
    let operation = req.operation();
    let power_shelf_ids = req.power_shelf_ids;

    if power_shelf_ids.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one power_shelf_id must be provided".to_string(),
        )
        .into());
    }

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if power_shelf_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} power_shelf_ids can be accepted"
        ))
        .into());
    }

    let operation = match operation {
        rpc::PowerShelfMaintenanceOperation::PowerOn => {
            model::power_shelf::PowerShelfMaintenanceOperation::PowerOn
        }
        rpc::PowerShelfMaintenanceOperation::PowerOff => {
            model::power_shelf::PowerShelfMaintenanceOperation::PowerOff
        }
        rpc::PowerShelfMaintenanceOperation::Unspecified => {
            return Err(CarbideError::InvalidArgument(
                "operation must be PowerOn or PowerOff".to_string(),
            )
            .into());
        }
    };

    let initiator = match (triggered_by, req.reference) {
        (Some(user), Some(reference)) => format!("{user} ({reference})"),
        (Some(user), None) => user,
        (None, Some(reference)) => reference,
        (None, None) => "admin-cli".to_string(),
    };

    let mut txn = api.txn_begin().await?;

    let existing = db_power_shelf::find_by(
        &mut txn,
        ObjectColumnFilter::List(db_power_shelf::IdColumn, &power_shelf_ids),
    )
    .await
    .map_err(CarbideError::from)?;

    let mut by_id: std::collections::HashMap<_, _> =
        existing.into_iter().map(|ps| (ps.id, ps)).collect();

    for power_shelf_id in &power_shelf_ids {
        let power_shelf =
            by_id
                .remove(power_shelf_id)
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "power_shelf",
                    id: power_shelf_id.to_string(),
                })?;

        if power_shelf.is_marked_as_deleted() {
            return Err(CarbideError::InvalidArgument(format!(
                "power shelf {power_shelf_id} is marked for deletion",
            ))
            .into());
        }

        db_power_shelf::set_power_shelf_maintenance_requested(
            &mut txn,
            *power_shelf_id,
            &initiator,
            operation,
        )
        .await
        .map_err(CarbideError::from)?;
    }

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn find_power_shelf_state_histories(
    api: &Api,
    request: Request<rpc::PowerShelfStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let power_shelf_ids = request.power_shelf_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if power_shelf_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if power_shelf_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::PowerShelf,
        &power_shelf_ids,
    )
    .await
    .map_err(CarbideError::from)?;

    let mut response = rpc::StateHistories::default();
    for (power_shelf_id, records) in results {
        response.histories.insert(
            power_shelf_id,
            ::rpc::forge::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;

    Ok(tonic::Response::new(response))
}

pub(crate) async fn find_power_shelf_health_histories(
    api: &Api,
    request: Request<rpc::PowerShelfHealthHistoriesRequest>,
) -> Result<Response<rpc::HealthHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    crate::handlers::health::find_health_histories(
        api,
        request.power_shelf_ids,
        db::health_history::HealthHistoryTableId::PowerShelf,
        request.start_time,
        request.end_time,
    )
    .await
}

pub(crate) async fn update_power_shelf_metadata(
    api: &Api,
    request: Request<rpc::PowerShelfMetadataUpdateRequest>,
) -> std::result::Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let power_shelf_id = request.power_shelf_id.ok_or_else(|| {
        CarbideError::from(RpcDataConversionError::MissingArgument("power_shelf_id"))
    })?;

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

    let power_shelves = db_power_shelf::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db_power_shelf::IdColumn, &power_shelf_id),
    )
    .await
    .map_err(CarbideError::from)?;

    let power_shelf =
        power_shelves
            .into_iter()
            .next()
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "power_shelf",
                id: power_shelf_id.to_string(),
            })?;

    let expected_version: config_version::ConfigVersion = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => power_shelf.version,
    };

    db_power_shelf::update_metadata(&mut txn, &power_shelf_id, expected_version, metadata).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

pub(crate) async fn list_power_shelf_health_reports(
    api: &Api,
    request: Request<rpc::ListPowerShelfHealthReportsRequest>,
) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let power_shelf_id = req
        .power_shelf_id
        .ok_or_else(|| CarbideError::MissingArgument("power_shelf_id"))?;

    let mut conn = api
        .database_connection
        .acquire()
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Database error: {}", e),
        })?;

    let power_shelf = db_power_shelf::find_by_id(&mut conn, &power_shelf_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        })?;

    Ok(Response::new(rpc::ListHealthReportResponse {
        health_report_entries: power_shelf
            .health_reports
            .into_iter()
            .map(|o| HealthReportEntry {
                report: Some(o.0.into()),
                mode: o.1 as i32,
            })
            .collect(),
    }))
}

pub(crate) async fn insert_power_shelf_health_report(
    api: &Api,
    request: Request<rpc::InsertPowerShelfHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let rpc::InsertPowerShelfHealthReportRequest {
        power_shelf_id,
        health_report_entry: Some(rpc::HealthReportEntry { report, mode }),
    } = request.into_inner()
    else {
        return Err(CarbideError::MissingArgument("override").into());
    };
    let power_shelf_id =
        power_shelf_id.ok_or_else(|| CarbideError::MissingArgument("power_shelf_id"))?;

    let Some(report) = report else {
        return Err(CarbideError::MissingArgument("report").into());
    };
    let Ok(mode) = rpc::HealthReportApplyMode::try_from(mode) else {
        return Err(CarbideError::InvalidArgument("mode".to_string()).into());
    };
    let mode: HealthReportApplyMode = mode.into();

    let mut txn = api.txn_begin().await?;

    let power_shelf = db_power_shelf::find_by_id(&mut txn, &power_shelf_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        })?;

    let mut report = health_report::HealthReport::try_from(report.clone())
        .map_err(|e| CarbideError::internal(e.to_string()))?;
    if report.observed_at.is_none() {
        report.observed_at = Some(chrono::Utc::now());
    }
    report.triggered_by = triggered_by;
    report.update_in_alert_since(power_shelf.health_reports.by_source(&report.source));

    match remove_power_shelf_health_report_by_source(&power_shelf, &mut txn, report.source.clone())
        .await
    {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    }

    db_power_shelf::insert_health_report(&mut txn, &power_shelf_id, mode, &report).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn remove_power_shelf_health_report(
    api: &Api,
    request: Request<rpc::RemovePowerShelfHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let rpc::RemovePowerShelfHealthReportRequest {
        power_shelf_id,
        source,
    } = request.into_inner();
    let power_shelf_id =
        power_shelf_id.ok_or_else(|| CarbideError::MissingArgument("power_shelf_id"))?;

    let mut txn = api.txn_begin().await?;

    let power_shelf = db_power_shelf::find_by_id(&mut txn, &power_shelf_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "power_shelf",
            id: power_shelf_id.to_string(),
        })?;

    remove_power_shelf_health_report_by_source(&power_shelf, &mut txn, source).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

async fn remove_power_shelf_health_report_by_source(
    power_shelf: &model::power_shelf::PowerShelf,
    txn: &mut db::Transaction<'_>,
    source: String,
) -> Result<(), CarbideError> {
    let mode = if power_shelf
        .health_reports
        .replace
        .as_ref()
        .map(|o| &o.source)
        == Some(&source)
    {
        HealthReportApplyMode::Replace
    } else if power_shelf.health_reports.merges.contains_key(&source) {
        HealthReportApplyMode::Merge
    } else {
        return Err(CarbideError::NotFoundError {
            kind: "power shelf health report with source",
            id: source,
        });
    };

    db_power_shelf::remove_health_report(&mut *txn, &power_shelf.id, mode, &source).await?;

    Ok(())
}
