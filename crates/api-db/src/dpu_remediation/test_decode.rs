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

use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};
use model::dpu_remediation::NewRemediation;
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use sqlx::PgPool;

use crate::DatabaseError;

#[crate::sqlx_test]
async fn remediation_reads_preserve_incompatible_json_errors(pool: PgPool) {
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
