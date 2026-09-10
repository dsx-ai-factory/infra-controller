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
use std::collections::HashSet;
use std::str::FromStr;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge::{self as rpc, HealthReportEntry};
use carbide_instrument::{Event, emit};
use carbide_rack::firmware_object::{
    rack_maintenance_access_token_key, rms_access_token_or_noauth,
};
use carbide_secrets::credentials::CredentialManager;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use component_manager::component_manager::{
    RackMaintenanceAccessToken, RackMaintenanceEligibility, RackMaintenanceRequestOutcome,
    request_rack_maintenance_via_state_controller,
};
use db::{
    ObjectColumnFilter, WithTransaction, machine as db_machine, power_shelf as db_power_shelf,
    rack as db_rack, switch as db_switch,
};
use futures_util::FutureExt;
use health_report::HealthReportApplyMode;
use model::machine::machine_search_config::MachineSearchConfig;
use model::metadata::Metadata;
use model::rack::{MaintenanceActivity, MaintenanceScope, RackState};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_request_data_redacted};
use crate::auth::AuthContext;
use crate::handlers::component_manager::component_manager_error_to_status;

pub(crate) async fn get_rack(
    api: &Api,
    request: Request<rpc::GetRackRequest>,
) -> Result<Response<rpc::GetRackResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let mut reader = api.db_reader();

    let racks = if let Some(id) = req.id {
        let rack_id = RackId::from_str(&id)
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid rack ID: {}", e)))?;
        db_rack::find_by(
            reader.as_mut(),
            ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
        )
        .await
        .map_err(CarbideError::from)?
    } else {
        db_rack::find_by(
            reader.as_mut(),
            ObjectColumnFilter::All::<db_rack::IdColumn>,
        )
        .await
        .map_err(CarbideError::from)?
    };

    let mut result = Vec::with_capacity(racks.len());
    for r in racks {
        let rpc_rack: rpc::Rack = r.into();
        result.push(rpc_rack);
    }

    Ok(Response::new(rpc::GetRackResponse { rack: result }))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::RackSearchFilter>,
) -> Result<Response<rpc::RackIdList>, Status> {
    log_request_data(&request);

    let filter: model::rack::RackSearchFilter = request.into_inner().into();

    let rack_ids = db::rack::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::RackIdList { rack_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::RacksByIdsRequest>,
) -> Result<Response<rpc::RackList>, Status> {
    log_request_data(&request);

    let rack_ids = request.into_inner().rack_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if rack_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if rack_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let racks = db::rack::find_by(
        &mut txn,
        ObjectColumnFilter::List(db::rack::IdColumn, &rack_ids),
    )
    .await?;

    let mut result = Vec::with_capacity(racks.len());
    for rack in racks {
        result.push(rack.into());
    }

    txn.rollback_or_log("read-only load of racks by id").await;

    Ok(Response::new(rpc::RackList { racks: result }))
}

pub(crate) async fn find_rack_state_histories(
    api: &Api,
    request: Request<rpc::RackStateHistoriesRequest>,
) -> Result<Response<rpc::StateHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let rack_ids = request.rack_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if rack_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if rack_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;

    let results = db::state_history::find_by_object_ids(
        &mut txn,
        db::state_history::StateHistoryTableId::Rack,
        &rack_ids,
    )
    .await
    .map_err(CarbideError::from)?;

    let mut response = rpc::StateHistories::default();
    for (rack_id, records) in results {
        response.histories.insert(
            rack_id,
            ::rpc::forge::StateHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;

    Ok(tonic::Response::new(response))
}

pub(crate) async fn find_rack_health_histories(
    api: &Api,
    request: Request<rpc::RackHealthHistoriesRequest>,
) -> Result<Response<rpc::HealthHistories>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    crate::handlers::health::find_health_histories(
        api,
        request.rack_ids,
        db::health_history::HealthHistoryTableId::Rack,
        request.start_time,
        request.end_time,
    )
    .await
}

pub(crate) async fn delete_rack(
    api: &Api,
    request: Request<rpc::DeleteRackRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    api.with_txn(|txn| {
        async move {
            let rack_id = RackId::from_str(&req.id)
                .map_err(|e| CarbideError::InvalidArgument(format!("invalid rack ID: {}", e)))?;
            let _rack = db_rack::find_by(
                txn.as_mut(),
                ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
            )
            .await
            .map_err(CarbideError::from)?
            .pop()
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "rack",
                id: rack_id.to_string(),
            })?;

            db_rack::mark_as_deleted(&rack_id, txn)
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("Marking rack deleted {}", e),
                })?;
            Ok::<_, Status>(())
        }
        .boxed()
    })
    .await??;
    Ok(Response::new(()))
}

