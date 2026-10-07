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

use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn sku_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE machine_skus ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let mut expected = Sku {
        id: "projection-sku".to_string(),
        schema_version: CURRENT_SKU_VERSION,
        description: "accelerator host".to_string(),
        created: Utc::now(),
        device_type: Some("host".to_string()),
        components: SkuComponents {
            chassis: SkuComponentChassis {
                vendor: "NVIDIA".to_string(),
                model: "projection-platform".to_string(),
                architecture: "x86_64".to_string(),
            },
            cpus: vec![SkuComponentCpu {
                vendor: "test-vendor".to_string(),
                model: "test-cpu".to_string(),
                thread_count: 64,
                count: 2,
            }],
            gpus: vec![],
            memory: vec![],
            infiniband_devices: vec![],
            storage: vec![],
            tpm: None,
        },
    };
    create(txn.as_mut(), &expected).await?;
    let found = find(txn.as_mut(), &[&expected.id]).await?;
    assert_eq!(found.len(), 1);
    expected.created = found[0].created;
    assert_eq!(
        serde_json::to_value(&found[0])?,
        serde_json::to_value(&expected)?
    );
    let excluded = "another-sku".to_string();
    for found in [
        find_matching(txn.as_mut(), &expected).await?,
        find_matching_with_exclusion(txn.as_mut(), &expected, Some(&excluded)).await?,
    ] {
        assert_eq!(
            serde_json::to_value(found.expect("the populated SKU matches"))?,
            serde_json::to_value(&expected)?
        );
    }
    txn.rollback().await?;
    Ok(())
}
