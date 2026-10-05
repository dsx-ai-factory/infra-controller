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
use model::attestation::TpmCaCert;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod test_explicit_columns;

pub async fn insert(
    txn: &mut PgConnection,
    not_valid_before: &DateTime<Utc>,
    not_valid_after: &DateTime<Utc>,
    ca_cert: &[u8],
    cert_subject: &[u8],
) -> DatabaseResult<Option<TpmCaCert>> {
    let query = "INSERT INTO tpm_ca_certs (not_valid_before, not_valid_after, ca_cert_der, cert_subject) VALUES ($1, $2, $3, $4) RETURNING id, not_valid_before, not_valid_after, ca_cert_der, cert_subject";

    let res = sqlx::query_as(query)
        .bind(not_valid_before)
        .bind(not_valid_after)
        .bind(ca_cert)
        .bind(cert_subject)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Some(res))
}

pub async fn get_by_subject(
    txn: &mut PgConnection,
    cert_subject: &[u8],
) -> DatabaseResult<Option<TpmCaCert>> {
    let query = "SELECT id, not_valid_before, not_valid_after, ca_cert_der, cert_subject FROM tpm_ca_certs WHERE cert_subject = ($1)";

    sqlx::query_as(query)
        .bind(cert_subject)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_all(txn: &mut PgConnection) -> DatabaseResult<Vec<TpmCaCert>> {
    let query = "SELECT id, not_valid_before, not_valid_after, cert_subject FROM tpm_ca_certs";

    sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn delete(txn: &mut PgConnection, ca_cert_id: i32) -> DatabaseResult<Option<TpmCaCert>> {
    let query = "DELETE FROM tpm_ca_certs WHERE id = ($1) RETURNING id, not_valid_before, not_valid_after, ca_cert_der, cert_subject";

    sqlx::query_as(query)
        .bind(ca_cert_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}