#[derive(Event)]
#[event(
    event_name = "rack_force_delete_access_token_cleanup_failed",
    metric_name = "carbide_rack_maintenance_access_token_cleanup_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "failed to delete rack maintenance access token during force delete",
    describe = "Number of rack maintenance access token cleanup failures"
)]
struct RackForceDeleteAccessTokenCleanupFailed {
    #[context]
    rack_id: RackId,
    #[context]
    error: String,
}

async fn delete_rack_maintenance_access_token_after_force_delete(
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) {
    if let Err(error) = credential_manager
        .delete_credentials(&rack_maintenance_access_token_key(rack_id))
        .await
    {
        emit(RackForceDeleteAccessTokenCleanupFailed {
            rack_id: rack_id.clone(),
            error: error.to_string(),
        });
    }
}

/// Force deletes a rack from the database.
/// Unlike `delete_rack` (soft delete), this immediately hard-deletes the rack
/// while retaining its state history.
pub(crate) async fn admin_force_delete_rack(
    api: &Api,
    request: Request<rpc::AdminForceDeleteRackRequest>,
) -> Result<Response<rpc::AdminForceDeleteRackResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();

    let rack_id = request
        .rack_id
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".to_string()))?;

    let mut txn = api.txn_begin().await?;

    let rack_list = db_rack::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?;

    if rack_list.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "rack",
            id: rack_id.to_string(),
        }
        .into());
    }

    db_rack::final_delete(&mut txn, &rack_id)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    delete_rack_maintenance_access_token_after_force_delete(
        api.credential_manager.as_ref(),
        &rack_id,
    )
    .await;

    Ok(Response::new(rpc::AdminForceDeleteRackResponse {
        rack_id: rack_id.to_string(),
    }))
}

pub(crate) async fn list_rack_health_reports(
    api: &Api,
    request: Request<rpc::ListRackHealthReportsRequest>,
) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let rack_id = req
        .rack_id
        .ok_or_else(|| CarbideError::MissingArgument("rack_id"))?;

    let rack = db_rack::find_by(
        api.db_reader().as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    Ok(Response::new(rpc::ListHealthReportResponse {
        health_report_entries: rack
            .health_reports
            .into_iter()
            .map(|o| HealthReportEntry {
                report: Some(o.0.into()),
                mode: o.1 as i32,
            })
            .collect(),
    }))
}

