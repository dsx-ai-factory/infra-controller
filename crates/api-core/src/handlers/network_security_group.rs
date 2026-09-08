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

use std::collections::HashSet;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use db::network_security_group;
use model::metadata::Metadata;
use model::network_security_group::{NetworkSecurityGroupRule, NetworkSecurityGroupRuleNet};
use model::tenant::{InvalidTenantOrg, TenantOrganizationId};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::CarbideError;
use crate::api::{Api, log_request_data, log_tenant_organization_id};

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::CreateNetworkSecurityGroupRequest>,
) -> Result<Response<rpc::CreateNetworkSecurityGroupResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Get the ID from the request
    let id = match req.id {
        None => NetworkSecurityGroupId::from(Uuid::new_v4()),
        Some(i) => i.parse::<NetworkSecurityGroupId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
                e.value(),
            ))
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

    // Prepare the rules list
    let (stateful_egress, rules) = {
        let attr = req.network_security_group_attributes.unwrap_or_default();

        (
            attr.stateful_egress,
            attr.rules
                .into_iter()
                .map(|r| r.try_into())
                .collect::<Result<Vec<_>, _>>()
                .map_err(CarbideError::from)?,
        )
    };

    validate_stateful_egress_enablement(
        None,
        stateful_egress,
        api.runtime_config
            .network_security_group
            .stateful_acls_enabled,
    )?;

    let max_nsg_size = api
        .runtime_config
        .network_security_group
        .max_network_security_group_size as usize;

    validate_expanded_rule_set(&rules, max_nsg_size)?;

    // Log tenant organization ID
    log_tenant_organization_id(&req.tenant_organization_id);

    // Parse tenant organization ID
    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    // Write a new NetworkSecurityGroup to the DB and get back
    // our new NetworkSecurityGroup.
    let network_security_group = network_security_group::create(
        &mut txn,
        &id,
        &tenant_organization_id,
        None,
        &metadata,
        stateful_egress,
        &rules,
    )
    .await?;

    // Prepare the response to send back
    let rpc_out = rpc::CreateNetworkSecurityGroupResponse {
        network_security_group: Some(network_security_group.try_into()?),
    };

    //  Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::FindNetworkSecurityGroupIdsRequest>,
) -> Result<Response<rpc::FindNetworkSecurityGroupIdsResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Log tenant organization ID if present
    if let Some(ref tenant_org_id) = req.tenant_organization_id {
        log_tenant_organization_id(tenant_org_id);
    }

    let tenant_organization_id = req
        .tenant_organization_id
        .map(|t| t.parse::<TenantOrganizationId>())
        .transpose()
        .map_err(|e: InvalidTenantOrg| {
            CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
        })?;

    let mut txn = api.txn_begin().await?;

    let network_security_group_ids = network_security_group::find_ids(
        &mut txn,
        req.name.as_deref(),
        tenant_organization_id.as_ref(),
        false,
    )
    .await?;

    let rpc_out = rpc::FindNetworkSecurityGroupIdsResponse {
        network_security_group_ids: network_security_group_ids
            .iter()
            .map(|i| i.to_string())
            .collect(),
    };

    txn.commit().await?;

    Ok(Response::new(rpc_out))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::FindNetworkSecurityGroupsByIdsRequest>,
) -> Result<Response<rpc::FindNetworkSecurityGroupsByIdsResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.network_security_group_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be submitted"
        ))
        .into());
    }

    if req.network_security_group_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    // Convert the IDs in the request to a list of NetworkSecurityGroupId
    // we can send to the DB.
    let network_security_group_ids: Vec<NetworkSecurityGroupId> = req
        .network_security_group_ids
        .iter()
        .map(|i| i.parse::<NetworkSecurityGroupId>())
        .collect::<Result<Vec<NetworkSecurityGroupId>, _>>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
                e.value(),
            ))
        })?;

    // Log tenant organization ID if present
    if let Some(ref tenant_org_id) = req.tenant_organization_id {
        log_tenant_organization_id(tenant_org_id);
    }

    let tenant_organization_id = req
        .tenant_organization_id
        .map(|t| t.parse::<TenantOrganizationId>())
        .transpose()
        .map_err(|e: InvalidTenantOrg| {
            CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
        })?;

    // Prepare our txn to grab the NetworkSecurityGroups from the DB
    let mut txn = api.txn_begin().await?;

    // Make our DB query for the IDs to get our NetworkSecurityGroups
    let network_security_groups = network_security_group::find_by_ids(
        &mut txn,
        &network_security_group_ids,
        tenant_organization_id.as_ref(),
        false,
    )
    .await?;

    // Convert the list of internal NetworkSecurityGroup to a
    // list of proto message NetworkSecurityGroup to send back
    // in the response.

    let rpc_network_security_groups = network_security_groups
        .into_iter()
        .map(|i| i.try_into())
        .collect::<Result<Vec<rpc::NetworkSecurityGroup>, _>>()?;

    // Prepare the response message
    let rpc_out = rpc::FindNetworkSecurityGroupsByIdsResponse {
        network_security_groups: rpc_network_security_groups,
    };

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::UpdateNetworkSecurityGroupRequest>,
) -> Result<Response<rpc::UpdateNetworkSecurityGroupResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    // Get the target ID
    let id = req.id.parse::<NetworkSecurityGroupId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
            e.value(),
        ))
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

    // Prepare the desired rules list
    let (stateful_egress, rules) = {
        let attr = req.network_security_group_attributes.unwrap_or_default();

        (
            attr.stateful_egress,
            attr.rules
                .into_iter()
                .map(|r| r.try_into())
                .collect::<Result<Vec<_>, _>>()
                .map_err(CarbideError::from)?,
        )
    };

    let max_nsg_size = api
        .runtime_config
        .network_security_group
        .max_network_security_group_size as usize;

    validate_expanded_rule_set(&rules, max_nsg_size)?;

    // Log tenant organization ID from request
    log_tenant_organization_id(&req.tenant_organization_id);

    // Parse tenant organization ID
    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Start a new transaction for a db write.
    let mut txn = api.txn_begin().await?;

    // Look up the NetworkSecurityGroup.  We'll need to check the current
    // version. We could probably do everything with a single query
    // with a few subqueries, but we'd only be able to send back a
    // NotFound, leaving the caller with no way to know if it was
    // because their NetworkSecurityGroup wasn't found or because the version
    // didn't match.
    let current_network_security_group = network_security_group::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        Some(&tenant_organization_id),
        true,
    )
    .await?;

    // If we found more than one, the DB is corrupt.
    if current_network_security_group.len() > 1 {
        // CarbideError::FindOneReturnedManyResultsError expects a uuid,
        // and we've said we want to move away from uuid::Uuid
        return Err(CarbideError::Internal {
            message: format!("multiple NetworkSecurityGroup records found for '{id}'"),
        }
        .into());
    }

    // This could have been because group doesn't exist
    // OR because the tenant org ID was wrong.
    // Is there a better way to get the details back aside
    // from just stuffing the `id` field of the error?
    let current_network_security_group = match current_network_security_group.first() {
        Some(i) => i,
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "NetworkSecurityGroup",
                id: format!(
                    "{} for tenant org `{}`",
                    metadata.name.clone(),
                    req.tenant_organization_id.clone(),
                ),
            }
            .into());
        }
    };

    validate_stateful_egress_enablement(
        Some(current_network_security_group.stateful_egress),
        stateful_egress,
        api.runtime_config
            .network_security_group
            .stateful_acls_enabled,
    )?;

    // Prepare the version match if present.
    if let Some(if_version_match) = req.if_version_match {
        let target_version = if_version_match
            .parse::<ConfigVersion>()
            .map_err(CarbideError::from)?;

        if current_network_security_group.version != target_version {
            return Err(CarbideError::ConcurrentModificationError(
                "NetworkSecurityGroup",
                target_version.to_string(),
            )
            .into());
        }
    };

    // Update record in the DB and get back
    // our new NetworkSecurityGroup state.
    let network_security_group = network_security_group::update(
        &mut txn,
        &id,
        &tenant_organization_id,
        &metadata,
        stateful_egress,
        &rules,
        current_network_security_group.version,
        None,
    )
    .await?;

    // Prepare the response to send back
    let rpc_out = rpc::UpdateNetworkSecurityGroupResponse {
        network_security_group: Some(network_security_group.try_into()?),
    };

    // Commit our txn if nothing has gone wrong so far.
    txn.commit().await?;

    // Send our response back.
    Ok(Response::new(rpc_out))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::DeleteNetworkSecurityGroupRequest>,
) -> Result<Response<rpc::DeleteNetworkSecurityGroupResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let id = req.id.parse::<NetworkSecurityGroupId>().map_err(|e| {
        CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
            e.value(),
        ))
    })?;

    // Log tenant organization ID from request
    log_tenant_organization_id(&req.tenant_organization_id);

    // Parse tenant organization ID
    let tenant_organization_id =
        req.tenant_organization_id
            .parse()
            .map_err(|e: InvalidTenantOrg| {
                CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
            })?;

    // Prepare our txn to delete from the DB
    let mut txn = api.txn_begin().await?;

    // Make our DB query for the NetworkSecurityGroup.
    // This is mainly to get a row-level lock if the record exists
    // to allow other code to coordinate on NetworkSecurityGroup
    // records.
    // For example, code that updates the NSG of an instance or VPC
    // should grab at least a row-level lock for the NSG it wants to use.
    let nsg = network_security_group::find_by_ids(
        &mut txn,
        std::slice::from_ref(&id),
        // We'll check tenant ownership separately from the query here so we don't hide a
        // 404 due to a mismatched tenant.
        None,
        true,
    )
    .await?
    .pop();

    // Since we needed to query for the record anyway,
    // we can save ourselves some extra work if it didn't exist.
    let Some(nsg) = nsg else {
        return Err(CarbideError::NotFoundError {
            kind: "NetworkSecurityGroup",
            id: id.to_string(),
        }
        .into());
    };

    if nsg.tenant_organization_id != tenant_organization_id {
        return Err(CarbideError::InvalidArgument(format!(
            "NetworkSecurityGroup `{}` is not owned by tenant `{}`",
            nsg.id.clone(),
            tenant_organization_id.clone()
        ))
        .into());
    }

    // Look for any related objects that have this NSG attached.
    // If an NSG is in use, it must not be deleted.
    let existing_associated_objects = network_security_group::find_objects_with_attachments(
        &mut txn,
        Some(std::slice::from_ref(&id)),
        Some(&tenant_organization_id),
    )
    .await?
    .pop();

    if existing_associated_objects
        .map(|a| a.has_attachments())
        .unwrap_or_default()
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "NetworkSecurityGroup {id} is associated with active objects"
        ))
        .into());
    }

    // Make our DB query to soft delete the NetworkSecurityGroup
    let _id = network_security_group::soft_delete(&mut txn, &id, &tenant_organization_id).await?;

    // Prepare the response message
    let rpc_out = rpc::DeleteNetworkSecurityGroupResponse {};

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn get_propagation_status(
    api: &Api,
    request: Request<rpc::GetNetworkSecurityGroupPropagationStatusRequest>,
) -> Result<Response<rpc::GetNetworkSecurityGroupPropagationStatusResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.vpc_ids.len() + req.instance_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs combined can be submitted"
        ))
        .into());
    }

    if req.vpc_ids.is_empty() && req.instance_ids.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "at least one VPC ID or instance ID must be provided".to_string(),
        )
        .into());
    }

    let vpc_ids = req
        .vpc_ids
        .iter()
        .map(|v| v.parse::<VpcId>())
        .collect::<Result<Vec<VpcId>, _>>()
        .map_err(|e| CarbideError::from(RpcDataConversionError::InvalidVpcId(e.to_string())))?;

    let instance_ids = req
        .instance_ids
        .iter()
        .map(|i| i.parse::<InstanceId>())
        .collect::<Result<Vec<InstanceId>, _>>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidInstanceId(e.to_string()))
        })?;

    // Prepare our txn to associate machines with the NetworkSecurityGroup
    let mut txn = api.txn_begin().await?;

    // Query the DB for propagation status.
    let (vpcs, instances) = network_security_group::get_propagation_status(
        &mut txn,
        req.network_security_group_ids
            .map(|nl| {
                nl.ids
                    .iter()
                    .map(|v| v.parse::<NetworkSecurityGroupId>())
                    .collect::<Result<Vec<NetworkSecurityGroupId>, _>>()
            })
            .transpose()
            .map_err(|e| {
                CarbideError::from(RpcDataConversionError::InvalidInstanceId(e.to_string()))
            })?
            .as_deref(),
        None,
        Some(&vpc_ids),
        Some(&instance_ids),
    )
    .await?;

    // Prepare the response message
    let rpc_out = rpc::GetNetworkSecurityGroupPropagationStatusResponse {
        vpcs: vpcs.into_iter().map(|v| v.into()).collect(),
        instances: instances.into_iter().map(|v| v.into()).collect(),
    };

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

