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
use model::attestation::SecretAkPub;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod test_explicit_columns;

pub async fn insert(
    txn: &mut PgConnection,
    secret: &Vec<u8>,
    ak_pub: &Vec<u8>,
) -> DatabaseResult<Option<SecretAkPub>> {
    let query = "INSERT INTO attestation_secret_ak_pub VALUES ($1, $2) RETURNING secret, ak_pub";
    let res = sqlx::query_as(query)
        .bind(secret.as_slice())
        .bind(ak_pub.as_slice())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Some(res))
}

pub async fn delete(
    txn: &mut PgConnection,
    secret: &Vec<u8>,
) -> DatabaseResult<Option<SecretAkPub>> {
    let query =
        "DELETE FROM attestation_secret_ak_pub WHERE secret = ($1) RETURNING secret, ak_pub";

    let res = sqlx::query_as(query)
        .bind(secret.as_slice())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Some(res))
}

pub async fn get_by_secret(
    txn: &mut PgConnection,
    secret: &Vec<u8>,
) -> DatabaseResult<Option<SecretAkPub>> {
    let query = "SELECT secret, ak_pub FROM attestation_secret_ak_pub WHERE secret = ($1)";

    sqlx::query_as(query)
        .bind(secret.as_slice())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}
