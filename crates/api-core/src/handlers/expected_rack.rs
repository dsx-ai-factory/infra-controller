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

use ::rpc::forge as rpc;
use carbide_uuid::rack::RackId;
use db::expected_rack as db_expected_rack;
use model::expected_rack::ExpectedRack;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

/// add_expected_rack creates an expected rack record. Returns AlreadyExists
/// if the expected rack record already exists.
pub(crate) async fn add_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRack>,
) -> Result<Response<()>, Status> {
    let rack: ExpectedRack = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;

    if api
        .runtime_config
        .rack_profiles
        .get(rack.rack_profile_id.as_str())
        .is_none()
    {
        return Err(CarbideError::InvalidArgument(format!(
            "unknown rack_profile_id: {}. must be one of: {:?}",
            rack.rack_profile_id,
            api.runtime_config.rack_profiles.keys().collect::<Vec<_>>()
        ))
        .into());
    }

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    if db_expected_rack::find_by_rack_id(&mut txn, &rack.rack_id)
        .await
        .map_err(CarbideError::from)?
        .is_some()
    {
        return Err(CarbideError::AlreadyFoundError {
            kind: "expected_rack",
            id: rack.rack_id.to_string(),
        }
        .into());
    }

    if api
        .runtime_config
        .rack_profiles
        .get(rack.rack_profile_id.as_str())
        .is_none()
    {
        return Err(CarbideError::InvalidArgument(format!(
            "unknown rack_profile_id: {}. must be one of: {:?}",
            rack.rack_profile_id,
            api.runtime_config.rack_profiles.keys().collect::<Vec<_>>()
        ))
        .into());
    }
    db_expected_rack::create(&mut txn, &rack)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(()))
}

/// delete_expected_rack deletes an expected rack by its rack_id.
pub(crate) async fn delete_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRackRequest>,
) -> Result<Response<()>, Status> {
    let req = request.into_inner();
    let rack_id = RackId::from_str(&req.rack_id)
        .map_err(|e| CarbideError::InvalidArgument(format!("invalid rack ID: {}", e)))?;
    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    db_expected_rack::delete(&mut txn, &rack_id)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(()))
}

/// update_expected_rack updates an existing expected rack's rack_profile_id and metadata.
pub(crate) async fn update_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRack>,
) -> Result<Response<()>, Status> {
    let rack: ExpectedRack = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;

    if api
        .runtime_config
        .rack_profiles
        .get(rack.rack_profile_id.as_str())
        .is_none()
    {
        return Err(CarbideError::InvalidArgument(format!(
            "unknown rack_profile_id: {}. must be one of: {:?}",
            rack.rack_profile_id,
            api.runtime_config.rack_profiles.keys().collect::<Vec<_>>()
        ))
        .into());
    }

    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    db_expected_rack::find_by_rack_id(&mut txn, &rack.rack_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_rack",
            id: rack.rack_id.to_string(),
        })?;

    db_expected_rack::update(&mut txn, &rack)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(()))
}

/// get_expected_rack returns a specific expected rack by its rack_id.
pub(crate) async fn get_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRackRequest>,
) -> Result<Response<rpc::ExpectedRack>, Status> {
    let req = request.into_inner();
    let rack_id = RackId::from_str(&req.rack_id)
        .map_err(|e| CarbideError::InvalidArgument(format!("invalid rack ID: {}", e)))?;
    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    let expected_rack = db_expected_rack::find_by_rack_id(&mut txn, &rack_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_rack",
            id: rack_id.to_string(),
        })?;
    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(rpc::ExpectedRack::from(expected_rack)))
}

/// get_all_expected_racks returns all expected racks.
pub(crate) async fn get_all_expected_racks(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::ExpectedRackList>, Status> {
    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    let expected_racks = db_expected_rack::find_all(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await.map_err(crate::CarbideError::from)?;
    let expected_racks: Vec<rpc::ExpectedRack> = expected_racks
        .into_iter()
        .map(rpc::ExpectedRack::from)
        .collect();
    Ok(Response::new(rpc::ExpectedRackList { expected_racks }))
}

/// replace_all_expected_racks clears all expected racks and creates new ones from the request.
pub(crate) async fn replace_all_expected_racks(
    api: &Api,
    request: Request<rpc::ExpectedRackList>,
) -> Result<Response<()>, Status> {
    let req = request.into_inner();
    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;

    db_expected_rack::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    for expected_rack in req.expected_racks {
        let rack: ExpectedRack = expected_rack.try_into().map_err(CarbideError::from)?;

        if api
            .runtime_config
            .rack_profiles
            .get(rack.rack_profile_id.as_str())
            .is_none()
        {
            return Err(CarbideError::InvalidArgument(format!(
                "unknown rack_profile_id: {}",
                rack.rack_profile_id
            ))
            .into());
        }

        db_expected_rack::create(&mut txn, &rack)
            .await
            .map_err(CarbideError::from)?;
    }

    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(()))
}

/// delete_all_expected_racks deletes all expected racks.
pub(crate) async fn delete_all_expected_racks(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<()>, Status> {
    let mut txn = api.txn_begin().await.map_err(crate::CarbideError::from)?;
    db_expected_rack::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await.map_err(crate::CarbideError::from)?;
    Ok(Response::new(()))
}
