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

use carbide_uuid::compute_allocation::ComputeAllocationId;
use carbide_uuid::instance_type::InstanceTypeId;
use config_version::ConfigVersion;
use model::compute_allocation::ComputeAllocation;
use model::metadata::Metadata;
use model::tenant::TenantOrganizationId;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, Row};

use crate::DatabaseError;

/// Something to hold the sum of a set of allocations.
#[derive(Clone, Debug, PartialEq)]
pub struct Sum {
    pub instance_type_id: InstanceTypeId,
    pub value: i32,
}

impl<'r> sqlx::FromRow<'r, PgRow> for Sum {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let total: Option<i32> = row.try_get("total")?;

        let total = Sum {
            instance_type_id: row.try_get("instance_type_id")?,
            value: total.unwrap_or_default(),
        };

        Ok(total)
    }
}

/// Creates a new ComputeAllocation DB record.  It enforces a unique `name` by
/// only creating if there is no active record found with the same name.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `id`                     - A reference to a ComputeAllocationId to be set as
///   the id for the new ComputeAllocation
/// * `tenant_organization_id` - A reference to a TenantOrganizationId containing the
///   tenant org that owns this ComputeAllocation
/// * `created_by`             - Optional String containing an ID to track the user who
///   created the ComputeAllocation
/// * `metadata`               - A reference to a Metadata struct containing extra
///   details about the ComputeAllocation
/// * `count`                  - The amount of compute (# of machines) to allocate.
/// * `instance_type_id`       - The type of machines being allocated.
pub async fn create(
    txn: &mut PgConnection,
    id: &ComputeAllocationId,
    tenant_organization_id: &TenantOrganizationId,
    created_by: Option<&str>,
    metadata: &Metadata,
    count: i32,
    instance_type_id: &InstanceTypeId,
) -> Result<ComputeAllocation, DatabaseError> {
    let query = "INSERT INTO compute_allocations
                (id, tenant_organization_id, instance_type_id, name, labels, description, count, version, created_by)
            SELECT $1, $2::varchar, $3::varchar, $4::varchar, $5::jsonb, $6::varchar, $7::int, $8::varchar, $9::varchar
            WHERE NOT EXISTS
                /* There should be a unique constraint on id.  The condition here is just defensive. */
                (SELECT id FROM compute_allocations WHERE (id=$1 OR (name=$4::varchar AND tenant_organization_id=$2::varchar)) AND deleted IS NULL)
            RETURNING id, tenant_organization_id, instance_type_id, name, labels,
                description, count, version, created_by, updated_by, created, deleted";

    match sqlx::query_as::<Postgres, ComputeAllocation>(query)
        .bind(id)
        .bind(tenant_organization_id.to_string())
        .bind(instance_type_id)
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(&metadata.description)
        .bind(count)
        .bind(ConfigVersion::initial())
        .bind(created_by)
        .fetch_one(txn)
        .await
    {
        Ok(compute_allocation) => Ok(compute_allocation),
        // This error should only show up when we didn't
        // create a record because the subquery found an existing name already.
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::AlreadyFoundError {
            kind: "ComputeAllocation",
            id: metadata.name.clone(),
        }),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Returns a list of IDs for all non-deleted ComputeAllocation records.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `name`                   - Optional String containing the name of a desired
///   ComputeAllocation
/// * `tenant_organization_id` - Optional TenantOrganizationId containing the tenant
///   org to match against ComputeAllocation records.
/// * `for_update`             - A boolean flag to acquire DB locks for
///   synchronization
pub async fn find_ids(
    txn: &mut PgConnection,
    name: Option<&str>,
    tenant_organization_id: Option<&TenantOrganizationId>,
    instance_type_ids: Option<&[InstanceTypeId]>,
    for_update: bool,
) -> Result<Vec<ComputeAllocationId>, DatabaseError> {
    let mut builder =
        sqlx::QueryBuilder::new("SELECT id FROM compute_allocations WHERE deleted is NULL");

    if name.is_some() {
        builder.push(" AND name = ");
        builder.push_bind(name);
    }

    if tenant_organization_id.is_some() {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    if let Some(instance_type_ids) = instance_type_ids {
        builder.push(" AND instance_type_id = ANY (");
        builder.push_bind(instance_type_ids);
        builder.push(" )");
    }

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err: sqlx::Error| DatabaseError::query(builder.sql(), err))
}

/// Queries the DB for non-deleted ComputeAllocation records
/// based on the supplied list of IDs
///
/// * `txn`                    - A reference to an active DB transaction
/// * `compute_allocation_ids` - A list of ComputeAllocationId values to use for
///   querying the Db for active ComputeAllocation records
/// * `tenant_organization_id` - Optional reference to TenantOrganizationId containing the
///   tenant org to match against ComputeAllocation records.
/// * `for_update`             - A boolean flag to acquire DB locks for synchronization
pub async fn find_by_ids(
    txn: &mut PgConnection,
    compute_allocation_ids: &[ComputeAllocationId],
    tenant_organization_id: Option<&TenantOrganizationId>,
    for_update: bool,
) -> Result<Vec<ComputeAllocation>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT id, tenant_organization_id, instance_type_id, name, labels,
            description, count, version, created_by, updated_by, created, deleted
         from compute_allocations WHERE deleted is NULL",
    );

    builder.push(" AND id = ANY(");
    builder.push_bind(compute_allocation_ids);
    builder.push(") ");

    if tenant_organization_id.is_some() {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.map(|t| t.to_string()));
    }

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|err: sqlx::Error| DatabaseError::query(builder.sql(), err))
}

