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
use ::rpc::network::vpc_virtualization_type_try_from_rpc;
use carbide_network::virtualization::{DEFAULT_NETWORK_VIRTUALIZATION_TYPE, VpcVirtualizationType};
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use db::resource_pool::ResourcePoolDatabaseError;
use db::vpc::{self};
use db::{self, ObjectColumnFilter, network_security_group};
use model::resource_pool;
use model::tenant::{InvalidTenantOrg, Tenant};
use model::vpc::{
    NewVpc, UpdateVpc, UpdateVpcVirtualization, VpcRoutingProfileOverrides, VpcStatus,
    VpcVirtualizationTypeCapabilities,
};
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::cfg::file::FnnConfig;

pub(crate) async fn create(
    api: &Api,
    request: Request<rpc::VpcCreationRequest>,
) -> Result<Response<rpc::Vpc>, Status> {
    log_request_data(&request);
    let vpc_creation_request = request.get_ref();

    let mut txn = api.txn_begin().await?;

    // Grab the tenant details and a row-lock if found so we can coordinate around the tenant record.
    // Missing tenants remain supported for non-FNN VPCs; FNN creation rejects them below after
    // resolving the requested virtualization type.
    let tenant =
        db::tenant::find(&vpc_creation_request.tenant_organization_id, true, &mut txn).await?;

    if let Some(ref nsg_id) = vpc_creation_request.network_security_group_id {
        let id = nsg_id.parse::<NetworkSecurityGroupId>().map_err(|e| {
            CarbideError::from(RpcDataConversionError::InvalidNetworkSecurityGroupId(
                e.value(),
            ))
        })?;

        // Query to check the validity of the NSG ID but to also grab
        // a row-level lock on it if it exists.
        if network_security_group::find_by_ids(
            &mut txn,
            std::slice::from_ref(&id),
            Some(
                &vpc_creation_request
                    .tenant_organization_id
                    .parse()
                    .map_err(|e: InvalidTenantOrg| {
                        CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
                    })?,
            ),
            true,
        )
        .await?
        .pop()
        .is_none()
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "NetworkSecurityGroup `{}` does not exist or is not owned by tenant `{}`",
                id, vpc_creation_request.tenant_organization_id,
            ))
            .into());
        }
    }

    // Resolve the virtualization type up front. Flat VPCs short-circuit
    // most of the FNN-flavored routing-profile validation below: Flat doesn't
    // have a NICo-managed data plane, so routing-profile semantics don't
    // apply. We still allocate a VNI and persist the VPC like any other type.
    let requested_virtualization_type = match vpc_creation_request.network_virtualization_type {
        None => DEFAULT_NETWORK_VIRTUALIZATION_TYPE,
        Some(v) => vpc_virtualization_type_try_from_rpc(v).map_err(CarbideError::from)?,
    };

    // FNN routing policy requires tenant context for inheritance and authorization.
    if requested_virtualization_type == VpcVirtualizationType::Fnn && tenant.is_none() {
        return Err(CarbideError::FailedPrecondition(format!(
            "tenant `{}` must exist before creating an FNN VPC",
            vpc_creation_request.tenant_organization_id
        ))
        .into());
    }

    // Non-FNN VPC creation retains the legacy missing-tenant behavior.
    if tenant.is_none() {
        tracing::warn!(
            tenant_organization_id = vpc_creation_request.tenant_organization_id.clone(),
            "Database record for tenant ID in VPC creation request not found"
        );
    }

    if vpc_creation_request.routing_profile_type.is_some()
        || vpc_creation_request.routing_profile_overrides.is_some()
    {
        requested_virtualization_type
            .ensure_supports_routing_profiles()
            .map_err(CarbideError::from)?;
    }

    if vpc_creation_request.slaac_enabled == Some(true) {
        requested_virtualization_type
            .ensure_supports_slaac()
            .map_err(CarbideError::from)?;
    }

    let requested_profile_type = vpc_creation_request.routing_profile_type.clone();
    let mut new_vpc = NewVpc::try_from(request.into_inner())?;

    let ResolvedVpcRouting {
        profile_type: resolved_profile_type,
        internal,
    } = resolve_vpc_routing(
        requested_virtualization_type,
        requested_profile_type.as_deref(),
        new_vpc.routing_profile_overrides.as_ref(),
        tenant.as_ref(),
        api.runtime_config.fnn.as_ref(),
        &new_vpc.tenant_organization_id,
    )?;

    let vni = Some(
        allocate_vpc_vni(
            api,
            &mut txn,
            &new_vpc.id.to_string(),
            internal,
            new_vpc.vni,
        )
        .await?,
    );

    new_vpc.routing_profile_type = resolved_profile_type;

    let vpc = db::vpc::persist(new_vpc, VpcStatus { vni }, &mut txn).await?;

    let rpc_out = vpc_to_rpc(vpc, api.runtime_config.fnn.as_ref());

    txn.commit().await?;

    Ok(Response::new(rpc_out))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<rpc::VpcUpdateRequest>,
) -> Result<Response<rpc::VpcUpdateResult>, Status> {
    log_request_data(&request);
    // Preserve operator errors previously returned by handler-level validation
    // without changing how unrelated request-conversion errors are represented.
    let vpc_update = UpdateVpc::try_from(request.into_inner()).map_err(|error| match error {
        RpcDataConversionError::MissingArgument("id") => {
            CarbideError::InvalidArgument("VPC ID is required".to_string()).into()
        }
        error @ RpcDataConversionError::InvalidNetworkSecurityGroupId(_) => {
            CarbideError::from(error).into()
        }
        error => Status::from(error),
    })?;

    let mut txn = api.txn_begin().await?;

    // Security-group and routing-profile changes both require validation
    // against the VPC's persisted tenant and virtualization type.
    if vpc_update.network_security_group_id.is_some()
        || vpc_update.routing_profile_overrides.is_some()
        || vpc_update.power_resource_group.is_some()
    {
        let Some(vpc) = db::vpc::find_by(
            &mut txn,
            ObjectColumnFilter::One(vpc::IdColumn, &vpc_update.id),
        )
        .await?
        .pop() else {
            return Err(CarbideError::NotFoundError {
                kind: "Vpc",
                id: vpc_update.id.to_string(),
            }
            .into());
        };

        // Validate ownership while taking a row lock on the security group.
        if let Some(ref network_security_group_id) = vpc_update.network_security_group_id
            && network_security_group::find_by_ids(
                &mut txn,
                std::slice::from_ref(network_security_group_id),
                Some(&vpc.config.tenant_organization_id.parse().map_err(
                    |e: InvalidTenantOrg| {
                        CarbideError::from(RpcDataConversionError::InvalidTenantOrg(e.to_string()))
                    },
                )?),
                true,
            )
            .await?
            .pop()
            .is_none()
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "NetworkSecurityGroup `{network_security_group_id}` does not exist or is not owned by tenant `{}`",
                vpc.config.tenant_organization_id
            ))
            .into());
        }

        // Only FNN data planes can consume the persisted routing policy.
        if let Some(routing_profile_overrides) = vpc_update.routing_profile_overrides.as_ref() {
            vpc.config
                .network_virtualization_type
                .ensure_supports_routing_profiles()
                .map_err(CarbideError::from)?;

            // Inline policy is meaningful only when the current runtime
            // configuration can supply the VPC's named base profile.
            let (Some(fnn), Some(_)) = (
                api.runtime_config.fnn.as_ref(),
                vpc.config.routing_profile_type.as_ref(),
            ) else {
                return Err(CarbideError::FailedPrecondition(
                    "FNN configuration and a named VPC routing profile are required to update routing-profile overrides"
                        .to_string(),
                )
                .into());
            };

            let mut candidate_config = vpc.config.clone();
            candidate_config.routing_profile_overrides = Some(routing_profile_overrides.clone());
            fnn.resolve_vpc_routing_profile(&candidate_config)?;
        }
    }

    // Note: Because VNI allocation happens on creation and depends on the routing profile type,
    // we can't allow VPCs to change routing profiles unless we also release and re-allocate their VNIs.
    // It's better to keep the property immutable.

    let vpc = db::vpc::update(&vpc_update, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::VpcUpdateResult {
        vpc: Some(vpc_to_rpc(vpc, api.runtime_config.fnn.as_ref())),
    }))
}