pub(crate) async fn insert_rack_health_report(
    api: &Api,
    request: Request<rpc::InsertRackHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let rpc::InsertRackHealthReportRequest {
        rack_id,
        health_report_entry: Some(rpc::HealthReportEntry { report, mode }),
    } = request.into_inner()
    else {
        return Err(CarbideError::MissingArgument("override").into());
    };
    let rack_id = rack_id.ok_or_else(|| CarbideError::MissingArgument("rack_id"))?;

    let Some(report) = report else {
        return Err(CarbideError::MissingArgument("report").into());
    };
    let Ok(mode) = rpc::HealthReportApplyMode::try_from(mode) else {
        return Err(CarbideError::InvalidArgument("mode".to_string()).into());
    };
    let mode: HealthReportApplyMode = mode.into();

    let mut txn = api.txn_begin().await?;

    let rack = db_rack::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    let mut report = health_report::HealthReport::try_from(report.clone())
        .map_err(|e| CarbideError::internal(e.to_string()))?;
    if report.observed_at.is_none() {
        report.observed_at = Some(chrono::Utc::now());
    }
    report.triggered_by = triggered_by;
    report.update_in_alert_since(rack.health_reports.by_source(&report.source));

    match remove_rack_override_by_source(&rack, &mut txn, report.source.clone()).await {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    }

    db_rack::insert_health_report(&mut txn, &rack.id, mode, &report).await?;

    txn.commit().await?;

    if let Some(handle) = api.bms_client.get() {
        handle.update_rack_leak_state(&rack.id, &report).await;
    }

    Ok(Response::new(()))
}

pub(crate) async fn remove_rack_health_report(
    api: &Api,
    request: Request<rpc::RemoveRackHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let rpc::RemoveRackHealthReportRequest { rack_id, source } = request.into_inner();
    let rack_id = rack_id.ok_or_else(|| CarbideError::MissingArgument("rack_id"))?;

    let mut txn = api.txn_begin().await?;

    let rack = db_rack::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    remove_rack_override_by_source(&rack, &mut txn, source).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

async fn remove_rack_override_by_source(
    rack: &model::rack::Rack,
    txn: &mut db::Transaction<'_>,
    source: String,
) -> Result<(), CarbideError> {
    let mode = if rack.health_reports.replace.as_ref().map(|o| &o.source) == Some(&source) {
        HealthReportApplyMode::Replace
    } else if rack.health_reports.merges.contains_key(&source) {
        HealthReportApplyMode::Merge
    } else {
        return Err(CarbideError::NotFoundError {
            kind: "rack override with source",
            id: source,
        });
    };

    db_rack::remove_health_report(&mut *txn, &rack.id, mode, &source).await?;

    Ok(())
}

pub(crate) async fn get_rack_profile(
    api: &Api,
    request: Request<rpc::GetRackProfileRequest>,
) -> Result<Response<rpc::GetRackProfileResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let rack_id = req
        .rack_id
        .ok_or_else(|| CarbideError::MissingArgument("rack_id"))?;

    let rack = db_rack::find_by(
        api.db_reader().as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    let rack_profile_id =
        rack.rack_profile_id
            .as_ref()
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "rack_profile_id for rack",
                id: rack_id.to_string(),
            })?;

    let profile = api
        .runtime_config
        .rack_profiles
        .get(rack_profile_id.as_str())
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "rack profile for rack_profile_id",
            id: rack_profile_id.to_string(),
        })?;

    let rpc_profile: rpc::RackProfile = profile.into();

    Ok(Response::new(rpc::GetRackProfileResponse {
        rack_id: Some(rack_id),
        rack_profile_id: Some(rack_profile_id.clone()),
        profile: Some(rpc_profile),
    }))
}

pub(crate) fn list_rack_profiles(
    api: &Api,
    request: Request<()>,
) -> Result<Response<rpc::ListRackProfilesResponse>, Status> {
    log_request_data(&request);

    Ok(Response::new(rpc::ListRackProfilesResponse {
        rack_profiles: configured_rack_profiles(&api.runtime_config.rack_profiles),
    }))
}

fn configured_rack_profiles(
    config: &model::rack_type::RackProfileConfig,
) -> Vec<rpc::ConfiguredRackProfile> {
    let mut rack_profiles = config.rack_profiles.iter().collect::<Vec<_>>();
    rack_profiles.sort_unstable_by_key(|(rack_profile_id, _)| *rack_profile_id);

    rack_profiles
        .into_iter()
        .map(|(rack_profile_id, profile)| (rack_profile_id.as_str(), profile).into())
        .collect()
}

