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

use std::collections::HashMap;
use std::str::FromStr;

use carbide_uuid::domain::DomainId;
use chrono::{DateTime, Utc};
use hickory_proto::rr::Name;
use model::dns::{Domain, NewDomain, SoaSnapshot};
use sqlx::{FromRow, PgConnection};

use super::super::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter};
use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod test_create_domain;

/// Validates a domain name according to DNS standards
fn validate_domain_name(name: &str) -> Result<(), DatabaseError> {
    if name != name.to_lowercase() {
        return Err(DatabaseError::InvalidArgument(
            "domain name must be lowercase".to_string(),
        ));
    }

    Name::from_str(name)
        .map_err(|_| DatabaseError::InvalidArgument(format!("invalid domain name: {}", name)))?;

    Ok(())
}

#[derive(Clone, Debug, FromRow)]
pub struct DbDomain {
    pub id: DomainId,
    pub name: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub soa: sqlx::types::Json<Option<dns_record::SoaRecord>>,
    pub domain_metadata_id: Option<i32>,
}

impl From<DbDomain> for Domain {
    fn from(db: DbDomain) -> Self {
        Domain {
            id: db.id,
            name: db.name,
            created: db.created,
            updated: db.updated,
            deleted: db.deleted,
            soa: db.soa.0.map(SoaSnapshot),
            metadata: None,
        }
    }
}

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for crate::dns::domain::IdColumn {
    type TableType = Domain;
    type ColumnType = DomainId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Copy, Clone)]
pub struct NameColumn;
impl<'a> ColumnInfo<'a> for NameColumn {
    type TableType = Domain;
    type ColumnType = &'a str;

    fn column_name(&self) -> &'static str {
        "name"
    }
}

pub async fn persist(value: NewDomain, txn: &mut PgConnection) -> DatabaseResult<Domain> {
    validate_domain_name(&value.name)?;

    // Create default metadata entry
    let metadata_id = super::domain_metadata::DbMetadata::create_default(txn).await?;

    let query =
        "INSERT INTO domains (name, soa, domain_metadata_id) VALUES ($1, $2, $3) returning *";
    match persist_inner_with_metadata(&value, metadata_id, txn, query).await {
        Ok(Some(domain)) => Ok(domain),
        Ok(None) => Err(DatabaseError::NotFoundError {
            kind: "domain",
            id: value.name,
        }),
        Err(err) => Err(err),
    }
}

/// Create the domain only if it would be the first one
pub async fn persist_first(
    value: &NewDomain,
    txn: &mut PgConnection,
) -> DatabaseResult<Option<Domain>> {
    validate_domain_name(&value.name)?;

    let metadata_id = super::domain_metadata::DbMetadata::create_default(txn).await?;

    let query = "
            INSERT INTO domains (name, soa, domain_metadata_id) SELECT $1, $2, $3
            WHERE NOT EXISTS (SELECT name FROM domains)
            RETURNING *";
    persist_inner_with_metadata(value, metadata_id, txn, query).await
}

async fn persist_inner_with_metadata(
    value: &NewDomain,
    metadata_id: i32,
    txn: &mut PgConnection,
    query: &'static str,
) -> DatabaseResult<Option<Domain>> {
    sqlx::query_as::<_, DbDomain>(query)
        .bind(&value.name)
        .bind(sqlx::types::Json(&value.soa))
        .bind(metadata_id)
        .fetch_optional(txn)
        .await
        .map(|opt| opt.map(Domain::from))
        .map_err(|e| DatabaseError::query(query, e))
}

/// Finds `domains` based on specified criteria, excluding deleted entries.
///
/// Returns `Vec<Domain>`
///
/// # Arguments
///
/// * [`ObjectColumnFilter`] - An enum that determines the query criteria
///
/// # Examples
pub async fn find_by<'a, C: ColumnInfo<'a, TableType = Domain>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<Domain>, DatabaseError> {
    find_all_by(txn, filter, false).await
}

/// Similar to [`Domain::find_by`] but lets you specify whether to include deleted results
pub async fn find_all_by<'a, C: ColumnInfo<'a, TableType = Domain>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
    include_deleted: bool,
) -> Result<Vec<Domain>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new("SELECT * FROM domains").filter(&filter);
    if !include_deleted {
        query.push(" AND deleted IS NULL");
    }
    query
        .build_query_as::<DbDomain>()
        .fetch_all(txn)
        .await
        .map(|domains| domains.into_iter().map(Domain::from).collect())
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

