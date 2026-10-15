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

use carbide_uuid::machine::{MachineIdSource, MachineType};
use sqlx::Connection;

use super::*;

#[crate::sqlx_test]
async fn ek_verification_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection commits DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE ek_cert_verification_status ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_queries(connection: &mut PgConnection) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x68; 32],
        MachineType::Host,
    );
    let mut expected = EkCertVerificationStatus {
        ek_sha256: vec![0xa1; 32],
        serial_num: "projection-ek-serial".to_string(),
        signing_ca_found: false,
        issuer: vec![0x30, 0x82, 0x00, 0xff],
        issuer_access_info: Some("https://ca.example.test/issuer.der".to_string()),
        machine_id,
    };
    let ca = crate::attestation::tpm_ca_certs::insert(
        txn.as_mut(),
        &"2026-09-01T12:00:00Z".parse()?,
        &"2027-09-01T12:00:00Z".parse()?,
        &[0x30, 0x81, 0xfe],
        &expected.issuer,
    )
    .await?
    .expect("the signing CA was inserted");
    let inserted = insert(
        txn.as_mut(),
        &expected.ek_sha256,
        &expected.serial_num,
        false,
        None,
        &expected.issuer,
        expected.issuer_access_info.as_deref().unwrap(),
        machine_id,
    )
    .await?
    .expect("the EK status was inserted");
    assert_status(&inserted, &expected);

    let by_hash = get_by_ek_sha256(txn.as_mut(), &expected.ek_sha256)
        .await?
        .expect("the EK status is persisted");
    assert_status(&by_hash, &expected);
    let by_machine = get_by_machine_id(txn.as_mut(), machine_id)
        .await?
        .expect("the EK status is associated with the machine");
    assert_status(&by_machine, &expected);
    for statuses in [
        get_by_unmatched_ca(txn.as_mut()).await?,
        get_by_issuer(txn.as_mut(), &expected.issuer).await?,
    ] {
        assert_eq!(statuses.len(), 1);
        assert_status(&statuses[0], &expected);
    }

    let updated =
        update_ca_verification_status(txn.as_mut(), &expected.ek_sha256, true, Some(ca.id)).await?;
    expected.signing_ca_found = true;
    assert_eq!(updated.len(), 1);
    assert_status(&updated[0], &expected);
    let persisted = get_by_machine_id(txn.as_mut(), machine_id)
        .await?
        .expect("the verified EK status is persisted");
    assert_status(&persisted, &expected);
    let ca_id: Option<i32> =
        sqlx::query_scalar("SELECT ca_id FROM ek_cert_verification_status WHERE ek_sha256 = $1")
            .bind(&expected.ek_sha256)
            .fetch_one(txn.as_mut())
            .await?;
    assert_eq!(ca_id, Some(ca.id));
    assert!(get_by_unmatched_ca(txn.as_mut()).await?.is_empty());

    let unmatched = unmatch_ca_verification_status(txn.as_mut(), ca.id)
        .await?
        .expect("the EK status was unmatched");
    expected.signing_ca_found = false;
    assert_status(&unmatched, &expected);
    let persisted = get_by_ek_sha256(txn.as_mut(), &expected.ek_sha256)
        .await?
        .expect("the unmatched EK status is persisted");
    assert_status(&persisted, &expected);
    let ca_id: Option<i32> =
        sqlx::query_scalar("SELECT ca_id FROM ek_cert_verification_status WHERE ek_sha256 = $1")
            .bind(&expected.ek_sha256)
            .fetch_one(txn.as_mut())
            .await?;
    assert_eq!(ca_id, None);

    let deleted = delete_ca_verification_status_by_machine_id(txn.as_mut(), &machine_id)
        .await?
        .expect("the EK status was deleted");
    assert_status(&deleted, &expected);
    assert!(get_by_machine_id(txn.as_mut(), machine_id).await?.is_none());
    txn.rollback().await?;
    Ok(())
}

fn assert_status(actual: &EkCertVerificationStatus, expected: &EkCertVerificationStatus) {
    assert_eq!(actual.ek_sha256, expected.ek_sha256);
    assert_eq!(actual.serial_num, expected.serial_num);
    assert_eq!(actual.signing_ca_found, expected.signing_ca_found);
    assert_eq!(actual.issuer, expected.issuer);
    assert_eq!(actual.issuer_access_info, expected.issuer_access_info);
    assert_eq!(actual.machine_id, expected.machine_id);
}
