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
use ::rpc::forge as rpc;
use config_version::ConfigVersion;
use db::{
    self, ConditionalWrite, ObjectColumnFilter, WithTransaction, instance, nvl_logical_partition,
};
use futures_util::FutureExt;
use model::nvl_logical_partition::NewLogicalPartition;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_tenant_organization_id};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::NvLinkLogicalPartitionCreationRequest>,
) -> Result<Response<rpc::NvLinkLogicalPartition>, Status> {
    log_request_data(&request);

    let request_inner = request.into_inner();

    // Log tenant organization ID if present in the config
    if let Some(ref config) = request_inner.config {
        log_tenant_organization_id(&config.tenant_organization_id);
    }

    let mut txn = api.txn_begin().await?;

    let req = NewLogicalPartition::try_from(request_inner)?;

    let metadata = req.config.metadata.clone();
    metadata.validate(true).map_err(CarbideError::from)?;

    let resp = nvl_logical_partition::create(&req, &mut txn)
        .await
        .map_err(CarbideError::from)?;
    let resp = rpc::NvLinkLogicalPartition::try_from(resp).map(Response::new)?;
    txn.commit().await?;

    Ok(resp)
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::NvLinkLogicalPartitionSearchFilter>,
) -> Result<Response<rpc::NvLinkLogicalPartitionIdList>, Status> {
    log_request_data(&request);

    let filter: model::nvl_logical_partition::NvLinkLogicalPartitionSearchFilter =
        request.into_inner().into();

    let partition_ids =
        db::nvl_logical_partition::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::NvLinkLogicalPartitionIdList {
        partition_ids,
    }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::NvLinkLogicalPartitionsByIdsRequest>,
) -> Result<Response<rpc::NvLinkLogicalPartitionList>, Status> {
    log_request_data(&request);

    let rpc::NvLinkLogicalPartitionsByIdsRequest { partition_ids, .. } = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if partition_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if partition_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let partitions = db::nvl_logical_partition::find_by(
        &api.database_connection,
        ObjectColumnFilter::List(nvl_logical_partition::IdColumn, &partition_ids),
    )
    .await
    .map_err(CarbideError::from)?;

    let mut result = Vec::with_capacity(partitions.len());
    for lp in partitions {
        result.push(lp.try_into()?);
    }

    Ok(Response::new(rpc::NvLinkLogicalPartitionList {
        partitions: result,
    }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::NvLinkLogicalPartitionDeletionRequest>,
) -> Result<Response<rpc::NvLinkLogicalPartitionDeletionResult>, Status> {
    log_request_data(&request);

    let id = request
        .into_inner()
        .id
        .ok_or_else(|| CarbideError::MissingArgument("id"))?;

    let mut partitions = db::nvl_logical_partition::find_by(
        &api.database_connection,
        ObjectColumnFilter::One(nvl_logical_partition::IdColumn, &id),
    )
    .await
    .map_err(CarbideError::from)?;

    let partition = match partitions.len() {
        1 => partitions.remove(0),
        _ => {
            return Err(CarbideError::NotFoundError {
                kind: "logical_partition",
                id: id.to_string(),
            }
            .into());
        }
    };

    // check if any instance's nvlink config  has this logical partition
    if instance::any_instance_referencing_nvlink_logical_partition(&api.database_connection, &id)
        .await
        .map_err(CarbideError::from)?
    {
        return Err(CarbideError::InvalidArgument(
            "logical partition is still referenced by instance(s)".to_string(),
        )
        .into());
    }

    let resp = api
        .with_txn(|txn| db::nvl_logical_partition::mark_as_deleted(&partition, txn).boxed())
        .await?
        .map(|_| rpc::NvLinkLogicalPartitionDeletionResult {})
        .map(Response::new)?;

    Ok(resp)
}

pub(crate) async fn for_tenant(
    api: &Api,
    request: Request<rpc::TenantSearchQuery>,
) -> Result<Response<rpc::NvLinkLogicalPartitionList>, Status> {
    log_request_data(&request);

    let rpc::TenantSearchQuery {
        tenant_organization_id,
    } = request.into_inner();

    let tenant_org_id_str: String = match tenant_organization_id {
        Some(id) => id,
        None => {
            return Err(CarbideError::MissingArgument("tenant_organization_id").into());
        }
    };

    log_tenant_organization_id(&tenant_org_id_str);

    let results =
        db::nvl_logical_partition::for_tenant(&api.database_connection, tenant_org_id_str)
            .await
            .map_err(CarbideError::from)?;

    let mut partitions = Vec::with_capacity(results.len());

    for result in results {
        partitions.push(result.try_into()?);
    }

    Ok(Response::new(rpc::NvLinkLogicalPartitionList {
        partitions,
    }))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::NvLinkLogicalPartitionUpdateRequest>,
) -> Result<Response<rpc::NvLinkLogicalPartitionUpdateResult>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let id = req
        .id
        .ok_or_else(|| CarbideError::InvalidArgument("ID must be provided".to_string()))?;

    let config = req
        .config
        .ok_or_else(|| CarbideError::InvalidArgument("config must be provided".to_string()))?;

    let metadata: model::metadata::Metadata = config
        .metadata
        .clone()
        .ok_or_else(|| CarbideError::InvalidArgument("metadata must be provided".to_string()))?
        .try_into()?;
    metadata.validate(true).map_err(CarbideError::from)?;

    let mut txn = api.txn_begin().await?;

    let mut partitions = db::nvl_logical_partition::find_by(
        &mut txn,
        ObjectColumnFilter::One(nvl_logical_partition::IdColumn, &id),
    )
    .await
    .map_err(CarbideError::from)?;

    let partition = match partitions.len() {
        1 => partitions.remove(0),
        _ => {
            return Err(CarbideError::NotFoundError {
                kind: "logical_partition",
                id: id.to_string(),
            }
            .into());
        }
    };

    log_tenant_organization_id(&config.tenant_organization_id);

    if config.tenant_organization_id != partition.tenant_organization_id.to_string() {
        return Err(CarbideError::InvalidArgument(
            "tenant organization ID should not be updated".to_string(),
        )
        .into());
    }

    if let Some(if_version_match) = req.if_version_match {
        let target_version = if_version_match
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?;

        if partition.config_version != target_version {
            return Err(CarbideError::ConcurrentModificationError(
                "LogicalPartition",
                target_version.to_string(),
            )
            .into());
        }
    };

    let name = metadata.name;
    let description = metadata.description;
    match db::nvl_logical_partition::update(&partition, name, description, &mut txn).await? {
        ConditionalWrite::Applied(_) => {}
        ConditionalWrite::NotApplied(nvl_logical_partition::LogicalPartitionNotCurrent) => {
            // A missing partition here was removed after the initial lookup, so
            // report it as a concurrent change just like a changed `config_version`.
            return Err(CarbideError::ConcurrentModificationError(
                "LogicalPartition",
                partition.config_version.to_string(),
            )
            .into());
        }
    }

    txn.commit().await?;

    Ok(Response::new(rpc::NvLinkLogicalPartitionUpdateResult {}))
}
