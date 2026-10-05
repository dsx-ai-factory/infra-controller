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

use std::collections::HashMap;

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::metadata::Metadata;
use sqlx::{Connection, PgConnection, PgPool};

use super::*;

#[crate::sqlx_test]
async fn remediation_queries_survive_added_columns(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_remediation_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection adds the columns.
    let mut migration_connection = pool.acquire().await?;
    let mut migration = migration_connection.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE dpu_remediations ADD COLUMN test_added_column text;
         ALTER TABLE applied_dpu_remediations ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_remediation_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_remediation_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x73; 32],
        MachineType::Dpu,
    );
    let dpu_machine_id = DpuMachineId::try_from(machine_id)?;
    crate::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;

    // Both INSERT statements return a remediation. Nonempty metadata also
    // catches omitted columns that `FromRow` would otherwise default away.
    for metadata in [
        None,
        Some(Metadata {
            name: "repair-interface".to_string(),
            description: "restore DPU interface configuration".to_string(),
            labels: HashMap::from([("component".to_string(), "network".to_string())]),
        }),
    ] {
        let mut expected = persist_remediation(
            NewRemediation {
                script: "echo repair-interface".to_string(),
                metadata: metadata.clone(),
                retries: 2,
                author: "script-author".to_string().into(),
            },
            &mut txn,
        )
        .await?;
        assert_eq!(expected.script, "echo repair-interface");
        assert_eq!(expected.metadata, metadata);
        assert_eq!(expected.retries, 2);
        assert_eq!(expected.author.to_string(), "script-author");
        assert!(expected.reviewer.is_none());
        assert!(!expected.enabled);

        persist_approve_remediation(
            ApproveRemediation {
                id: expected.id,
                reviewer: "script-reviewer".to_string().into(),
            },
            &mut txn,
        )
        .await?;
        persist_enable_remediation(EnableRemediation { id: expected.id }, &mut txn).await?;
        expected.reviewer = Some("script-reviewer".to_string().into());
        expected.enabled = true;
        let found = find_remediations_by_ids(&mut txn, &[expected.id]).await?;
        assert_eq!(found.len(), 1);
        let found = &found[0];
        assert_eq!(found.id, expected.id);
        assert_eq!(found.script, expected.script);
        assert_eq!(found.metadata, expected.metadata);
        assert_eq!(found.retries, expected.retries);
        assert_eq!(found.author.to_string(), expected.author.to_string());
        assert_eq!(
            found.reviewer.as_ref().map(ToString::to_string),
            expected.reviewer.as_ref().map(ToString::to_string),
        );
        assert_eq!(found.enabled, expected.enabled);
        assert_eq!(found.creation_time, expected.creation_time);

        let status = HashMap::from([("detail".to_string(), "interface restored".to_string())]);
        let applied = persist_applied_remediation(
            NewAppliedRemediation {
                id: expected.id,
                dpu_machine_id: dpu_machine_id.to_string(),
                attempt: 2,
                succeeded: true,
                status: status.clone(),
            },
            &mut txn,
        )
        .await?;
        assert_eq!(applied.id, expected.id);
        assert_eq!(applied.dpu_machine_id, dpu_machine_id);
        assert_eq!(applied.attempt, 2);
        assert!(applied.succeeded);
        assert_eq!(applied.status, status);

        for (operation, records) in [
            (
                "by remediation",
                find_applied_remediations_by(
                    &mut txn,
                    ObjectColumnFilter::One(AppliedRemediationIdColumn, &expected.id),
                )
                .await?,
            ),
            (
                "by remediation and machine",
                find_remediations_by_remediation_id_and_machine(
                    &mut txn,
                    expected.id,
                    &dpu_machine_id,
                )
                .await?,
            ),
        ] {
            assert_eq!(records.len(), 1, "{operation}");
            let stored = &records[0];
            assert_eq!(stored.id, applied.id, "{operation}");
            assert_eq!(stored.dpu_machine_id, applied.dpu_machine_id, "{operation}");
            assert_eq!(stored.attempt, applied.attempt, "{operation}");
            assert_eq!(stored.succeeded, applied.succeeded, "{operation}");
            assert_eq!(stored.status, applied.status, "{operation}");
            assert_eq!(stored.applied_time, applied.applied_time, "{operation}");
        }
    }

    // Replaying after rollback exercises both INSERT branches again on the same connection.
    txn.rollback().await?;
    Ok(())
}
