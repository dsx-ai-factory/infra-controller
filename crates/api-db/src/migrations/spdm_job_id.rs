// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use model::machine::{AttestationMode, MachineState, ManagedHostState, SpdmMeasuringState};
use sqlx::PgPool;

use super::{IgnoringMissing, MIGRATION_LAYOUT, Migrator};

const VERSION: i64 = 20260917113009;
const MIGRATION: &str = include_str!("../../migrations/20260917113009_spdm_job_id.sql");

#[crate::sqlx_test]
async fn spdm_job_migration_restarts_legacy_polling(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Exercise the complete migration against its actual predecessor schema.
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .execute(&pool)
        .await?;
    let epoch = MIGRATION_LAYOUT.epochs.last().unwrap();
    epoch.squash.run(&pool).await?;
    Migrator::with_migrations(
        epoch
            .post_squash
            .iter()
            .filter(|migration| migration.version < VERSION)
            .cloned()
            .collect(),
    )
    .ignoring_missing()
    .run(&pool)
    .await?;

    struct Case {
        name: &'static str,
        input: &'static str,
        expected: ManagedHostState,
    }
    let job_id = uuid::uuid!("123e4567-e89b-12d3-a456-426614174000");
    let cases = [
        Case {
            name: "host initialization written by a live legacy controller",
            input: r#"{"state":"hostinit","machine_state":{"state":"spdmmeasuring","spdm_measuring_state":"pollresult"}}"#,
            expected: ManagedHostState::HostInit {
                machine_state: MachineState::SpdmMeasuring {
                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                },
            },
        },
        Case {
            name: "pre-assignment legacy polling",
            input: r#"{"state":"preassignedmeasuring","spdm_measuring_state":"pollresult"}"#,
            expected: ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
            },
        },
        Case {
            name: "post-assignment legacy polling",
            input: r#"{"state":"postassignedmeasuring","attestation_mode":{"spdmattestation":{"spdm_measuring_state":"pollresult"}}}"#,
            expected: ManagedHostState::PostAssignedMeasuring {
                attestation_mode: AttestationMode::SpdmAttestation {
                    spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                },
            },
        },
        Case {
            name: "UUID-bearing polling is preserved",
            input: r#"{"state":"preassignedmeasuring","spdm_measuring_state":{"pollresult":"123e4567-e89b-12d3-a456-426614174000"}}"#,
            expected: ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state: SpdmMeasuringState::PollResult(job_id),
            },
        },
        Case {
            name: "unrelated state is preserved",
            input: r#"{"state":"ready"}"#,
            expected: ManagedHostState::Ready,
        },
    ];

    // Populate the old schema with legacy and already-compatible states.
    for (index, case) in cases.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO machines (id, controller_state, dpf)
               VALUES ($1, $2::jsonb, '{"enabled": false, "used_for_ingestion": false}'::jsonb)"#,
        )
        .bind(format!("spdm-migration-{index}"))
        .bind(case.input)
        .execute(&pool)
        .await?;
    }
    let mut writer = pool.begin().await?;
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    sqlx::query(
        "UPDATE machines SET controller_state = $1::jsonb, name = 'live legacy writer'
         WHERE id = 'spdm-migration-0'",
    )
    .bind(cases[0].input)
    .execute(&mut *writer)
    .await?;

    let mut migration = pool.begin().await?;
    let migration_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *migration)
        .await?;
    // Commit the old writer only once the migration is waiting on its row.
    // PostgreSQL must recheck that row and reset the freshly committed legacy state.
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(
            async {
                sqlx::raw_sql(MIGRATION).execute(&mut *migration).await?;
                migration.commit().await
            },
            async {
                loop {
                    let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                        .bind(writer_pid)
                        .bind(migration_pid)
                        .fetch_one(&pool)
                        .await?;
                    if blocked {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                writer.commit().await
            }
        )
    })
    .await??;

    // Check the complete JSON as well as deserialization into the current model.
    for (index, case) in cases.iter().enumerate() {
        let sqlx::types::Json(actual): sqlx::types::Json<serde_json::Value> =
            sqlx::query_scalar("SELECT controller_state FROM machines WHERE id = $1")
                .bind(format!("spdm-migration-{index}"))
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            actual,
            serde_json::to_value(&case.expected)?,
            "{}",
            case.name
        );
        assert_eq!(
            serde_json::from_value::<ManagedHostState>(actual)?,
            case.expected,
            "{}",
            case.name
        );
    }
    let name: String =
        sqlx::query_scalar("SELECT name FROM machines WHERE id = 'spdm-migration-0'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(name, "live legacy writer");
    Ok(())
}
