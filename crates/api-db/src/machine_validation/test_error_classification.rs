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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::machine_validation::MachineValidationId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_validation::MachineValidationState;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::db_read::PgPoolReader;
use crate::{DatabaseError, machine, machine_validation_config, machine_validation_result};

#[crate::sqlx_test]
async fn find_by_id_decodes_legacy_success_and_rejects_unknown_state(pool: PgPool) {
    let mut txn = pool.begin().await.expect("begin fixture transaction");
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x67; 32],
        MachineType::Host,
    );
    for (state, expected) in [
        ("Completed", Ok(MachineValidationState::Success)),
        ("Unknown", Err("invalid machine_validation.state: Unknown")),
    ] {
        let id = MachineValidationId::new();
        // Seed stored spellings directly; typed writers emit only canonical states.
        sqlx::query(
            "INSERT INTO machine_validation
                (id, machine_id, name, state, end_time, total, completed)
             VALUES ($1, $2, 'stored-state', $3, NOW(), 1, 1)",
        )
        .bind(id)
        .bind(machine_id)
        .bind(state)
        .execute(txn.as_mut())
        .await
        .expect("insert validation with the stored state");

        let result = super::find_by_id(txn.as_mut(), &id).await;
        match expected {
            Ok(expected_state) => {
                let validation = result.expect("a legacy successful state must decode");
                assert_eq!(
                    validation.status.expect("run has a status").state,
                    expected_state
                );
            }
            Err(expected_error) => {
                let error = result.expect_err("an unknown validation state must fail decoding");
                let DatabaseError::Sqlx(annotated) = &error else {
                    panic!("expected Sqlx, got {error:?}");
                };
                let sqlx::Error::Decode(source) = &annotated.source else {
                    panic!("expected Decode, got {error:?}");
                };
                assert_eq!(annotated.query, "machine_validation find_by");
                assert!(source.to_string().contains(expected_error), "{error:?}");
            }
        }
    }
}

#[crate::sqlx_test]
async fn missing_external_config_returns_not_found(pool: PgPool) {
    let mut txn = pool.begin().await.expect("begin fixture transaction");
    let name = "missing-config";
    let cases = [
        (
            "get",
            machine_validation_config::find_config_by_name(txn.as_mut(), name).await,
        ),
        (
            "remove",
            machine_validation_config::remove_config(txn.as_mut(), name).await,
        ),
    ];

    for (operation, result) in cases {
        let error = result.expect_err("a missing config must fail");
        let DatabaseError::NotFoundError { kind, id } = &error else {
            panic!("{operation}: expected NotFoundError, got {error:?}");
        };
        assert_eq!(*kind, "machine_validation_external_config", "{operation}");
        assert_eq!(id, name, "{operation}");
    }
}

#[crate::sqlx_test]
async fn malformed_external_config_preserves_decode_error(pool: PgPool) {
    let mut txn = pool.begin().await.expect("begin fixture transaction");
    let name = "invalid-version";
    // Seed through SQL because `ConfigVersion` cannot represent malformed stored data.
    sqlx::query(
        "INSERT INTO machine_validation_external_config (name, description, config, version)
         VALUES ($1, 'valid description', $2, 'invalid-version')",
    )
    .bind(name)
    .bind(b"{}".as_slice())
    .execute(txn.as_mut())
    .await
    .expect("insert config with an invalid version");

    let cases = [
        (
            "SELECT",
            machine_validation_config::find_config_by_name(txn.as_mut(), name).await,
        ),
        (
            "DELETE",
            machine_validation_config::remove_config(txn.as_mut(), name).await,
        ),
    ];

    for (operation, result) in cases {
        let error = result.expect_err("an invalid config version must fail decoding");
        let DatabaseError::Sqlx(annotated) = &error else {
            panic!("{operation}: expected Sqlx, got {error:?}");
        };
        let sqlx::Error::ColumnDecode { index, .. } = &annotated.source else {
            panic!("{operation}: expected ColumnDecode, got {error:?}");
        };
        assert_eq!(index, "\"version\"", "{operation}");
        assert!(annotated.query.starts_with(operation), "{error:?}");
        assert!(
            annotated
                .query
                .contains("FROM machine_validation_external_config WHERE name=$1"),
            "{error:?}"
        );
    }

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM machine_validation_external_config WHERE name = $1)",
    )
    .bind(name)
    .fetch_one(txn.as_mut())
    .await
    .expect("check the deletion before the transaction is rolled back");
    assert!(
        !exists,
        "DELETE succeeds before SQLx decodes the returned row"
    );
}

#[tokio::test]
async fn current_validation_reads_preserve_machine_lookup_error() {
    // A closed lazy pool returns `PoolClosed` without needing a database connection.
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .expect("construct lazy pool");
    pool.close().await;
    let mut reader = PgPoolReader::from(pool);
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x67; 32],
        MachineType::Host,
    );
    let error = machine::find_one(&mut reader, &machine_id, MachineSearchConfig::default())
        .await
        .expect_err("the closed pool must reject the machine lookup");
    let DatabaseError::Sqlx(expected) = error else {
        panic!("expected a SQLx machine lookup error, got {error:?}");
    };
    let cases = [
        (
            "runs",
            super::find(&mut reader, &machine_id, false).await.map(drop),
        ),
        (
            "results",
            machine_validation_result::find_by_machine_id(&mut reader, &machine_id, false)
                .await
                .map(drop),
        ),
    ];

    for (operation, result) in cases {
        let error = result.expect_err("the closed pool must reject current validation reads");
        let DatabaseError::Sqlx(annotated) = &error else {
            panic!("{operation}: expected Sqlx, got {error:?}");
        };
        assert!(
            matches!(annotated.source, sqlx::Error::PoolClosed),
            "{operation}: {error:?}"
        );
        assert_eq!(annotated.query, expected.query, "{operation}");
        assert_eq!(annotated.file, expected.file, "{operation}");
        assert_eq!(annotated.line, expected.line, "{operation}");
        assert_eq!(
            tonic::Status::from(error).code(),
            tonic::Code::Internal,
            "{operation}"
        );
    }
}
