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
use carbide_uuid::dhcp_option_set::DhcpOptionSetId;
use config_version::ConfigVersion;
use model::dhcp_option_set::{DhcpOptionSet, DhcpOptions};
use model::metadata::Metadata;
use model::tenant::TenantOrganizationId;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

const KIND: &str = "DhcpOptionSet";

/// Creates a new DhcpOptionSet record at its initial version.
pub async fn create(
    txn: &mut PgConnection,
    id: &DhcpOptionSetId,
    tenant_organization_id: &TenantOrganizationId,
    metadata: &Metadata,
    options: &DhcpOptions,
) -> DatabaseResult<DhcpOptionSet> {
    let query = "INSERT INTO dhcp_option_sets
            (id, tenant_organization_id, name, description, labels, options, version)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, tenant_organization_id, name, description, labels, options, version, created";
    sqlx::query_as(query)
        .bind(id)
        .bind(tenant_organization_id.to_string())
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(sqlx::types::Json(options))
        .bind(ConfigVersion::initial())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Returns the DhcpOptionSet records with the given IDs, optionally limited
/// to one tenant and locked for update.
pub async fn find_by_ids(
    txn: &mut PgConnection,
    ids: &[DhcpOptionSetId],
    tenant_organization_id: Option<&TenantOrganizationId>,
    for_update: bool,
) -> DatabaseResult<Vec<DhcpOptionSet>> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT id, tenant_organization_id, name, description, labels, options, version, created
         FROM dhcp_option_sets WHERE id = ANY(",
    );
    builder.push_bind(ids);
    builder.push(")");

    if let Some(tenant_organization_id) = tenant_organization_id {
        builder.push(" AND tenant_organization_id = ");
        builder.push_bind(tenant_organization_id.to_string());
    }
    if for_update {
        builder.push(" ORDER BY id FOR UPDATE");
    }

    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Replaces the metadata and options of a DhcpOptionSet, failing if the
/// record is no longer at `expected_version`.
pub async fn update(
    txn: &mut PgConnection,
    id: &DhcpOptionSetId,
    tenant_organization_id: &TenantOrganizationId,
    metadata: &Metadata,
    options: &DhcpOptions,
    expected_version: ConfigVersion,
) -> DatabaseResult<DhcpOptionSet> {
    let query = "UPDATE dhcp_option_sets
        SET name = $1, description = $2, labels = $3, options = $4, version = $5
        WHERE id = $6 AND tenant_organization_id = $7 AND version = $8
        RETURNING id, tenant_organization_id, name, description, labels, options, version, created";
    sqlx::query_as(query)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(sqlx::types::Json(options))
        .bind(expected_version.increment())
        .bind(id)
        .bind(tenant_organization_id.to_string())
        .bind(expected_version)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError(KIND, expected_version.to_string())
            }
            e => DatabaseError::query(query, e),
        })
}

/// Returns whether any live instance or VPC uses the DhcpOptionSet.
pub async fn is_in_use(txn: &mut PgConnection, id: &DhcpOptionSetId) -> DatabaseResult<bool> {
    let query =
        "SELECT EXISTS (SELECT 1 FROM instances WHERE dhcp_option_set_id = $1 AND deleted IS NULL)
            OR EXISTS (SELECT 1 FROM vpcs WHERE dhcp_option_set_id = $1 AND deleted IS NULL)";
    sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Deletes a DhcpOptionSet owned by the given tenant.
pub async fn delete(
    txn: &mut PgConnection,
    id: &DhcpOptionSetId,
    tenant_organization_id: &TenantOrganizationId,
) -> DatabaseResult<()> {
    let query = "DELETE FROM dhcp_option_sets WHERE id = $1 AND tenant_organization_id = $2";
    let result = sqlx::query(query)
        .bind(id)
        .bind(tenant_organization_id.to_string())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: KIND,
            id: id.to_string(),
        });
    }
    Ok(())
}
