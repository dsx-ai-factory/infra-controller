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

use std::sync::Arc;

use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn managed_host_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_managed_host_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep prepared statements on the API connection while a migration commits.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE explored_managed_hosts ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_managed_host_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_managed_host_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let expected = ExploredManagedHost {
        host_bmc_ip: "192.0.2.76".parse()?,
        dpus: vec![ExploredDpu {
            bmc_ip: "192.0.2.77".parse()?,
            host_pf_mac_address: Some("02:00:00:00:00:77".parse()?),
            host_chassis_id: Some("Chassis-7".to_string()),
            report: Arc::default(),
        }],
    };
    update(txn.as_mut(), &[&expected]).await?;
    assert_eq!(
        find_by_ips(txn.as_mut(), vec![expected.host_bmc_ip]).await?,
        vec![expected.clone()],
    );
    assert_eq!(find_all(txn.as_mut()).await?, vec![expected]);
    txn.rollback().await?;
    Ok(())
}