/// Releases the operator-selected inactive allocation without changing VPC configuration.
pub(crate) async fn release_inactive_vni(
    api: &Api,
    request: Request<rpc::VpcReleaseInactiveVniRequest>,
) -> Result<Response<rpc::VpcReleaseInactiveVniResult>, Status> {
    log_request_data(&request);
    let request = request.into_inner();
    let vpc_id = request.id.ok_or(CarbideError::MissingArgument("id"))?;
    let version = request
        .if_version_match
        .ok_or(CarbideError::MissingArgument("if_version_match"))?;
    let expected_version = version
        .parse::<ConfigVersion>()
        .map_err(|_| CarbideError::from(RpcDataConversionError::InvalidConfigVersion(version)))?;
    let expected_inactive_vni = request
        .expected_inactive_vni
        .ok_or(CarbideError::MissingArgument("expected_inactive_vni"))?;
    if !(1..=0x00ff_ffff).contains(&expected_inactive_vni) {
        return Err(CarbideError::InvalidArgument(
            "expected_inactive_vni must be between 1 and 16777215".to_string(),
        )
        .into());
    }

    let mut txn = api.txn_begin().await?;
    let mut vpc = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(vpc::IdColumn, &vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "Vpc",
        id: vpc_id.to_string(),
    })?;

    // Check the original version before inspecting allocations: repeating a
    // committed request must not release a subsequently retained allocation.
    if vpc.version != expected_version {
        return Err(
            CarbideError::ConcurrentModificationError("vpc", expected_version.to_string()).into(),
        );
    }

    let released_inactive_vni =
        release_inactive_vpc_vni(api, &mut txn, &vpc, expected_inactive_vni)
            .await?
            .try_into()
            .map_err(|_| {
                CarbideError::internal(
                    "released VPC VNI cannot be represented by the RPC API".to_string(),
                )
            })?;
    vpc.version = db::vpc::increment_vpc_version(&mut txn, vpc_id, expected_version).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::VpcReleaseInactiveVniResult {
        vpc: Some(vpc_to_rpc(vpc, api.runtime_config.fnn.as_ref())),
        released_inactive_vni,
    }))
}

