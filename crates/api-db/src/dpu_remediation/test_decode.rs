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

use carbide_test_support::Case;
use carbide_test_support::Outcome::Yields;
use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};
use model::dpu_remediation::NewRemediation;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::PgPool;

use crate::DatabaseError;

#[crate::sqlx_test]
async fn id_lookups_ignore_unrelated_json_while_record_reads_reject_it(pool: PgPool) {
    let mut txn = pool.begin().await.expect("begin remediation decode test");
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x74; 32],
        MachineType::Dpu,
    );
    let dpu_machine_id = DpuMachineId::try_from(machine_id).expect("DPU machine ID");
    crate::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .expect("create DPU");
    let remediation = super::persist_remediation(
        NewRemediation {
            script: "echo repair-interface".to_string(),
            metadata: None,
            retries: 0,
            author: "script-author".to_string().into(),
        },
        &mut txn,
    )
    .await
    .expect("create remediation");

    // PostgreSQL accepts these JSON objects, but the models require string values.
    sqlx::query(
        "UPDATE dpu_remediations SET metadata_labels = '{\"component\": 42}'::jsonb
         WHERE id = $1",
    )
    .bind(remediation.id)
    .execute(txn.as_mut())
    .await
    .expect("store metadata labels with a non-string value");
    sqlx::query(
        "INSERT INTO applied_dpu_remediations (id, dpu_machine_id, attempt, succeeded, status)
         VALUES ($1, $2, 1, false, '{\"detail\": 42}'::jsonb)",
    )
    .bind(remediation.id)
    .bind(dpu_machine_id)
    .execute(txn.as_mut())
    .await
    .expect("store remediation status with a non-string value");

    assert_eq!(
        super::find_remediation_ids(&mut txn)
            .await
            .expect("list remediation IDs without decoding metadata labels"),
        vec![remediation.id],
    );
    for (scenario, id_query) in [
        (
            "by machine ignores status JSON",
            super::AppliedRemediationIdQueryType::Machine(dpu_machine_id),
        ),
        (
            "by remediation ignores status JSON",
            super::AppliedRemediationIdQueryType::RemediationId(remediation.id),
        ),
    ] {
        let txn = &mut txn;
        Case {
            scenario,
            input: id_query,
            expect: Yields((vec![remediation.id], vec![dpu_machine_id])),
        }
        .check_async(|id_query| async move {
            super::find_applied_remediation_ids(txn, id_query)
                .await
                .map_err(|error| error.to_string())
        })
        .await;
    }

    for (column, result) in [
        (
            "metadata_labels",
            super::find_remediations_by_ids(&mut txn, &[remediation.id])
                .await
                .map(drop),
        ),
        (
            "status",
            super::find_remediations_by_remediation_id_and_machine(
                &mut txn,
                remediation.id,
                &dpu_machine_id,
            )
            .await
            .map(drop),
        ),
    ] {
        let error = result.expect_err("incompatible JSON must fail the remediation read");
        let DatabaseError::Sqlx(annotated) = &error else {
            panic!("{column}: expected Sqlx, got {error:?}");
        };
        let sqlx::Error::ColumnDecode { index, source } = &annotated.source else {
            panic!("{column}: expected ColumnDecode, got {error:?}");
        };
        assert_eq!(index, &format!("\"{column}\""));
        assert!(
            source
                .downcast_ref::<serde_json::Error>()
                .is_some_and(serde_json::Error::is_data),
            "{column}: {source:?}"
        );
    }
}

// Every applied attempt contributes an ID, including repeated IDs for retries;
// the selector must still exclude other machines or remediations.
#[crate::sqlx_test]
async fn applied_id_lookups_preserve_filters_and_attempts(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    assert!(super::find_remediation_ids(&mut txn).await?.is_empty());
    let [first_machine_id, second_machine_id, missing_machine_id] =
        [0x76, 0x77, 0x78].map(|byte| {
            DpuMachineId::try_from(MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [byte; 32],
                MachineType::Dpu,
            ))
            .expect("DPU fixture ID")
        });
    for machine_id in [first_machine_id, second_machine_id] {
        crate::machine::create(
            txn.as_mut(),
            None,
            machine_id.as_machine_id(),
            ManagedHostState::Ready,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
    }
    let first_remediation_id = RemediationId::new();
    let second_remediation_id = RemediationId::new();
    let missing_remediation_id = RemediationId::new();
    for (remediation_id, metadata_name) in [
        (first_remediation_id, None),
        (second_remediation_id, Some("named-remediation")),
    ] {
        sqlx::query(
            "INSERT INTO dpu_remediations (id, script, script_author, metadata_name)
             VALUES ($1, 'echo repair-interface', 'script-author', $2)",
        )
        .bind(remediation_id)
        .bind(metadata_name)
        .execute(txn.as_mut())
        .await?;
    }
    let mut remediation_ids = super::find_remediation_ids(&mut txn).await?;
    remediation_ids.sort_unstable();
    let mut expected_ids = vec![first_remediation_id, second_remediation_id];
    expected_ids.sort_unstable();
    assert_eq!(remediation_ids, expected_ids);

    for (remediation_id, machine_id, attempt, succeeded) in [
        (first_remediation_id, first_machine_id, 1, false),
        (first_remediation_id, first_machine_id, 2, true),
        (second_remediation_id, first_machine_id, 1, false),
        (first_remediation_id, second_machine_id, 1, false),
    ] {
        sqlx::query(
            "INSERT INTO applied_dpu_remediations (id, dpu_machine_id, attempt, succeeded)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(remediation_id)
        .bind(machine_id)
        .bind(attempt)
        .bind(succeeded)
        .execute(txn.as_mut())
        .await?;
    }

    // These queries have no ordering contract; retain duplicates when comparing.
    let mut expected_remediation_ids = vec![
        first_remediation_id,
        first_remediation_id,
        second_remediation_id,
    ];
    expected_remediation_ids.sort_unstable();
    let mut expected_machine_ids = vec![first_machine_id, first_machine_id, second_machine_id];
    expected_machine_ids.sort_unstable();
    for case in [
        Case {
            scenario: "filter by machine and retain every attempt",
            input: super::AppliedRemediationIdQueryType::Machine(first_machine_id),
            expect: Yields((expected_remediation_ids, vec![first_machine_id])),
        },
        Case {
            scenario: "filter by remediation and retain every attempt",
            input: super::AppliedRemediationIdQueryType::RemediationId(first_remediation_id),
            expect: Yields((vec![first_remediation_id], expected_machine_ids)),
        },
        Case {
            scenario: "no attempts for machine retains supplied machine ID",
            input: super::AppliedRemediationIdQueryType::Machine(missing_machine_id),
            expect: Yields((vec![], vec![missing_machine_id])),
        },
        Case {
            scenario: "no attempts for remediation retains supplied remediation ID",
            input: super::AppliedRemediationIdQueryType::RemediationId(missing_remediation_id),
            expect: Yields((vec![missing_remediation_id], vec![])),
        },
    ] {
        let txn = &mut txn;
        case.check_async(|id_query| async move {
            super::find_applied_remediation_ids(txn, id_query)
                .await
                .map(|(mut remediation_ids, mut machine_ids)| {
                    remediation_ids.sort_unstable();
                    machine_ids.sort_unstable();
                    (remediation_ids, machine_ids)
                })
                .map_err(|error| error.to_string())
        })
        .await;
    }

    Ok(())
}
