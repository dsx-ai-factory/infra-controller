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

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};

use super::*;

#[crate::sqlx_test]
async fn rack_reads_preserve_json_decode_errors(pool: sqlx::PgPool) {
    // Each value is valid JSONB but incompatible with the target model. Store it
    // through SQL so the production reader must preserve the decoding failure.
    let pool = &pool;
    check_cases_async(
        [
            Case {
                scenario: "nonempty outcome object without its tag",
                input: (
                    "controller_state_outcome",
                    r#"{"reason":"missing outcome"}"#,
                    "missing field `outcome`",
                ),
                expect: Yields(()),
            },
            Case {
                scenario: "outcome JSON null",
                input: ("controller_state_outcome", "null", "invalid type: null"),
                expect: Yields(()),
            },
            Case {
                scenario: "health object without merges",
                input: ("health_reports", "{}", "missing field `merges`"),
                expect: Yields(()),
            },
            Case {
                scenario: "health JSON null",
                input: ("health_reports", "null", "invalid type: null"),
                expect: Yields(()),
            },
            Case {
                scenario: "firmware job with non-array machines",
                input: (
                    "firmware_upgrade_job",
                    r#"{"machines":"invalid"}"#,
                    "expected a sequence",
                ),
                expect: Yields(()),
            },
            Case {
                scenario: "firmware job JSON null",
                input: ("firmware_upgrade_job", "null", "invalid type: null"),
                expect: Yields(()),
            },
            Case {
                scenario: "NVOS job without required artifact fields",
                input: ("nvos_update_job", "{}", "missing field `firmware_id`"),
                expect: Yields(()),
            },
            Case {
                scenario: "NVOS job JSON null",
                input: ("nvos_update_job", "null", "invalid type: null"),
                expect: Yields(()),
            },
        ],
        |(column, value, expected_error)| async move {
            let mut txn = pool.begin().await.map_err(|e| format!("begin: {e}"))?;
            let rack_id = RackId::new("json-decode-rack");
            create(&mut txn, &rack_id, None, &RackConfig::default(), None)
                .await
                .map_err(|e| format!("create rack: {e}"))?;
            // Column names come only from the fixed case table; values stay bound.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE racks SET {column} = $1::jsonb WHERE id = $2"
            )))
            .bind(value)
            .bind(&rack_id)
            .execute(txn.as_mut())
            .await
            .map_err(|e| format!("store incompatible JSON: {e}"))?;

            let error = find_by(txn.as_mut(), ObjectColumnFilter::One(IdColumn, &rack_id))
                .await
                .err()
                .ok_or_else(|| "rack read accepted incompatible JSON".to_string())?;
            let DatabaseError::Sqlx(annotated) = &error else {
                return Err(format!("expected Sqlx, got {error:?}"));
            };
            let sqlx::Error::ColumnDecode { index, source } = &annotated.source else {
                return Err(format!("expected ColumnDecode, got {error:?}"));
            };
            if index != &format!("{column:?}") {
                return Err(format!("expected column {column:?}, got {index}"));
            }
            let source = source
                .downcast_ref::<serde_json::Error>()
                .ok_or_else(|| format!("expected serde_json::Error, got {source:?}"))?;
            if !source.is_data() {
                return Err(format!("expected a JSON data error, got {source:?}"));
            }
            if !source.to_string().contains(expected_error) {
                return Err(format!("expected {expected_error:?}, got {source}"));
            }
            txn.rollback().await.map_err(|e| format!("rollback: {e}"))
        },
    )
    .await;
}

#[crate::sqlx_test]
async fn rack_reads_legacy_empty_outcome_without_dropping_empty_firmware_job(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let rack_id = RackId::new("legacy-json-rack");
    create(&mut txn, &rack_id, None, &RackConfig::default(), None).await?;

    // Older binaries can still create racks with `{}` during a rolling upgrade.
    // The same JSON is a valid, present firmware job, not a shared absence marker.
    sqlx::query(
        "UPDATE racks SET controller_state_outcome = '{}'::jsonb,
                          firmware_upgrade_job = '{}'::jsonb WHERE id = $1",
    )
    .bind(&rack_id)
    .execute(txn.as_mut())
    .await?;
    let rack = find_by(txn.as_mut(), ObjectColumnFilter::One(IdColumn, &rack_id))
        .await?
        .pop()
        .expect("created rack");

    assert!(rack.controller_state_outcome.is_none());
    assert_eq!(
        serde_json::to_value(rack.firmware_upgrade_job)?,
        serde_json::to_value(Some(FirmwareUpgradeJob::default()))?
    );
    txn.rollback().await?;
    Ok(())
}
