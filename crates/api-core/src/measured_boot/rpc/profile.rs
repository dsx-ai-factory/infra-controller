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
 * gRPC handlers for measurement profile related API calls.
 */

use std::collections::HashMap;

use carbide_uuid::measured_boot::MeasurementSystemProfileId;
use db::measured_boot::interface::profile::{
    export_measurement_profile_records, get_bundles_for_profile_id, get_bundles_for_profile_name,
    get_machines_for_profile_id, get_machines_for_profile_name,
};
use measured_boot::profile::MeasurementSystemProfile;
use rpc::protos::measured_boot::{
    CreateMeasurementSystemProfileRequest, CreateMeasurementSystemProfileResponse,
    DeleteMeasurementSystemProfileRequest, DeleteMeasurementSystemProfileResponse,
    ListMeasurementSystemProfileBundlesRequest, ListMeasurementSystemProfileBundlesResponse,
    ListMeasurementSystemProfileMachinesRequest, ListMeasurementSystemProfileMachinesResponse,
    ListMeasurementSystemProfilesRequest, ListMeasurementSystemProfilesResponse,
    MeasurementSystemProfileRecordPb, RenameMeasurementSystemProfileRequest,
    RenameMeasurementSystemProfileResponse, ShowMeasurementSystemProfileRequest,
    ShowMeasurementSystemProfileResponse, ShowMeasurementSystemProfilesRequest,
    ShowMeasurementSystemProfilesResponse, delete_measurement_system_profile_request,
    list_measurement_system_profile_bundles_request,
    list_measurement_system_profile_machines_request, rename_measurement_system_profile_request,
    show_measurement_system_profile_request,
};
use sqlx::PgConnection;
use tonic::Status;

use crate::CarbideError;
use crate::api::Api;

/// handle_create_system_measurement_profile handles the
/// CreateMeasurementSystemProfile API endpoint.
pub(crate) async fn handle_create_system_measurement_profile(
    api: &Api,
    req: CreateMeasurementSystemProfileRequest,
) -> Result<CreateMeasurementSystemProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    // sys_vendor and product_name are the two baseline attrs, so
    // just treat them as requirements, and then smash the
    // remaining ones on as "extra-attrs".
    let mut vals = HashMap::from([
        (String::from("sys_vendor"), req.vendor),
        (String::from("product_name"), req.product),
    ]);
    for kv_pair in req.extra_attrs.into_iter() {
        vals.insert(kv_pair.key, kv_pair.value);
    }

    let system_profile = db::measured_boot::profile::new(&mut txn, req.name, &vals)
        .await
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;

    txn.commit().await?;
    Ok(CreateMeasurementSystemProfileResponse {
        system_profile: Some(system_profile.into()),
    })
}

/// handle_rename_measurement_system_profile handles the
/// RenameMeasurementSystemProfile API endpoint.
pub(crate) async fn handle_rename_measurement_system_profile(
    api: &Api,
    req: RenameMeasurementSystemProfileRequest,
) -> Result<RenameMeasurementSystemProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let profile = match req.selector {
        // Rename for the given system_profile ID.
        Some(rename_measurement_system_profile_request::Selector::ProfileId(
            system_profile_uuid,
        )) => db::measured_boot::profile::rename_for_id(
            &mut txn,
            system_profile_uuid,
            req.new_profile_name,
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("rename failed: {e}"),
        })?,

        // Rename for the given system_profile name.
        Some(rename_measurement_system_profile_request::Selector::ProfileName(
            system_profile_name,
        )) => db::measured_boot::profile::rename_for_name(
            &mut txn,
            system_profile_name,
            req.new_profile_name,
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("rename failed: {e}"),
        })?,

        // ID or name is needed.
        None => {
            return Err(
                CarbideError::InvalidArgument("rename selector is required".to_string()).into(),
            );
        }
    };

    txn.commit().await?;
    Ok(RenameMeasurementSystemProfileResponse {
        profile: Some(profile.into()),
    })
}

/// handle_delete_measurement_system_profile handles the
/// DeleteMeasurementSystemProfile API endpoint.
pub(crate) async fn handle_delete_measurement_system_profile(
    api: &Api,
    req: DeleteMeasurementSystemProfileRequest,
) -> Result<DeleteMeasurementSystemProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let profile: Option<MeasurementSystemProfile> = match req.selector {
        // Deleting a profile based on profile ID.
        Some(delete_measurement_system_profile_request::Selector::ProfileId(profile_uuid)) => {
            delete_for_uuid(&mut txn, profile_uuid).await?
        }
        // Deleting a profile based on profile name.
        Some(delete_measurement_system_profile_request::Selector::ProfileName(profile_name)) => {
            delete_for_name(&mut txn, profile_name).await?
        }
        // Trying to delete a profile without a selector.
        None => {
            return Err(
                CarbideError::InvalidArgument("profile selector is required".to_string()).into(),
            );
        }
    };

    let system_profile = profile.ok_or(CarbideError::NotFoundError {
        kind: "profile",
        id: "provided selector".to_string(),
    })?;

    txn.commit().await?;
    Ok(DeleteMeasurementSystemProfileResponse {
        system_profile: Some(system_profile.into()),
    })
}

