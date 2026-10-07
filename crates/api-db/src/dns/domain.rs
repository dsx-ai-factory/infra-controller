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
#[cfg(test)]
mod test_explicit_columns;

/// Requires lowercase spelling and a name accepted by the DNS name parser.
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
    /// Default record TTL; absence means the site default.
    pub default_ttl: Option<model::dns::ZoneTtl>,
    /// Owning VPC, or `None` for an infrastructure domain.
    pub vpc_id: Option<carbide_uuid::vpc::VpcId>,
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
            default_ttl: db.default_ttl,
            vpc_id: db.vpc_id,
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

/// Creates an infrastructure domain, or a forward domain owned by a live VPC.
///
/// Live names must be unique across all owners, ignoring case and trailing
/// dots, and each VPC may own at most one live domain. Parent and child domain
/// names may coexist.
///
/// Call this inside a transaction so the VPC row lock stays held from owner
/// validation until the domain is committed.
pub async fn persist(value: NewDomain, txn: &mut PgConnection) -> DatabaseResult<Domain> {
    validate_domain_name(&value.name)?;
    validate_scope(&value.name, value.vpc_id, txn).await?;

    // Create default metadata entry
    let metadata_id = super::domain_metadata::DbMetadata::create_default(txn).await?;

    let query = "INSERT INTO domains (name, soa, domain_metadata_id, vpc_id, default_ttl)
                 VALUES ($1, $2, $3, $4, $5)
                 RETURNING id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id";

    match persist_inner_with_metadata(&value, metadata_id, txn, query).await {
        Ok(Some(domain)) => Ok(domain),
        Ok(None) => Err(DatabaseError::NotFoundError {
            kind: "domain",
            id: value.name,
        }),
        Err(err) if err.violates_constraint("domains_live_name_key") => Err(
            DatabaseError::InvalidArgument(format!("domain {} already exists", value.name)),
        ),
        Err(err) if err.violates_constraint("domains_live_vpc_zone_key") => {
            Err(DatabaseError::InvalidArgument(format!(
                "VPC {} already owns a domain",
                value
                    .vpc_id
                    .expect("only a VPC-owned insert can hit this index")
            )))
        }
        Err(err) => Err(err),
    }
}

/// Creates the initial domain in an empty `domains` table.
///
/// Returns `None` if any row already exists, including deleted rows.
/// The caller must use a transaction, as for [`persist`].
pub async fn persist_first(
    value: &NewDomain,
    txn: &mut PgConnection,
) -> DatabaseResult<Option<Domain>> {
    validate_domain_name(&value.name)?;

    validate_scope(&value.name, value.vpc_id, txn).await?;

    let metadata_id = super::domain_metadata::DbMetadata::create_default(txn).await?;

    let query = "
            INSERT INTO domains (name, soa, domain_metadata_id, vpc_id, default_ttl)
            SELECT $1, $2, $3, $4, $5
            WHERE NOT EXISTS (SELECT name FROM domains)
            RETURNING id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id";
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
        .bind(value.vpc_id)
        .bind(value.default_ttl)
        .fetch_optional(txn)
        .await
        .map(|opt| opt.map(Domain::from))
        .map_err(|e| DatabaseError::query(query, e))
}

