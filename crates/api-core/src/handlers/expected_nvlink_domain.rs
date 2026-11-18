/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use db::expected_nvlink_domain as domains;
use model::expected_nvlink_domain::ExpectedNvLinkDomain;
use rpc::forge;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data};

fn declaration_id(value: Option<rpc::common::Uuid>) -> Result<Uuid, CarbideError> {
    let value = value.ok_or(CarbideError::MissingArgument("id"))?;
    let id = Uuid::parse_str(&value.value)
        .map_err(|_| CarbideError::InvalidArgument("id must be a UUID".into()))?;
    if id.is_nil() {
        return Err(CarbideError::InvalidArgument("id must not be nil".into()));
    }
    Ok(id)
}

pub(crate) async fn create(
    api: &Api,
    request: Request<forge::CreateExpectedNvLinkDomainRequest>,
) -> Result<Response<forge::ExpectedNvLinkDomain>, Status> {
    log_request_data(&request);
    let domain: ExpectedNvLinkDomain = request
        .into_inner()
        .try_into()
        .map_err(CarbideError::from)?;
    let mut txn = api.txn_begin().await?;
    domains::create(&mut txn, &domain)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(domain.into()))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<forge::ExpectedNvLinkDomainRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let id = declaration_id(request.into_inner().id)?;
    let mut txn = api.txn_begin().await?;
    domains::delete(&mut txn, id)
        .await
        .map_err(CarbideError::from)?;
    txn.commit().await?;
    Ok(Response::new(()))
}

pub(crate) async fn get(
    api: &Api,
    request: Request<forge::ExpectedNvLinkDomainRequest>,
) -> Result<Response<forge::ExpectedNvLinkDomain>, Status> {
    log_request_data(&request);
    let id = declaration_id(request.into_inner().id)?;
    let domain = domains::find_by_ids(api.db_reader().as_mut(), &[id])
        .await
        .map_err(CarbideError::from)?
        .pop()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "expected_nvlink_domain",
            id: id.to_string(),
        })?;
    Ok(Response::new(domain.into()))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<forge::ExpectedNvLinkDomainSearchFilter>,
) -> Result<Response<forge::ExpectedNvLinkDomainIdList>, Status> {
    log_request_data(&request);
    let group = request.into_inner().rack_group_id;
    if let Some(id) = &group {
        rpc::model::expected_rack_group::validate_rack_group_id(id).map_err(CarbideError::from)?;
    }
    let ids = domains::find_ids(api.db_reader().as_mut(), group.as_ref())
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(forge::ExpectedNvLinkDomainIdList {
        ids: ids.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<forge::ExpectedNvLinkDomainsByIdsRequest>,
) -> Result<Response<forge::ExpectedNvLinkDomainList>, Status> {
    log_request_data(&request);
    let ids = request.into_inner().ids;
    let limit = api.runtime_config.max_find_by_ids as usize;
    if ids.is_empty() || ids.len() > limit {
        return Err(
            CarbideError::InvalidArgument(format!("provide between 1 and {limit} IDs")).into(),
        );
    }
    let ids = ids
        .into_iter()
        .map(|id| declaration_id(Some(id)))
        .collect::<Result<Vec<_>, _>>()?;
    let domains = domains::find_by_ids(api.db_reader().as_mut(), &ids)
        .await
        .map_err(CarbideError::from)?;
    Ok(Response::new(forge::ExpectedNvLinkDomainList {
        domains: domains.into_iter().map(Into::into).collect(),
    }))
}