async fn release_inactive_vpc_vni(
    api: &Api,
    txn: &mut PgConnection,
    vpc: &model::vpc::Vpc,
    expected_inactive_vni: u32,
) -> Result<i32, CarbideError> {
    let active_vni = vpc.status.vni.ok_or_else(|| {
        CarbideError::FailedPrecondition(format!("VPC `{}` does not have an active VNI", vpc.id))
    })?;

    let internal_pool = &api.common_pools.ethernet.pool_vpc_vni;
    let external_pool = &api.common_pools.ethernet.pool_external_vpc_vni;
    let owner_id = vpc.id.to_string();
    let internal_vni = db::resource_pool::find_owned_allocation(
        internal_pool,
        txn,
        resource_pool::OwnerType::Vpc,
        &owner_id,
    )
    .await
    .map_err(db::DatabaseError::from)?;
    let external_vni = db::resource_pool::find_owned_allocation(
        external_pool,
        txn,
        resource_pool::OwnerType::Vpc,
        &owner_id,
    )
    .await
    .map_err(db::DatabaseError::from)?;

    let (inactive_pool, inactive_vni) = match (internal_vni, external_vni) {
        (Some(internal_vni), Some(external_vni))
            if internal_vni == active_vni && external_vni != active_vni =>
        {
            (external_pool, external_vni)
        }
        (Some(internal_vni), Some(external_vni))
            if external_vni == active_vni && internal_vni != active_vni =>
        {
            (internal_pool, internal_vni)
        }
        (Some(internal_vni), None) if internal_vni == active_vni => {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC `{}` does not have an inactive VNI allocation (active VNI `{active_vni}`)",
                vpc.id,
            )));
        }
        (None, Some(external_vni)) if external_vni == active_vni => {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC `{}` does not have an inactive VNI allocation (active VNI `{active_vni}`)",
                vpc.id,
            )));
        }
        _ => {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC `{}` has inconsistent VNI allocations: active VNI `{active_vni}`, internal allocation {internal_vni:?}, external allocation {external_vni:?}",
                vpc.id,
            )));
        }
    };

    if i64::from(inactive_vni) != i64::from(expected_inactive_vni) {
        return Err(CarbideError::FailedPrecondition(format!(
            "VPC `{}` has inactive VNI `{inactive_vni}`, not expected VNI `{expected_inactive_vni}`",
            vpc.id,
        )));
    }

    // The ownership lookup holds this allocation's row lock through commit,
    // so releasing the checked value cannot free another owner's allocation.
    db::resource_pool::release(inactive_pool, txn, inactive_vni).await?;

    Ok(inactive_vni)
}

