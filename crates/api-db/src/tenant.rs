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

use config_version::ConfigVersion;
use model::metadata::Metadata;
use model::tenant::{Tenant, TenantPublicKeyValidationRequest};
use sqlx::PgConnection;

use super::ObjectFilter;
use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

type OrganizationID = String;

pub async fn create_and_persist(
    organization_id: String,
    metadata: Metadata,
    routing_profile_type: Option<String>,
    txn: &mut PgConnection,
) -> Result<Tenant, DatabaseError> {
    let version = ConfigVersion::initial();
    let query = "INSERT INTO tenants (organization_id, organization_name, version, routing_profile_type) VALUES ($1, $2, $3, $4)
        RETURNING organization_id, organization_name, version, routing_profile_type";

    sqlx::query_as(query)
        .bind(organization_id)
        .bind(metadata.name)
        .bind(version)
        .bind(routing_profile_type)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find<S: AsRef<str>>(
    organization_id: S,
    for_update: bool,
    txn: &mut PgConnection,
) -> Result<Option<Tenant>, DatabaseError> {
    let mut query = sqlx::QueryBuilder::new(
        "SELECT organization_id, organization_name, version, routing_profile_type
         FROM tenants WHERE organization_id = $1",
    );

    if for_update {
        query.push(" FOR UPDATE ");
    }

    let results = query
        .build_query_as()
        .bind(organization_id.as_ref())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    Ok(results)
}
/// Expects locking/coordination and the check for organization_id's
/// existence to be handled by the caller.
pub async fn update(
    organization_id: String,
    metadata: Metadata,
    expected_version: ConfigVersion,
    routing_profile_type: Option<String>,
    txn: &mut PgConnection,
) -> DatabaseResult<Tenant> {
    let next_version = expected_version.increment();

    let query = "UPDATE tenants
            SET
                version=$1,
                organization_name=$2,
                routing_profile_type=$3
            WHERE
                organization_id=$4
                AND
                version=$5
            RETURNING organization_id, organization_name, version, routing_profile_type";

    sqlx::query_as(query)
        .bind(next_version)
        .bind(metadata.name)
        .bind(routing_profile_type)
        .bind(organization_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("tenant", expected_version.to_string())
            }
            error => DatabaseError::query(query, error),
        })
}

/// Increments the tenant version. Used when identity config or token delegation changes.
/// Caller must hold a transaction. Returns error if tenant not found.
pub async fn increment_version<S: AsRef<str>>(
    organization_id: S,
    txn: &mut PgConnection,
) -> DatabaseResult<()> {
    let tenant = find(organization_id.as_ref(), true, txn)
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "Tenant",
            id: organization_id.as_ref().to_string(),
        })?;
    let next_version = tenant.version.increment();
    let query = "UPDATE tenants SET version=$1 WHERE organization_id=$2 AND version=$3";
    let result = sqlx::query(query)
        .bind(next_version)
        .bind(organization_id.as_ref())
        .bind(tenant.version)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::ConcurrentModificationError(
            "tenant",
            tenant.version.to_string(),
        ));
    }
    Ok(())
}

pub async fn find_tenant_organization_ids(
    txn: impl DbReader<'_>,
    search_config: model::tenant::TenantSearchFilter,
) -> Result<Vec<OrganizationID>, DatabaseError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT organization_id FROM tenants");

    if let Some(tenant_org_name) = &search_config.tenant_organization_name {
        qb.push(" WHERE organization_name = ");
        qb.push_bind(tenant_org_name);
    }

    let tenant_organization_ids: Vec<OrganizationID> = qb
        .build_query_as::<(String,)>()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("find_tenant_organization_ids", e))?
        .into_iter()
        .map(|row| row.0)
        .collect();

    Ok(tenant_organization_ids)
}

pub async fn validate_public_key(
    request: &TenantPublicKeyValidationRequest,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let instance = crate::instance::find_by_id(&mut *txn, request.instance_id)
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "instance",
            id: request.instance_id.to_string(),
        })?;

    let keysets = crate::tenant_keyset::find(
        Some(instance.config.tenant.tenant_organization_id.to_string()),
        ObjectFilter::List(&instance.config.tenant.tenant_keyset_ids),
        true,
        txn,
    )
    .await?;

    request.validate_key(keysets).map_err(DatabaseError::from)
}

pub async fn load_by_organization_ids(
    txn: &mut PgConnection,
    organization_ids: &[String],
) -> Result<Vec<Tenant>, DatabaseError> {
    let query = "SELECT organization_id, organization_name, version, routing_profile_type
        from tenants WHERE organization_id = ANY($1)";
    sqlx::query_as(query)
        .bind(organization_ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use std::ops::DerefMut;

    use sqlx::Connection;

    use super::*;

    #[crate::sqlx_test]
    async fn tenant_queries_survive_added_columns(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut api_connection = pool.acquire().await?;
        exercise_tenant_queries(&mut api_connection).await?;
        assert!(api_connection.cached_statements_size() > 0);

        // Keep the API connection and its prepared statements while another
        // connection applies the schema change, just as a migration would.
        let mut migration = pool.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL lock_timeout = '5s';
             ALTER TABLE tenants ADD COLUMN test_added_column text;",
        )
        .execute(&mut *migration)
        .await?;
        migration.commit().await?;

        exercise_tenant_queries(&mut api_connection).await?;
        Ok(())
    }

    async fn exercise_tenant_queries(
        connection: &mut PgConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = connection.begin().await?;
        let metadata = Metadata {
            name: "projection tenant".to_string(),
            ..Default::default()
        };
        let tenant = create_and_persist(
            "projection-tenant".to_string(),
            metadata.clone(),
            Some("external".to_string()),
            &mut txn,
        )
        .await?;
        assert_eq!(tenant.organization_id.as_str(), "projection-tenant");
        assert_eq!(tenant.metadata, metadata);
        assert_eq!(tenant.routing_profile_type.as_deref(), Some("external"));
        assert_eq!(tenant.version.version_nr(), 1);

        let metadata = Metadata {
            name: "renamed tenant".to_string(),
            ..Default::default()
        };
        let updated = update(
            tenant.organization_id.to_string(),
            metadata.clone(),
            tenant.version,
            Some("internal".to_string()),
            &mut txn,
        )
        .await?;
        assert_eq!(updated.organization_id, tenant.organization_id);
        assert_eq!(updated.metadata, metadata);
        assert_eq!(updated.routing_profile_type.as_deref(), Some("internal"));
        assert_eq!(updated.version.version_nr(), 2);
        assert_eq!(
            find(updated.organization_id.as_str(), true, &mut txn).await?,
            Some(updated.clone())
        );
        assert_eq!(
            load_by_organization_ids(&mut txn, &[updated.organization_id.to_string()]).await?,
            vec![updated]
        );

        // Roll back the fixture so both passes decode an inserted row.
        // The connection retains its prepared statements after the rollback.
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_null_organization_name(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let result = sqlx::query(
            r#"
            INSERT INTO tenants (organization_id, version, organization_name)
            VALUES
            ('zqrrhxea4ktv', 'V1-T1733777281821769', NULL)
            "#,
        )
        .execute(txn.deref_mut())
        .await;
        let Err(sqlx::Error::Database(e)) = result else {
            panic!("Inserting a NULL should have failed");
        };
        assert!(matches!(e.kind(), sqlx::error::ErrorKind::NotNullViolation));
    }
}
