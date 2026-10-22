// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Save a remote pool's `Define` request before sending it to the provider.
//!
//! An unanswered call may already have succeeded. Retrying the saved request
//! lets the provider acknowledge that same definition instead of creating a new
//! revision. Accepted requests stay available for allocations using older revisions.
//! Integrated pools keep their existing resource rows and do not use this store.
//! Wiring for startup and pool growth follows in
//! [#7140](https://github.com/dsx-ai-factory/infra-controller/issues/7140).
//!
//! Provider errors leave the request pending, including a rejected definition.
//! Resolve the provider's reported problem before retrying; submitting different
//! input cannot replace that request. Operator recovery for rejected definitions
//! must be settled in #7140 before enabling remote pools in production.

use std::time::Duration;

use carbide_resource_pool_api::v1;
use db::DatabaseError;
use db::resource_pool::{binding as bindings, registration as storage};
use model::resource_pool::registration::ResourcePoolRegistration;
use model::resource_pool::{ResourcePoolBackendKind, ResourcePoolBinding};
use prost::Message;
use sqlx::PgPool;

use crate::grpc::{GrpcResourcePoolBackend, GrpcResourcePoolError, MAX_MESSAGE_BYTES};
use crate::transaction::database_transaction;
use crate::value::{ResourcePoolEligibility, ValueConversionError, wire_domain};

#[cfg(test)]
mod tests;

// Bound the whole operation, including connection acquisition and multiple SQL
// statements or RPCs. Individual statements and RPCs have shorter limits.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);

