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

use carbide_uuid::machine::MachineId;
use model::attestation::EkCertVerificationStatus;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

#[cfg(test)]
mod test_explicit_columns;

pub async fn get_by_ek_sha256(
    txn: &mut PgConnection,
    ek_sha256: &[u8],
) -> DatabaseResult<Option<EkCertVerificationStatus>> {
    let query = "SELECT ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id FROM ek_cert_verification_status WHERE ek_sha256 = ($1)";

    sqlx::query_as(query)
        .bind(ek_sha256)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_by_unmatched_ca(
    txn: &mut PgConnection,
) -> DatabaseResult<Vec<EkCertVerificationStatus>> {
    let query = "SELECT ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id FROM ek_cert_verification_status WHERE signing_ca_found = FALSE";

    sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_by_issuer(
    txn: &mut PgConnection,
    issuer: &[u8],
) -> DatabaseResult<Vec<EkCertVerificationStatus>> {
    let query = "SELECT ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id FROM ek_cert_verification_status WHERE issuer = ($1)";

    sqlx::query_as(query)
        .bind(issuer)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn get_by_machine_id(
    txn: impl DbReader<'_>,
    machine_id: MachineId,
) -> DatabaseResult<Option<EkCertVerificationStatus>> {
    let query = "SELECT ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id FROM ek_cert_verification_status WHERE machine_id = ($1)";

    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn update_ca_verification_status(
    txn: &mut PgConnection,
    ek_sha256: &[u8],
    signing_ca_found: bool,
    ca_id: Option<i32>,
) -> DatabaseResult<Vec<EkCertVerificationStatus>> {
    let query = "UPDATE ek_cert_verification_status SET signing_ca_found=$1, ca_id=$2 WHERE ek_sha256=$3 RETURNING ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id";
    sqlx::query_as(query)
        .bind(signing_ca_found)
        .bind(ca_id)
        .bind(ek_sha256)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn unmatch_ca_verification_status(
    txn: &mut PgConnection,
    ca_id: i32,
) -> DatabaseResult<Option<EkCertVerificationStatus>> {
    let query = "UPDATE ek_cert_verification_status SET signing_ca_found=false, ca_id=null WHERE ca_id=$1 RETURNING ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id";
    sqlx::query_as(query)
        .bind(ca_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn delete_ca_verification_status_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<Option<EkCertVerificationStatus>> {
    let query = "DELETE FROM ek_cert_verification_status WHERE machine_id=$1 RETURNING ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id";
    sqlx::query_as(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    txn: &mut PgConnection,
    ek_sha256: &[u8],
    serial_num: &str,
    signing_ca_found: bool,
    ca_id: Option<i32>,
    issuer: &[u8],
    issuer_access_info: &str,
    machine_id: MachineId,
) -> DatabaseResult<Option<EkCertVerificationStatus>> {
    let query = "INSERT INTO ek_cert_verification_status VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING ek_sha256, serial_num, signing_ca_found, issuer, issuer_access_info, machine_id";

    sqlx::query_as(query)
        .bind(ek_sha256)
        .bind(serial_num)
        .bind(signing_ca_found)
        .bind(ca_id)
        .bind(issuer)
        .bind(issuer_access_info)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
        .map(Some)
}
