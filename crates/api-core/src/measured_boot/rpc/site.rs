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

/*!
 * gRPC handlers for measured boot site management API calls.
 */

use std::str::FromStr;

use ::rpc::errors::RpcDataConversionError;
use carbide_uuid::measured_boot::TrustedMachineId;
use db::DatabaseError;
use db::measured_boot::interface::profile::get_measurement_profile_record_by_id;
use db::measured_boot::interface::site::{
    get_approved_machines, get_approved_profiles, insert_into_approved_machines,
    insert_into_approved_profiles, list_attestation_summary,
    remove_from_approved_machines_by_approval_id, remove_from_approved_machines_by_machine_id,
    remove_from_approved_profiles_by_approval_id, remove_from_approved_profiles_by_profile_id,
};
use measured_boot::records::{
    MeasurementApprovedMachineRecord, MeasurementApprovedProfileRecord, MeasurementApprovedType,
};
use measured_boot::site::{MachineAttestationSummaryList, SiteModel};
use rpc::protos::measured_boot::{
    AddMeasurementTrustedMachineRequest, AddMeasurementTrustedMachineResponse,
    AddMeasurementTrustedProfileRequest, AddMeasurementTrustedProfileResponse,
    ExportSiteMeasurementsRequest, ExportSiteMeasurementsResponse, ImportSiteMeasurementsRequest,
    ImportSiteMeasurementsResponse, ImportSiteResult, ListAttestationSummaryRequest,
    ListAttestationSummaryResponse, ListMeasurementTrustedMachinesRequest,
    ListMeasurementTrustedMachinesResponse, ListMeasurementTrustedProfilesRequest,
    ListMeasurementTrustedProfilesResponse, MeasurementApprovedMachineRecordPb,
    MeasurementApprovedProfileRecordPb, RemoveMeasurementTrustedMachineRequest,
    RemoveMeasurementTrustedMachineResponse, RemoveMeasurementTrustedProfileRequest,
    RemoveMeasurementTrustedProfileResponse, remove_measurement_trusted_machine_request,
    remove_measurement_trusted_profile_request,
};
use tonic::Status;

use crate::CarbideError;
use crate::api::Api;

fn measurement_trust_removal_error(
    error: DatabaseError,
    kind: &'static str,
    id: String,
) -> CarbideError {
    if error.is_not_found() {
        CarbideError::NotFoundError { kind, id }
    } else {
        CarbideError::Internal {
            message: format!("removal failed: {error}"),
        }
    }
}

/// handle_import_site_measurements handles the ImportSiteMeasurements
/// API endpoint.
pub(crate) async fn handle_import_site_measurements(
    api: &Api,
    req: ImportSiteMeasurementsRequest,
) -> Result<ImportSiteMeasurementsResponse, Status> {
    let mut txn = api.txn_begin().await?;

    // Convert the site model from the SiteModelPb (and
    // make sure its good).
    let site_model = match req.model {
        Some(site_model_pb) => SiteModel::try_from(site_model_pb).map_err(|e| {
            CarbideError::InvalidArgument(format!("input site model failed translation: {e}"))
        })?,
        None => {
            return Err(
                CarbideError::InvalidArgument("site model cannot be empty".to_string()).into(),
            );
        }
    };

    // And now import it!
    let result = db::measured_boot::site::import(&mut txn, &site_model)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("site import failed: {e}"),
        })
        .map(|_| ImportSiteMeasurementsResponse {
            result: ImportSiteResult::Success.into(),
        });

    txn.commit().await?;
    Ok(result?)
}

/// handle_export_site_measurements handles the ExportSiteMeasurements
/// API endpoint.
pub(crate) async fn handle_export_site_measurements(
    api: &Api,
    _req: ExportSiteMeasurementsRequest,
) -> Result<ExportSiteMeasurementsResponse, Status> {
    let site_model = db::measured_boot::site::export(&mut api.db_reader())
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("export failed: {e}"),
        })?;

    Ok(ExportSiteMeasurementsResponse {
        model: Some(site_model.into()),
    })
}

/// handle_add_measurement_trusted_machine handles the
/// AddMeasurementTrustedMachine API endpoint.
pub(crate) async fn handle_add_measurement_trusted_machine(
    api: &Api,
    req: AddMeasurementTrustedMachineRequest,
) -> Result<AddMeasurementTrustedMachineResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let approval_type = req.approval_type();
    let approval_record = insert_into_approved_machines(
        &mut txn,
        TrustedMachineId::from_str(&req.machine_id).map_err(|_| {
            CarbideError::from(RpcDataConversionError::InvalidMachineId(req.machine_id))
        })?,
        MeasurementApprovedType::from(approval_type),
        Some(req.pcr_registers),
        Some(req.comments),
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("failed to insert trusted machine approval: {e}"),
    })?;

    txn.commit().await?;
    Ok(AddMeasurementTrustedMachineResponse {
        approval_record: Some(approval_record.into()),
    })
}