pub(crate) async fn update_virtualization(
    api: &Api,
    request: Request<rpc::VpcUpdateVirtualizationRequest>,
) -> Result<Response<rpc::VpcUpdateVirtualizationResult>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let updater = UpdateVpcVirtualization::try_from(request.into_inner())?;

    // Serialize this transition with VpcPrefix creation. A tenant-managed
    // SitePrefix can be attached only to FNN, so the VPC must remain FNN for
    // as long as any such VpcPrefix is retained (including soft deletion).
    let current_vpc = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &updater.id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "vpc",
        id: updater.id.to_string(),
    })?;
    if current_vpc.config.slaac_enabled {
        updater
            .network_virtualization_type
            .ensure_supports_slaac()
            .map_err(|error| CarbideError::FailedPrecondition(error.to_string()))?;
    }
    if updater.network_virtualization_type != VpcVirtualizationType::Fnn
        && db::vpc_prefix::has_tenant_managed_site_prefix(&mut txn, current_vpc.id).await?
    {
        return Err(CarbideError::FailedPrecondition(
            "a VPC with tenant-managed SitePrefix address space must remain FNN".to_string(),
        )
        .into());
    }

    let instances = db::instance::find_ids(
        &mut txn,
        model::instance::InstanceSearchFilter {
            label: None,
            tenant_org_id: None,
            vpc_id: Some(updater.id.to_string()),
            instance_type_id: None,
        },
    )
    .await?;

    if !instances.is_empty() {
        return Err(CarbideError::internal(format!(
            "cannot modify VPC virtualization type in VPC with existing instances (found: {})",
            instances.len()
        ))
        .into());
    }
    db::vpc::update_virtualization(&updater, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::VpcUpdateVirtualizationResult {}))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<rpc::VpcDeletionRequest>,
) -> Result<Response<rpc::VpcDeletionResult>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    // TODO: This needs to validate that nothing references the VPC anymore
    // (like NetworkSegments)
    let vpc_id: VpcId = request
        .into_inner()
        .id
        .ok_or(CarbideError::MissingArgument("id"))?;

    let vpc = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "vpc",
        id: vpc_id.to_string(),
    })?;

    // An empty VPC can still have peer DPUs importing its previous VNI.
    // Deletion must not bypass the operator's explicit inactive-VNI release.
    let owner_id = vpc_id.to_string();
    let mut owned_allocation = None;
    for pool in [
        &api.common_pools.ethernet.pool_vpc_vni,
        &api.common_pools.ethernet.pool_external_vpc_vni,
    ] {
        if let Some(vni) = db::resource_pool::find_owned_allocation(
            pool,
            &mut txn,
            resource_pool::OwnerType::Vpc,
            &owner_id,
        )
        .await
        .map_err(db::DatabaseError::from)?
        {
            if owned_allocation.is_some() || Some(vni) != vpc.status.vni {
                return Err(CarbideError::FailedPrecondition(format!(
                    "VPC `{vpc_id}` has an inactive or inconsistent VNI allocation; release the inactive VNI before deleting the VPC",
                )).into());
            }
            owned_allocation = Some((pool, vni));
        }
    }

    if db::vpc::try_delete(&mut txn, vpc_id).await?.is_none() {
        // Release an allocation only when this transaction deleted the VPC.
        return Err(CarbideError::NotFoundError {
            kind: "vpc",
            id: vpc_id.to_string(),
        }
        .into());
    }

    if let Some((pool, vni)) = owned_allocation {
        // The ownership lookup above keeps the allocation locked until commit.
        db::resource_pool::release(pool, &mut txn, vni).await?;
    }

    // Delete associated VPC peerings
    db::vpc_peering::delete_by_vpc_id(&mut txn, vpc_id).await?;

    txn.commit().await?;

    Ok(Response::new(rpc::VpcDeletionResult {}))
}

pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::VpcSearchFilter>,
) -> Result<Response<rpc::VpcIdList>, Status> {
    log_request_data(&request);

    let filter: model::vpc::VpcSearchFilter = request.into_inner().into();

    let vpc_ids = db::vpc::find_ids(&api.database_connection, filter).await?;

    Ok(Response::new(rpc::VpcIdList { vpc_ids }))
}

pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::VpcsByIdsRequest>,
) -> Result<Response<rpc::VpcList>, Status> {
    log_request_data(&request);

    let vpc_ids = request.into_inner().vpc_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if vpc_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if vpc_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let db_vpcs = db::vpc::find_by(
        &api.database_connection,
        ObjectColumnFilter::List(vpc::IdColumn, &vpc_ids),
    )
    .await;

    let result = db_vpcs
        .map(|vpc| rpc::VpcList {
            vpcs: vpc
                .into_iter()
                .map(|vpc| vpc_to_rpc(vpc, api.runtime_config.fnn.as_ref()))
                .collect(),
        })
        .map(Response::new)?;

    Ok(result)
}

/// Converts a persisted VPC to RPC and populates its runtime-derived effective routing profile.
///
/// The effective profile is omitted when routing profiles are unsupported, FNN is disabled, or
/// the named runtime profile cannot be resolved.
fn vpc_to_rpc(vpc: model::vpc::Vpc, fnn_config: Option<&FnnConfig>) -> rpc::Vpc {
    let effective_routing_profile = if vpc
        .config
        .network_virtualization_type
        .supports_routing_profiles()
    {
        fnn_config.and_then(|fnn| {
            // A persisted VPC can outlive its named runtime profile. In that
            // case the status remains available without an effective profile.
            fnn.resolve_vpc_routing_profile(&vpc.config)
                .ok()
                .map(|profile| rpc::VpcEffectiveRoutingProfile::from(profile.as_ref()))
        })
    } else {
        None
    };

    let mut rpc_vpc = rpc::Vpc::from(vpc);
    rpc_vpc
        .status
        .get_or_insert_default()
        .effective_routing_profile = effective_routing_profile;
    rpc_vpc
}

/// Allocate a value from the vpc vni resource pool.
///
/// If the pool exists but is empty or has en error, return that.
async fn allocate_vpc_vni(
    api: &Api,
    txn: &mut PgConnection,
    owner_id: &str,
    internal: bool,
    requested_vni: Option<i32>,
) -> Result<i32, CarbideError> {
    // If FNN is not configured, then there is no distinction between internal
    // and external tenants: they're all internal.  This matches how things are
    // deployed today.

    let source_pool = if internal {
        &api.common_pools.ethernet.pool_vpc_vni
    } else {
        &api.common_pools.ethernet.pool_external_vpc_vni
    };

    match (
        db::resource_pool::allocate(
            source_pool,
            txn,
            resource_pool::OwnerType::Vpc,
            owner_id,
            requested_vni,
        )
        .await,
        requested_vni,
    ) {
        (Ok(val), _) => Ok(val),
        (
            Err(
                error @ ResourcePoolDatabaseError::ResourcePool(
                    resource_pool::ResourcePoolError::Empty,
                ),
            ),
            requested_vni,
        ) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_vni.is_some(),
                source_pool.name(),
                &error,
            );
            Err(CarbideError::ResourceExhausted(format!(
                "pool {}",
                source_pool.name
            )))
        }
        (Err(error), Some(requested_vni))
            if db::resource_pool::is_requested_value_unavailable(&error) =>
        {
            db::resource_pool::emit_requested_vni_unavailable(
                source_pool.value_type,
                owner_id,
                requested_vni,
                source_pool.name(),
            );
            Err(CarbideError::FailedPrecondition(format!(
                "VNI `{}` cannot be requested or is already allocated",
                requested_vni
            )))
        }
        (Err(ResourcePoolDatabaseError::Database(error)), Some(_)) => {
            db::resource_pool::emit_database_allocation_failure(
                source_pool.value_type,
                owner_id,
                true,
                source_pool.name(),
                &error,
            );
            Err((*error).into())
        }
        (Err(err), requested_vni) => {
            db::resource_pool::emit_allocation_failure(
                source_pool.value_type,
                owner_id,
                requested_vni.is_some(),
                source_pool.name(),
                &err,
            );
            Err(err.into())
        }
    }
}

