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
use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use model::nic_firmware::{
    NicFirmwareBaseline, NicFirmwareHardware, NicFirmwareProfileConfig, NicFirmwareProfileId,
    resolve_nic_firmware,
};
use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data_redacted};
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateNicFirmwareProfileRequest>,
) -> Result<Response<rpc::NicFirmwareProfileResponse>, Status> {
    let request = request.into_inner();
    // Exclude artifact sources, including rejected URLs, from request logs.
    log_request_data_redacted(format!(
        "id: {}, config_present: {}, entries: {}",
        request.id.escape_default(),
        request.config.is_some(),
        request
            .config
            .as_ref()
            .map_or(0, |config| config.entries.len()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let config = required_config(request.config)?;
    let mut txn = api.txn_begin().await?;
    let profile = db::nic_firmware::create(&mut txn, &id, &config)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, config_version = %profile.version, "Created NIC firmware profile");
    Ok(Response::new(rpc::NicFirmwareProfileResponse {
        profile: Some(profile.into()),
    }))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateNicFirmwareProfileRequest>,
) -> Result<Response<rpc::NicFirmwareProfileResponse>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "id: {}, if_version_match: {:?}, config_present: {}, entries: {}",
        request.id.escape_default(),
        request
            .if_version_match
            .as_deref()
            .map(|version| version.escape_default().to_string()),
        request.config.is_some(),
        request
            .config
            .as_ref()
            .map_or(0, |config| config.entries.len()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let config = required_config(request.config)?;
    let expected_version = required_version(request.if_version_match)?;
    let mut txn = api.txn_begin().await?;
    let profile = db::nic_firmware::update(&mut txn, &id, &config, expected_version)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, config_version = %profile.version, "Updated NIC firmware profile");
    Ok(Response::new(rpc::NicFirmwareProfileResponse {
        profile: Some(profile.into()),
    }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteNicFirmwareProfileRequest>,
) -> Result<Response<()>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "id: {}, if_version_match: {:?}",
        request.id.escape_default(),
        request
            .if_version_match
            .as_deref()
            .map(|version| version.escape_default().to_string()),
    ));
    let id: NicFirmwareProfileId = request.id.parse().map_err(CarbideError::from)?;
    let expected_version = required_version(request.if_version_match)?;
    let mut txn = api.txn_begin().await?;
    db::nic_firmware::delete(&mut txn, &id, expected_version)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    tracing::info!(nic_firmware_profile_id = %id, "Deleted NIC firmware profile");
    Ok(Response::new(()))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareProfileIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareProfileIdsResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let hardware = request
        .into_inner()
        .hardware
        .map(NicFirmwareHardware::try_from)
        .transpose()
        .map_err(CarbideError::from)?;
    let ids = match hardware {
        Some(hardware) => db::nic_firmware::find_compatible_ids(api.pg_pool(), &hardware).await,
        None => db::nic_firmware::find_ids(api.pg_pool()).await,
    }
    .map_err(CarbideError::from)?;
    Ok(Response::new(rpc::FindNicFirmwareProfileIdsResponse {
        profile_ids: ids.into_iter().map(|id| id.to_string()).collect(),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareProfilesByIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareProfilesByIdsResponse>, Status> {
    let ids = request.into_inner().profile_ids;
    log_request_data_redacted(format!("profile_ids count: {}", ids.len()));
    let limit = api.runtime_config.max_find_by_ids as usize;
    if ids.is_empty() || ids.len() > limit {
        return Err(CarbideError::InvalidArgument(format!(
            "between 1 and {limit} profile IDs must be provided"
        ))
        .into());
    }
    let ids = ids
        .iter()
        .map(|id| id.parse())
        .collect::<Result<Vec<NicFirmwareProfileId>, _>>()
        .map_err(CarbideError::from)?;
    let profiles = db::nic_firmware::find_by_ids(api.pg_pool(), &ids)
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(rpc::FindNicFirmwareProfilesByIdsResponse {
        profiles: profiles.into_iter().map(Into::into).collect(),
    }))
}

/// `set_site_default` creates, replaces or explicitly clears one assignment.
/// It never schedules firmware or changes a machine's state.
pub(crate) async fn set_site_default(
    api: &Api,
    request: Request<rpc::SetNicFirmwareSiteDefaultRequest>,
) -> Result<Response<rpc::NicFirmwareSiteDefaultResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let request = request.into_inner();
    let hardware = NicFirmwareHardware::try_from(
        request
            .hardware
            .ok_or(CarbideError::MissingArgument("hardware"))?,
    )
    .map_err(CarbideError::from)?;
    let version = request
        .if_version_match
        .map(|version| required_version(Some(version)))
        .transpose()?;
    let profile_id: Option<NicFirmwareProfileId> = match request
        .action
        .ok_or(CarbideError::MissingArgument("action"))?
    {
        rpc::set_nic_firmware_site_default_request::Action::ProfileId(id) => {
            Some(id.parse().map_err(CarbideError::from)?)
        }
        rpc::set_nic_firmware_site_default_request::Action::Clear(()) => None,
    };
    let mut txn = api.txn_begin().await.map_err(CarbideError::from)?;
    let binding = match (profile_id, version) {
        (Some(id), version) => Some(
            db::nic_firmware::set_site_default(&mut txn, &hardware, &id, version)
                .await
                .map_err(CarbideError::from)?,
        ),
        (None, Some(version)) => {
            db::nic_firmware::clear_site_default(&mut txn, &hardware, version)
                .await
                .map_err(CarbideError::from)?;
            None
        }
        (None, None) => return Err(CarbideError::MissingArgument("if_version_match").into()),
    };
    txn.commit().await.map_err(CarbideError::from)?;
    Ok(Response::new(rpc::NicFirmwareSiteDefaultResponse {
        site_default: binding.map(Into::into),
    }))
}

/// `find_site_default_ids` lists hardware keys, optionally selected by profile.
pub(crate) async fn find_site_default_ids(
    api: &Api,
    request: Request<rpc::NicFirmwareSiteDefaultSearchFilter>,
) -> Result<Response<rpc::FindNicFirmwareSiteDefaultIdsResponse>, Status> {
    let request = request.into_inner();
    log_request_data_redacted(format!(
        "profile_id: {:?}",
        request
            .profile_id
            .as_deref()
            .map(|id| id.escape_default().to_string()),
    ));
    let profile_id: Option<NicFirmwareProfileId> = request
        .profile_id
        .map(|id| id.parse())
        .transpose()
        .map_err(CarbideError::from)?;
    let hardware = db::nic_firmware::find_site_default_ids(api.pg_pool(), profile_id.as_ref())
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(rpc::FindNicFirmwareSiteDefaultIdsResponse {
        hardware: hardware.into_iter().map(Into::into).collect(),
    }))
}

/// `find_site_defaults_by_ids` reads a bounded batch of hardware assignments.
pub(crate) async fn find_site_defaults_by_ids(
    api: &Api,
    request: Request<rpc::FindNicFirmwareSiteDefaultsByIdsRequest>,
) -> Result<Response<rpc::FindNicFirmwareSiteDefaultsByIdsResponse>, Status> {
    let hardware = request.into_inner().hardware;
    log_request_data_redacted(format!("hardware count: {}", hardware.len()));
    let limit = api.runtime_config.max_find_by_ids as usize;
    if hardware.is_empty() || hardware.len() > limit {
        return Err(CarbideError::InvalidArgument(format!(
            "between 1 and {limit} hardware pairs must be provided"
        ))
        .into());
    }
    let hardware = hardware
        .into_iter()
        .map(NicFirmwareHardware::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(CarbideError::from)?;
    let bindings = db::nic_firmware::find_site_defaults_by_ids(api.pg_pool(), &hardware)
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(
        rpc::FindNicFirmwareSiteDefaultsByIdsResponse {
            site_defaults: bindings.into_iter().map(Into::into).collect(),
        },
    ))
}

/// `plan` resolves stored observations against a consistent catalog snapshot.
/// Per-device errors are returned in the report without failing the request
/// or hiding results for other devices.
pub(crate) async fn plan(
    api: &Api,
    request: Request<rpc::GetNicFirmwarePlanRequest>,
) -> Result<Response<rpc::GetNicFirmwarePlanResponse>, Status> {
    log_request_data_redacted(
        format!("{:?}", request.get_ref())
            .escape_default()
            .to_string(),
    );
    let request = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(request.machine_id.as_ref())?;
    let allocation: Option<NicFirmwareProfileId> = request
        .allocation_profile_id
        .map(|id| id.parse())
        .transpose()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await.map_err(CarbideError::from)?;
    // A plan must not combine an old assignment with a newly edited profile.
    let query = "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY";
    sqlx::query(query)
        .execute(&mut **txn)
        .await
        .map_err(|error| CarbideError::from(db::DatabaseError::query(query, error)))?;
    let machine = db::machine::find_one(&mut txn, &machine_id, Default::default())
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;
    let bindings = db::nic_firmware::find_site_defaults(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    let profile_ids: Vec<_> = bindings
        .iter()
        .map(|binding| binding.profile_id.clone())
        .chain(allocation.iter().cloned())
        .collect();
    let profiles = db::nic_firmware::find_by_ids(&mut txn, &profile_ids)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await.map_err(CarbideError::from)?;
    let mut response = rpc::GetNicFirmwarePlanResponse::default();
    if let Some(observation) = machine.status.mlx_device_observation {
        response.observed_at = Some(observation.observed_at.into());
        for device in observation.devices {
            let managed = super::mlx_device_identity::managed_dpu_machine_ids(
                device.base_mac,
                &machine.status.interfaces,
            );
            let mut target = rpc::NicFirmwareTargetPlan {
                identity: Some(::rpc::protos::mlx_device::MlxDeviceIdentity {
                    device_info: Some(device.clone().into()),
                    managed_dpu_machine_ids: managed.iter().copied().map(Into::into).collect(),
                }),
                ..Default::default()
            };
            let resolution = (|| {
                if !managed.is_empty() {
                    return Err(
                        "managed-DPU association excludes this device from NIC firmware planning"
                            .to_string(),
                    );
                }
                let hardware = NicFirmwareHardware {
                    part_number: device
                        .part_number
                        .ok_or("device report is missing a part number")?,
                    psid: device.psid.ok_or("device report is missing a PSID")?,
                }
                .validate()
                .map_err(|error| error.to_string())?;
                let binding = bindings.iter().find(|binding| binding.hardware == hardware);
                target.site_default_version = binding.map(|binding| binding.version.to_string());
                resolve_nic_firmware(
                    &hardware,
                    &profiles,
                    NicFirmwareBaseline {
                        site_default: binding.map(|binding| &binding.profile_id),
                        ..Default::default()
                    },
                    allocation.as_ref(),
                )
                .map_err(|error| error.to_string())
            })();
            target.result = match resolution {
                Ok(resolution) => resolution.map(|firmware| {
                    rpc::nic_firmware_target_plan::Result::Firmware(firmware.into())
                }),
                Err(error) => Some(rpc::nic_firmware_target_plan::Result::Error(error)),
            };
            response.targets.push(target);
        }
    }
    Ok(Response::new(response))
}

fn required_version(version: Option<String>) -> Result<ConfigVersion, CarbideError> {
    let version = version.ok_or(CarbideError::MissingArgument("if_version_match"))?;
    version
        .parse()
        .map_err(|_| RpcDataConversionError::InvalidConfigVersion(version).into())
}

fn required_config(
    config: Option<rpc::NicFirmwareProfileConfig>,
) -> Result<NicFirmwareProfileConfig, CarbideError> {
    config
        .ok_or(CarbideError::MissingArgument("config"))?
        .try_into()
        .map_err(CarbideError::from)
}
