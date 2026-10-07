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
use model::tenant::{TenantKeyset, TenantKeysetId, TenantKeysetIdentifier, UpdateTenantKeyset};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, ObjectFilter};

pub async fn create(
    value: &TenantKeyset,
    txn: &mut PgConnection,
) -> Result<TenantKeyset, DatabaseError> {
    let query = "INSERT INTO tenant_keysets VALUES($1, $2, $3, $4)
        RETURNING organization_id, keyset_id, content, version";

    sqlx::query_as(query)
        .bind(value.keyset_identifier.organization_id.to_string())
        .bind(&value.keyset_identifier.keyset_id)
        .bind(sqlx::types::Json(&value.keyset_content))
        .bind(value.version.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::tenant::TenantKeysetSearchFilter,
) -> Result<Vec<TenantKeysetId>, DatabaseError> {
    // build query
    let mut builder =
        sqlx::QueryBuilder::new("SELECT organization_id, keyset_id FROM tenant_keysets");
    if let Some(tenant_org_id) = &filter.tenant_org_id {
        builder.push(" WHERE organization_id = ");
        builder.push_bind(tenant_org_id);
    }
    // execute
    let query = builder.build_query_as();
    let ids: Vec<TenantKeysetId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("tenant_keyset::find_ids", e))?;

    Ok(ids)
}

pub async fn find_by_ids(
    txn: impl DbReader<'_>,
    ids: Vec<model::tenant::TenantKeysetIdentifier>,
    include_key_data: bool,
) -> Result<Vec<TenantKeyset>, DatabaseError> {
    // build query
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT organization_id, keyset_id, content, version
         FROM tenant_keysets WHERE (organization_id, keyset_id) IN ",
    );
    builder.push_tuples(ids.iter(), |mut b, id| {
        b.push_bind(id.organization_id.to_string())
            .push_bind(&id.keyset_id);
    });
    // execute
    let query = builder.build_query_as();
    let mut keysets: Vec<TenantKeyset> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("tenant_keyset::find_by_ids", e))?;

    if !include_key_data {
        for data in &mut keysets {
            data.keyset_content.public_keys.clear();
        }
    }

    Ok(keysets)
}

pub async fn find(
    organization_id: Option<String>,
    keyset_filter: ObjectFilter<'_, String>,
    include_key_data: bool,
    txn: &mut PgConnection,
) -> Result<Vec<TenantKeyset>, DatabaseError> {
    let mut result = if let Some(organization_id) = organization_id {
        match keyset_filter {
            ObjectFilter::All => sqlx::query_as(
                "SELECT organization_id, keyset_id, content, version
                     FROM tenant_keysets WHERE organization_id = $1",
            )
            .bind(organization_id.to_string())
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::new("keyset All", e)),

            ObjectFilter::One(keyset_id) => {
                let query = "SELECT organization_id, keyset_id, content, version
                    FROM tenant_keysets WHERE organization_id = $1 AND keyset_id = $2";
                sqlx::query_as(query)
                    .bind(organization_id.to_string())
                    .bind(keyset_id)
                    .fetch_all(txn)
                    .await
                    .map_err(|e| DatabaseError::query(query, e))
            }

            ObjectFilter::List(keyset_ids) => {
                let query = "SELECT organization_id, keyset_id, content, version
                    FROM tenant_keysets WHERE organization_id = $1 AND keyset_id = ANY($2)";
                sqlx::query_as(query)
                    .bind(organization_id.to_string())
                    .bind(keyset_ids)
                    .fetch_all(txn)
                    .await
                    .map_err(|e| DatabaseError::query(query, e))
            }
        }
    } else {
        let query = "SELECT organization_id, keyset_id, content, version FROM tenant_keysets";
        sqlx::query_as::<_, TenantKeyset>(query)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))
    }?;

    if !include_key_data {
        for data in &mut result {
            data.keyset_content.public_keys.clear();
        }
    }

    Ok(result)
}