pub(crate) async fn get_attachments(
    api: &Api,
    request: Request<rpc::GetNetworkSecurityGroupAttachmentsRequest>,
) -> Result<Response<rpc::GetNetworkSecurityGroupAttachmentsResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if req.network_security_group_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be submitted"
        ))
        .into());
    }

    if req.network_security_group_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let network_security_group_ids = req
        .network_security_group_ids
        .iter()
        .map(|v| v.parse::<NetworkSecurityGroupId>())
        .collect::<Result<Vec<NetworkSecurityGroupId>, _>>()
        .map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
                e.value(),
            ))
        })?;

    let mut txn = api.txn_begin().await?;

    // Query the DB for propagation status.
    let attachments = network_security_group::find_objects_with_attachments(
        &mut txn,
        Some(&network_security_group_ids),
        None,
    )
    .await?;

    // Prepare the response message
    let rpc_out = rpc::GetNetworkSecurityGroupAttachmentsResponse {
        attachments: attachments.into_iter().map(|a| a.into()).collect(),
    };

    // Commit if nothing has gone wrong up to now
    txn.commit().await?;

    // Send our response back
    Ok(Response::new(rpc_out))
}

/// Rejects newly enabled stateful egress when the site does not support stateful ACLs.
fn validate_stateful_egress_enablement(
    current_stateful_egress: Option<bool>,
    requested_stateful_egress: bool,
    stateful_acls_enabled: bool,
) -> Result<(), CarbideError> {
    // Existing stateful groups remain editable if the site flag is disabled after creation.
    if !stateful_acls_enabled && requested_stateful_egress && current_stateful_egress != Some(true)
    {
        return Err(CarbideError::InvalidArgument(
            "stateful_egress requires network_security_group.stateful_acls_enabled to be true"
                .to_string(),
        ));
    }

    Ok(())
}