pub(crate) async fn update_rack_metadata(
    api: &Api,
    request: Request<rpc::RackMetadataUpdateRequest>,
) -> std::result::Result<tonic::Response<()>, tonic::Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let rack_id = request
        .rack_id
        .ok_or_else(|| CarbideError::from(RpcDataConversionError::MissingArgument("rack_id")))?;

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

    let rack = db_rack::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    let expected_version: config_version::ConfigVersion = match request.if_version_match {
        Some(version) => version.parse().map_err(CarbideError::from)?,
        None => rack.version,
    };

    db_rack::update_metadata(&mut txn, &rack_id, expected_version, metadata).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(()))
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn set_maintenance_access_token(
    maintenance_access_token: &mut Option<String>,
    access_token: Option<String>,
) -> Result<(), CarbideError> {
    let Some(access_token) = access_token else {
        return Ok(());
    };

    if let Some(existing) = maintenance_access_token {
        if existing != &access_token {
            return Err(CarbideError::InvalidArgument(
                "rack maintenance activities must use the same access_token".into(),
            ));
        }
        return Ok(());
    }

    *maintenance_access_token = Some(access_token);
    Ok(())
}

#[derive(Event)]
#[event(
    event_name = "rack_maintenance_termination_access_token_cleanup_failed",
    metric_name = "carbide_rack_maintenance_access_token_cleanup_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "failed to delete rack maintenance access token during termination",
    describe = "Number of rack maintenance access token cleanup failures"
)]
struct RackMaintenanceTerminationAccessTokenCleanupFailed {
    #[context]
    rack_id: RackId,
    #[context]
    error: String,
}

async fn delete_rack_maintenance_access_token_after_termination(
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) {
    if let Err(error) = credential_manager
        .delete_credentials(&rack_maintenance_access_token_key(rack_id))
        .await
    {
        emit(RackMaintenanceTerminationAccessTokenCleanupFailed {
            rack_id: rack_id.clone(),
            error: error.to_string(),
        });
    }
}

pub(crate) async fn terminate_rack_maintenance(
    api: &Api,
    request: Request<rpc::RackMaintenanceTerminateRequest>,
) -> Result<Response<rpc::RackMaintenanceTerminateResponse>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let rack_id = request
        .rack_id
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".into()))?;

    let mut txn = api.txn_begin().await?;
    if !db_rack::lock_for_update(txn.as_mut(), &rack_id)
        .await
        .map_err(CarbideError::from)?
    {
        return Err(CarbideError::NotFoundError {
            kind: "rack",
            id: rack_id.to_string(),
        }
        .into());
    }

    let mut rack = db_rack::find_by(
        &mut txn,
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    if !matches!(rack.controller_state.value, RackState::Maintenance { .. }) {
        return Err(CarbideError::InvalidArgument(format!(
            "rack {rack_id} is not in maintenance (current: {:?})",
            rack.controller_state.value
        ))
        .into());
    }

    if !rack.config.maintenance_termination_requested {
        rack.config.maintenance_termination_requested = true;
        db_rack::update(txn.as_mut(), &rack_id, &rack.config)
            .await
            .map_err(CarbideError::from)?;
    }
    txn.commit().await?;

    // The database request is durable before external credential cleanup. The
    // rack controller will still terminate if the credential store is unavailable.
    delete_rack_maintenance_access_token_after_termination(
        api.credential_manager.as_ref(),
        &rack_id,
    )
    .await;

    tracing::info!(rack_id = %rack_id, "Rack maintenance termination requested");
    Ok(Response::new(rpc::RackMaintenanceTerminateResponse {}))
}