/// handle_remove_measurement_trusted_machine handles the
/// RemoveMeasurementTrustedMachine API endpoint.
pub(crate) async fn handle_remove_measurement_trusted_machine(
    api: &Api,
    req: RemoveMeasurementTrustedMachineRequest,
) -> Result<RemoveMeasurementTrustedMachineResponse, Status> {
    let mut txn = api.txn_begin().await?;

    let approval_record: MeasurementApprovedMachineRecord = match req.selector {
        // Remove by approval ID.
        Some(remove_measurement_trusted_machine_request::Selector::ApprovalId(approval_uuid)) => {
            let id = approval_uuid.to_string();
            remove_from_approved_machines_by_approval_id(&mut txn, approval_uuid)
                .await
                .map_err(|error| {
                    measurement_trust_removal_error(
                        error,
                        "measurement trusted machine approval",
                        id,
                    )
                })?
        }
        // Remove by machine ID.
        Some(remove_measurement_trusted_machine_request::Selector::MachineId(machine_id)) => {
            remove_from_approved_machines_by_machine_id(
                &mut txn,
                TrustedMachineId::from_str(&machine_id).map_err(|_| {
                    CarbideError::from(RpcDataConversionError::InvalidMachineId(machine_id.clone()))
                })?,
            )
            .await
            .map_err(|error| {
                measurement_trust_removal_error(
                    error,
                    "measurement trusted machine approval",
                    machine_id,
                )
            })?
        }
        // Oops, forgot to set a selector.
        None => {
            return Err(CarbideError::InvalidArgument(
                "approval or machine ID selector missing".into(),
            )
            .into());
        }
    };

    txn.commit().await?;
    Ok(RemoveMeasurementTrustedMachineResponse {
        approval_record: Some(approval_record.into()),
    })
}

/// handle_list_measurement_trusted_machines handles the
/// ListMeasurementTrustedMachines API endpoint.
pub(crate) async fn handle_list_measurement_trusted_machines(
    api: &Api,
    _req: ListMeasurementTrustedMachinesRequest,
) -> Result<ListMeasurementTrustedMachinesResponse, Status> {
    let approval_records: Vec<MeasurementApprovedMachineRecordPb> =
        get_approved_machines(&api.database_connection)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to fetch machine approvals: {e}"),
            })?
            .into_iter()
            .map(|record| record.into())
            .collect();

    Ok(ListMeasurementTrustedMachinesResponse { approval_records })
}

/// handle_add_measurement_trusted_profile handles the
/// AddMeasurementTrustedProfile API endpoint.
pub(crate) async fn handle_add_measurement_trusted_profile(
    api: &Api,
    req: AddMeasurementTrustedProfileRequest,
) -> Result<AddMeasurementTrustedProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let approval_type = req.approval_type();
    let profile_id = req
        .profile_id
        .ok_or(CarbideError::MissingArgument("profile_id"))?;
    get_measurement_profile_record_by_id(&mut txn, profile_id)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to fetch measurement system profile: {e}"),
        })?
        .ok_or(CarbideError::NotFoundError {
            kind: "MeasurementSystemProfile",
            id: profile_id.to_string(),
        })?;
    let approval_record = insert_into_approved_profiles(
        &mut txn,
        profile_id,
        MeasurementApprovedType::from(approval_type),
        req.pcr_registers,
        req.comments,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("failed to insert trusted profile approval: {e}"),
    })?;

    txn.commit().await?;
    Ok(AddMeasurementTrustedProfileResponse {
        approval_record: Some(approval_record.into()),
    })
}

/// handle_remove_measurement_trusted_profile handles the
/// RemoveMeasurementTrustedProfile API endpoint.
pub(crate) async fn handle_remove_measurement_trusted_profile(
    api: &Api,
    req: RemoveMeasurementTrustedProfileRequest,
) -> Result<RemoveMeasurementTrustedProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let approval_record: MeasurementApprovedProfileRecord = match req.selector {
        // Remove by approval ID.
        Some(remove_measurement_trusted_profile_request::Selector::ApprovalId(approval_uuid)) => {
            let id = approval_uuid.to_string();
            remove_from_approved_profiles_by_approval_id(&mut txn, approval_uuid)
                .await
                .map_err(|error| {
                    measurement_trust_removal_error(
                        error,
                        "measurement trusted profile approval",
                        id,
                    )
                })?
        }
        // Remove by profile ID.
        Some(remove_measurement_trusted_profile_request::Selector::ProfileId(profile_id)) => {
            let id = profile_id.to_string();
            remove_from_approved_profiles_by_profile_id(&mut txn, profile_id)
                .await
                .map_err(|error| {
                    measurement_trust_removal_error(
                        error,
                        "measurement trusted profile approval",
                        id,
                    )
                })?
        }
        // Oops, forgot to set a selector.
        None => {
            return Err(CarbideError::InvalidArgument(
                "approval or profile ID selector missing".into(),
            )
            .into());
        }
    };

    txn.commit().await?;
    Ok(RemoveMeasurementTrustedProfileResponse {
        approval_record: Some(approval_record.into()),
    })
}

/// handle_list_measurement_trusted_profiles handles the
/// ListMeasurementTrustedProfiles API endpoint.
pub(crate) async fn handle_list_measurement_trusted_profiles(
    api: &Api,
    _req: ListMeasurementTrustedProfilesRequest,
) -> Result<ListMeasurementTrustedProfilesResponse, Status> {
    let approval_records: Vec<MeasurementApprovedProfileRecordPb> =
        get_approved_profiles(&api.database_connection)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed to fetch profile approvals: {e}"),
            })?
            .into_iter()
            .map(|record| record.into())
            .collect();

    Ok(ListMeasurementTrustedProfilesResponse { approval_records })
}

pub(crate) async fn handle_list_attestation_summary(
    api: &Api,
    _req: ListAttestationSummaryRequest,
) -> Result<ListAttestationSummaryResponse, Status> {
    let attestation_summary = list_attestation_summary(&api.database_connection)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("failed to fetch attestation summary: {e}"),
        })?;

    Ok(ListAttestationSummaryResponse::from(
        MachineAttestationSummaryList(attestation_summary),
    ))
}