/// Deletes the Keyset
/// - Returns `Ok(true)` if the keyset existed and got deleted
/// - Returns `Ok(false)` if the keyset did not exist
/// - Returns `Err(_)` in case of other errors
pub async fn delete(
    keyset_identifier: &TenantKeysetIdentifier,
    txn: &mut PgConnection,
) -> Result<bool, DatabaseError> {
    let query = "DELETE FROM tenant_keysets WHERE organization_id = $1 AND keyset_id = $2
        RETURNING organization_id, keyset_id, content, version";

    match sqlx::query_as::<_, TenantKeyset>(query)
        .bind(keyset_identifier.organization_id.as_str())
        .bind(&keyset_identifier.keyset_id)
        .fetch_one(txn)
        .await
    {
        Ok(_) => Ok(true),
        Err(sqlx::Error::RowNotFound) => Ok(false),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

pub async fn update(
    value: &UpdateTenantKeyset,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    // Validate if sent version is same.
    let current_keyset = find(
        Some(value.keyset_identifier.organization_id.to_string()),
        ObjectFilter::One(value.keyset_identifier.keyset_id.clone()),
        false,
        txn,
    )
    .await?;

    if current_keyset.is_empty() {
        return Err(DatabaseError::NotFoundError {
            kind: "keyset",
            id: format!("{:?}", value.keyset_identifier),
        });
    }

    let expected_version = value
        .if_version_match
        .clone()
        .unwrap_or(current_keyset[0].version.to_string());

    let query = "UPDATE tenant_keysets SET content=$1, version=$2 WHERE organization_id=$3 AND keyset_id=$4 AND version=$5
        RETURNING organization_id, keyset_id, content, version";
    match sqlx::query_as::<_, TenantKeyset>(query)
        .bind(sqlx::types::Json(&value.keyset_content))
        .bind(value.version.to_string())
        .bind(value.keyset_identifier.organization_id.to_string())
        .bind(&value.keyset_identifier.keyset_id)
        .bind(&expected_version)
        .fetch_one(txn)
        .await
    {
        Ok(_) => Ok(()),
        Err(sqlx::Error::RowNotFound) => Err(DatabaseError::ConcurrentModificationError(
            "keyset",
            expected_version,
        )),
        Err(e) => Err(DatabaseError::query(query, e)),
    }
}

#[cfg(test)]
mod tests {
    use model::tenant::{PublicKey, TenantKeysetContent, TenantPublicKey};
    use sqlx::Connection;

    use super::*;

    #[crate::sqlx_test]
    async fn tenant_keyset_queries_survive_added_columns(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut api_connection = pool.acquire().await?;
        exercise_tenant_keyset_queries(&mut api_connection).await?;
        assert!(api_connection.cached_statements_size() > 0);

        // Keep the API connection and its prepared statements while another
        // connection applies the schema change, just as a migration would.
        let mut migration = pool.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL lock_timeout = '5s';
             ALTER TABLE tenant_keysets ADD COLUMN test_added_column text;",
        )
        .execute(&mut *migration)
        .await?;
        migration.commit().await?;

        exercise_tenant_keyset_queries(&mut api_connection).await?;
        Ok(())
    }

    async fn exercise_tenant_keyset_queries(
        connection: &mut PgConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = connection.begin().await?;
        let mut expected = TenantKeyset {
            keyset_identifier: TenantKeysetIdentifier {
                organization_id: "projection-tenant".parse()?,
                keyset_id: "projection-keyset".to_string(),
            },
            keyset_content: TenantKeysetContent {
                public_keys: vec![TenantPublicKey {
                    public_key: PublicKey {
                        algo: Some("ssh-ed25519".to_string()),
                        key: "projection-public-key".to_string(),
                        comment: Some("key comment".to_string()),
                    },
                    comment: Some("tenant comment".to_string()),
                }],
            },
            version: "V1-T1733777281821769".to_string(),
        };
        assert_eq!(create(&expected, &mut txn).await?, expected);

        let update_request = UpdateTenantKeyset {
            keyset_identifier: expected.keyset_identifier.clone(),
            keyset_content: TenantKeysetContent {
                public_keys: vec![TenantPublicKey {
                    public_key: PublicKey {
                        key: "replacement-public-key".to_string(),
                        ..expected.keyset_content.public_keys[0].public_key.clone()
                    },
                    comment: Some("replacement comment".to_string()),
                }],
            },
            version: "V2-T1733777281821770".to_string(),
            if_version_match: Some(expected.version.clone()),
        };
        update(&update_request, &mut txn).await?;
        expected.keyset_content = update_request.keyset_content;
        expected.version = update_request.version;

        for (operation, keysets) in [
            (
                "find_by_ids",
                find_by_ids(&mut *txn, vec![expected.keyset_identifier.clone()], true).await?,
            ),
            (
                "find all for tenant",
                find(
                    Some("projection-tenant".to_string()),
                    ObjectFilter::All,
                    true,
                    &mut txn,
                )
                .await?,
            ),
            (
                "find one for tenant",
                find(
                    Some("projection-tenant".to_string()),
                    ObjectFilter::One(expected.keyset_identifier.keyset_id.clone()),
                    true,
                    &mut txn,
                )
                .await?,
            ),
            (
                "find list for tenant",
                find(
                    Some("projection-tenant".to_string()),
                    ObjectFilter::List(&[expected.keyset_identifier.keyset_id.clone()]),
                    true,
                    &mut txn,
                )
                .await?,
            ),
            (
                "find all tenants",
                find(None, ObjectFilter::All, true, &mut txn).await?,
            ),
        ] {
            assert_eq!(keysets, vec![expected.clone()], "{operation}");
        }

        assert!(delete(&expected.keyset_identifier, &mut txn).await?);

        // Roll back the fixture so both passes decode an inserted row.
        // The connection retains its prepared statements after the rollback.
        txn.rollback().await?;
        Ok(())
    }
}