/// handle_show_measurement_system_profile handles the
/// ShowMeasurementSystemProfile API endpoint.
pub(crate) async fn handle_show_measurement_system_profile(
    api: &Api,
    req: ShowMeasurementSystemProfileRequest,
) -> Result<ShowMeasurementSystemProfileResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let system_profile = match req.selector {
        // Show a system profile with the given profile ID.
        Some(show_measurement_system_profile_request::Selector::ProfileId(profile_uuid)) => {
            db::measured_boot::profile::load_from_id(&mut txn, profile_uuid)
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("{e}"),
                })?
        }
        // Show a system profile with the given profile name.
        Some(show_measurement_system_profile_request::Selector::ProfileName(profile_name)) => {
            db::measured_boot::profile::load_from_name(&mut txn, profile_name)
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("{e}"),
                })?
        }
        // Show all system profiles.
        None => return Err(CarbideError::InvalidArgument("selector required".to_string()).into()),
    };
    txn.commit().await?;

    Ok(ShowMeasurementSystemProfileResponse {
        system_profile: Some(system_profile.into()),
    })
}

/// handle_show_measurement_system_profiles handles the
/// ShowMeasurementSystemProfiles API endpoint.
pub(crate) async fn handle_show_measurement_system_profiles(
    api: &Api,
    _req: ShowMeasurementSystemProfilesRequest,
) -> Result<ShowMeasurementSystemProfilesResponse, Status> {
    Ok(ShowMeasurementSystemProfilesResponse {
        system_profiles: db::measured_boot::profile::get_all(&mut api.db_reader())
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .into_iter()
            .map(|profile| profile.into())
            .collect(),
    })
}

/// handle_list_measurement_system_profiles handles the
/// ListMeasurementSystemProfiles API endpoint.
pub(crate) async fn handle_list_measurement_system_profiles(
    api: &Api,
    _req: ListMeasurementSystemProfilesRequest,
) -> Result<ListMeasurementSystemProfilesResponse, Status> {
    let system_profiles: Vec<MeasurementSystemProfileRecordPb> =
        export_measurement_profile_records(&api.database_connection)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .into_iter()
            .map(|record| record.into())
            .collect();

    Ok(ListMeasurementSystemProfilesResponse { system_profiles })
}

/// handle_list_measurement_system_profile_bundles handles the
/// ListMeasurementSystemProfileBundles API endpoint.
pub(crate) async fn handle_list_measurement_system_profile_bundles(
    api: &Api,
    req: ListMeasurementSystemProfileBundlesRequest,
) -> Result<ListMeasurementSystemProfileBundlesResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let bundle_ids = match req.selector {
        // ...and do it by profile ID.
        Some(list_measurement_system_profile_bundles_request::Selector::ProfileId(
            profile_uuid,
        )) => get_bundles_for_profile_id(&mut txn, profile_uuid)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?,

        // ...or do it by profile name.
        Some(list_measurement_system_profile_bundles_request::Selector::ProfileName(
            profile_name,
        )) => get_bundles_for_profile_name(&mut txn, profile_name)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?,

        // ... either a UUID or name is required.
        None => return Err(CarbideError::InvalidArgument("selector required".to_string()).into()),
    };

    txn.commit().await?;

    Ok(ListMeasurementSystemProfileBundlesResponse { bundle_ids })
}

/// handle_list_measurement_system_profile_machines handles the
/// ListMeasurementSystemProfileMachines API endpoint.
pub(crate) async fn handle_list_measurement_system_profile_machines(
    api: &Api,
    req: ListMeasurementSystemProfileMachinesRequest,
) -> Result<ListMeasurementSystemProfileMachinesResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let machine_ids: Vec<String> = match req.selector {
        // ...and do it by profile ID.
        Some(list_measurement_system_profile_machines_request::Selector::ProfileId(profile_id)) => {
            get_machines_for_profile_id(&mut txn, profile_id)
                .await
                .map_err(|e| CarbideError::Internal {
                    message: format!("{e}"),
                })?
                .drain(..)
                .map(|machine_id| machine_id.to_string())
                .collect()
        }
        // ...or do it by profile name.
        Some(list_measurement_system_profile_machines_request::Selector::ProfileName(
            profile_name,
        )) => get_machines_for_profile_name(&mut txn, profile_name)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .drain(..)
            .map(|machine_id| machine_id.to_string())
            .collect(),
        // ...and it has to be either by ID or name.
        None => return Err(CarbideError::InvalidArgument("selector required".to_string()).into()),
    };
    txn.commit().await?;

    Ok(ListMeasurementSystemProfileMachinesResponse { machine_ids })
}

/// delete_for_uuid specifically handles deleting
/// a system profile by ID.
async fn delete_for_uuid(
    txn: &mut PgConnection,
    profile_id: MeasurementSystemProfileId,
) -> Result<Option<MeasurementSystemProfile>, Status> {
    match db::measured_boot::profile::delete_for_id(txn, profile_id).await {
        Ok(optional_profile) => Ok(optional_profile),
        Err(e) => Err(CarbideError::Internal {
            message: format!("error deleting profile: {e}"),
        }
        .into()),
    }
}

/// delete_for_name specifically handles deleting
/// a system profile by name.
async fn delete_for_name(
    txn: &mut PgConnection,
    profile_name: String,
) -> Result<Option<MeasurementSystemProfile>, Status> {
    match db::measured_boot::profile::delete_for_name(txn, profile_name).await {
        Ok(optional_profile) => Ok(optional_profile),
        Err(e) => Err(CarbideError::Internal {
            message: format!("error deleting profile: {e}"),
        }
        .into()),
    }
}
