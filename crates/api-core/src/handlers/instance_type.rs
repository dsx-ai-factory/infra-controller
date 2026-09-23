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
use ::rpc::forge::InstanceTypeAllocationStats;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::{HostMachineId, MachineIdSubtypeParseError};
use config_version::ConfigVersion;
use db::{ObjectFilter, compute_allocation, instance, instance_type};
use model::instance_type::InstanceTypeMachineCapabilityFilter;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use model::metadata::Metadata;
use model::tenant::InvalidTenantOrg;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::cfg::file::ComputeAllocationEnforcement;

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateInstanceTypeRequest>,
) -> Result<Response<rpc::CreateInstanceTypeResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Get the ID from the request
    let id = match req.id {
        None => InstanceTypeId::from(Uuid::new_v4()),
        Some(i) => i.parse::<InstanceTypeId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
        })?,
    };

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

    // Prepare the capabilities list
    let mut desired_capabilities = Vec::<InstanceTypeMachineCapabilityFilter>::new();

    for cap in req
        .instance_type_attributes
        .unwrap_or(rpc::InstanceTypeAttributes {
            ..Default::default()
        })
        .desired_capabilities
    {
        desired_capabilities.push(cap.try_into()?);
    }

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    // Write a new instance type to the DB and get back
    // our new InstanceType.
    let instance_type =
        instance_type::create(&mut txn, &id, &metadata, &desired_capabilities).await?;

    // Prepare the response to send back
    let rpc_out = rpc::CreateInstanceTypeResponse {
        instance_type: Some(instance_type.try_into()?),
    };

    //  Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::FindInstanceTypeIdsRequest>,
) -> Result<Response<rpc::FindInstanceTypeIdsResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let instance_type_ids = instance_type::find_ids(&mut txn, false).await?;

    let rpc_out = rpc::FindInstanceTypeIdsResponse {
        instance_type_ids: instance_type_ids.iter().map(|i| i.to_string()).collect(),
    };

    txn.commit().await?;

    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindInstanceTypesByIdsRequest>,
) -> Result<Response<rpc::FindInstanceTypesByIdsResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.instance_type_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be submitted"
        ))
        .into());
    }

    if req.instance_type_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut instance_type_ids = Vec::<InstanceTypeId>::with_capacity(req.instance_type_ids.len());

    // Convert the IDs in the request to a list of InstanceTypeId
    // we can send to the DB.
    for id in req.instance_type_ids {
        instance_type_ids.push(id.parse::<InstanceTypeId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
        })?);
    }

    // Prepare our txn to grab the instance types from the DB
    let mut txn = api.txn_begin().await?;

    // Make our DB query for the IDs to get our instance types
    let instance_types = instance_type::find_by_ids(&mut txn, &instance_type_ids, false).await?;

    let rpc_instance_types = if !req.include_allocation_stats {
        instance_types
            .into_iter()
            .map(|i| i.try_into())
            .collect::<Result<Vec<rpc::InstanceType>, _>>()?
    } else {
        // Get the machine and instance details for the instance types
        let instance_type_association_details =
            instance_type::get_association_details(&mut txn, &instance_type_ids).await?;

        // Get the sum of the active allocations for the instance type.
        let total_allocations = compute_allocation::sum_allocations(
            &mut txn,
            &instance_type_ids,
            req.tenant_organization_id
                .map(|t| {
                    t.parse().map_err(|e: InvalidTenantOrg| {
                        CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
                    })
                })
                .transpose()?
                .as_ref(),
            false,
        )
        .await?;

        let mut rpc_instance_types = Vec::<rpc::InstanceType>::new();

        for itype in instance_types {
            let instance_type_id = itype.id.clone();
            let mut rpc_out: rpc::InstanceType = itype.try_into()?;

            if let Some(instance_type_assoc_details) =
                instance_type_association_details.get(&instance_type_id)
            {
                let max_allocatable: u32 = total_allocations
                    .get(&instance_type_id)
                    .copied()
                    .unwrap_or_default();

                // Grab the count of machines in a good state.
                let good_machine_count: u32 = db::managed_host::load_by_machine_ids(
                    &mut txn,
                    &instance_type_assoc_details.machine_ids,
                    LoadSnapshotOptions {
                        ..LoadSnapshotOptions::default()
                    },
                )
                .await
                .map_err(CarbideError::from)?
                .into_iter()
                .filter(|(_, mhs)| mhs.is_usable_as_instance(false).is_ok())
                .count()
                .try_into()
                .map_err(|e: TryFromIntError| CarbideError::Internal {
                    message: e.to_string(),
                })?;

                // Calculate the number of additional instances that a user _should_
                // be able to create.
                let unused = instance_type_assoc_details
                    .total_machines
                    .saturating_sub(max_allocatable);

                // Get number of additional instances that a user can
                // _actually_ create after filtering out all unused
                // machines in bad states
                let unused_usable = good_machine_count.saturating_sub(max_allocatable);

                rpc_out.allocation_stats = Some(InstanceTypeAllocationStats {
                    max_allocatable,
                    used: instance_type_assoc_details.total_instances,
                    unused,
                    unused_usable,
                });
            }

            rpc_instance_types.push(rpc_out);
        }

        rpc_instance_types
    };

    // Prepare the response message
    let rpc_out = rpc::FindInstanceTypesByIdsResponse {
        instance_types: rpc_instance_types,
    };

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateInstanceTypeRequest>,
) -> Result<Response<rpc::UpdateInstanceTypeResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Get the target ID
    let id = req.id.parse::<InstanceTypeId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
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

    // Prepare the desired capabilities list
    let mut desired_capabilities = Vec::<InstanceTypeMachineCapabilityFilter>::new();

    for cap in req
        .instance_type_attributes
        .unwrap_or(rpc::InstanceTypeAttributes {
            ..Default::default()
        })
        .desired_capabilities
    {
        desired_capabilities.push(cap.try_into()?);
    }

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    // Look up the instance type.  We'll need to check the current
    // version. We could probably do everything with a single query
    // with a few subqueries, but we'd only be able to send back a
    // NotFound, leaving the caller with no way to know if it was
    // because their instance type wasn't found or because the version
    // didn't match.  We'll need to also bump the version, anyway.
    let mut current_instance_type =
        instance_type::find_by_ids(&mut txn, std::slice::from_ref(&id), true).await?;

    // If we found more than one, the DB is corrupt.
    if current_instance_type.len() > 1 {
        // CarbideError::FindOneReturnedManyResultsError expects a uuid,
        // and we've said we want to move away from uuid::Uuid
        return Err(CarbideError::Internal {
            message: format!("multiple InstanceType records found for '{id}'"),
        }
        .into());
    }

    let current_instance_type = match current_instance_type.pop() {
        Some(i) => i,
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "InstanceType",
                id: metadata.name.clone(),
            }
            .into());
        }
    };

    // Prepare the version match if present.
    if let Some(if_version_match) = req.if_version_match {
        let target_version = if_version_match
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?;

        if current_instance_type.version != target_version {
            return Err(CarbideError::ConcurrentModificationError(
                "InstanceType",
                target_version.to_string(),
            )
            .into());
        }
    };

    // Look for any related machines.  Instance types associated with machines
    // should not be updated.  This is another one that could be a subquery, but
    // we want the caller to know the actual reason for failure.
    let existing_associated_machines =
        db::machine::find_ids_by_instance_type_id(&mut txn, &id, true).await?;

    // Forge-cloud allows users to change metadata changes (name, description, and label),
    // so we'll need to allow the same here.
    // The burden of maintaining the order of the capability filters is on the caller.
    // Capability filters are NOT allowed to change if an InstanceType is in use.
    if current_instance_type.desired_capabilities != desired_capabilities
        && !existing_associated_machines.is_empty()
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "InstanceType {id} is associated with active machines"
        ))
        .into());
    }

    // Update instance in the DB and get back
    // our new InstanceType state.
    let instance_type = instance_type::update(
        &mut txn,
        &id,
        &metadata,
        &desired_capabilities,
        current_instance_type.version,
    )
    .await?;

    // Prepare the response to send back
    let rpc_out = rpc::UpdateInstanceTypeResponse {
        instance_type: Some(instance_type.try_into()?),
    };

    // Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteInstanceTypeRequest>,
) -> Result<Response<rpc::DeleteInstanceTypeResponse>, Status> {
    log_request_data(&request);

    let id = request
        .into_inner()
        .id
        .parse::<InstanceTypeId>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
        })?;

    // Prepare our txn to delete from the DB
    let mut txn = api.txn_begin().await?;

    // Look for any related machines.  Forge-Cloud provides users with
    // the behavior of removing all machine associations to an InstanceType for machines
    // as long as all machines affected have no associated instances.
    // We need to replicate this here so that it's a single call.

    //  This will also grab a row lock on the requested machines so we can
    // coordinate with the instance allocation handler.
    let existing_associated_machines =
        db::machine::find_ids_by_instance_type_id(&mut txn, &id, true).await?;

    // Check that there are no associated instances for the machines.
    let instances = instance::find_by_machine_ids(
        &mut txn,
        &existing_associated_machines
            .iter()
            .map(|v| &v.0)
            .collect::<Vec<_>>(),
    )
    .await?;

    if !instances.is_empty() {
        return Err(CarbideError::FailedPrecondition(format!(
            "InstanceType {id} is associated with machines that have active instances"
        ))
        .into());
    }

    // Make our DB query to remove the machine associations.
    db::machine::remove_instance_type_associations(
        &mut txn,
        &existing_associated_machines
            .iter()
            .map(|v| (&v.0, &v.1))
            .collect::<Vec<_>>(),
    )
    .await?;

    // Make our DB query to soft delete the instance type
    instance_type::soft_delete(&mut txn, &id).await?;

    // Prepare the response message
    let rpc_out = rpc::DeleteInstanceTypeResponse {};

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn associate_machines(
    api: &Api,
    request: Request<rpc::AssociateMachinesWithInstanceTypeRequest>,
) -> Result<Response<rpc::AssociateMachinesWithInstanceTypeResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.machine_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} machine IDs can be submitted"
        ))
        .into());
    }

    if req.machine_ids.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one machine ID must be provided".to_string(),
        )
        .into());
    }

    let instance_type_id = req
        .instance_type_id
        .parse::<InstanceTypeId>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
        })?;

    // Prepare our txn to associate machines with the instance type
    let mut txn = api.txn_begin().await?;

    // Query the DB to make sure the instance type is valid/active.
    let instance_types =
        instance_type::find_by_ids(&mut txn, std::slice::from_ref(&instance_type_id), true).await?;

    if instance_types.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "InstanceType",
            id: req.instance_type_id,
        }
        .into());
    }

    // Convert the rpc machine ID strings into MachineId, but reject if any
    // DPU machine IDs are found.
    let machine_ids = req
        .machine_ids
        .into_iter()
        .map(|id| id.parse::<HostMachineId>())
        .collect::<Result<Vec<HostMachineId>, MachineIdSubtypeParseError>>()
        .map_err(|e| CarbideError::from(RpcDataConversionError::InvalidMachineId(e.to_string())))?;

    let instance_type_id = req
        .instance_type_id
        .parse::<InstanceTypeId>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
        })?;

    // Query the DB to make sure the instance type is valid/active.
    let instance_types =
        instance_type::find_by_ids(&mut txn, std::slice::from_ref(&instance_type_id), true).await?;

    if instance_types.len() > 1 {
        return Err(CarbideError::Internal {
            message: format!("multiple InstanceType records found for '{instance_type_id}'"),
        }
        .into());
    }

    if instance_types.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "InstanceType",
            id: req.instance_type_id,
        }
        .into());
    }

    // Grab the requested machines so we can row-lock and
    // also get their most recent snapshots so we can check
    // their capabilities.
    let machines = db::machine::find(
        &mut txn,
        ObjectFilter::List(&machine_ids),
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;

    // Check that there are no associated instances for the machines.
    // I expected machine.has_instance() to handle this, but the data
    // that drives that doesn't seem to get persisted until sometime in
    // the future after an instance is created in the DB.
    let instances =
        instance::find_by_machine_ids(&mut txn, &machine_ids.iter().collect::<Vec<_>>()).await?;

    if !instances.is_empty() {
        return Err(CarbideError::FailedPrecondition(
            "one or more machines have instances assigned".to_string(),
        )
        .into());
    }

    let mut machine_versions = Vec::new();

    // Go through the requested machines and make sure they
    //actually meet the requirements of the instance type.
    for machine in machines.iter() {
        let capabilities = machine
            .to_capabilities()
            .ok_or(CarbideError::InvalidArgument(format!(
                "capabilities of machine {} do not satisfy the requested InstanceType ({})",
                machine.id, instance_type_id
            )))?;

        if !instance_types[0].matches_capability_set(&capabilities) {
            return Err(CarbideError::InvalidArgument(format!(
                "capabilities of machine {} do not satisfy the requested InstanceType ({})",
                machine.id, instance_type_id
            ))
            .into());
        }
        machine_versions.push((&machine.id, &machine.version));
    }

    // Make our DB query for the association
    let ids = db::machine::associate_machines_with_instance_type(
        &mut txn,
        &instance_type_id,
        &machine_versions,
    )
    .await?;

    if ids.len() != machine_versions.len() {
        tracing::error!(
            ids = ?ids,
            machine_versions = ?machine_versions,
            "Not all machine-to-instance-type associations were updated; row locking should have prevented this",
        )
    }

    // Prepare the response message
    let rpc_out = rpc::AssociateMachinesWithInstanceTypeResponse {};

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn remove_machine_association(
    api: &Api,
    request: Request<rpc::RemoveMachineInstanceTypeAssociationRequest>,
) -> Result<Response<rpc::RemoveMachineInstanceTypeAssociationResponse>, Status> {
    log_request_data(&request);

    let machine_id = request
        .into_inner()
        .machine_id
        .parse::<HostMachineId>()
        .map_err(|e| CarbideError::from(RpcDataConversionError::InvalidMachineId(e.to_string())))?;

    // Prepare our txn to associate machines with the instance type
    let mut txn = api.txn_begin().await?;

    // Grab a row lock on the requested machine so we can
    // coordinate with the instance allocation handler and
    // check for the existence of instances.
    let mut machines = db::machine::find(
        &mut txn,
        ObjectFilter::List(&[machine_id]),
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;

    let Some(machine) = machines.pop() else {
        return Err(CarbideError::NotFoundError {
            kind: "Machine",
            id: machine_id.to_string(),
        }
        .into());
    };

    // Check that there are no associated instances for the machines.
    let instances = instance::find_by_machine_ids(&mut txn, &[&machine_id]).await?;

    if let Some(instance) = instances.first()
        && instance.deleted.is_none()
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "machine {} has instance assigned. this operation is allowed only in terminating state",
            machine.id
        ))
        .into());
    }

    if let Some(ref instance_type_id) = machine.config.instance_type_id {
        // Query the DB for the instance type so that we can use a row-level lock for coordination.
        // We need this so that ComputeAllocation additions and updates that increase allocations can't exceed the number
        // of machines associated with a type.
        instance_type::find_by_ids(&mut txn, std::slice::from_ref(instance_type_id), true).await?;

        // Check that removing the machine from the instance-type won't cause the number of machines associated with the instance-type
        // to drop below the total number of allocations for the instance-type.

        // Start by getting the number of existing machines for the instance type
        // and subtract 1.
        let new_total_machine_count = instance_type::get_association_details(
            &mut txn,
            std::slice::from_ref(instance_type_id),
        )
        .await?
        .get(instance_type_id)
        .ok_or_else(|| CarbideError::Internal {
            message: format!(
                "expected InstanceType for InstanceTypeID of machine {} but found none",
                machine.id
            ),
        })?
        .total_machines
        .saturating_sub(1);

        // Next, get the sum of the active allocations for the instance type.
        let allocation_total = compute_allocation::sum_allocations(
            &mut txn,
            std::slice::from_ref(instance_type_id),
            None,
            false,
        )
        .await?
        .get(instance_type_id)
        .copied();

        let has_allocations = allocation_total.is_some();

        let total_allocations = allocation_total.unwrap_or_default();

        // Check if the new machine count would drop below the sum of active allocation counts.
        if new_total_machine_count < total_allocations {
            let machine_id = machine.id;
            // # enforce_if_present:  If allocations are found for instance type ID, enforce it; otherwise, it's like no limits.
            // # always:              Enforce allocations.  If none are found, its a constraint value of 0 (i.e., you get nothing).
            // # warn_only (default): Don't enforce, but log what would have happened if they were enforced.
            match (
                has_allocations,
                &api.runtime_config.compute_allocation_enforcement,
            ) {
                (_, ComputeAllocationEnforcement::Always)
                | (true, ComputeAllocationEnforcement::EnforceIfPresent) => {
                    return Err(CarbideError::FailedPrecondition(
                        "request to remove machine from instance type would reduce associated machine count below current tenant allocations count"
                            .to_string(),
                    ).into());
                }
                (false, ComputeAllocationEnforcement::EnforceIfPresent) => {
                    tracing::debug!(%machine_id, %instance_type_id, "EnforceIfPresent set but no allocations seen during machine removal from instance type");
                }
                (_, ComputeAllocationEnforcement::WarnOnly) => {
                    tracing::warn!(%machine_id, %instance_type_id, "request to remove machine from instance type would reduce associated machine count below current tenant allocations count but enforcement is not enabled");
                }
            }
        }

        // Make our DB query to remove the association
        db::machine::remove_instance_type_associations(
            &mut txn,
            &[(&machine.id, &machine.version)],
        )
        .await?;
    }

    // Prepare the response message
    let rpc_out = rpc::RemoveMachineInstanceTypeAssociationResponse {};

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}