/// Finds live domains named `name`.
///
/// Reverse-zone names compare case-insensitively without a trailing dot
/// because those spellings identify the same DNS zone. Forward-domain names
/// retain their exact-match behavior.
pub async fn find_by_name(
    txn: impl DbReader<'_>,
    name: &str,
) -> Result<Vec<Domain>, DatabaseError> {
    if let Some(reverse_zone_name) = super::normalize_reverse_zone_name(name) {
        find_reverse_zone_by_normalized_name(txn, &reverse_zone_name).await
    } else {
        find_by(txn, ObjectColumnFilter::One(NameColumn, &name)).await
    }
}

/// Finds the live reverse-zone identity represented by `name`, treating a
/// trailing dot as presentation rather than part of the identity.
pub async fn find_reverse_zone_by_normalized_name(
    txn: impl DbReader<'_>,
    name: &str,
) -> Result<Vec<Domain>, DatabaseError> {
    let query = "SELECT * FROM domains
                 WHERE lower(rtrim(name, '.')) = $1
                   AND deleted IS NULL
                   AND (
                       lower(rtrim(name, '.')) LIKE '%.in-addr.arpa'
                       OR lower(rtrim(name, '.')) LIKE '%.ip6.arpa'
                   )";
    let name = super::normalize_domain(name);
    sqlx::query_as::<_, DbDomain>(query)
        .bind(name)
        .fetch_all(txn)
        .await
        .map(|domains| domains.into_iter().map(Domain::from).collect())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Find the domain with the given ID, even if it is deleted.
pub async fn find_by_uuid(
    txn: impl DbReader<'_>,
    uuid: DomainId,
) -> Result<Option<Domain>, DatabaseError> {
    find_all_by(txn, ObjectColumnFilter::One(IdColumn, &uuid), true)
        .await
        .map(|f| f.first().cloned())
}

/// Locks a live domain identity while a caller creates a reference to it.
///
/// The shared advisory lock permits concurrent reference creation but conflicts
/// with the exclusive lock acquired by [`find_by_uuid_for_delete`]. Callers
/// must keep the transaction open until the reference is persisted.
pub async fn lock_live_for_reference(
    txn: &mut PgConnection,
    uuid: DomainId,
) -> Result<(), DatabaseError> {
    let lock_query = "SELECT pg_advisory_xact_lock_shared(
                          hashtextextended('domains:id:' || $1::text, 0)
                      )";
    sqlx::query(lock_query)
        .bind(uuid)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock_query, error))?;

    let query = "SELECT EXISTS (SELECT 1 FROM domains WHERE id = $1 AND deleted IS NULL)";
    let exists: bool = sqlx::query_scalar(query)
        .bind(uuid)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if !exists {
        return Err(DatabaseError::NotFoundError {
            kind: "domain",
            id: uuid.to_string(),
        });
    }

    Ok(())
}

/// Exclusively locks a domain identity for deletion, then finds the domain.
/// Deleted domains are included so repeated deletion remains idempotent.
pub async fn find_by_uuid_for_delete(
    txn: &mut PgConnection,
    uuid: DomainId,
) -> Result<Option<Domain>, DatabaseError> {
    let lock_query = "SELECT pg_advisory_xact_lock(
                          hashtextextended('domains:id:' || $1::text, 0)
                      )";
    sqlx::query(lock_query)
        .bind(uuid)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock_query, error))?;

    let query = "SELECT * FROM domains WHERE id = $1";
    sqlx::query_as::<_, DbDomain>(query)
        .bind(uuid)
        .fetch_optional(txn)
        .await
        .map(|domain| domain.map(Domain::from))
        .map_err(|error| DatabaseError::query(query, error))
}

/// Reports whether a live network segment or machine interface references the
/// domain. The caller must first hold the delete lock for this domain.
pub async fn has_live_references(
    txn: &mut PgConnection,
    uuid: DomainId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT EXISTS (
                     SELECT 1
                     FROM network_segments
                     WHERE subdomain_id = $1 AND deleted IS NULL
                 ) OR EXISTS (
                     SELECT 1
                     FROM machine_interfaces
                     WHERE domain_id = $1
                 )";
    sqlx::query_scalar(query)
        .bind(uuid)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Batched counterpart to [`find_by_uuid`]: fetch every domain in `ids` with a single
/// `WHERE id = ANY($1)` query (deleted entries included, matching `find_by_uuid`), keyed by id.
///
/// Ids that have no matching row are simply absent from the returned map, so callers can
/// reproduce `find_by_uuid`'s "not found" handling with a `.get(&id)` lookup.
pub async fn find_by_uuids(
    txn: impl DbReader<'_>,
    ids: &[DomainId],
) -> Result<HashMap<DomainId, Domain>, DatabaseError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    find_all_by(txn, ObjectColumnFilter::List(IdColumn, ids), true)
        .await
        .map(|domains| domains.into_iter().map(|d| (d.id, d)).collect())
}

