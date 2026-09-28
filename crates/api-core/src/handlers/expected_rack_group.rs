/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::HashSet;

use ::rpc::forge as rpc;
use ::rpc::model::expected_rack_group::validate_rack_group_id;
use carbide_uuid::rack::RackGroupId;
use db::expected_rack_group as db_expected_rack_group;
use model::expected_rack_group::ExpectedRackGroup;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

pub(crate) async fn add_expected_rack_group(
    api: &Api,
    request: Request<rpc::ExpectedRackGroup>,
) -> Result<Response<()>, Status> {
    let group: ExpectedRackGroup = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    db_expected_rack_group::create(&mut txn, &group)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn update_expected_rack_group(
    api: &Api,
    request: Request<rpc::ExpectedRackGroup>,
) -> Result<Response<()>, Status> {
    let group: ExpectedRackGroup = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    db_expected_rack_group::update(&mut txn, &group)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn delete_expected_rack_group(
    api: &Api,
    request: Request<rpc::ExpectedRackGroupRequest>,
) -> Result<Response<()>, Status> {
    let rack_group_id = RackGroupId::new(request.into_inner().rack_group_id);
    validate_rack_group_id(&rack_group_id).map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    db_expected_rack_group::delete(&mut txn, &rack_group_id)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn get_expected_rack_group(
    api: &Api,
    request: Request<rpc::ExpectedRackGroupRequest>,
) -> Result<Response<rpc::ExpectedRackGroup>, Status> {
    let rack_group_id = RackGroupId::new(request.into_inner().rack_group_id);
    validate_rack_group_id(&rack_group_id).map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    let group = db_expected_rack_group::find_by_rack_group_id(&mut txn, &rack_group_id)
        .await
        .map_err(CarbideError::from)?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_rack_group",
            id: rack_group_id.to_string(),
        })?;
    txn.commit().await?;
    Ok(Response::new(group.into()))
}

pub(crate) async fn get_all_expected_rack_groups(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<rpc::ExpectedRackGroupList>, Status> {
    let mut txn = api.txn_begin().await?;
    let groups = db_expected_rack_group::find_all(&mut txn).await?;
    txn.rollback_or_log("read-only load of all expected rack groups")
        .await;
    Ok(Response::new(rpc::ExpectedRackGroupList {
        expected_rack_groups: groups.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn find_ids(
    api: &Api,
    _request: Request<rpc::ExpectedRackGroupSearchFilter>,
) -> Result<Response<rpc::ExpectedRackGroupIdList>, Status> {
    let mut txn = api.txn_begin().await?;
    let rack_group_ids = db_expected_rack_group::find_ids(&mut txn).await?;
    txn.rollback_or_log("read-only expected rack group IDs")
        .await;
    Ok(Response::new(rpc::ExpectedRackGroupIdList {
        rack_group_ids,
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::ExpectedRackGroupsByIdsRequest>,
) -> Result<Response<rpc::ExpectedRackGroupList>, Status> {
    let ids = request.into_inner().rack_group_ids;
    let limit = api.runtime_config.max_find_by_ids as usize;
    if ids.is_empty() || ids.len() > limit {
        return Err(Status::invalid_argument(format!(
            "provide between 1 and {limit} IDs"
        )));
    }
    for id in &ids {
        validate_rack_group_id(id).map_err(CarbideError::from)?;
    }
    let mut txn = api.txn_begin().await?;
    let groups = db_expected_rack_group::find_by_ids(&mut txn, &ids)
        .await
        .map_err(CarbideError::from)?;
    txn.rollback_or_log("read-only load of expected rack groups by id")
        .await;
    Ok(Response::new(rpc::ExpectedRackGroupList {
        expected_rack_groups: groups.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn replace_all_expected_rack_groups(
    api: &Api,
    request: Request<rpc::ExpectedRackGroupList>,
) -> Result<Response<()>, Status> {
    let groups: Vec<ExpectedRackGroup> = request
        .into_inner()
        .expected_rack_groups
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()
        .map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;
    let mut group_ids = HashSet::with_capacity(groups.len());
    for group in &groups {
        if !group_ids.insert(&group.rack_group_id) {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate rack_group_id: {}",
                group.rack_group_id
            ))
            .into());
        }
    }

    db_expected_rack_group::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    for group in &groups {
        db_expected_rack_group::create(&mut txn, group)
            .await
            .map_err(CarbideError::from)?;
    }
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn delete_all_expected_rack_groups(
    api: &Api,
    _request: Request<()>,
) -> Result<Response<()>, Status> {
    let mut txn = api.txn_begin().await?;
    db_expected_rack_group::clear(&mut txn)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}
