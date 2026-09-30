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
use carbide_authn::middleware::Principal;
use db::dns::domain;
use db::{self, ObjectColumnFilter};
use model::dns::NewDomain;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;
use crate::auth::AuthContext;

/// Validates a caller-supplied default TTL into the zone's range.
fn zone_ttl_argument(secs: Option<u32>) -> Result<Option<model::dns::ZoneTtl>, CarbideError> {
    secs.map(model::dns::ZoneTtl::try_from)
        .transpose()
        .map_err(|error| CarbideError::InvalidArgument(error.to_string()))
}

/// Rejects a proposed domain name at or below either reverse-DNS tree root.
///
/// Reverse lookups derive PTRs from inventory, not stored zones. Accepting
/// a zone write would imply support for reverse authority that is not served.
/// Network lifecycle maintains compatibility rows directly for rollback;
/// this validation applies to explicit domain API creation.
fn ensure_not_reverse_zone_name(proposed_name: &str) -> Result<(), CarbideError> {
    let normalized = db::dns::normalize_domain(proposed_name.trim());
    if matches!(normalized.as_str(), "in-addr.arpa" | "ip6.arpa")
        || db::dns::normalize_reverse_zone_name(&normalized).is_some()
    {
        return Err(CarbideError::InvalidArgument(format!(
            "{proposed_name} is a reverse DNS zone; only inventory-derived PTR records are supported, not reverse domain creation"
        )));
    }

    Ok(())
}

/// Additional authorization beyond RPC RBAC: only the REST site's exact
/// service identity may supply a reserved ID or cancel one. Admin CLI users
/// retain ordinary create/delete but cannot claim another tenant's identity.
fn require_site_agent<T>(request: &Request<T>) -> Result<(), Status> {
    let allowed = request.extensions().get::<AuthContext>().is_some_and(|auth| {
        auth.principals.len() == 1
            && matches!(
                &auth.principals[0],
                Principal::SpiffeServiceIdentifier(identifier) if identifier == "elektra-site-agent"
            )
    });
    if !allowed {
        return Err(CarbideError::PermissionDeniedError(
            "reserved domain IDs require the site-agent identity".to_string(),
        )
        .into());
    }
    Ok(())
}

/// RPC RBAC permits both the operator CLI and site agent on these two
/// methods. Only the site agent may use a reserved ID; conversely its service
/// identity must never use the unrestricted operator operation. A request
/// containing both identities is not an operator request.
fn authorize_domain_intent<T>(request: &Request<T>, reserved: bool) -> Result<(), Status> {
    if reserved {
        return require_site_agent(request);
    }
    if request
        .extensions()
        .get::<AuthContext>()
        .is_some_and(|auth| {
            auth.principals.iter().any(|principal| matches!(
            principal,
            Principal::SpiffeServiceIdentifier(identifier) if identifier == "elektra-site-agent"
        ))
        })
    {
        return Err(CarbideError::PermissionDeniedError(
            "site-agent domain writes require a reserved ID operation".to_string(),
        )
        .into());
    }
    // The RPC middleware performs ordinary operator RBAC before dispatch.
    // Direct test-harness callers have no AuthContext and bypass that layer.
    Ok(())
}

pub(crate) async fn create(
    api: &Api,
    request: Request<CreateDomainRequest>,
) -> Result<Response<Domain>, Status> {
    crate::api::log_request_data(&request);

    // A reserved ID is an internal REST replay identity, not a user-supplied
    // ID. The CLI and legacy callers continue to request a fresh Core ID.
    authorize_domain_intent(&request, request.get_ref().reserved_id.is_some())?;

    let mut txn = api.txn_begin().await?;
    let req = request.into_inner();
    ensure_not_reverse_zone_name(&req.name)?;
    // Internal retry intent is a DNS identity, not a presentation spelling.
    // Leave legacy/admin names unchanged for compatibility.
    let name = if req.reserved_id.is_some() {
        db::dns::normalize_domain(&req.name)
    } else {
        req.name
    };
    let new_domain = NewDomain {
        default_ttl: zone_ttl_argument(req.default_ttl)?,
        ..NewDomain::new(name)
    };

    let domain = if let Some(id) = req.reserved_id {
        // This lock is shared with delete and reference writers. After a
        // response-lost create, retrying the same reserved ID finds the exact
        // committed zone instead of creating a second forward-name match.
        domain::lock_id_exclusive(txn.as_mut(), id).await?;
        if domain::is_reserved_id_cancelled(txn.as_mut(), id).await? {
            return Err(CarbideError::FailedPrecondition(format!(
                "reserved domain ID {id} has been cancelled"
            ))
            .into());
        }
        if let Some((existing, reserved, created_ttl)) =
            domain::reserved_create_intent(txn.as_mut(), id).await?
        {
            if existing.deleted.is_some()
                || !reserved
                || existing.name != new_domain.name
                || created_ttl != new_domain.default_ttl
            {
                return Err(CarbideError::FailedPrecondition(format!(
                    "domain ID {id} is deleted or belongs to a different create request"
                ))
                .into());
            }
            existing
        } else {
            domain::persist_reserved(new_domain, id, txn.as_mut()).await?
        }
    } else {
        domain::persist(new_domain, txn.as_mut()).await?
    };

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

    let mut domain = domain::find_by_uuid_for_delete(txn.as_mut(), uuid)
        .await?
        .filter(|domain| domain.deleted.is_none())
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "domain",
            id: uuid.to_string(),
        })?;

    // Renaming a domain is not supported. The name may be omitted or sent
    // back unchanged so a caller updating another field need not read the
    // row first.
    if !domain_proto.name.is_empty() && domain_proto.name != domain.name {
        return Err(CarbideError::InvalidArgument(format!(
            "renaming domain {} to {} is not supported; delete it and create a new domain",
            domain.name, domain_proto.name
        ))
        .into());
    }
    // Omission preserves the stored default; the wire cannot clear it.
    if let Some(default_ttl) = zone_ttl_argument(domain_proto.default_ttl)? {
        domain.default_ttl = Some(default_ttl);
    }

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

    authorize_domain_intent(&request, request.get_ref().cancel_reserved_id)?;
    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    let uuid = req.id.ok_or_else(|| CarbideError::MissingArgument("id"))?;

    let domain = match domain::find_by_uuid_for_delete(txn.as_mut(), uuid).await? {
        Some(domain) => domain,
        None if req.cancel_reserved_id => {
            // The same per-ID lock is held by a future/retried reserved create.
            // Commit a terminal cancellation before reporting success.
            domain::cancel_reserved_id(txn.as_mut(), uuid).await?;
            txn.commit().await?;
            return Ok(Response::new(DomainDeletionResult {}));
        }
        None => {
            return Err(CarbideError::NotFoundError {
                kind: "domain",
                id: uuid.to_string(),
            }
            .into());
        }
    };

    if req.cancel_reserved_id {
        // A SiteAgent may cancel only rows created with a reserved ID, never
        // reinterpret a legacy/admin domain as its own REST projection.
        let (_, is_reserved, _) = domain::reserved_create_intent(txn.as_mut(), uuid)
            .await?
            .expect("domain was found under the same exclusive ID lock");
        if !is_reserved {
            return Err(CarbideError::FailedPrecondition(format!(
                "domain ID {uuid} was not created with a reserved ID"
            ))
            .into());
        }
    }
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
        default_ttl: None,
        reserved_id: None,
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
            default_ttl: None,
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
        cancel_reserved_id: false,
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