/// Resolution of routing-related state for a VPC at create time. The
/// `internal` flag isn't strictly part of the routing profile, but it
/// gets decided together with `profile_type` from the same inputs
/// (request + tenant + site FNN config), so we return both as one
/// value.
#[derive(Debug)]
struct ResolvedVpcRouting {
    /// The routing-profile-type name to persist on the VPC. `None`
    /// for VPC types without a NICo-managed data plane, or when
    /// neither the request nor the tenant supplies one.
    profile_type: Option<String>,

    /// Whether the VPC is "internal" -- drives VNI pool selection
    /// (`vpc-vni` internal pool vs `external-vpc-vni` external pool)
    /// and a couple of downstream behaviors.
    internal: bool,
}

impl Default for ResolvedVpcRouting {
    /// Default resolution for VPC types that don't accept a
    /// `routing_profile_type` field (Flat today). `profile_type` is
    /// `None` because there's nothing to resolve. `internal` carries
    /// the default value the VNI allocator should pool from -- it IS
    /// part of the routing-profile concept (every profile has an
    /// `internal: bool`), but in the no-profile case we pick a
    /// conservative default since the field still has to flow
    /// downstream to the VNI pool selector.
    ///
    /// TODO(chet): Consider switching callers to
    /// `Option<ResolvedVpcRouting>` so the no-profile case doesn't
    /// silently masquerade as "internal."
    fn default() -> Self {
        Self {
            profile_type: None,
            internal: true,
        }
    }
}

