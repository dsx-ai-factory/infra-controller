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

use model::dns::metadata::DomainMetadata;
use sqlx::postgres::PgRow;
use sqlx::{Error, FromRow, Row};

use crate::DatabaseError;

#[derive(Debug, Clone, Default)]
pub struct DbMetadata {
    allow_axfr_from: Vec<String>,
}

impl<'r> FromRow<'r, PgRow> for DbMetadata {
    fn from_row(row: &'r PgRow) -> Result<Self, Error> {
        Ok(DbMetadata {
            allow_axfr_from: row.try_get("allow_axfr_from")?,
        })
    }
}

pub async fn metadata_for_domain(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    domain_name: &str,
) -> Result<DbMetadata, DatabaseError> {
    let domain_name = crate::dns::normalize_domain(domain_name);

    let query = "SELECT m.allow_axfr_from
                 FROM domain_metadata m JOIN domains d ON m.id = d.domain_metadata_id
                 WHERE d.name = $1";
    let metadata: DbMetadata = sqlx::query_as(query)
        .bind(domain_name)
        .fetch_one(&mut **txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(metadata)
}

impl From<DbMetadata> for DomainMetadata {
    fn from(metadata: DbMetadata) -> Self {
        DomainMetadata {
            allow_axfr_from: metadata.allow_axfr_from,
        }
    }
}

impl DbMetadata {
    pub async fn persist(&self, txn: &mut sqlx::PgConnection) -> Result<i32, DatabaseError> {
        let query = "INSERT INTO domain_metadata (allow_axfr_from) VALUES ($1) RETURNING id";
        let row: (i32,) = sqlx::query_as(query)
            .bind(&self.allow_axfr_from)
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        Ok(row.0)
    }

    pub async fn create_default(txn: &mut sqlx::PgConnection) -> Result<i32, DatabaseError> {
        Self::default().persist(txn).await
    }
}
