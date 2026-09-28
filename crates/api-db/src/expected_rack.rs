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

use carbide_uuid::rack::RackId;
use model::expected_rack::ExpectedRack;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// find_by_rack_id finds an expected rack by its rack_id.
pub async fn find_by_rack_id(
    txn: &mut PgConnection,
    rack_id: &RackId,
) -> Result<Option<ExpectedRack>, DatabaseError> {
    let sql = "SELECT * FROM expected_racks WHERE rack_id=$1";
    sqlx::query_as(sql)
        .bind(rack_id)
        .fetch_optional(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// find_all returns all expected racks.
pub async fn find_all(txn: &mut PgConnection) -> DatabaseResult<Vec<ExpectedRack>> {
    let sql = "SELECT * FROM expected_racks";
    sqlx::query_as(sql)
        .fetch_all(txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))
}

/// create creates a new expected rack.
pub async fn create(txn: &mut PgConnection, rack: &ExpectedRack) -> DatabaseResult<ExpectedRack> {
    let query = "INSERT INTO expected_racks
             (rack_id, rack_profile_id, metadata_name, metadata_description, metadata_labels)
             VALUES
             ($1::varchar, $2::varchar, $3::varchar, $4::varchar, $5::jsonb) RETURNING *";

    sqlx::query_as(query)
        .bind(&rack.rack_id)
        .bind(&rack.rack_profile_id)
        .bind(&rack.metadata.name)
        .bind(&rack.metadata.description)
        .bind(sqlx::types::Json(&rack.metadata.labels))
        .fetch_one(txn)
        .await
        .map_err(|err: sqlx::Error| match err {
            sqlx::Error::Database(ref e) if e.constraint() == Some("expected_racks_pkey") => {
                DatabaseError::AlreadyFoundError {
                    kind: "expected_rack",
                    id: "rack_id already exists".to_string(),
                }
            }
            _ => DatabaseError::query(query, err),
        })
}

/// delete deletes an expected rack by its rack_id.
pub async fn delete(txn: &mut PgConnection, rack_id: &RackId) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_racks WHERE rack_id=$1";

    let result = sqlx::query(query)
        .bind(rack_id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_rack",
            id: rack_id.to_string(),
        });
    }

    Ok(())
}

/// clear deletes all expected racks.
pub async fn clear(txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "DELETE FROM expected_racks";

    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

/// Updates metadata without changing the profile selected at creation.
pub async fn update(txn: &mut PgConnection, rack: &ExpectedRack) -> DatabaseResult<()> {
    let query = "UPDATE expected_racks SET metadata_name=$1, metadata_description=$2, metadata_labels=$3 WHERE rack_id=$4";

    let result = sqlx::query(query)
        .bind(&rack.metadata.name)
        .bind(&rack.metadata.description)
        .bind(sqlx::types::Json(&rack.metadata.labels))
        .bind(&rack.rack_id)
        .execute(txn)
        .await
        .map_err(|err| DatabaseError::query(query, err))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "expected_rack",
            id: rack.rack_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests;
