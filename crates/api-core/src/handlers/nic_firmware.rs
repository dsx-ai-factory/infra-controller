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
use config_version::ConfigVersion;
use model::nic_firmware::{NicFirmwareProfileConfig, NicFirmwareProfileId};
use rpc::forge as rpc;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_request_data_redacted};

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
    log_request_data(&request);
    let ids = db::nic_firmware::find_ids(api.pg_pool())
        .await
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
