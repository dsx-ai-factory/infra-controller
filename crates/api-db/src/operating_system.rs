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

use carbide_ipxe_renderer::{
    IpxeTemplateArtifact, IpxeTemplateArtifactCacheStrategy, IpxeTemplateParameter,
};
use carbide_uuid::{DbColumns, DbTable};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::DatabaseError;

pub const OS_STATUS_READY: &str = "READY";
pub const OS_STATUS_PROVISIONING: &str = "PROVISIONING";

fn ipxe_parameters_from_json(
    j: Option<&sqlx::types::Json<serde_json::Value>>,
) -> Vec<IpxeTemplateParameter> {
    j.and_then(|j| j.0.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let obj = v.as_object()?;
                    Some(IpxeTemplateParameter {
                        name: obj.get("name")?.as_str()?.to_string(),
                        value: obj.get("value")?.as_str().unwrap_or("").to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn ipxe_artifacts_from_json(
    j: Option<&sqlx::types::Json<serde_json::Value>>,
) -> Vec<IpxeTemplateArtifact> {
    j.and_then(|j| j.0.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let obj = v.as_object()?;
                    let cache_strategy = match obj
                        .get("cache_strategy")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                    {
                        1 => IpxeTemplateArtifactCacheStrategy::LocalOnly,
                        2 => IpxeTemplateArtifactCacheStrategy::CachedOnly,
                        3 => IpxeTemplateArtifactCacheStrategy::RemoteOnly,
                        _ => IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                    };
                    Some(IpxeTemplateArtifact {
                        name: obj.get("name")?.as_str()?.to_string(),
                        url: obj.get("url")?.as_str().unwrap_or("").to_string(),
                        sha: obj.get("sha").and_then(|v| v.as_str()).map(String::from),
                        auth_type: obj
                            .get("auth_type")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        auth_token: obj
                            .get("auth_token")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        cache_strategy,
                        cached_url: obj
                            .get("cached_url")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

impl From<&OperatingSystem> for model::operating_system_definition::OperatingSystem {
    fn from(row: &OperatingSystem) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.name.clone(),
            description: row.description.clone(),
            tenant_organization_id: row.org.clone(),
            type_: row.type_.clone(),
            status: row.status.clone(),
            is_active: row.is_active,
            allow_override: row.allow_override,
            phone_home_enabled: row.phone_home_enabled,
            user_data: row.user_data.clone(),
            created: row.created.to_rfc3339(),
            updated: row.updated.to_rfc3339(),
            ipxe_script: row.ipxe_script.clone(),
            ipxe_template_id: row.ipxe_template_id.clone(),
            ipxe_template_parameters: ipxe_parameters_from_json(row.ipxe_parameters.as_ref()),
            ipxe_template_artifacts: ipxe_artifacts_from_json(row.ipxe_artifacts.as_ref()),
            ipxe_template_definition_hash: row.ipxe_definition_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, FromRow, Deserialize)]
pub struct OperatingSystem {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub org: Option<String>,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    pub is_active: bool,
    pub allow_override: bool,
    pub phone_home_enabled: bool,
    pub user_data: Option<String>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub deleted: Option<DateTime<Utc>>,
    pub ipxe_script: Option<String>,
    pub ipxe_template_id: Option<String>,
    pub ipxe_parameters: Option<sqlx::types::Json<serde_json::Value>>,
    pub ipxe_artifacts: Option<sqlx::types::Json<serde_json::Value>>,
    pub ipxe_definition_hash: Option<String>,
}

// The derive doesn't support SQLx field attributes. Keep `type_` mapped to
// SQL's `type` column, matching `FromRow`.
impl DbTable for OperatingSystem {
    fn db_table_name() -> &'static str {
        "operating_systems"
    }

    fn db_table_columns() -> DbColumns {
        DbColumns::new(&[
            "id",
            "name",
            "description",
            "org",
            "type",
            "status",
            "is_active",
            "allow_override",
            "phone_home_enabled",
            "user_data",
            "created",
            "updated",
            "deleted",
            "ipxe_script",
            "ipxe_template_id",
            "ipxe_parameters",
            "ipxe_artifacts",
            "ipxe_definition_hash",
        ])
    }
}

pub async fn get(
    txn: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<OperatingSystem, DatabaseError> {
    let query = format!(
        "SELECT {} FROM operating_systems WHERE id = $1 AND deleted IS NULL",
        OperatingSystem::db_table_columns()
    );
    sqlx::query_as::<_, OperatingSystem>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Fetches multiple operating systems by id. Missing ids are skipped (no error).
pub async fn get_many(
    txn: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    ids: &[Uuid],
) -> Result<Vec<OperatingSystem>, DatabaseError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        "SELECT {} FROM operating_systems WHERE id = ANY($1) AND deleted IS NULL",
        OperatingSystem::db_table_columns()
    );
    sqlx::query_as::<_, OperatingSystem>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Returns only ids for operating systems matching the filter (optional org). Order by name.
pub async fn list_ids(
    txn: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    org: Option<&str>,
) -> Result<Vec<Uuid>, DatabaseError> {
    if let Some(org) = org {
        let query =
            "SELECT id FROM operating_systems WHERE org = $1 AND deleted IS NULL ORDER BY name";
        sqlx::query_scalar::<_, Uuid>(query)
            .bind(org)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))
    } else {
        let query = "SELECT id FROM operating_systems WHERE deleted IS NULL ORDER BY name";
        sqlx::query_scalar::<_, Uuid>(query)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))
    }
}

#[derive(Debug)]
pub struct CreateOperatingSystem {
    pub id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub org: Option<String>,
    pub type_: String,
    pub status: String,
    pub is_active: bool,
    pub allow_override: bool,
    pub phone_home_enabled: bool,
    pub user_data: Option<String>,
    pub ipxe_script: Option<String>,
    pub ipxe_template_id: Option<String>,
    pub ipxe_parameters: Option<serde_json::Value>,
    pub ipxe_artifacts: Option<serde_json::Value>,
    pub ipxe_definition_hash: Option<String>,
}

pub async fn create(
    txn: &mut PgConnection,
    input: &CreateOperatingSystem,
) -> Result<OperatingSystem, DatabaseError> {
    let row = if let Some(id) = input.id {
        let query = format!(
            "INSERT INTO operating_systems
            (id, name, description, org, type, status, is_active, allow_override, phone_home_enabled, user_data,
             ipxe_script, ipxe_template_id, ipxe_parameters, ipxe_artifacts, ipxe_definition_hash)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            RETURNING {}",
            OperatingSystem::db_table_columns()
        );
        sqlx::query_as::<_, OperatingSystem>(sqlx::AssertSqlSafe(query.as_str()))
            .bind(id)
            .bind(&input.name)
            .bind(&input.description)
            .bind(&input.org)
            .bind(&input.type_)
            .bind(&input.status)
            .bind(input.is_active)
            .bind(input.allow_override)
            .bind(input.phone_home_enabled)
            .bind(&input.user_data)
            .bind(&input.ipxe_script)
            .bind(&input.ipxe_template_id)
            .bind(input.ipxe_parameters.as_ref().map(sqlx::types::Json))
            .bind(input.ipxe_artifacts.as_ref().map(sqlx::types::Json))
            .bind(&input.ipxe_definition_hash)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(&query, e))?
    } else {
        let query = format!(
            "INSERT INTO operating_systems
            (name, description, org, type, status, is_active, allow_override, phone_home_enabled, user_data,
             ipxe_script, ipxe_template_id, ipxe_parameters, ipxe_artifacts, ipxe_definition_hash)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            RETURNING {}",
            OperatingSystem::db_table_columns()
        );
        sqlx::query_as::<_, OperatingSystem>(sqlx::AssertSqlSafe(query.as_str()))
            .bind(&input.name)
            .bind(&input.description)
            .bind(&input.org)
            .bind(&input.type_)
            .bind(&input.status)
            .bind(input.is_active)
            .bind(input.allow_override)
            .bind(input.phone_home_enabled)
            .bind(&input.user_data)
            .bind(&input.ipxe_script)
            .bind(&input.ipxe_template_id)
            .bind(input.ipxe_parameters.as_ref().map(sqlx::types::Json))
            .bind(input.ipxe_artifacts.as_ref().map(sqlx::types::Json))
            .bind(&input.ipxe_definition_hash)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(&query, e))?
    };
    Ok(row)
}

#[derive(Debug)]
pub struct UpdateOperatingSystem {
    pub id: Uuid,
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_active: Option<bool>,
    pub allow_override: Option<bool>,
    pub phone_home_enabled: Option<bool>,
    pub user_data: Option<String>,
    pub ipxe_script: Option<String>,
    pub ipxe_template_id: Option<String>,
    pub ipxe_parameters: Option<serde_json::Value>,
    pub ipxe_artifacts: Option<serde_json::Value>,
    pub ipxe_definition_hash: Option<String>,
    /// If Some, overrides the stored status string (e.g. "READY", "PROVISIONING").
    /// If None, the existing status is preserved.
    pub status: Option<String>,
}

pub async fn update(
    txn: &mut PgConnection,
    existing: &OperatingSystem,
    input: &UpdateOperatingSystem,
) -> Result<OperatingSystem, DatabaseError> {
    let name = input.name.as_deref().unwrap_or(&existing.name);
    let description = input
        .description
        .as_deref()
        .or(existing.description.as_deref());
    let is_active = input.is_active.unwrap_or(existing.is_active);
    let allow_override = input.allow_override.unwrap_or(existing.allow_override);
    let phone_home_enabled = input
        .phone_home_enabled
        .unwrap_or(existing.phone_home_enabled);
    let user_data = input.user_data.as_deref().or(existing.user_data.as_deref());
    let ipxe_script = input
        .ipxe_script
        .as_deref()
        .or(existing.ipxe_script.as_deref());
    let ipxe_template_id = input
        .ipxe_template_id
        .as_deref()
        .or(existing.ipxe_template_id.as_deref());
    let status = input.status.as_deref().unwrap_or(&existing.status);

    let ipxe_parameters: Option<sqlx::types::Json<&serde_json::Value>> = input
        .ipxe_parameters
        .as_ref()
        .or(existing.ipxe_parameters.as_ref().map(|j| &j.0))
        .map(sqlx::types::Json);
    let ipxe_artifacts: Option<sqlx::types::Json<&serde_json::Value>> = input
        .ipxe_artifacts
        .as_ref()
        .or(existing.ipxe_artifacts.as_ref().map(|j| &j.0))
        .map(sqlx::types::Json);

    let ipxe_definition_hash = input
        .ipxe_definition_hash
        .as_deref()
        .or(existing.ipxe_definition_hash.as_deref());

    let query = format!(
        "UPDATE operating_systems SET
        name = $1, description = $2, is_active = $3, allow_override = $4,
        phone_home_enabled = $5, user_data = $6, ipxe_script = $7,
        ipxe_template_id = $8, ipxe_parameters = $9, ipxe_artifacts = $10,
        ipxe_definition_hash = $11, status = $12, updated = NOW()
        WHERE id = $13 AND deleted IS NULL
        RETURNING {}",
        OperatingSystem::db_table_columns()
    );
    sqlx::query_as::<_, OperatingSystem>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(name)
        .bind(description)
        .bind(is_active)
        .bind(allow_override)
        .bind(phone_home_enabled)
        .bind(user_data)
        .bind(ipxe_script)
        .bind(ipxe_template_id)
        .bind(ipxe_parameters)
        .bind(ipxe_artifacts)
        .bind(ipxe_definition_hash)
        .bind(status)
        .bind(input.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

pub async fn delete(txn: &mut PgConnection, id: Uuid) -> Result<(), DatabaseError> {
    let query = "UPDATE operating_systems SET deleted = NOW(), updated = NOW() WHERE id = $1 AND deleted IS NULL";
    let result = sqlx::query(query)
        .bind(id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "OperatingSystem",
            id: id.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use model::operating_system_definition::{OS_TYPE_IPXE, OS_TYPE_TEMPLATED_IPXE};
    use uuid::Uuid;

    use super::*;

    #[crate::sqlx_test]
    async fn test_get_returns_err_for_missing_id(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let id = Uuid::nil();
        let err = get(&mut *txn, id).await.unwrap_err();
        assert!(matches!(err, crate::DatabaseError::Sqlx(_)));
    }

    #[crate::sqlx_test]
    async fn test_get_many_returns_empty_for_missing_ids(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let rows = get_many(&mut *txn, &[Uuid::nil()]).await.unwrap();
        assert!(rows.is_empty());
    }

    #[crate::sqlx_test]
    async fn test_create_and_get_preserve_record_columns(pool: sqlx::PgPool) {
        // Both INSERT paths and `get` use the same column list. Check the
        // decoded values, including `type_`, nullable fields, and JSON data.
        fn assert_record(actual: &OperatingSystem, expected: &OperatingSystem, operation: &str) {
            let context = format!("{operation} for {}", expected.name);
            assert_eq!(actual.id, expected.id, "{context}: id");
            assert_eq!(actual.name, expected.name, "{context}: name");
            assert_eq!(
                actual.description, expected.description,
                "{context}: description"
            );
            assert_eq!(actual.org, expected.org, "{context}: org");
            assert_eq!(actual.type_, expected.type_, "{context}: type_");
            assert_eq!(actual.status, expected.status, "{context}: status");
            assert_eq!(actual.is_active, expected.is_active, "{context}: is_active");
            assert_eq!(
                actual.allow_override, expected.allow_override,
                "{context}: allow_override"
            );
            assert_eq!(
                actual.phone_home_enabled, expected.phone_home_enabled,
                "{context}: phone_home_enabled"
            );
            assert_eq!(actual.user_data, expected.user_data, "{context}: user_data");
            assert_eq!(actual.created, expected.created, "{context}: created");
            assert_eq!(actual.updated, expected.updated, "{context}: updated");
            assert_eq!(actual.deleted, expected.deleted, "{context}: deleted");
            assert_eq!(
                actual.ipxe_script, expected.ipxe_script,
                "{context}: ipxe_script"
            );
            assert_eq!(
                actual.ipxe_template_id, expected.ipxe_template_id,
                "{context}: ipxe_template_id"
            );
            assert_eq!(
                actual.ipxe_parameters, expected.ipxe_parameters,
                "{context}: ipxe_parameters"
            );
            assert_eq!(
                actual.ipxe_artifacts, expected.ipxe_artifacts,
                "{context}: ipxe_artifacts"
            );
            assert_eq!(
                actual.ipxe_definition_hash, expected.ipxe_definition_hash,
                "{context}: ipxe_definition_hash"
            );
        }

        let cases = [
            CreateOperatingSystem {
                id: None,
                name: "no-org-os".to_string(),
                description: None,
                org: None,
                type_: OS_TYPE_IPXE.to_string(),
                status: OS_STATUS_READY.to_string(),
                is_active: true,
                allow_override: false,
                phone_home_enabled: true,
                user_data: None,
                ipxe_script: Some("#!ipxe\nboot".to_string()),
                ipxe_template_id: None,
                ipxe_parameters: None,
                ipxe_artifacts: None,
                ipxe_definition_hash: None,
            },
            CreateOperatingSystem {
                id: Some(Uuid::new_v4()),
                name: "org-os".to_string(),
                description: Some("Template-based operating system".to_string()),
                org: Some("acme".to_string()),
                type_: OS_TYPE_TEMPLATED_IPXE.to_string(),
                status: OS_STATUS_PROVISIONING.to_string(),
                is_active: false,
                allow_override: true,
                phone_home_enabled: false,
                user_data: Some("#cloud-config\nhostname: test-host".to_string()),
                ipxe_script: None,
                ipxe_template_id: Some("ubuntu-template".to_string()),
                ipxe_parameters: Some(serde_json::json!([
                    {"name": "arch", "value": "aarch64"}
                ])),
                ipxe_artifacts: Some(serde_json::json!([
                    {"name": "kernel", "url": "https://example.com/vmlinuz", "cache_strategy": 2}
                ])),
                ipxe_definition_hash: Some("0123456789abcdef".repeat(4)),
            },
        ];

        for input in cases {
            let mut txn = pool.begin().await.unwrap();
            // The timestamp defaults use the transaction's `NOW()` value.
            let timestamp: DateTime<Utc> = sqlx::query_scalar("SELECT NOW()")
                .fetch_one(&mut *txn)
                .await
                .unwrap();

            let created = create(&mut txn, &input).await.unwrap();
            assert!(!created.id.is_nil(), "{} has an ID", input.name);
            let expected = OperatingSystem {
                id: input.id.unwrap_or(created.id),
                name: input.name,
                description: input.description,
                org: input.org,
                type_: input.type_,
                status: input.status,
                is_active: input.is_active,
                allow_override: input.allow_override,
                phone_home_enabled: input.phone_home_enabled,
                user_data: input.user_data,
                created: timestamp,
                updated: timestamp,
                deleted: None,
                ipxe_script: input.ipxe_script,
                ipxe_template_id: input.ipxe_template_id,
                ipxe_parameters: input.ipxe_parameters.map(sqlx::types::Json),
                ipxe_artifacts: input.ipxe_artifacts.map(sqlx::types::Json),
                ipxe_definition_hash: input.ipxe_definition_hash,
            };
            assert_record(&created, &expected, "create");

            let fetched = get(&mut *txn, created.id).await.unwrap();
            assert_record(&fetched, &expected, "get");
        }
    }
}
