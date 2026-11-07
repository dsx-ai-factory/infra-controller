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
use db::resource_pool::ResourcePoolDatabaseError;
use db::{ConditionalWrite, ObjectColumnFilter, ib_partition};
use model::ib::DEFAULT_IB_FABRIC_NAME;
use model::ib_partition::{IBPartitionStatus, NewIBPartition, PartitionKey};
use model::resource_pool;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::IbPartitionCreationRequest>,
) -> Result<Response<rpc::IbPartition>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();
    let requested_pkey = req
        .config
        .as_ref()
        .and_then(|c| {
            c.pkey
                .as_ref()
                .map(|pkey| u16::from_str_radix(pkey.trim_start_matches("0x"), 16))
        })
        .transpose()
        .map_err(|e| CarbideError::InvalidArgument(format!("invalid pkey value: {}", e)))?;
    let mut resp = NewIBPartition::try_from(req)?;
    let fabric_config = api.ib_fabric_manager.get_config();

    // IB Configurations.
    resp.config.mtu = Some(fabric_config.mtu.clone());
    resp.config.rate_limit = Some(fabric_config.rate_limit.clone());
    resp.config.service_level = Some(fabric_config.service_level.clone());

    let allocated_pkey = allocate_pkey(api, &mut txn, &resp.metadata.name, requested_pkey).await?;

    if requested_pkey.is_some() {
        resp.config.pkey = allocated_pkey;
    }

    let resp = db::ib_partition::create(
        resp,
        &mut txn,
        fabric_config.max_partition_per_tenant,
        IBPartitionStatus {
            pkey: allocated_pkey,
            partition: None,
            mtu: None,
            rate_limit: None,
            service_level: None,
        },
    )
    .await
    .map_err(|e| {
        if e.is_not_found() {
            // During IB partition creation, the insert query checks that the number of existing partitions
            // is less than <max_partition_per_tenant> by using a sub-select query in a WHERE clause.
            // The 'RowNotFound' error means that the number of existing partitions exceeded the limit
            // and no insert was performed.
            CarbideError::InvalidArgument(
                "maximum limit of infiniband partitions had been reached".into(),
            )
        } else {
            CarbideError::from(e)
        }
    })?;
    let resp = rpc::IbPartition::try_from(resp).map(Response::new)?;

    txn.commit().await?;

    Ok(resp)
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::IbPartitionUpdateRequest>,
) -> Result<Response<rpc::IbPartition>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    let id = req.id.ok_or(CarbideError::MissingArgument("id"))?;
    let config = req.config.ok_or(CarbideError::MissingArgument("config"))?;
    let metadata = req
        .metadata
        .ok_or(CarbideError::MissingArgument("metadata"))?;

    let mut partitions = db::ib_partition::find_by(
        &mut txn,
        ObjectColumnFilter::One(ib_partition::IdColumn, &id),
    )
    .await?;

    let partition = match partitions.len() {
        1 => partitions.remove(0),
        _ => {
            return Err(CarbideError::NotFoundError {
                kind: "ib_partition",
                id: id.to_string(),
            }
            .into());
        }
    };

    if let Some(if_version_match) = req.if_version_match {
        let target_version = if_version_match
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?;

        if partition.version != target_version {
            return Err(CarbideError::ConcurrentModificationError(
                "IBPartition",
                target_version.to_string(),
            )
            .into());
        }
    }

    if config.tenant_organization_id != partition.config.tenant_organization_id.to_string() {
        return Err(CarbideError::InvalidArgument(
            "tenant organization ID should not be updated".to_string(),
        )
        .into());
    }

    if config.pkey.is_some() {
        let req_pkey = config
            .pkey
            .as_ref()
            .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .and_then(|v| PartitionKey::try_from(v).ok());
        let cur_pkey = partition.status.as_ref().and_then(|s| s.pkey);
        if req_pkey != cur_pkey {
            return Err(CarbideError::InvalidArgument(
                "partition key cannot be updated".to_string(),
            )
            .into());
        }
    }

    let metadata = metadata.try_into().map_err(CarbideError::from)?;
    let resp = match db::ib_partition::update_metadata(
        partition.id,
        partition.version,
        &metadata,
        &mut txn,
    )
    .await?
    {
        ConditionalWrite::Applied(partition) => partition,
        ConditionalWrite::NotApplied(ib_partition::PartitionNotCurrent) => {
            // The initial lookup found the partition. Its disappearance or a
            // changed config version after that read is a concurrent change.
            return Err(CarbideError::ConcurrentModificationError(
                "IBPartition",
                partition.version.to_string(),
            )
            .into());
        }
    };
    txn.commit().await?;

    Ok(Response::new(rpc::IbPartition::try_from(resp)?))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::IbPartitionSearchFilter>,
) -> Result<Response<rpc::IbPartitionIdList>, Status> {
    log_request_data(&request);

    let filter: model::ib_partition::IbPartitionSearchFilter = request.into_inner().into();

    let ib_partition_ids = db::ib_partition::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::IbPartitionIdList { ib_partition_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::IbPartitionsByIdsRequest>,
) -> Result<Response<rpc::IbPartitionList>, Status> {
    log_request_data(&request);

    let rpc::IbPartitionsByIdsRequest {
        ib_partition_ids, ..
    } = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if ib_partition_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if ib_partition_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let partitions = db::ib_partition::find_by(
        &api.database_connection,
        ObjectColumnFilter::List(ib_partition::IdColumn, &ib_partition_ids),
    )
    .await?;

    let mut result = Vec::with_capacity(partitions.len());
    for ibp in partitions {
        result.push(ibp.try_into()?);
    }
    Ok(Response::new(rpc::IbPartitionList {
        ib_partitions: result,
    }))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::IbPartitionDeletionRequest>,
) -> Result<Response<rpc::IbPartitionDeletionResult>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let rpc::IbPartitionDeletionRequest { id, .. } = request.into_inner();

    let uuid = id.ok_or(CarbideError::MissingArgument("id"))?;

    let mut segments = db::ib_partition::find_by(
        &mut txn,
        ObjectColumnFilter::One(ib_partition::IdColumn, &uuid),
    )
    .await?;

    let segment = match segments.len() {
        1 => segments.remove(0),
        _ => {
            return Err(CarbideError::NotFoundError {
                kind: "ib_partition",
                id: uuid.to_string(),
            }
            .into());
        }
    };

    // Best-effort guard: reject deletion when instances are still bound.
    // It is still possible for a new reference to be made after this check is made.
    // In that case partition will remain in terminating state until the reference is removed.
    let instance_count =
        db::ib_partition::count_instances_referencing_partition(&mut txn, segment.id).await?;
    if instance_count > 0 {
        return Err(CarbideError::FailedPrecondition(format!(
            "IB partition cannot be deleted while {instance_count} instance(s) are still using it"
        ))
        .into());
    }

    let resp = db::ib_partition::mark_as_deleted(&segment, &mut txn)
        .await
        .map(|_| rpc::IbPartitionDeletionResult {})
        .map(Response::new)?;

    txn.commit().await?;

    Ok(resp)
}