/// `RegistrationError` reports why a definition could not be accepted locally.
/// Provider or database failures can leave a saved request awaiting acknowledgement;
/// use `resume` to finish that request before submitting different input.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    /// A database read, write, or commit failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The provider call failed, possibly after accepting the request.
    #[error(transparent)]
    Provider(#[from] GrpcResourcePoolError),
    /// The supplied eligibility contains a malformed entry.
    #[error(transparent)]
    Value(#[from] ValueConversionError),
    /// Saved protobuf bytes could not be decoded.
    #[error("could not decode saved resource pool definition")]
    Decode(#[from] prost::DecodeError),
    /// Input or saved data does not match the pool's registration contract.
    #[error("invalid resource pool registration: {0}")]
    Invalid(&'static str),
    /// A different request is pending, or the supplied predecessor is missing
    /// or no longer current.
    #[error("resource pool registration conflicts with its saved revision")]
    Conflict,
    /// Waiting stopped; the saved request or acknowledgement may still have committed.
    #[error("resource pool registration deadline exceeded")]
    DeadlineExceeded,
}

/// `define` saves and sends a complete remote pool definition, returning it only
/// after the provider's acknowledgement is committed locally.
///
/// The first request uses `previous_revision = None`. Growth supplies the latest
/// accepted revision and keeps its eligibility entries as an unchanged prefix.
/// Convert new declarations with `ResourcePoolEligibility::try_from`, then append
/// their entries to the accepted definition before requesting growth. This API
/// does not expand inventory or reconcile edited seed configuration.
///
/// Matching retries reuse the latest saved request. Different pending input or a
/// stale predecessor returns `Conflict`. An unchanged accepted definition needs
/// no new revision. After later growth, use `resume` for an older known revision;
/// `define` compares against the latest revision only.
/// Each call has a 30-second budget; errors leave any committed
/// request available to `resume`. No database transaction spans provider I/O.
pub async fn define(
    database: &PgPool,
    backend: &GrpcResourcePoolBackend,
    binding: &ResourcePoolBinding,
    previous_revision: Option<i64>,
    eligibility: &ResourcePoolEligibility,
) -> Result<v1::DefineRequest, RegistrationError> {
    tokio::time::timeout(REGISTRATION_TIMEOUT, async {
        check_binding(binding)?;
        if eligibility.domain != binding.value_domain || eligibility.entries.is_empty() {
            return Err(RegistrationError::Invalid(
                "eligibility must match the pool's value domain and contain entries",
            ));
        }
        for entry in &eligibility.entries {
            entry
                .validate(wire_domain(eligibility.domain))
                .map_err(|error| ValueConversionError::Wire(Box::new(error)))?;
        }

        // Reject a stale binding before Describe so it never reaches the provider.
        let mut txn = database_transaction(database).await?;
        if bindings::load(txn.as_pgconn(), &binding.pool_name)
            .await?
            .as_ref()
            != Some(binding)
        {
            return Err(RegistrationError::Invalid(
                "binding differs from its saved selection",
            ));
        }
        let existing = storage::latest(txn.as_pgconn(), binding.binding_id).await?;
        txn.commit().await?;

        let pool = if let Some(existing) = existing {
            decode(&existing, binding)?
                .pool
                .ok_or(RegistrationError::Invalid("saved definition has no pool"))?
        } else {
            if previous_revision.is_some() {
                return Err(RegistrationError::Conflict);
            }
            // Keep the large gRPC future out of the surrounding async stack.
            let description = Box::pin(backend.describe(v1::DescribeRequest {
                pool_alias: binding.remote_pool.clone(),
            }))
            .await?;
            if description.authority_id != binding.authority_id
                || description.value_domain != wire_domain(binding.value_domain) as i32
            {
                return Err(RegistrationError::Invalid(
                    "provider identity or value domain differs from the binding",
                ));
            }
            v1::PoolBinding {
                authority_id: description.authority_id,
                pool_id: description.pool_id,
                binding_id: binding.binding_id.to_string(),
                logical_pool: binding.pool_name.clone(),
            }
        };

        let mut txn = database_transaction(database).await?;
        if storage::lock_binding(txn.as_pgconn(), binding.binding_id).await? != *binding {
            return Err(RegistrationError::Invalid(
                "binding differs from its saved selection",
            ));
        }
        let latest = storage::latest(txn.as_pgconn(), binding.binding_id).await?;
        let registration = match prepare(binding, pool, latest, previous_revision, eligibility)? {
            PreparedRegistration::Existing(registration) => registration,
            PreparedRegistration::New(registration) => {
                storage::insert(txn.as_pgconn(), &registration).await?;
                registration
            }
        };
        txn.commit().await?;
        send(database, backend, binding, registration).await
    })
    .await
    .map_err(|_| RegistrationError::DeadlineExceeded)?
}

/// `resume` retries one saved revision without regenerating its request or
/// looking up the provider alias again. Already accepted revisions return without
/// an RPC. Resuming an older revision never makes it the current definition.
/// The 30-second budget includes database waits and the provider call; timeout or
/// cancellation does not prove acknowledgement failed to commit. Retry this same
/// revision to read its saved state.
pub async fn resume(
    database: &PgPool,
    backend: &GrpcResourcePoolBackend,
    binding: &ResourcePoolBinding,
    revision: i64,
) -> Result<v1::DefineRequest, RegistrationError> {
    tokio::time::timeout(REGISTRATION_TIMEOUT, async {
        check_binding(binding)?;
        let mut txn = database_transaction(database).await?;
        let stored = storage::load(txn.as_pgconn(), binding.binding_id, revision)
            .await?
            .ok_or(RegistrationError::Invalid("revision has not been prepared"))?;
        txn.commit().await?;
        send(database, backend, binding, stored).await
    })
    .await
    .map_err(|_| RegistrationError::DeadlineExceeded)?
}

/// `accepted` loads an acknowledged definition for allocation validation.
/// Missing and pending revisions return `None`; accepted older revisions remain
/// available after growth. Reading a revision does not make it current.
/// The lookup has a 30-second budget, including acquiring a database connection.
/// Production callers will select allocation eligibility through this lookup in
/// [#7140](https://github.com/dsx-ai-factory/infra-controller/issues/7140).
pub async fn accepted(
    database: &PgPool,
    binding: &ResourcePoolBinding,
    revision: i64,
) -> Result<Option<v1::DefineRequest>, RegistrationError> {
    tokio::time::timeout(REGISTRATION_TIMEOUT, async {
        check_binding(binding)?;
        let mut txn = database_transaction(database).await?;
        let stored = storage::load(txn.as_pgconn(), binding.binding_id, revision).await?;
        txn.commit().await?;
        stored
            .filter(|stored| stored.accepted)
            .map(|stored| decode(&stored, binding))
            .transpose()
    })
    .await
    .map_err(|_| RegistrationError::DeadlineExceeded)?
}

/// `latest_accepted` loads the newest acknowledged definition for pool inspection.
/// Pending growth leaves the previous accepted revision available. `None` means
/// this binding has no acknowledged definition; malformed saved requests return
/// an error. The saved provider pool identity and revision are returned unchanged.
/// This read has a 30-second budget, including acquiring a database connection,
/// and does not contact the provider or change registration state.
pub async fn latest_accepted(
    database: &PgPool,
    binding: &ResourcePoolBinding,
) -> Result<Option<v1::DefineRequest>, RegistrationError> {
    tokio::time::timeout(REGISTRATION_TIMEOUT, async {
        check_binding(binding)?;
        let mut txn = database_transaction(database).await?;
        let stored = storage::latest_accepted(txn.as_pgconn(), binding.binding_id).await?;
        txn.commit().await?;
        stored.map(|stored| decode(&stored, binding)).transpose()
    })
    .await
    .map_err(|_| RegistrationError::DeadlineExceeded)?
}

/// `PreparedRegistration` distinguishes a saved retry from a request to insert.
enum PreparedRegistration {
    Existing(ResourcePoolRegistration),
    New(ResourcePoolRegistration),
}

/// `prepare` reuses the current request or appends one successor while the
/// caller holds the binding lock.
fn prepare(
    binding: &ResourcePoolBinding,
    pool: v1::PoolBinding,
    latest: Option<ResourcePoolRegistration>,
    previous_revision: Option<i64>,
    eligibility: &ResourcePoolEligibility,
) -> Result<PreparedRegistration, RegistrationError> {
    let revision = if let Some(latest) = latest {
        let prior = decode(&latest, binding)?;
        if prior.pool.as_ref() != Some(&pool) {
            return Err(RegistrationError::Invalid("resolved provider pool changed"));
        }
        let same_input = prior.eligibility == eligibility.entries;
        if same_input
            && (prior.previous_revision == previous_revision.map(|value| value.to_string())
                || (latest.accepted && previous_revision == Some(latest.revision)))
        {
            return Ok(PreparedRegistration::Existing(latest));
        }
        if !latest.accepted || previous_revision != Some(latest.revision) {
            return Err(RegistrationError::Conflict);
        }
        if !eligibility.entries.starts_with(&prior.eligibility) {
            return Err(RegistrationError::Invalid(
                "growth must keep previous eligibility entries unchanged",
            ));
        }
        latest
            .revision
            .checked_add(1)
            .ok_or(RegistrationError::Invalid("revision limit reached"))?
    } else {
        if previous_revision.is_some() {
            return Err(RegistrationError::Conflict);
        }
        1
    };
    let request = v1::DefineRequest {
        pool: Some(pool),
        eligibility_revision: revision.to_string(),
        previous_revision: previous_revision.map(|value| value.to_string()),
        value_domain: wire_domain(binding.value_domain) as i32,
        eligibility: eligibility.entries.clone(),
    };
    // Do not save a request the shared gRPC client will always reject as too large.
    if request.encoded_len() > MAX_MESSAGE_BYTES {
        return Err(RegistrationError::Invalid(
            "definition exceeds the gRPC message size limit",
        ));
    }
    Ok(PreparedRegistration::New(ResourcePoolRegistration {
        binding_id: binding.binding_id,
        revision,
        request: request.encode_to_vec(),
        accepted: false,
    }))
}

/// `send` commits acknowledgement separately from the provider call. A lost reply
/// or failed commit leaves the original request available for an identical retry.
async fn send(
    database: &PgPool,
    backend: &GrpcResourcePoolBackend,
    binding: &ResourcePoolBinding,
    stored: ResourcePoolRegistration,
) -> Result<v1::DefineRequest, RegistrationError> {
    let request = decode(&stored, binding)?;
    if !stored.accepted {
        // Keep the large gRPC future out of the surrounding async stack.
        Box::pin(backend.define(request.clone())).await?;
        let mut txn = database_transaction(database).await?;
        storage::acknowledge(txn.as_pgconn(), stored.binding_id, stored.revision).await?;
        txn.commit().await?;
    }
    Ok(request)
}

/// `decode` checks the saved request against the row and selected pool before use.
fn decode(
    stored: &ResourcePoolRegistration,
    binding: &ResourcePoolBinding,
) -> Result<v1::DefineRequest, RegistrationError> {
    let request = v1::DefineRequest::decode(stored.request.as_slice())?;
    let pool = request
        .pool
        .as_ref()
        .ok_or(RegistrationError::Invalid("saved definition has no pool"))?;
    if stored.binding_id != binding.binding_id
        || request.eligibility_revision != stored.revision.to_string()
        || request.value_domain != wire_domain(binding.value_domain) as i32
        || pool.binding_id != binding.binding_id.to_string()
        || pool.logical_pool != binding.pool_name
        || pool.authority_id != binding.authority_id
        || pool.pool_id.is_empty()
    {
        return Err(RegistrationError::Invalid(
            "saved definition does not match its pool binding",
        ));
    }
    Ok(request)
}

/// `check_binding` keeps Integrated pools and empty provider identities out of registration.
fn check_binding(binding: &ResourcePoolBinding) -> Result<(), RegistrationError> {
    if binding.backend_kind != ResourcePoolBackendKind::Grpc
        || binding.pool_name.is_empty()
        || binding.authority_id.is_empty()
        || binding.remote_pool.is_empty()
    {
        return Err(RegistrationError::Invalid(
            "registration requires a named gRPC pool binding",
        ));
    }
    Ok(())
}
