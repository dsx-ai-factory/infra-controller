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

//! Tests for the `machine_skus.updated` migration: existing rows stay NULL and
//! rows written without the column (by a pre-migration binary) get the default.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

const UPDATED_MIGRATION: &str =
    include_str!("../../migrations/20261023025000_sku_updated_timestamp.sql");

const INSERT_WITHOUT_UPDATED: &str = "INSERT INTO machine_skus (id, description, components, schema_version, created) \
     VALUES ($1, 'test', '{}'::jsonb, 4, $2)";

async fn updated_of(pool: &PgPool, id: &str) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT updated FROM machine_skus WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[crate::sqlx_test]
async fn existing_skus_keep_null_updated_and_new_rows_get_default(
    pool: PgPool,
) -> eyre::Result<()> {
    crate::migrations::migrate_before(&pool, 20261023025000).await;

    let created = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    sqlx::query(INSERT_WITHOUT_UPDATED)
        .bind("existing")
        .bind(created)
        .execute(&pool)
        .await?;

    let mut txn = pool.begin().await?;
    sqlx::raw_sql(UPDATED_MIGRATION).execute(&mut *txn).await?;
    txn.commit().await?;

    assert_eq!(updated_of(&pool, "existing").await, None);

    // An old binary does not know the column; the default must apply.
    let before = Utc::now();
    sqlx::query(INSERT_WITHOUT_UPDATED)
        .bind("written-by-old-binary")
        .bind(created)
        .execute(&pool)
        .await?;
    let updated = updated_of(&pool, "written-by-old-binary")
        .await
        .expect("default must populate updated");
    assert!(updated >= before - chrono::Duration::seconds(5));

    Ok(())
}
