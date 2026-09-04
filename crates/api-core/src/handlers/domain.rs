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
use ::rpc::protos::dns::{
    CreateDomainRequest, Domain, DomainDeletionRequest, DomainDeletionResult, DomainList,
    DomainSearchQuery, UpdateDomainRequest,
};
use carbide_uuid::domain::DomainId;
use db::dns::domain;
use db::{self, ObjectColumnFilter};
use model::dns::NewDomain;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

/// Locks every reverse-zone identity affected by a domain write, then rejects
/// a proposed name already used by another live domain. Updates may retain
/// their own normalized name, while creates have no domain to exclude.
async fn lock_and_ensure_reverse_zone_name_available(
    txn: &mut db::Transaction<'_>,
    names_to_lock: &[String],
    proposed_name: &str,
    current_domain_id: Option<DomainId>,
) -> Result<(), CarbideError> {
    db::dns::lock_reverse_zone_names(txn, names_to_lock).await?;

    let Some(reverse_zone_name) = db::dns::normalize_reverse_zone_name(proposed_name) else {
        return Ok(());
    };
    let domains = domain::find_reverse_zone_by_normalized_name(txn, &reverse_zone_name).await?;
    if domains
        .iter()
        .any(|existing| Some(existing.id) != current_domain_id)
    {
        return Err(CarbideError::AlreadyFoundError {
            kind: "reverse DNS zone",
            id: reverse_zone_name,
        });
    }

    Ok(())
}

pub(crate) async fn create(
    api: &Api,
    request: Request<CreateDomainRequest>,
) -> Result<Response<Domain>, Status> {
    crate::api::log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    lock_and_ensure_reverse_zone_name_available(
        &mut txn,
        std::slice::from_ref(&req.name),
        &req.name,
        None,
    )
    .await?;
    let new_domain = NewDomain::new(req.name);

    let domain = domain::persist(new_domain, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(Domain::from(domain)))
}

pub(crate) async fn update(
    api: &Api,
    request: Request<UpdateDomainRequest>,
) -> Result<Response<Domain>, Status> {
    crate::api::log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    let domain_proto = req
        .domain
        .ok_or_else(|| CarbideError::MissingArgument("domain"))?;

    let uuid = domain_proto
        .id
        .ok_or_else(|| CarbideError::MissingArgument("id"))?;

    let mut domain =
        domain::find_by_uuid(&mut txn, uuid)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "domain",
                id: uuid.to_string(),
            })?;

    let previous_name = domain.name.clone();
    domain.name = domain_proto.name;

    let reverse_zone_names = [previous_name, domain.name.clone()];
    lock_and_ensure_reverse_zone_name_available(
        &mut txn,
        &reverse_zone_names,
        &domain.name,
        Some(domain.id),
    )
    .await?;

    domain.increment_serial();

    let updated_domain = domain::update(&domain, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(Domain::from(updated_domain)))
}

pub(crate) async fn delete(
    api: &Api,
    request: Request<DomainDeletionRequest>,
) -> Result<Response<DomainDeletionResult>, Status> {
    crate::api::log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    let uuid = req.id.ok_or_else(|| CarbideError::MissingArgument("id"))?;

    let domain = domain::find_by_uuid_for_delete(txn.as_mut(), uuid)
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "domain",
            id: uuid.to_string(),
        })?;

    if domain.deleted.is_some() {
        txn.commit().await?;
        return Ok(Response::new(DomainDeletionResult {}));
    }

    db::dns::lock_reverse_zone_names(&mut txn, std::slice::from_ref(&domain.name)).await?;

    if domain::has_live_references(txn.as_mut(), uuid).await? {
        return Err(CarbideError::FailedPrecondition(format!(
            "domain {uuid} is still referenced by a network segment or machine interface"
        ))
        .into());
    }

    domain::delete(domain, &mut txn).await?;

    txn.commit().await?;

    Ok(Response::new(DomainDeletionResult {}))
}

pub(crate) async fn find(
    api: &Api,
    request: Request<DomainSearchQuery>,
) -> Result<Response<DomainList>, Status> {
    crate::api::log_request_data(&request);

    let DomainSearchQuery { id, name, .. } = request.into_inner();

    let domains = match (id, name) {
        (Some(id), _) => {
            domain::find_by(
                &api.database_connection,
                ObjectColumnFilter::One(domain::IdColumn, &id),
            )
            .await
        }
        (None, Some(name)) => domain::find_by_name(&api.database_connection, &name).await,
        (None, None) => {
            domain::find_by(
                &api.database_connection,
                ObjectColumnFilter::<domain::IdColumn>::All,
            )
            .await
        }
    };

    let result = domains
        .map(|domain| ::rpc::protos::dns::DomainList {
            domains: domain.into_iter().map(Domain::from).collect(),
        })
        .map(Response::new)
        .map_err(CarbideError::from)?;

    Ok(result)
}

