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

use std::num::TryFromIntError;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use carbide_uuid::compute_allocation::ComputeAllocationId;
use carbide_uuid::instance_type::InstanceTypeId;
use config_version::ConfigVersion;
use db::{compute_allocation, instance, instance_type, machine};
use model::compute_allocation::MAX_COMPUTE_ALLOCATION_SIZE;
use model::metadata::Metadata;
use model::tenant::{InvalidTenantOrg, TenantOrganizationId};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateComputeAllocationRequest>,
) -> Result<Response<rpc::CreateComputeAllocationResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let (instance_type_id, count) = req
        .attributes
        .map(|attr| -> Result<(InstanceTypeId, u32), CarbideError> {
            if attr.count > MAX_COMPUTE_ALLOCATION_SIZE {
                return Err(CarbideError::from(RpcDataConversionError::InvalidValue(
                    "count".to_string(),
                    format!("exceeds max allocation size of {MAX_COMPUTE_ALLOCATION_SIZE}"),
                )));
            }

            let i = attr
                .instance_type_id
                .parse::<InstanceTypeId>()
                .map_err(|e| {
                    CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
                })?;
            Ok((i, attr.count))
        })
        .transpose()?
        .ok_or(CarbideError::from(RpcDataConversionError::MissingArgument(
            "attributes",
        )))?;

    // Get the ID from the request
    let id = match req.id {
        None => ComputeAllocationId::from(Uuid::new_v4()),
        Some(i) => i,
    };

    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Prepare the metadata
    let metadata = match req.metadata {
        Some(m) => Metadata::try_from(m).map_err(CarbideError::from)?,
        _ => {
            return Err(
                CarbideError::from(RpcDataConversionError::MissingArgument("metadata")).into(),
            );
        }
    };

    metadata.validate(true).map_err(CarbideError::from)?;

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    // Grab a row-level lock on instance type ID for coordination with the other handlers
    // so we can check that a) the number of machines associated with the instance type supports this new
    // allocation, and b) we don't allow concurrent adds or concurrent add+updates to exceed that machine count.
    // We need this so that an allocation can't be added until we're done because
    // a concurrent addition wouldn't be seen by the select for_udpate.
    instance_type::find_by_ids(&mut txn, std::slice::from_ref(&instance_type_id), true).await?;

    // Grab the sum of existing allocations for all tenants,
    // and increase it by the new amount.
    // We are able to skip the row-level lock here because adds or updates that
    // increase should be coordinating around the instance type.

    let (new_tenant_allocation_total, overflow) = compute_allocation::sum_allocations(
        &mut txn,
        std::slice::from_ref(&instance_type_id),
        None,
        false,
    )
    .await?
    .get(&instance_type_id)
    .copied()
    .unwrap_or_default()
    .overflowing_add(count);

    if overflow {
        return Err(CarbideError::InvalidArgument(
            "requested allocation would cause total allocations to exceed u32 limits".to_string(),
        )
        .into());
    }

    // Then grab the total number of machines associated with the instance type.
    // We don't need the row-level lock for the machine because machine/type assoc/dissoc are coordinated
    // around a row-level lock of its instance type.  A row-level lock here would
    // just block instance creation for no good reason.
    let machine_count = machine::find_ids_by_instance_type_id(&mut txn, &instance_type_id, false)
        .await
        .map_err(CarbideError::from)?
        .len();

    if machine_count
        < new_tenant_allocation_total
            .try_into()
            .map_err(|e: TryFromIntError| CarbideError::Internal {
                message: format!("unable to compare current machine and allocation counts - {e}"),
            })?
    {
        return Err(CarbideError::FailedPrecondition(format!(
                "requested allocation would increase allocation count ({new_tenant_allocation_total}) above associated machines count ({machine_count})"
            ))
            .into());
    }

    // Write a new ComputeAllocation to the DB and get back
    // our new ComputeAllocation.
    let compute_allocation = compute_allocation::create(
        &mut txn,
        &id,
        &tenant_organization_id,
        req.created_by.as_deref(),
        &metadata,
        count
            .try_into()
            .map_err(|e: TryFromIntError| CarbideError::Internal {
                message: format!("allocation count cannot be converted to i32 - {e}"),
            })?,
        &instance_type_id,
    )
    .await?;

    // Prepare the response to send back
    let rpc_out = rpc::CreateComputeAllocationResponse {
        allocation: Some(compute_allocation.try_into()?),
    };

    //  Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::FindComputeAllocationIdsRequest>,
) -> Result<Response<rpc::FindComputeAllocationIdsResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let mut txn = api.txn_begin().await?;

    let instance_type_ids = req
        .instance_type_id
        .map(|i| i.parse::<InstanceTypeId>())
        .transpose()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.to_string()))
        })?
        .map(|i| vec![i]);

    let allocation_ids = compute_allocation::find_ids(
        &mut txn,
        req.name.as_deref(),
        req.tenant_organization_id
            .map(|t| t.parse::<TenantOrganizationId>())
            .transpose()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?
            .as_ref(),
        instance_type_ids.as_deref(),
        false,
    )
    .await?;

    let rpc_out = rpc::FindComputeAllocationIdsResponse {
        ids: allocation_ids,
    };

    txn.commit().await?;

    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindComputeAllocationsByIdsRequest>,
) -> Result<Response<rpc::FindComputeAllocationsByIdsResponse>, Status> {
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

    // Prepare our txn to grab the ComputeAllocations from the DB
    let mut txn = api.txn_begin().await?;

    // Make our DB query for the IDs to get our ComputeAllocations
    let compute_allocations =
        compute_allocation::find_by_ids(&mut txn, &req.ids, None, false).await?;

    // Convert the list of internal ComputeAllocation to a
    // list of proto message ComputeAllocation to send back
    // in the response.

    let rpc_compute_allocations = compute_allocations
        .into_iter()
        .map(|i| i.try_into())
        .collect::<Result<Vec<rpc::ComputeAllocation>, _>>()?;

    // Prepare the response message
    let rpc_out = rpc::FindComputeAllocationsByIdsResponse {
        allocations: rpc_compute_allocations,
    };

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateComputeAllocationRequest>,
) -> Result<Response<rpc::UpdateComputeAllocationResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let (instance_type_id, count) = req
        .attributes
        .map(|attr| -> Result<(InstanceTypeId, u32), CarbideError> {
            if attr.count > MAX_COMPUTE_ALLOCATION_SIZE {
                return Err(CarbideError::from(RpcDataConversionError::InvalidValue(
                    "count".to_string(),
                    format!("exceeds max allocation size of {MAX_COMPUTE_ALLOCATION_SIZE}"),
                )));
            }

            let i = attr
                .instance_type_id
                .parse::<InstanceTypeId>()
                .map_err(|e| {
                    CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
                })?;
            Ok((i, attr.count))
        })
        .transpose()?
        .ok_or(CarbideError::from(RpcDataConversionError::MissingArgument(
            "attributes",
        )))?;

    // Get the target ID
    let id = req
        .id
        .ok_or(CarbideError::from(RpcDataConversionError::MissingArgument(
            "id",
        )))?;

    // Prepare the metadata
    let metadata = match req.metadata {
        Some(m) => Metadata::try_from(m).map_err(CarbideError::from)?,
        _ => {
            return Err(
                CarbideError::from(RpcDataConversionError::MissingArgument("metadata")).into(),
            );
        }
    };

    metadata.validate(true).map_err(CarbideError::from)?;

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Look up the ComputeAllocation.  We'll need to check the current
    // version.
    let current_compute_allocation = compute_allocation::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        Some(&tenant_organization_id),
        true,
    )
    .await?;

    // If we found more than one, the DB is corrupt.
    if current_compute_allocation.len() > 1 {
        return Err(CarbideError::Internal {
            message: format!("multiple ComputeAllocation records found for '{id}'"),
        }
        .into());
    }

    // This could have been because allocation doesn't exist
    // OR because the tenant org ID was wrong.
    let current_compute_allocation = match current_compute_allocation.first() {
        Some(i) => i,
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "ComputeAllocation",
                id: format!(
                    "{} for tenant org `{}`",
                    metadata.name.clone(),
                    req.tenant_organization_id.clone(),
                ),
            }
            .into());
        }
    };

    // Check instance type id against the one we find in the actual record for the
    // requested allocation.
    if current_compute_allocation.instance_type_id != instance_type_id {
        return Err(CarbideError::InvalidArgument(format!("requested ComputeAllocation record '{id}' is not associated with requested InstanceTypeId '{instance_type_id}'"))
        .into());
    }

    // If the update is _increasing_ the allocation count, then we also need to grab ALL allocations
    // to make sure the sum is not going to exceed the total number of machines associated with the instance type
    // across ALL tenants.
    if count > current_compute_allocation.count {
        let alloc_count_increase = count - current_compute_allocation.count;

        // Make our DB query for the IDs to get and row-lock our instance types.
        // We need this so that an allocation can't be added until we're done because
        // a concurrent addition wouldn't be seen by the select for_udpate.
        instance_type::find_by_ids(&mut txn, std::slice::from_ref(&instance_type_id), true).await?;

        // Grab the sum of existing allocations for all tenants,
        // and increase it by the amount the existing allocation is being increased.
        // We are able to skip the row-level lock here because adds or updates that increase should be coordinating around the instance type.
        let new_tenant_allocation_total = compute_allocation::sum_allocations(
            &mut txn,
            std::slice::from_ref(&instance_type_id),
            None,
            false,
        )
        .await?
        .get(&instance_type_id)
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "expected allocation sum for instance type `{instance_type_id}` not found"
            ),
        })? + alloc_count_increase;

        // Then grab the total number of machines associated with the instance type.
        // We don't need the row-level lock for the machine because machine/type assoc/dissoc are coordinated
        // around a row-level lock of its instance type.  A row-level lock here would
        // just block instance creation for no good reason.
        let machine_count =
            machine::find_ids_by_instance_type_id(&mut txn, &instance_type_id, false)
                .await
                .map_err(CarbideError::from)?
                .len();

        if machine_count
            < new_tenant_allocation_total
                .try_into()
                .map_err(|e: TryFromIntError| CarbideError::Internal {
                    message: format!(
                        "unable to compare current machine and allocation counts - {e}"
                    ),
                })?
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "requested update would increase allocation count ({new_tenant_allocation_total}) above associated machines count ({machine_count})"
            ))
            .into());
        }
    } else if count < current_compute_allocation.count {
        let alloc_count_decrease = current_compute_allocation.count - count;

        // Grab the sum of existing allocations for the tenant,
        // and decrease it by the amount the existing allocation is being decreased.
        // We need row-level locking here to coordinate with concurrent decrease or
        // delete attempts for the tenant's allocations.
        let new_tenant_allocation_total = compute_allocation::sum_allocations(
            &mut txn,
            std::slice::from_ref(&instance_type_id),
            Some(&tenant_organization_id),
            true,
        )
        .await?
        .get(&instance_type_id)
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "expected allocation sum for instance type `{instance_type_id}` not found"
            ),
        })? - alloc_count_decrease;

        // Now we need to grab the count of instances for the tenant for this instance type.
        // We will need to compare the count against the new allocation total to make sure the
        // total isn't dropping below the count of already-created instances.
        let filter = model::instance::InstanceSearchFilter {
            label: None,
            tenant_org_id: Some(req.tenant_organization_id),
            vpc_id: None,
            instance_type_id: Some(instance_type_id.to_string()),
        };

        let instance_count = instance::count_ids(&mut txn, filter).await?;

        if instance_count > i64::from(new_tenant_allocation_total) {
            return Err(CarbideError::FailedPrecondition(format!(
                "requested update would decrease allocation count ({new_tenant_allocation_total}) below existing instances count ({instance_count})"
            ))
            .into());
        }
    }

    // Prepare the version match if present.
    if let Some(if_version_match) = req.if_version_match {
        let target_version = if_version_match
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?;

        if current_compute_allocation.version != target_version {
            return Err(CarbideError::ConcurrentModificationError(
                "ComputeAllocation",
                target_version.to_string(),
            )
            .into());
        }
    };

    // Update record in the DB and get back
    // our new ComputeAllocation state.
    let compute_allocation = compute_allocation::update(
        &mut txn,
        &id,
        &tenant_organization_id,
        &metadata,
        count
            .try_into()
            .map_err(|e: TryFromIntError| CarbideError::Internal {
                message: format!("allocation count cannot be converted to i32 - {e}"),
            })?,
        current_compute_allocation.version,
        req.updated_by.as_deref(),
    )
    .await?;

    // Prepare the response to send back
    let rpc_out = rpc::UpdateComputeAllocationResponse {
        allocation: Some(compute_allocation.try_into()?),
    };

    // Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteComputeAllocationRequest>,
) -> Result<Response<rpc::DeleteComputeAllocationResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let id = req
        .id
        .ok_or(CarbideError::from(RpcDataConversionError::MissingArgument(
            "id",
        )))?;

    // Prepare our txn to delete from the DB
    let mut txn = api.txn_begin().await?;

    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Make our DB query for the ComputeAllocation.
    // We return without error if something wasn't found because it was already soft-deleted,
    // so we'll check tenant ownership separately with the query here so we don't hide a
    // 404 due to a mismatched tenant.
    // We also use this to coordinate other allocation-related operations as fine-grained as possible/necessary.
    let allocation = compute_allocation::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        Some(&tenant_organization_id),
        true,
    )
    .await?
    .pop();

    // Since we needed to query for the record anyway,
    // we can save ourselves some extra work if it didn't exist.
    if let Some(allocation) = allocation
        && allocation.deleted.is_none()
    {
        if allocation.tenant_organization_id != tenant_organization_id {
            return Err(CarbideError::InvalidArgument(format!(
                "ComputeAllocation `{}` is not owned by tenant `{}`",
                allocation.id.clone(),
                tenant_organization_id.clone()
            ))
            .into());
        }

        // Grab the sum of existing allocations for the tenant,
        // and decrease it by the amount the existing allocation is being decreased.
        // We need row-level locking here to coordinate with concurrent decrease or
        // delete attempts for the tenant's allocations.
        let new_tenant_allocation_total = compute_allocation::sum_allocations(
            &mut txn,
            std::slice::from_ref(&allocation.instance_type_id),
            Some(&tenant_organization_id),
            true,
        )
        .await?
        .get(&allocation.instance_type_id)
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "expected allocation sum for instance type `{}` not found",
                allocation.instance_type_id
            ),
        })? - allocation.count;

        // Now we need to grab the count of instances for the tenant for this instance type.
        // We will need to compare the count against the new allocation total to make sure the
        // total isn't dropping below the count of already-created instances.
        let filter = model::instance::InstanceSearchFilter {
            label: None,
            tenant_org_id: Some(req.tenant_organization_id),
            vpc_id: None,
            instance_type_id: Some(allocation.instance_type_id.to_string()),
        };

        let instance_count = instance::count_ids(&mut txn, filter).await?;

        if instance_count > i64::from(new_tenant_allocation_total) {
            return Err(CarbideError::FailedPrecondition(format!(
                "requested delete would decrease allocation count ({new_tenant_allocation_total}) below existing instances count ({instance_count})"
            ))
            .into());
        }

        // Make our DB query to soft delete the ComputeAllocation
        compute_allocation::soft_delete(&mut txn, &id, &tenant_organization_id).await?;
    }

    // Prepare the response message
    let rpc_out = rpc::DeleteComputeAllocationResponse {};

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}
