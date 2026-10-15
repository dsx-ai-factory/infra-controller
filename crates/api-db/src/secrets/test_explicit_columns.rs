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

use chrono::{DateTime, Utc};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn secret_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE secrets ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let entries = [
        ("credentials/a", "old-kek"),
        ("credentials/a", "new-kek"),
        ("credentials/b", "old-kek"),
        ("/import-marker", "old-kek"),
    ]
    .map(|(path, kek_id)| NewSecretEntry {
        path,
        encrypted_value: &[0x00, 0x81, 0xff],
        nonce: &[0x02, 0x82],
        kek_id,
        encrypted_dek: &[0x03, 0x83, 0xfe],
        dek_nonce: &[0x04, 0x84],
    });
    insert_many(txn.as_mut(), &entries).await?;

    let generated: Vec<(SecretId, i64, DateTime<Utc>)> =
        sqlx::query_as("SELECT secret_id, seq, created_at FROM secrets ORDER BY seq")
            .fetch_all(txn.as_mut())
            .await?;
    assert_eq!(generated.len(), entries.len());
    let expected = generated
        .into_iter()
        .zip(&entries)
        .map(|((secret_id, seq, created_at), entry)| SecretRow {
            secret_id,
            seq,
            path: entry.path.to_string(),
            encrypted_value: entry.encrypted_value.to_vec(),
            nonce: entry.nonce.to_vec(),
            kek_id: entry.kek_id.to_string(),
            created_at,
            encrypted_dek: entry.encrypted_dek.to_vec(),
            dek_nonce: entry.dek_nonce.to_vec(),
        })
        .collect::<Vec<_>>();

    let latest = get_latest(txn.as_mut(), "credentials/a")
        .await?
        .expect("the credential has journal entries");
    assert_rows(&[latest], &[&expected[1]]);
    assert_rows(
        &get_history(txn.as_mut(), "credentials/a").await?,
        &[&expected[1], &expected[0]],
    );
    let by_id = get_by_id(txn.as_mut(), expected[0].secret_id)
        .await?
        .expect("the old journal entry remains addressable");
    assert_rows(&[by_id], &[&expected[0]]);
    assert_rows(
        &get_all_for_kek_id(txn.as_mut(), "old-kek").await?,
        &[&expected[2], &expected[0]],
    );
    assert_rows(
        &get_latest_with_kek_id(txn.as_mut(), "old-kek").await?,
        &[&expected[2]],
    );
    assert_rows(
        &find_batch_after(txn.as_mut(), None, 3).await?,
        &[&expected[0], &expected[1], &expected[2]],
    );
    assert_rows(
        &find_batch_after(txn.as_mut(), Some(expected[2].seq), 3).await?,
        &[&expected[3]],
    );
    txn.rollback().await?;
    Ok(())
}

fn assert_rows(actual: &[SecretRow], expected: &[&SecretRow]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.secret_id, expected.secret_id);
        assert_eq!(actual.seq, expected.seq);
        assert_eq!(actual.path, expected.path);
        assert_eq!(actual.encrypted_value, expected.encrypted_value);
        assert_eq!(actual.nonce, expected.nonce);
        assert_eq!(actual.kek_id, expected.kek_id);
        assert_eq!(actual.created_at, expected.created_at);
        assert_eq!(actual.encrypted_dek, expected.encrypted_dek);
        assert_eq!(actual.dek_nonce, expected.dek_nonce);
    }
}