pub(crate) async fn for_tenant(
    api: &Api,
    request: Request<rpc::TenantSearchQuery>,
) -> Result<Response<rpc::IbPartitionList>, Status> {
    log_request_data(&request);

    let rpc::TenantSearchQuery {
        tenant_organization_id,
    } = request.into_inner();

    let _tenant_organization_id: String = match tenant_organization_id {
        Some(id) => id,
        None => {
            return Err(CarbideError::MissingArgument("tenant_organization_id").into());
        }
    };

    let results =
        db::ib_partition::for_tenant(&api.database_connection, _tenant_organization_id).await?;

    let mut ib_partitions = Vec::with_capacity(results.len());

    for result in results {
        ib_partitions.push(result.try_into()?);
    }

    Ok(Response::new(rpc::IbPartitionList { ib_partitions }))
}

/// Allocate a value from the pkey resource pool.
///
/// If the pool doesn't exist return error.
/// If the pool exists but is empty or has en error, return that.
async fn allocate_pkey(
    api: &Api,
    txn: &mut PgConnection,
    owner_id: &str,
    requested_pkey: Option<u16>,
) -> Result<Option<PartitionKey>, CarbideError> {
    let source_pool = api
        .common_pools
        .infiniband
        .pkey_pools
        .get(DEFAULT_IB_FABRIC_NAME)
        .ok_or_else(|| CarbideError::internal("IB fabric is not configured".to_string()))?;

    match db::resource_pool::allocate(
        source_pool,
        txn,
        resource_pool::OwnerType::IBPartition,
        owner_id,
        requested_pkey,
    )
    .await
    {
        Ok(val) => Ok(Some(PartitionKey::try_from(val).map_err(|_| {
            CarbideError::internal(format!(
                "partition key {val} return from pool is not a valid pkey. pool definition is invalid"
            ))
        })?)),
        Err(error @ ResourcePoolDatabaseError::ResourcePool(
            resource_pool::ResourcePoolError::Empty,
        )) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_pkey.is_some(),
                "pkey",
                &error,
            );
            Err(CarbideError::ResourceExhausted(
                "pool pkey".to_string(),
            ))
        }
        Err(err) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_pkey.is_some(),
                "pkey",
                &err,
            );
            Err(err.into())
        }
    }
}