/// Queries the DB for non-deleted ComputeAllocation records
/// based on the supplied list of instance type IDs and sums their
/// allocation count to give a total allocation for an instance type.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `instance_type_ids`      - An optional list of instance type IDs for querying.
/// * `tenant_organization_id` - Optional reference to TenantOrganizationId containing the
///   tenant org to match against ComputeAllocation records.
/// * `compute_allocation_ids` - Optional list of ComputeAllocationId values to use for
///   querying the Db for active ComputeAllocation records
/// * `for_update`             - A boolean flag to acquire DB locks for synchronization
pub async fn sum_allocations(
    txn: &mut PgConnection,
    instance_type_ids: &[InstanceTypeId],
    tenant_organization_id: Option<&TenantOrganizationId>,
    for_update: bool,
) -> Result<HashMap<InstanceTypeId, u32>, DatabaseError> {
    let ids = find_ids(
        txn,
        None,
        tenant_organization_id,
        Some(instance_type_ids),
        for_update,
    )
    .await?;

    let allocs = find_by_ids(txn, &ids, None, for_update).await?;

    let mut sums: HashMap<InstanceTypeId, u32> = HashMap::new();

    for alloc in allocs {
        sums.entry(alloc.instance_type_id)
            .and_modify(|i: &mut u32| {
                *i = i.saturating_add(alloc.count);
            })
            .or_insert(alloc.count);
    }

    Ok(sums)
}