/// Soft-deletes a domain while its update timestamp still matches the snapshot
/// whose reverse-zone lock the caller acquired. A zero-row update means that
/// snapshot is stale, including when another writer updated or deleted the
/// domain or the row no longer exists.
pub async fn delete(value: Domain, txn: &mut PgConnection) -> Result<Domain, DatabaseError> {
    // PostgreSQL evaluates both assignments from the pre-update row. Reusing
    // this expression gives `updated` and `deleted` the same monotonic value,
    // so the returned row has one timestamp for the deletion version.
    let query = "UPDATE domains
                 SET updated = GREATEST(statement_timestamp(), updated + interval '1 microsecond'),
                     deleted = GREATEST(statement_timestamp(), updated + interval '1 microsecond')
                 WHERE id = $1
                   AND updated = $2
                 RETURNING *";
    sqlx::query_as::<_, DbDomain>(query)
        .bind(value.id)
        .bind(value.updated)
        .fetch_one(txn)
        .await
        .map(Domain::from)
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("domain", value.updated.to_rfc3339())
            }
            error => DatabaseError::query(query, error),
        })
}

/// Updates a domain while its update timestamp still matches the snapshot whose
/// reverse-zone locks the caller acquired. A zero-row update also covers a row
/// that no longer exists. The new timestamp always advances, including for
/// multiple updates in one transaction, so a later writer cannot reuse the same
/// snapshot.
pub async fn update(value: &Domain, txn: &mut PgConnection) -> Result<Domain, DatabaseError> {
    validate_domain_name(&value.name)?;

    let query = "UPDATE domains
                 SET name = $1,
                     updated = GREATEST(statement_timestamp(), updated + interval '1 microsecond'),
                     soa = $2
                 WHERE id = $3
                   AND updated = $4
                 RETURNING *";

    sqlx::query_as::<_, DbDomain>(query)
        .bind(&value.name)
        .bind(sqlx::types::Json(&value.soa))
        .bind(value.id)
        .bind(value.updated)
        .fetch_one(txn)
        .await
        .map(Domain::from)
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("domain", value.updated.to_rfc3339())
            }
            error => DatabaseError::query(query, error),
        })
}

#[cfg(test)]
#[test]
fn test_generate_domain_serial_format() {
    use chrono::Utc;
    let now = Utc::now();
    let expected_serial = now.format("%Y%m%d01").to_string().parse::<u32>().unwrap();

    let serial = dns_record::SoaRecord::generate_new_serial();

    assert_eq!(serial, expected_serial);
}

#[cfg(test)]
mod test_find_by_uuids {
    use carbide_test_support::query_counter::count_queries;
    use model::dns::NewDomain;

    use crate as db;

    #[crate::sqlx_test]
    async fn find_by_uuids_collapses_n_plus_one(pool: sqlx::PgPool) {
        const N: usize = 8;

        // Seed N distinct domains.
        let mut txn = pool.begin().await.expect("begin");
        let mut ids = Vec::with_capacity(N);
        for i in 0..N {
            let domain =
                db::dns::domain::persist(NewDomain::new(format!("n{i}.metal.net")), &mut txn)
                    .await
                    .expect("persist domain");
            ids.push(domain.id);
        }
        txn.commit().await.expect("commit");

        // BEFORE: one find_by_uuid per id. The reads run straight off the pool
        // -- no transaction -- so the count reflects only the find_by_uuid
        // calls, not begin/commit statements.
        let (looped, before_count) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                let mut names = std::collections::HashMap::new();
                for id in ids {
                    let domain = db::dns::domain::find_by_uuid(pool, *id)
                        .await
                        .expect("find_by_uuid")
                        .expect("domain present");
                    names.insert(domain.id, domain.name);
                }
                names
            })
            .await
        };

        // AFTER: a single batched find_by_uuids.
        let (batched, after_count) = {
            let pool = &pool;
            let ids = &ids;
            count_queries(async move {
                db::dns::domain::find_by_uuids(pool, ids)
                    .await
                    .expect("find_by_uuids")
            })
            .await
        };

        // Data equality: same set of (id -> name) pairs.
        assert_eq!(batched.len(), N, "batched returned all N domains");
        let batched_names = batched
            .into_iter()
            .map(|(id, domain)| (id, domain.name))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            looped, batched_names,
            "batched call returns the same id->name mapping as the loop"
        );

        // Bite-check: the loop MUST be more than one query, or the measurement is vacuous.
        assert!(
            before_count > 1,
            "bite-check failed: looped find_by_uuid issued {before_count} queries (expected > 1)"
        );
        assert_eq!(
            before_count, N,
            "looped find_by_uuid issues one query per id"
        );
        assert_eq!(
            after_count, 1,
            "batched find_by_uuids issues a single query"
        );

        println!(
            "dns::domain N+1: before(loop find_by_uuid)={before_count} after(find_by_uuids)={after_count} (N={N})"
        );
    }
}