pub(crate) async fn on_demand_rack_maintenance(
    api: &Api,
    request: Request<rpc::RackMaintenanceOnDemandRequest>,
) -> Result<Response<rpc::RackMaintenanceOnDemandResponse>, Status> {
    log_request_data_redacted("RackMaintenanceOnDemandRequest { redacted }");

    let req = request.into_inner();

    let rack_id = req
        .rack_id
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".into()))?;

    let rack = db_rack::find_by(
        api.db_reader().as_mut(),
        ObjectColumnFilter::One(db_rack::IdColumn, &rack_id),
    )
    .await
    .map_err(CarbideError::from)?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "rack",
        id: rack_id.to_string(),
    })?;

    if !matches!(
        *rack.controller_state,
        RackState::Ready | RackState::Error { .. }
    ) {
        return Err(CarbideError::InvalidArgument(format!(
            "rack {} is not in ready or error state (current: {:?}). maintenance can only be requested when the rack is ready or in error",
            rack_id, *rack.controller_state
        ))
        .into());
    }

    if rack.config.maintenance_requested.is_some() {
        return Err(CarbideError::InvalidArgument(format!(
            "on-demand maintenance for rack {} is already scheduled",
            rack_id,
        ))
        .into());
    }

    use rpc::maintenance_activity_config::Activity as ProtoActivity;

    let proto_scope = req.scope.unwrap_or_default();

    let mut activities = Vec::with_capacity(proto_scope.activities.len());
    let mut maintenance_access_token = None;
    for entry in &proto_scope.activities {
        let activity = match &entry.activity {
            Some(ProtoActivity::FirmwareUpgrade(fw)) => {
                let firmware_version = non_empty_string(&fw.firmware_version);
                let access_token = rms_access_token_or_noauth(fw.access_token.as_deref());

                if firmware_version.is_none() {
                    return Err(CarbideError::InvalidArgument(
                        "firmware-upgrade rack maintenance requires SOT JSON in firmware_version"
                            .into(),
                    )
                    .into());
                }
                if let Some(config_json) = firmware_version.as_deref() {
                    serde_json::from_str::<serde_json::Value>(config_json).map_err(|error| {
                        CarbideError::InvalidArgument(format!(
                            "firmware-upgrade firmware_version must contain valid SOT JSON: {error}"
                        ))
                    })?;
                }
                set_maintenance_access_token(&mut maintenance_access_token, Some(access_token))?;

                MaintenanceActivity::FirmwareUpgrade {
                    firmware_version,
                    components: fw.components.clone(),
                    force_update: fw.force_update,
                }
            }
            Some(ProtoActivity::NvosUpdate(nvos)) => {
                let config_json = non_empty_string(&nvos.config_json);
                let access_token = rms_access_token_or_noauth(nvos.access_token.as_deref());

                if config_json.is_none() {
                    return Err(CarbideError::InvalidArgument(
                        "nvos-update rack maintenance requires SOT JSON in config_json".into(),
                    )
                    .into());
                }
                let config_json = config_json.expect("validated above");
                serde_json::from_str::<serde_json::Value>(&config_json).map_err(|error| {
                    CarbideError::InvalidArgument(format!(
                        "nvos-update config_json must contain valid SOT JSON: {error}"
                    ))
                })?;
                set_maintenance_access_token(&mut maintenance_access_token, Some(access_token))?;

                MaintenanceActivity::NvosUpdate { config_json }
            }
            Some(ProtoActivity::ConfigureNmxCluster(_)) => MaintenanceActivity::ConfigureNmxCluster,
            Some(ProtoActivity::PowerSequence(_)) => MaintenanceActivity::PowerSequence,
            None => {
                return Err(CarbideError::InvalidArgument(
                    "maintenance activity entry has no activity set".into(),
                )
                .into());
            }
        };
        activities.push(activity);
    }

    let scope = MaintenanceScope {
        machine_ids: proto_scope
            .machine_ids
            .iter()
            .map(|s| HostMachineId::from_str(s))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid machine_id: {e}")))?,
        switch_ids: proto_scope
            .switch_ids
            .iter()
            .map(|s| SwitchId::from_str(s))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid switch_id: {e}")))?,
        power_shelf_ids: proto_scope
            .power_shelf_ids
            .iter()
            .map(|s| PowerShelfId::from_str(s))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CarbideError::InvalidArgument(format!("invalid power_shelf_id: {e}")))?,
        activities,
    };

    if !scope.is_full_rack() {
        let mut reader = api.db_reader();

        if !scope.machine_ids.is_empty() {
            let rack_machines: HashSet<HostMachineId> =
                db_machine::find_machine_ids::<HostMachineId>(
                    reader.as_mut(),
                    MachineSearchConfig {
                        rack_id: Some(rack_id.clone()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(CarbideError::from)?
                .into_iter()
                .collect();

            let foreign: Vec<_> = scope
                .machine_ids
                .iter()
                .filter(|id| !rack_machines.contains(id))
                .collect();
            if !foreign.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "machine(s) [{}] do not belong to rack {rack_id}",
                    foreign
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ))
                .into());
            }
        }

        if !scope.switch_ids.is_empty() {
            let rack_switches: HashSet<SwitchId> = db_switch::find_ids(
                reader.as_mut(),
                model::switch::SwitchSearchFilter {
                    rack_id: Some(rack_id.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(CarbideError::from)?
            .into_iter()
            .collect();

            let foreign: Vec<_> = scope
                .switch_ids
                .iter()
                .filter(|id| !rack_switches.contains(id))
                .collect();
            if !foreign.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "switch(es) [{}] do not belong to rack {rack_id}",
                    foreign
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ))
                .into());
            }
        }

        if !scope.power_shelf_ids.is_empty() {
            let rack_power_shelves: HashSet<PowerShelfId> = db_power_shelf::find_ids(
                reader.as_mut(),
                model::power_shelf::PowerShelfSearchFilter {
                    rack_id: Some(rack_id.clone()),
                    ..Default::default()
                },
            )
            .await
            .map_err(CarbideError::from)?
            .into_iter()
            .collect();

            let foreign: Vec<_> = scope
                .power_shelf_ids
                .iter()
                .filter(|id| !rack_power_shelves.contains(id))
                .collect();
            if !foreign.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "power shelf/shelves [{}] do not belong to rack {rack_id}",
                    foreign
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ))
                .into());
            }
        }
    }

    let maintenance_access_token =
        maintenance_access_token.map(|token| RackMaintenanceAccessToken {
            credential_manager: api.credential_manager.as_ref(),
            token,
        });

    let schedule_result = request_rack_maintenance_via_state_controller(
        &api.database_connection,
        &rack_id,
        scope,
        RackMaintenanceEligibility::AllowErrorRecovery,
        maintenance_access_token,
    )
    .await;

    let scheduling_error = match schedule_result {
        Ok(
            RackMaintenanceRequestOutcome::Scheduled
            | RackMaintenanceRequestOutcome::AlreadyPending,
        ) => None,
        Ok(RackMaintenanceRequestOutcome::Busy) => Some(
            CarbideError::InvalidArgument(format!(
                "on-demand maintenance for rack {} is already scheduled",
                rack_id,
            ))
            .into(),
        ),
        Ok(RackMaintenanceRequestOutcome::Deferred { state }) => Some(
            CarbideError::InvalidArgument(format!(
                "rack {} is not in ready or error state (current: {:?}). maintenance can only be requested when the rack is ready or in error",
                rack_id, state,
            ))
            .into(),
        ),
        Err(error) => Some(component_manager_error_to_status(error)),
    };

    if let Some(status) = scheduling_error {
        return Err(status);
    }

    tracing::info!(
        rack_id = %rack_id,
        "On-demand maintenance scheduled",
    );

    Ok(Response::new(rpc::RackMaintenanceOnDemandResponse {}))
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use model::rack_type::{RackProfile, RackProfileConfig};

    use super::*;

    const ACCESS_TOKEN_CLEANUP_FAILURE_METRIC: &str =
        "carbide_rack_maintenance_access_token_cleanup_failures_total";

    #[test]
    fn configured_rack_profiles_are_sorted_and_support_empty_config() {
        carbide_test_support::value_scenarios!(
            run = |rack_profile_ids: &[&str]| {
                let config = RackProfileConfig {
                    rack_profiles: rack_profile_ids
                        .iter()
                        .map(|rack_profile_id| {
                            ((*rack_profile_id).to_string(), RackProfile::default())
                        })
                        .collect(),
                };

                configured_rack_profiles(&config)
                    .into_iter()
                    .map(|configured| {
                        configured
                            .rack_profile_id
                            .expect("configured profile must have an ID")
                            .to_string()
                    })
                    .collect::<Vec<_>>()
            };
            "runtime rack profile configuration" {
                &[][..] => Vec::<String>::new(),
                &["zulu", "alpha"][..] => vec!["alpha".to_string(), "zulu".to_string()],
            }
        );
    }

    #[derive(Debug, PartialEq)]
    struct AccessTokenCleanupObservation {
        counter_delta: f64,
        log_count: usize,
        level: Option<tracing::Level>,
        metadata_name: Option<String>,
        message: Option<String>,
        event_name: Option<String>,
        metric_name: Option<String>,
        rack_id: Option<String>,
        error: Option<String>,
    }

    #[test]
    fn rack_force_delete_access_token_cleanup_emits_only_on_failure() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let rack_id = RackId::from("rack-1");

        carbide_test_support::value_scenarios!(
            run = |delete_fails: bool| {
                let credential_manager = TestCredentialManager::default();
                credential_manager.set_delete_credentials_failure(delete_fails);
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    runtime.block_on(delete_rack_maintenance_access_token_after_force_delete(
                        &credential_manager,
                        &rack_id,
                    ));
                })
                .into_iter()
                .filter(|log| {
                    log.field("event_name")
                        == Some("rack_force_delete_access_token_cleanup_failed")
                })
                .collect::<Vec<_>>();
                let log = logs.first();

                AccessTokenCleanupObservation {
                    counter_delta: metrics
                        .counter_delta(ACCESS_TOKEN_CLEANUP_FAILURE_METRIC, &[]),
                    log_count: logs.len(),
                    level: log.map(|log| log.level),
                    metadata_name: log.map(|log| log.metadata_name.clone()),
                    message: log.map(|log| log.message.clone()),
                    event_name: log
                        .and_then(|log| log.field("event_name"))
                        .map(str::to_string),
                    metric_name: log
                        .and_then(|log| log.field("metric_name"))
                        .map(str::to_string),
                    rack_id: log
                        .and_then(|log| log.field("rack_id"))
                        .map(str::to_string),
                    error: log.and_then(|log| log.field("error")).map(str::to_string),
                }
            };
            "credential cleanup outcome" {
                false => AccessTokenCleanupObservation {
                    counter_delta: 0.0,
                    log_count: 0,
                    level: None,
                    metadata_name: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    rack_id: None,
                    error: None,
                },
                true => AccessTokenCleanupObservation {
                    counter_delta: 1.0,
                    log_count: 1,
                    level: Some(tracing::Level::WARN),
                    metadata_name: Some(
                        "rack_force_delete_access_token_cleanup_failed".to_string(),
                    ),
                    message: Some(
                        "failed to delete rack maintenance access token during force delete"
                            .to_string(),
                    ),
                    event_name: Some(
                        "rack_force_delete_access_token_cleanup_failed".to_string(),
                    ),
                    metric_name: Some(ACCESS_TOKEN_CLEANUP_FAILURE_METRIC.to_string()),
                    rack_id: Some("rack-1".to_string()),
                    error: Some(
                        "Secrets operation failed: test credential delete failure".to_string(),
                    ),
                },
            }
        );
    }
}
