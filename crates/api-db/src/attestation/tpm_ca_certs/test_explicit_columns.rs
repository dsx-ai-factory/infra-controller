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
async fn tpm_ca_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE tpm_ca_certs ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let not_valid_before = "2026-09-01T12:00:00Z".parse()?;
    let not_valid_after = "2027-09-01T12:00:00Z".parse()?;
    let ca_cert_der = [0x30, 0x81, 0x00, 0xff];
    let cert_subject = [0x31, 0x82, 0x00, 0xfe];
    let inserted = insert(
        txn.as_mut(),
        &not_valid_before,
        &not_valid_after,
        &ca_cert_der,
        &cert_subject,
    )
    .await?
    .expect("the CA certificate was inserted");
    let mut expected = TpmCaCert {
        id: inserted.id,
        not_valid_before,
        not_valid_after,
        ca_cert_der: ca_cert_der.to_vec(),
        cert_subject: cert_subject.to_vec(),
    };
    assert_cert(&inserted, &expected);
    let found = get_by_subject(txn.as_mut(), &cert_subject)
        .await?
        .expect("the CA certificate is persisted");
    assert_cert(&found, &expected);

    let listed = get_all(txn.as_mut()).await?;
    assert_eq!(listed.len(), 1);
    expected.ca_cert_der.clear();
    assert_cert(&listed[0], &expected);

    let deleted = delete(txn.as_mut(), inserted.id)
        .await?
        .expect("the CA certificate was deleted");
    expected.ca_cert_der = ca_cert_der.to_vec();
    assert_cert(&deleted, &expected);
    assert!(get_by_subject(txn.as_mut(), &cert_subject).await?.is_none());
    txn.rollback().await?;
    Ok(())
}

fn assert_cert(actual: &TpmCaCert, expected: &TpmCaCert) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.not_valid_before, expected.not_valid_before);
    assert_eq!(actual.not_valid_after, expected.not_valid_after);
    assert_eq!(actual.ca_cert_der, expected.ca_cert_der);
    assert_eq!(actual.cert_subject, expected.cert_subject);
}
