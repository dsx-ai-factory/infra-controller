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

//! Save each resource pool's backend selection and reject changes to its allocator.

use std::collections::HashMap;

use model::resource_pool::{
    ResourcePoolBinding, ResourcePoolDef, ResourcePoolSelection, ResourcePoolValueDomain, ValueType,
};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::DatabaseError;
use crate::db_read::DbReader;

/// `BindingError` reports database failures, conflicting backend selections,
/// or incompatible values in a pool.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    /// A database read or write failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The requested selection does not match the pool's saved binding.
    #[error(
        "resource pool {name:?} binding cannot change: stored {stored:?}, requested {requested:?}"
    )]
    Conflict {
        /// Pool whose requested selection was rejected.
        name: String,
        /// Saved binding, which remains unchanged.
        stored: Box<ResourcePoolBinding>,
        /// Requested selection that conflicts with the saved binding.
        requested: Box<ResourcePoolSelection>,
    },
    /// The pool mixes incompatible values, such as integers and IP addresses.
    #[error("resource pool {name:?} has conflicting stored value domains {first} and {second}")]
    MixedValueDomains {
        /// Pool containing incompatible kinds of values.
        name: String,
        /// First value family found in the pool's resource rows.
        first: ResourcePoolValueDomain,
        /// Different value family found in the same pool.
        second: ResourcePoolValueDomain,
    },
}

/// `load` returns a pool's saved binding, or `None` if it has no binding yet.
async fn load(
    db: impl DbReader<'_>,
    name: &str,
) -> Result<Option<ResourcePoolBinding>, DatabaseError> {
    let query = "SELECT * FROM resource_pool_binding WHERE pool_name = $1";
    sqlx::query_as(query)
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `all` loads every saved binding, including pools removed from configuration.
/// Results are ordered by logical pool name.
pub async fn all(db: impl DbReader<'_>) -> Result<Vec<ResourcePoolBinding>, DatabaseError> {
    let query = "SELECT * FROM resource_pool_binding ORDER BY pool_name";
    sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `bind_or_verify` saves a pool's first backend selection in the caller's transaction.
/// Repeated or concurrent matching selections return the original binding UUID
/// and backend configuration name. Changing an existing pool's allocator,
/// backend implementation, remote pool name, or value family fails without
/// updating the row.
/// Callers must check that the selected backend is supported before calling this.
pub async fn bind_or_verify(
    txn: &mut PgConnection,
    requested: &ResourcePoolSelection,
) -> Result<ResourcePoolBinding, BindingError> {
    let query = "INSERT INTO resource_pool_binding
        (binding_id, pool_name, value_domain, backend_name, backend_kind, authority_id, remote_pool)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (pool_name) DO NOTHING";
    sqlx::query(query)
        .bind(Uuid::new_v4())
        .bind(&requested.pool_name)
        .bind(requested.value_domain)
        .bind(&requested.backend_name)
        .bind(requested.backend_kind)
        .bind(&requested.authority_id)
        .bind(&requested.remote_pool)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    // Another instance may be inserting this pool's binding at the same time.
    // Our insert waits for its transaction. Read in a separate statement so
    // READ COMMITTED can see the binding that instance committed.
    let stored =
        load(txn, &requested.pool_name)
            .await?
            .ok_or_else(|| DatabaseError::NotFoundError {
                kind: "resource pool binding after insert",
                id: requested.pool_name.clone(),
            })?;
    if !stored.matches(requested) {
        return Err(BindingError::Conflict {
            name: requested.pool_name.clone(),
            stored: Box::new(stored),
            requested: Box::new(requested.clone()),
        });
    }
    Ok(stored)
}

#[derive(sqlx::FromRow)]
struct ExistingPool {
    name: String,
    value_type: Option<ValueType>,
    definition: Option<sqlx::types::Json<ResourcePoolDef>>,
}

/// `existing_pool_domains` reads each pool's value family from its resource rows.
/// If a pool has no rows, it uses the pool definition saved in the database.
/// IPv4 and IPv6 may share a pool; mixing other families returns an error.
pub async fn existing_pool_domains(
    db: impl DbReader<'_>,
) -> Result<HashMap<String, ResourcePoolValueDomain>, BindingError> {
    let query = "SELECT name, value_type, NULL::jsonb AS definition
        FROM resource_pool GROUP BY name, value_type
        UNION ALL
        SELECT name, NULL::resource_pool_type, definition FROM resource_pool_def
        WHERE NOT EXISTS (SELECT 1 FROM resource_pool
            WHERE resource_pool.name = resource_pool_def.name)";
    let rows: Vec<ExistingPool> = sqlx::query_as(query)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let mut pools = HashMap::new();
    for ExistingPool {
        name,
        value_type,
        definition,
    } in rows
    {
        let value_type = match (value_type, definition) {
            (Some(value_type), _) => value_type,
            (None, Some(definition)) => definition.pool_type.into(),
            (None, None) => {
                return Err(DatabaseError::Internal {
                    message: format!("resource pool {name:?} has neither a type nor a definition"),
                }
                .into());
            }
        };
        let value_domain = ResourcePoolValueDomain::from(value_type);
        if let Some(first) = pools.insert(name.clone(), value_domain)
            && first != value_domain
        {
            return Err(BindingError::MixedValueDomains {
                name,
                first,
                second: value_domain,
            });
        }
    }
    Ok(pools)
}

#[cfg(test)]
mod tests;
