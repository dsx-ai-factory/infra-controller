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
use ::rpc::forge as rpc;
use carbide_uuid::dhcp_option_set::DhcpOptionSetId;
use config_version::ConfigVersion;
use db::dhcp_option_set;
use model::dhcp_option_set::DhcpOptions;
use model::metadata::Metadata;
use model::tenant::{InvalidTenantOrg, TenantOrganizationId};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_tenant_organization_id};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateDhcpOptionSetRequest>,
) -> Result<Response<rpc::CreateDhcpOptionSetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let id = req.id.unwrap_or_else(DhcpOptionSetId::new);
    let tenant_organization_id = parse_tenant(&req.tenant_organization_id)?;
    let metadata = parse_metadata(req.metadata)?;
    let options = parse_options(req.options)?;

    let mut txn = api.txn_begin().await?;
    let option_set =
        dhcp_option_set::create(&mut txn, &id, &tenant_organization_id, &metadata, &options)
            .await?;
    txn.commit().await?;

    Ok(Response::new(rpc::CreateDhcpOptionSetResponse {
        dhcp_option_set: Some(option_set.into()),
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindDhcpOptionSetsByIdsRequest>,
) -> Result<Response<rpc::FindDhcpOptionSetsByIdsResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be submitted"
        ))
        .into());
    }
    if req.ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let tenant_organization_id = req
        .tenant_organization_id
        .as_deref()
        .map(parse_tenant)
        .transpose()?;

    let mut txn = api.txn_begin().await?;
    let option_sets =
        dhcp_option_set::find_by_ids(&mut txn, &req.ids, tenant_organization_id.as_ref(), false)
            .await?;
    txn.commit().await?;

    Ok(Response::new(rpc::FindDhcpOptionSetsByIdsResponse {
        dhcp_option_sets: option_sets.into_iter().map(Into::into).collect(),
    }))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateDhcpOptionSetRequest>,
) -> Result<Response<rpc::UpdateDhcpOptionSetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let id = req.id.ok_or(CarbideError::MissingArgument("id"))?;
    let tenant_organization_id = parse_tenant(&req.tenant_organization_id)?;
    let metadata = parse_metadata(req.metadata)?;
    let options = parse_options(req.options)?;

    let mut txn = api.txn_begin().await?;
    let current = dhcp_option_set::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        Some(&tenant_organization_id),
        true,
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "DhcpOptionSet",
        id: id.to_string(),
    })?;

    let expected_version = match req.if_version_match {
        Some(version) => version
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?,
        None => current.version,
    };

    let option_set = dhcp_option_set::update(
        &mut txn,
        &id,
        &tenant_organization_id,
        &metadata,
        &options,
        expected_version,
    )
    .await?;
    txn.commit().await?;

    Ok(Response::new(rpc::UpdateDhcpOptionSetResponse {
        dhcp_option_set: Some(option_set.into()),
    }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteDhcpOptionSetRequest>,
) -> Result<Response<rpc::DeleteDhcpOptionSetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    let id = req.id.ok_or(CarbideError::MissingArgument("id"))?;
    let tenant_organization_id = parse_tenant(&req.tenant_organization_id)?;

    let mut txn = api.txn_begin().await?;
    // Lock the set so it cannot be attached between the check and the delete.
    if dhcp_option_set::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        Some(&tenant_organization_id),
        true,
    )
    .await?
    .is_empty()
    {
        return Err(CarbideError::NotFoundError {
            kind: "DhcpOptionSet",
            id: id.to_string(),
        }
        .into());
    }
    if dhcp_option_set::is_in_use(&mut txn, &id).await? {
        return Err(CarbideError::FailedPrecondition(format!(
            "DhcpOptionSet {id} is in use by an instance or VPC; detach it before deleting"
        ))
        .into());
    }
    dhcp_option_set::delete(&mut txn, &id, &tenant_organization_id).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::DeleteDhcpOptionSetResponse {}))
}

fn parse_tenant(tenant_organization_id: &str) -> Result<TenantOrganizationId, CarbideError> {
    log_tenant_organization_id(tenant_organization_id);
    tenant_organization_id
        .parse()
        .map_err(|e: InvalidTenantOrg| {
            RpcDataConversionError::InvalidTenantOrg(e.to_string()).into()
        })
}

fn parse_metadata(metadata: Option<rpc::Metadata>) -> Result<Metadata, CarbideError> {
    let metadata = Metadata::try_from(metadata.ok_or(CarbideError::MissingArgument("metadata"))?)?;
    metadata.validate(true)?;
    Ok(metadata)
}

fn parse_options(options: Option<rpc::DhcpOptions>) -> Result<DhcpOptions, CarbideError> {
    let options = DhcpOptions::try_from(options.ok_or(CarbideError::MissingArgument("options"))?)?;
    options.validate()?;
    Ok(options)
}