fn validate_expanded_rule_set(
    rules: &[NetworkSecurityGroupRule],
    limit: usize,
) -> Result<(), CarbideError> {
    let mut total_rules = 0u32;

    let mut ids = HashSet::<Option<String>>::new();

    if rules.len() > limit {
        return Err(CarbideError::InvalidArgument(format!(
            "expanded rule set contains more than {limit} maximum number of rules"
        )));
    }

    for rule in rules {
        if !ids.insert(rule.id.clone()) {
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate rule ID `{}` found in rule set",
                rule.id.clone().unwrap_or_default()
            )));
        }

        match (&rule.src_net, &rule.dst_net) {
            (NetworkSecurityGroupRuleNet::Prefix(_), NetworkSecurityGroupRuleNet::Prefix(_)) => {
                // Negative ranges are caught when we convert from rpc to internal struct.
                // so we can keep this simple.
                let rule_count = (rule.src_port_end.unwrap_or_default()
                    - rule.src_port_start.unwrap_or_default()
                    + 1)
                .saturating_mul(
                    rule.dst_port_end.unwrap_or_default() - rule.dst_port_start.unwrap_or_default()
                        + 1,
                );

                total_rules = match total_rules.overflowing_add(rule_count) {
                    (_, true) => {
                        return Err(CarbideError::InvalidArgument(format!(
                            "expanded rule set contains more than {limit} maximum number of rules"
                        )));
                    }
                    (v, false) => v,
                };

                if total_rules as usize > limit {
                    return Err(CarbideError::InvalidArgument(format!(
                        "expanded rule set contains more than {limit} maximum number of rules"
                    )));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    /// Verifies the validator rejects only new stateful intent while site support is disabled.
    ///
    /// Existing stateful groups must remain editable after an operator disables the site flag.
    #[test]
    fn stateful_egress_enablement_requires_site_support() {
        scenarios!(
            run = |(current, requested, site_enabled)| {
                validate_stateful_egress_enablement(current, requested, site_enabled).map_err(drop)
            };
            "site support disabled" {
                // Stateless NSGs remain supported when stateful ACLs are unavailable.
                (None, false, false) => Yields(()),
                // Creation must not persist stateful intent the site cannot provide.
                (None, true, false) => Fails,
                // A stateless NSG cannot become stateful while support is unavailable.
                (Some(false), true, false) => Fails,
                // Disabling the site flag must not prevent unrelated edits to existing stateful NSGs.
                (Some(true), true, false) => Yields(()),
            }

            "site support enabled" {
                // Stateful NSGs are valid when the site advertises the required capability.
                (None, true, true) => Yields(()),
            }
        );
    }
}