/// Resolves the routing-profile and `internal` flag for a VPC create
/// request from (1) the VPC's virtualization type's capabilities,
/// (2) the request's `routing_profile_type` and inline overrides,
/// (3) the tenant's `routing_profile_type`, and (4) the site's FNN
/// config. Surfaces any contradictions as [`CarbideError`].
///
/// This exists as a function so that resolution rules can be
/// more easily unit-tested directly, vs. as part of a wider
/// flow.
fn resolve_vpc_routing(
    virt_type: VpcVirtualizationType,
    requested_profile_type: Option<&str>,
    vpc_profile_overrides: Option<&VpcRoutingProfileOverrides>,
    tenant: Option<&Tenant>,
    fnn_config: Option<&FnnConfig>,
    organization_id: &str,
) -> Result<ResolvedVpcRouting, CarbideError> {
    // Only VPC types that use routing profiles (FNN today) run the
    // full resolution. ETV and Flat short-circuit to the default --
    // no profile stored, `internal: true` so VNI allocation lands in
    // the internal pool. The REST API at
    // `infra-controller-rest/api/pkg/api/handler/vpc.go` rejects
    // `routingProfile` on non-FNN creates upstream; this short-circuit
    // is the defense-in-depth gate at the carbide-core layer.
    if !virt_type.supports_routing_profiles() {
        return Ok(ResolvedVpcRouting::default());
    }

    let tenant_profile_type = tenant.and_then(|t| t.routing_profile_type.as_deref());

    match (requested_profile_type, tenant_profile_type) {
        // Every FNN VPC needs a tenant profile to establish its named routing policy and
        // authorize any explicitly requested profile or inline overrides.
        (_, None) => Err(CarbideError::FailedPrecondition(format!(
            "tenant `{organization_id}` must have a routing profile before creating an FNN VPC"
        ))),

        // Tenant has a routing profile; resolve the request against it.
        (request_profile_type, Some(tenant_profile_type)) => {
            match fnn_config {
                // Explicit profile properties require FNN configuration.
                None if request_profile_type.is_some() || vpc_profile_overrides.is_some() => {
                    Err(CarbideError::FailedPrecondition(
                        "FNN configuration required to request routing-profile for VPCs"
                            .to_string(),
                    ))
                }

                // FNN disabled with no explicit request: inherit the
                // tenant's profile name; force `internal=true` (legacy
                // pre-FNN behavior).
                None => Ok(ResolvedVpcRouting {
                    profile_type: Some(tenant_profile_type.to_owned()),
                    internal: true,
                }),

                // Resolve the selected base and enforce its tenant access
                // boundary. Inline VPC properties cannot change access tiers.
                Some(fnn) => {
                    let profile_type = request_profile_type.unwrap_or(tenant_profile_type);
                    let base_profile = fnn.routing_profiles.get(profile_type).ok_or_else(|| {
                        CarbideError::NotFoundError {
                            kind: "routing_profile",
                            id: profile_type.to_owned(),
                        }
                    })?;
                    let tenant_profile =
                        fnn.routing_profiles
                            .get(tenant_profile_type)
                            .ok_or_else(|| CarbideError::NotFoundError {
                                kind: "routing_profile",
                                id: tenant_profile_type.to_owned(),
                            })?;
                    if base_profile.access_tier.unwrap_or_default()
                        < tenant_profile.access_tier.unwrap_or_default()
                    {
                        return Err(CarbideError::FailedPrecondition(
                            "requested VPC routing-profile access tier is broader than associated tenant routing-profile access tier"
                                .to_string(),
                        ));
                    }
                    Ok(ResolvedVpcRouting {
                        profile_type: Some(profile_type.to_owned()),
                        internal: base_profile.internal.unwrap_or_default(),
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;
    use config_version::ConfigVersion;
    use model::metadata::Metadata;

    use super::*;
    use crate::cfg::file::FnnRoutingProfileConfig;

    fn tenant_with_profile(profile: Option<&str>) -> Tenant {
        Tenant {
            organization_id: "test-org".parse().unwrap(),
            routing_profile_type: profile.map(|s| s.to_string()),
            metadata: Metadata::new_with_default_name(),
            version: ConfigVersion::initial(),
        }
    }

    fn fnn_with_profiles(profiles: &[(&str, FnnRoutingProfileConfig)]) -> FnnConfig {
        FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: profiles
                .iter()
                .map(|(name, profile)| ((*name).to_string(), profile.clone()))
                .collect::<HashMap<_, _>>(),
            use_vpc_vrf_loopback: false,
        }
    }

    fn profile(internal: bool, access_tier: u32) -> FnnRoutingProfileConfig {
        FnnRoutingProfileConfig {
            internal: Some(internal),
            access_tier: Some(access_tier),
            ..Default::default()
        }
    }

    struct RoutingResolutionInput {
        network_virtualization_type: VpcVirtualizationType,
        requested_profile_type: Option<&'static str>,
        routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
        tenant: Option<Tenant>,
        fnn_config: Option<FnnConfig>,
    }
    type RoutingResolution = (Option<String>, bool);

    #[derive(Debug, PartialEq, Eq)]
    enum RoutingResolutionFailure {
        FailedPrecondition,
        NotFound(&'static str),
        Unexpected(String),
    }

    fn resolve_routing_case(
        input: RoutingResolutionInput,
    ) -> Result<RoutingResolution, RoutingResolutionFailure> {
        resolve_vpc_routing(
            input.network_virtualization_type,
            input.requested_profile_type,
            input.routing_profile_overrides.as_ref(),
            input.tenant.as_ref(),
            input.fnn_config.as_ref(),
            "test-org",
        )
        .map(|resolved| (resolved.profile_type, resolved.internal))
        .map_err(|error| match error {
            CarbideError::FailedPrecondition(_) => RoutingResolutionFailure::FailedPrecondition,
            CarbideError::NotFoundError { kind, .. } => RoutingResolutionFailure::NotFound(kind),
            error => RoutingResolutionFailure::Unexpected(error.to_string()),
        })
    }

    #[test]
    fn resolve_vpc_routing_scenarios() {
        use RoutingResolutionFailure::{FailedPrecondition, NotFound};
        use VpcVirtualizationType::{Flat, Fnn};

        scenarios!(resolve_routing_case:
            "VPC types without routing-profile support short-circuit" {
                // Flat VPCs ignore all routing-profile inputs and retain the internal allocation
                // default because they have no NICo-managed routing profile.
                RoutingResolutionInput {
                    network_virtualization_type: Flat,
                    requested_profile_type: Some("EXTERNAL"),
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("INTERNAL"))),
                    fnn_config: Some(fnn_with_profiles(&[
                        ("EXTERNAL", profile(false, 2)),
                        ("INTERNAL", profile(true, 1)),
                    ])),
                } => Yields((None, true)),
            }

            "missing tenant routing profile" {
                // An FNN VPC cannot use the legacy internal default without a named tenant
                // profile because downstream policy resolution requires that name.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: None,
                    tenant: None,
                    fnn_config: None,
                } => FailsWith(FailedPrecondition),
                // Enabling FNN cannot make missing tenant routing context resolvable.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: None,
                    tenant: None,
                    fnn_config: Some(fnn_with_profiles(&[])),
                } => FailsWith(FailedPrecondition),
                // A named profile request needs tenant context to authorize its access tier.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: Some("EXTERNAL"),
                    routing_profile_overrides: None,
                    tenant: None,
                    fnn_config: None,
                } => FailsWith(FailedPrecondition),
                // Inline properties need tenant context to select and authorize their base
                // profile, even when no profile name is requested explicitly.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: Some(VpcRoutingProfileOverrides::default()),
                    tenant: None,
                    fnn_config: None,
                } => FailsWith(FailedPrecondition),
            }

            "FNN configuration disabled" {
                // An explicit profile cannot be resolved when its FNN definition is unavailable.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: Some("EXTERNAL"),
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("INTERNAL"))),
                    fnn_config: None,
                } => FailsWith(FailedPrecondition),
                // Inline properties cannot be applied without an FNN base-profile definition.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: Some(VpcRoutingProfileOverrides::default()),
                    tenant: Some(tenant_with_profile(Some("INTERNAL"))),
                    fnn_config: None,
                } => FailsWith(FailedPrecondition),
                // With no explicit VPC routing properties, preserve pre-FNN behavior by
                // inheriting the tenant's stored profile name and forcing internal allocation.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("INTERNAL"))),
                    fnn_config: None,
                } => Yields((Some("INTERNAL".to_string()), true)),
            }

            "tenant profile inheritance" {
                // With FNN enabled and no explicit request, inherit both the tenant profile name
                // and its internal-allocation policy.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("EXTERNAL"))),
                    fnn_config: Some(fnn_with_profiles(&[("EXTERNAL", profile(false, 2))])),
                } => Yields((Some("EXTERNAL".to_string()), false)),
                // Inline properties overlay the tenant's base profile but cannot change its
                // protected internal-allocation policy.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: Some(VpcRoutingProfileOverrides::default()),
                    tenant: Some(tenant_with_profile(Some("INTERNAL"))),
                    fnn_config: Some(fnn_with_profiles(&[("INTERNAL", profile(true, 1))])),
                } => Yields((Some("INTERNAL".to_string()), true)),
            }

            "requested profile access tier" {
                // An ADMIN tenant may select a narrower EXTERNAL routing profile.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: Some("EXTERNAL"),
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("ADMIN"))),
                    fnn_config: Some(fnn_with_profiles(&[
                        ("ADMIN", profile(true, 0)),
                        ("EXTERNAL", profile(false, 2)),
                    ])),
                } => Yields((Some("EXTERNAL".to_string()), false)),
                // An EXTERNAL tenant may not broaden its access by selecting ADMIN.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: Some("ADMIN"),
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("EXTERNAL"))),
                    fnn_config: Some(fnn_with_profiles(&[
                        ("EXTERNAL", profile(false, 2)),
                        ("ADMIN", profile(true, 0)),
                    ])),
                } => FailsWith(FailedPrecondition),
            }

            "unknown named profile" {
                // Reject an explicit profile name that has no runtime FNN definition.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: Some("DOES_NOT_EXIST"),
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("EXTERNAL"))),
                    fnn_config: Some(fnn_with_profiles(&[("EXTERNAL", profile(false, 2))])),
                } => FailsWith(NotFound("routing_profile")),
                // Reject an unresolved tenant base profile even when the VPC does not request a
                // different profile explicitly.
                RoutingResolutionInput {
                    network_virtualization_type: Fnn,
                    requested_profile_type: None,
                    routing_profile_overrides: None,
                    tenant: Some(tenant_with_profile(Some("UNDEFINED"))),
                    fnn_config: Some(fnn_with_profiles(&[("EXTERNAL", profile(false, 2))])),
                } => FailsWith(NotFound("routing_profile")),
            }
        );
    }
}
