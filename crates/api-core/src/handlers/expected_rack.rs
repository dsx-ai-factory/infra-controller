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

async fn derive_expected_rack(
    api: &Api,
    txn: &mut sqlx::PgConnection,
    mut request: rpc::ExpectedRack,
) -> Result<ExpectedRack, CarbideError> {
    let rack_id = request
        .rack_id
        .as_ref()
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".into()))?;
    let groups = db::expected_rack_group::find_by_rack_id(txn, rack_id).await?;
    let group = match groups.as_slice() {
        [group] => group,
        [] => {
            return Err(CarbideError::InvalidArgument(format!(
                "no expected rack group contains rack {rack_id}"
            )));
        }
        _ => {
            return Err(CarbideError::InvalidArgument(format!(
                "multiple expected rack groups contain rack {rack_id}"
            )));
        }
    };
    let profile_id = model::expected_rack::derive_rack_profile_id(group, rack_id)
        .map_err(CarbideError::InvalidArgument)?;
    if api
        .runtime_config
        .rack_profiles
        .get(profile_id.as_str())
        .is_none()
    {
        return Err(CarbideError::InvalidArgument(format!(
            "derived rack profile is not configured: {profile_id}"
        )));
    }
    request.rack_profile_id = Some(profile_id);
    request.try_into().map_err(CarbideError::from)
}

/// add_expected_rack creates an expected rack record. Returns AlreadyExists
/// if the expected rack record already exists.
pub(crate) async fn add_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRack>,
) -> Result<Response<()>, Status> {
    let mut txn = api.txn_begin().await?;
    let request = request.into_inner();
    let rack_id = request
        .rack_id
        .as_ref()
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".into()))?;

    if db_expected_rack::find_by_rack_id(&mut txn, rack_id)
        .await
        .map_err(CarbideError::from)?
        .is_some()
    {
        return Err(CarbideError::AlreadyFoundError {
            kind: "expected_rack",
            id: rack_id.to_string(),
        }
        .into());
    }

    let rack = derive_expected_rack(api, &mut txn, request).await?;

    db_expected_rack::create(&mut txn, &rack)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;
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
    let mut txn = api.txn_begin().await?;
    db_expected_rack::delete(&mut txn, &rack_id)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}

/// Updates metadata while preserving the profile selected when the rack was created.
pub(crate) async fn update_expected_rack(
    api: &Api,
    request: Request<rpc::ExpectedRack>,
) -> Result<Response<()>, Status> {
    let mut request = request.into_inner();
    let rack_id = request
        .rack_id
        .as_ref()
        .ok_or_else(|| CarbideError::InvalidArgument("rack_id is required".into()))?;
    let mut txn = api.txn_begin().await?;
    let existing = db_expected_rack::find_by_rack_id(&mut txn, rack_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_rack",
            id: rack_id.to_string(),
        })?;
    request.rack_profile_id = Some(existing.rack_profile_id);
    let rack: ExpectedRack = request.try_into().map_err(CarbideError::from)?;

    db_expected_rack::update(&mut txn, &rack)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;
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
    let mut txn = api.txn_begin().await?;
    let expected_rack = db_expected_rack::find_by_rack_id(&mut txn, &rack_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_rack",
            id: rack_id.to_string(),
        })?;
    txn.commit().await?;
    Ok(Response::new(rpc::ExpectedRack::from(expected_rack)))
}

/// get_all_expected_racks returns all expected racks.
pub(crate) async fn get_all_expected_racks(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::ExpectedRackList>, Status> {
    let mut txn = api.txn_begin().await?;
    let expected_racks = db_expected_rack::find_all(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
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
    let mut txn = api.txn_begin().await?;

    db_expected_rack::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;

    for expected_rack in req.expected_racks {
        let rack = derive_expected_rack(api, &mut txn, expected_rack).await?;

        db_expected_rack::create(&mut txn, &rack)
            .await
            .map_err(CarbideError::from)?;
    }

    txn.commit().await?;
    Ok(Response::new(()))
}

/// delete_all_expected_racks deletes all expected racks.
pub(crate) async fn delete_all_expected_racks(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<()>, Status> {
    let mut txn = api.txn_begin().await?;
    db_expected_rack::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}
