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
async fn attestation_secret_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE attestation_secret_ak_pub ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let secret = vec![0x00, 0x80, 0xff];
    let ak_pub = vec![0x30, 0x81, 0xfe];
    let inserted = insert(txn.as_mut(), &secret, &ak_pub)
        .await?
        .expect("the attestation secret was inserted");
    assert_eq!(inserted.secret, secret);
    assert_eq!(inserted.ak_pub, ak_pub);
    let found = get_by_secret(txn.as_mut(), &secret)
        .await?
        .expect("the attestation secret is persisted");
    assert_eq!(found.secret, secret);
    assert_eq!(found.ak_pub, ak_pub);
    let deleted = delete(txn.as_mut(), &secret)
        .await?
        .expect("the attestation secret was deleted");
    assert_eq!(deleted.secret, secret);
    assert_eq!(deleted.ak_pub, ak_pub);
    assert!(get_by_secret(txn.as_mut(), &secret).await?.is_none());
    txn.rollback().await?;
    Ok(())
}