// ============================================================================
// LEGACY ADAPTER HANDLERS - DEPRECATED
// These handlers provide backward compatibility
// They convert legacy types to new types and delegate to the handlers above
// TODO: Remove these once clients have migrated
// ============================================================================

use ::rpc::protos::forge::{
    DomainDeletionLegacy, DomainDeletionResultLegacy, DomainLegacy, DomainListLegacy,
    DomainSearchQueryLegacy,
};

/// Compatibility adapter for legacy create_domain RPC
pub(crate) async fn create_legacy_compat(
    api: &Api,
    request: Request<DomainLegacy>,
) -> Result<Response<DomainLegacy>, Status> {
    tracing::warn!(
        "Legacy RPC method create_domain_legacy called - please migrate to CreateDomain"
    );

    let domain_legacy = request.into_inner();

    // Convert legacy Domain to CreateDomainRequest
    let create_request = CreateDomainRequest {
        name: domain_legacy.name,
    };

    // Call the new handler
    let response = create(api, Request::new(create_request)).await?;
    let domain = response.into_inner();

    // Convert new Domain back to legacy format (drops metadata/soa)
    Ok(Response::new(DomainLegacy {
        id: domain.id,
        name: domain.name,
        created: domain.created,
        updated: domain.updated,
        deleted: domain.deleted,
    }))
}

/// Compatibility adapter for legacy update_domain RPC
pub(crate) async fn update_legacy_compat(
    api: &Api,
    request: Request<DomainLegacy>,
) -> Result<Response<DomainLegacy>, Status> {
    tracing::warn!(
        "Legacy RPC method update_domain_legacy called - please migrate to UpdateDomain"
    );

    let domain_legacy = request.into_inner();

    // Convert legacy Domain to UpdateDomainRequest
    let update_request = UpdateDomainRequest {
        domain: Some(Domain {
            id: domain_legacy.id,
            name: domain_legacy.name,
            created: domain_legacy.created,
            updated: domain_legacy.updated,
            deleted: domain_legacy.deleted,
            metadata: None, // Legacy doesn't have metadata
            soa: None,      // Legacy doesn't have SOA
        }),
    };

    // Call the new handler
    let response = update(api, Request::new(update_request)).await?;
    let domain = response.into_inner();

    // Convert new Domain back to legacy format
    Ok(Response::new(DomainLegacy {
        id: domain.id,
        name: domain.name,
        created: domain.created,
        updated: domain.updated,
        deleted: domain.deleted,
    }))
}

/// Compatibility adapter for legacy delete_domain RPC
pub(crate) async fn delete_legacy_compat(
    api: &Api,
    request: Request<DomainDeletionLegacy>,
) -> Result<Response<DomainDeletionResultLegacy>, Status> {
    tracing::warn!(
        "Legacy RPC method delete_domain_legacy called - please migrate to DeleteDomain"
    );

    let deletion_legacy = request.into_inner();

    // Convert to new request format
    let deletion_request = DomainDeletionRequest {
        id: deletion_legacy.id,
    };

    // Call the new handler
    let _ = delete(api, Request::new(deletion_request)).await?;

    // Return legacy result format
    Ok(Response::new(DomainDeletionResultLegacy {}))
}

/// Compatibility adapter for legacy find_domain RPC
pub(crate) async fn find_legacy_compat(
    api: &Api,
    request: Request<DomainSearchQueryLegacy>,
) -> Result<Response<DomainListLegacy>, Status> {
    tracing::warn!("Legacy RPC method find_domain_legacy called - please migrate to FindDomain");

    let query_legacy = request.into_inner();

    // Convert to new query format
    let query = DomainSearchQuery {
        id: query_legacy.id,
        name: query_legacy.name,
    };

    // Call the new handler
    let response = find(api, Request::new(query)).await?;
    let domain_list = response.into_inner();

    // Convert new DomainList to legacy format
    Ok(Response::new(DomainListLegacy {
        domains: domain_list
            .domains
            .into_iter()
            .map(|d| DomainLegacy {
                id: d.id,
                name: d.name,
                created: d.created,
                updated: d.updated,
                deleted: d.deleted,
            })
            .collect(),
    }))
}