/// Requires a live owning VPC and restricts VPC-owned domains to forward zones.
///
/// Domain creation and VPC deletion take the same VPC row lock. Keep the
/// transaction open through insertion so the owner cannot be deleted after
/// this check and before the domain is created.
///
/// Domains without a VPC owner skip these checks. Network setup still writes
/// infrastructure reverse-zone rows for rollback compatibility.
async fn validate_scope(
    name: &str,
    vpc_id: Option<carbide_uuid::vpc::VpcId>,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let Some(vpc_id) = vpc_id else {
        return Ok(());
    };

    let name = model::dns::Fqdn::parse(name)
        .map_err(|error| DatabaseError::InvalidArgument(format!("invalid domain name: {error}")))?;
    let reverse_roots = ["in-addr.arpa.", "ip6.arpa."]
        .map(|root| model::dns::Fqdn::parse(root).expect("reverse tree roots are valid names"));
    let is_reverse_zone = reverse_roots.iter().any(|root| name.is_within(root));

    let vpcs = crate::vpc::find_by_with_lock(
        &mut *txn,
        ObjectColumnFilter::One(crate::vpc::IdColumn, &vpc_id),
        crate::vpc::VpcRowLock::Mutation,
    )
    .await?;
    if vpcs.is_empty() {
        return Err(DatabaseError::NotFoundError {
            kind: "VPC",
            id: vpc_id.to_string(),
        });
    }
    if is_reverse_zone {
        return Err(DatabaseError::InvalidArgument(
            "VPC domains must be forward zones".to_string(),
        ));
    }
    Ok(())
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
    let mut query = FilterableQueryBuilder::new(
        "SELECT id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id FROM domains",
    )
    .filter(&filter);
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

/// Finds the live infrastructure domain with the longest name in `candidates`.
/// Returns `None` if no live infrastructure domain matches.
///
/// Pass the queried name's label suffixes, lowercase and without a trailing
/// dot (see `Fqdn::suffixes`). Comparing with `lower(rtrim(name, '.'))` also
/// matches stored names with uppercase letters or trailing dots. This lets
/// one query check the suffixes without loading every domain.
///
/// Registering a VPC domain only records ownership. Skip these rows so the
/// DNS handler cannot return their apex SOA or use them for authoritative
/// negative answers. An enclosing infrastructure zone can still be
/// authoritative for names beneath the VPC domain.
/// TODO: include VPC-owned zones when VPC record publication is implemented.
pub async fn find_longest_live_zone(
    txn: impl DbReader<'_>,
    candidates: &[String],
) -> Result<Option<Domain>, DatabaseError> {
    let query =
        "SELECT id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id
                 FROM domains
                 WHERE deleted IS NULL
                   AND vpc_id IS NULL
                   AND lower(rtrim(name, '.')) = ANY($1)
                 ORDER BY length(rtrim(name, '.')) DESC, name
                 LIMIT 1";
    sqlx::query_as::<_, DbDomain>(query)
        .bind(candidates)
        .fetch_optional(txn)
        .await
        .map(|domain| domain.map(Domain::from))
        .map_err(|error| DatabaseError::query(query, error))
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
    let query =
        "SELECT id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id
                 FROM domains
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
                 RETURNING id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id";
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

/// Writes the snapshot's name, SOA, and default TTL while its timestamp and
/// VPC owner still match the stored row; ownership is never changed here.
/// A missing row or a timestamp or owner mismatch returns
/// `ConcurrentModificationError`.
///
/// The timestamp advances even for multiple updates in one transaction, so
/// a later writer cannot reuse the same snapshot. The caller must hold an
/// explicit transaction and acquire reverse-zone locks for reverse domains.
pub async fn update(value: &Domain, txn: &mut PgConnection) -> Result<Domain, DatabaseError> {
    validate_domain_name(&value.name)?;

    let query = "UPDATE domains
                 SET name = $1,
                     updated = GREATEST(statement_timestamp(), updated + interval '1 microsecond'),
                     soa = $2,
                     default_ttl = $6
                 WHERE id = $3
                   AND updated = $4
                   AND vpc_id IS NOT DISTINCT FROM $5
                 RETURNING id, name, default_ttl, vpc_id, created, updated, deleted, soa, domain_metadata_id";

    sqlx::query_as::<_, DbDomain>(query)
        .bind(&value.name)
        .bind(sqlx::types::Json(&value.soa))
        .bind(value.id)
        .bind(value.updated)
        .bind(value.vpc_id)
        .bind(value.default_ttl)
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

// Records are derived from inventory, so no single write path owns a zone's
// content. Each inventory writer that changes what a zone publishes calls one
// of these helpers in its own transaction. `bump_serial` holds the rule; the
// other helpers only resolve which zones an inventory row publishes into.

/// Advances the serial of every live zone in `domain_ids`.
///
/// `updated` advances as well, so `update` and `delete`, which compare it as
/// an optimistic-lock token, fail with `ConcurrentModificationError` instead
/// of writing a serial computed from the value they read before this bump.
pub async fn bump_serial(txn: &mut PgConnection, domain_ids: &[DomainId]) -> DatabaseResult<()> {
    if domain_ids.is_empty() {
        return Ok(());
    }
    let query = "UPDATE domains
                 SET soa = jsonb_set(soa, '{serial}', to_jsonb(GREATEST(
                         (soa->>'serial')::bigint + 1,
                         floor(extract(epoch FROM statement_timestamp()))::bigint))),
                     updated = GREATEST(statement_timestamp(), updated + interval '1 microsecond')
                 WHERE id = ANY($1) AND deleted IS NULL AND soa ? 'serial'";
    sqlx::query(query)
        .bind(domain_ids)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Advances the serial of the zone each of the given machine interfaces
/// publishes into.
pub async fn bump_serial_for_interfaces(
    txn: &mut PgConnection,
    interface_ids: &[carbide_uuid::machine::MachineInterfaceId],
) -> DatabaseResult<()> {
    if interface_ids.is_empty() {
        return Ok(());
    }
    let query = "SELECT DISTINCT domain_id FROM machine_interfaces
                 WHERE id = ANY($1) AND domain_id IS NOT NULL";
    let zones: Vec<DomainId> = sqlx::query_scalar(query)
        .bind(interface_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    bump_serial(txn, &zones).await
}

/// Advances the serial of the zone a machine interface publishes into.
pub async fn bump_serial_for_interface(
    txn: &mut PgConnection,
    interface_id: carbide_uuid::machine::MachineInterfaceId,
) -> DatabaseResult<()> {
    bump_serial_for_interfaces(txn, &[interface_id]).await
}

/// Advances the serial of the zone each interface of a machine publishes into.
pub async fn bump_serial_for_machine_interfaces(
    txn: &mut PgConnection,
    machine_id: &carbide_uuid::machine::MachineId,
) -> DatabaseResult<()> {
    let query = "SELECT DISTINCT domain_id FROM machine_interfaces
                 WHERE machine_id = $1 AND domain_id IS NOT NULL";
    let zones: Vec<DomainId> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    bump_serial(txn, &zones).await
}

/// Advances the serial of each segment's subdomain, the zone that publishes
/// instance addresses allocated on that segment.
pub async fn bump_serial_for_segments(
    txn: &mut PgConnection,
    segment_ids: &[carbide_uuid::network::NetworkSegmentId],
) -> DatabaseResult<()> {
    if segment_ids.is_empty() {
        return Ok(());
    }
    let query = "SELECT DISTINCT subdomain_id FROM network_segments
                 WHERE id = ANY($1) AND subdomain_id IS NOT NULL";
    let zones: Vec<DomainId> = sqlx::query_scalar(query)
        .bind(segment_ids)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    bump_serial(txn, &zones).await
}

#[cfg(test)]
mod test_find_longest_live_zone {
    use model::dns::NewDomain;

    use crate as db;

    #[crate::sqlx_test]
    async fn finds_the_longest_live_site_zone(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.expect("begin");
        for name in ["example.com", "mysite.example.com."] {
            db::dns::domain::persist(NewDomain::new(name), &mut txn)
                .await
                .expect("persist domain");
        }
        let deleted = db::dns::domain::persist(NewDomain::new("gpu.mysite.example.com"), &mut txn)
            .await
            .expect("persist domain");
        db::dns::domain::delete(deleted, &mut txn)
            .await
            .expect("delete domain");

        // Registering a VPC domain must not make the DNS handler select it as a zone.
        let vpc_id = crate::test_support::vpc::insert_vpc(txn.as_mut(), "unpublished").await;
        db::dns::domain::persist(
            NewDomain {
                vpc_id: Some(vpc_id),
                ..NewDomain::new("owned.example")
            },
            &mut txn,
        )
        .await
        .expect("persist VPC-owned domain");

        let suffixes = |name: &str| {
            model::dns::Fqdn::parse(name)
                .expect("fixture name")
                .suffixes()
        };

        let held = db::dns::domain::find_longest_live_zone(
            txn.as_mut(),
            &suffixes("gpu.mysite.example.com."),
        )
        .await
        .expect("query")
        .expect("a live zone encloses the name");
        assert_eq!(
            held.name, "mysite.example.com.",
            "the longest live match wins over its parent and its deleted child, in stored spelling"
        );

        for name in ["www.example.org.", "www.owned.example."] {
            let held = db::dns::domain::find_longest_live_zone(txn.as_mut(), &suffixes(name))
                .await
                .expect("query");
            assert!(
                held.is_none(),
                "no live site zone encloses {name}: {held:?}"
            );
        }
    }
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