/// Updates a ComputeAllocation record in the DB.
///
/// * `txn`                    - A reference to an active DB transaction
/// * `id`                     - A reference to a ComputeAllocationId to be set as the id
///   for the new ComputeAllocation
/// * `tenant_organization_id` - A reference to a TenantOrganizationId for the tenant org that owns
///   this ComputeAllocation.  The update will will be ignored if
///   this ID does not match that of the requested record.
///   ***Callers are expected to verify the relationship prior to calling this function.***
/// * `metadata`               - A reference to a Metadata struct containing extra details about
///   the ComputeAllocation
/// * `count`                  - Size of the allocation. (# of machines)
/// * `expected_version`       - The version the record is expected to have prior to the update.
///   This will be auto-incremented. If this version passed in does not match the reality of the
///   record, the update will be rejected, but ***callers are expected to have verified this version
///   matches the record in advance.***
/// * `updated_by`             - Optional String containing an ID to track the user who updated the
///   ComputeAllocation
pub async fn update(
    txn: &mut PgConnection,
    id: &ComputeAllocationId,
    tenant_organization_id: &TenantOrganizationId,
    metadata: &Metadata,
    count: i32,
    expected_version: ConfigVersion,
    updated_by: Option<&str>,
) -> Result<ComputeAllocation, DatabaseError> {
    let query = "UPDATE compute_allocations
            SET
                name=$1::varchar,
                labels=$2::jsonb,
                description=$3::varchar,
                count=$4::int,
                version=$5::varchar,
                updated_by=$6::varchar
            WHERE
                /*
                    All but the final `AND NOT EXISTS` are here to be defensive.
                    The cases should have already been covered by a query in advance.
                */
                id=$7
                AND version = $8::varchar
                AND deleted IS NULL
                AND tenant_organization_id = $9::varchar
                AND NOT EXISTS
                    (SELECT id FROM compute_allocations WHERE id!=$7 AND (name=$1::varchar AND tenant_organization_id=$9::varchar AND deleted IS NULL))
            RETURNING id, tenant_organization_id, instance_type_id, name, labels,
                description, count, version, created_by, updated_by, created, deleted";

    match sqlx::query_as::<Postgres, ComputeAllocation>(query)
        .bind(&metadata.name)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(&metadata.description)
        .bind(count)
        .bind(expected_version.increment())
        .bind(updated_by)
        .bind(id)
        .bind(expected_version)
        .bind(tenant_organization_id.to_string())
        .fetch_one(txn)
        .await
    {
        Ok(compute_allocation) => Ok(compute_allocation),
        // This error should only show up when we didn't
        // update a record because the subquery found an existing name already.
        // deleted and version should have already been checked and reported
        // before calling update()
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::AlreadyFoundError {
            kind: "ComputeAllocation",
            id: metadata.name.clone(),
        }),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

/// Soft deletes an allocation by updating the deleted column in the DB.
/// If the record with that ID is already deleted, nothing changes and Ok(None)
/// is returned.
///
/// This does ***NOT*** check for any associations with other objects like VPC, Instance, etc.
///
/// * `txn`                       - A reference to an active DB transaction
/// * `compute_allocation_id`     - An ComputeAllocationId for the ComputeAllocation to be soft-deleted.
/// * `tenant_organization_id`    - A reference to a TenantOrganizationId of the tenant organization
///   that owns the ComputeAllocation.  The delete will be ignored if this ID does not match that of
///   the requested record. ***Callers are expected to verify the relationship prior to calling this
///   function.***
pub async fn soft_delete(
    txn: &mut PgConnection,
    compute_allocation_id: &ComputeAllocationId,
    tenant_organization_id: &TenantOrganizationId,
) -> Result<Option<ComputeAllocationId>, DatabaseError> {
    let query = "UPDATE compute_allocations SET deleted=NOW() WHERE id=$1 AND tenant_organization_id=$2::varchar AND deleted is NULL RETURNING id";

    sqlx::query_as(query)
        .bind(compute_allocation_id)
        .bind(tenant_organization_id.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|err: sqlx::Error| DatabaseError::query(query, err))
}

#[cfg(test)]
mod tests {
    use sqlx::Connection;

    use super::*;

    #[crate::sqlx_test]
    async fn compute_allocation_queries_survive_added_columns(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut api_connection = pool.acquire().await?;
        exercise_compute_allocation_queries(&mut api_connection).await?;
        assert!(api_connection.cached_statements_size() > 0);

        // Keep the API connection and its prepared statements while another
        // connection applies the schema change, just as a migration would.
        let mut migration = pool.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL lock_timeout = '5s';
             ALTER TABLE compute_allocations ADD COLUMN test_added_column text;",
        )
        .execute(&mut *migration)
        .await?;
        migration.commit().await?;

        exercise_compute_allocation_queries(&mut api_connection).await?;
        Ok(())
    }

    async fn exercise_compute_allocation_queries(
        connection: &mut PgConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = connection.begin().await?;
        let tenant = crate::tenant::create_and_persist(
            "projection-tenant".to_string(),
            Metadata {
                name: "allocation tenant".to_string(),
                ..Default::default()
            },
            None,
            &mut txn,
        )
        .await?;
        let instance_type_id: InstanceTypeId = "projection-instance-type".parse()?;
        crate::instance_type::create(
            &mut txn,
            &instance_type_id,
            &Metadata {
                name: "allocation instance type".to_string(),
                ..Default::default()
            },
            &[],
        )
        .await?;

        let id = ComputeAllocationId::new();
        let metadata = Metadata {
            name: "projection allocation".to_string(),
            description: "allocation description".to_string(),
            labels: HashMap::from([("allocation-label".to_string(), "original".to_string())]),
        };
        let mut expected = create(
            &mut txn,
            &id,
            &tenant.organization_id,
            Some("allocation creator"),
            &metadata,
            7,
            &instance_type_id,
        )
        .await?;
        assert_eq!(expected.id, id);
        assert_eq!(expected.tenant_organization_id, tenant.organization_id);
        assert_eq!(expected.instance_type_id, instance_type_id);
        assert_eq!(expected.metadata, metadata);
        assert_eq!(expected.count, 7);
        assert_eq!(expected.version.version_nr(), 1);
        assert_eq!(expected.created_by.as_deref(), Some("allocation creator"));
        assert_eq!(expected.updated_by, None);
        assert_eq!(expected.deleted, None);
        assert!(expected.created > chrono::DateTime::UNIX_EPOCH);

        let metadata = Metadata {
            name: "renamed allocation".to_string(),
            description: "updated description".to_string(),
            labels: HashMap::from([("allocation-label".to_string(), "updated".to_string())]),
        };
        let updated = update(
            &mut txn,
            &id,
            &tenant.organization_id,
            &metadata,
            11,
            expected.version,
            Some("allocation updater"),
        )
        .await?;
        assert_eq!(updated.version.version_nr(), 2);
        expected.version = updated.version;
        expected.metadata = metadata;
        expected.count = 11;
        expected.updated_by = Some("allocation updater".to_string());
        assert_eq!(updated, expected);
        assert_eq!(
            find_by_ids(&mut txn, &[id], Some(&tenant.organization_id), true).await?,
            vec![expected]
        );

        // Roll back the fixture so both passes decode an inserted row.
        // The connection retains its prepared statements after the rollback.
        txn.rollback().await?;
        Ok(())
    }
}
