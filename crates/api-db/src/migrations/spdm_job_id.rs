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
    // Multiple devices on one machine must share a job, while another machine
    // needs its own job so job-wide operations cannot affect both machines.
    sqlx::raw_sql(
        r#"INSERT INTO spdm_machine_devices_attestation
               (machine_id, device_id, nonce, state, started_at)
           SELECT machine_id, device_id, gen_random_uuid(), state::jsonb, now()
           FROM (VALUES ('spdm-migration-0', 'GPU-1', '{"PollEvidenceCollection":{"task_id":"legacy-task","retry_count":0}}'),
                        ('spdm-migration-0', 'GPU-2', '"Passed"'),
                        ('spdm-migration-1', 'GPU-1', '"Passed"')) AS devices(machine_id, device_id, state)"#,
    )
    .execute(&pool)
    .await?;
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

    // Legacy polling must retain its BMC task ID and claim; other states stay unclaimed.
    let claims: Vec<(Option<String>, bool)> = sqlx::query_as(
        "SELECT state #>> '{PollEvidenceCollection,task_id}', evidence_collection_locked
         FROM spdm_machine_devices_attestation ORDER BY machine_id, device_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        claims,
        vec![
            (Some("legacy-task".to_string()), true),
            (None, false),
            (None, false),
        ]
    );

    // The primary key must enforce required job IDs after the backfill.
    let jobs: Vec<(String, Vec<uuid::Uuid>)> = sqlx::query_as(
        "SELECT machine_id, array_agg(job_id ORDER BY device_id)
         FROM spdm_machine_devices_attestation GROUP BY machine_id ORDER BY machine_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].0, "spdm-migration-0");
    assert_eq!(jobs[0].1.len(), 2);
    assert_eq!(jobs[0].1[0], jobs[0].1[1]);
    assert_eq!(jobs[1].0, "spdm-migration-1");
    assert_eq!(jobs[1].1.len(), 1);
    assert_ne!(jobs[0].1[0], jobs[1].1[0]);
    for (_, job_ids) in &jobs {
        assert!(job_ids.iter().all(|job_id| !job_id.is_nil()));
    }

    // Pre-migration rows have unknown origin, even when their machine was polling.
    let origins: Vec<bool> = sqlx::query_scalar(
        "SELECT triggered_by_mh_controller FROM spdm_machine_devices_attestation",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(origins, vec![false; 3]);

    // Writers supplying the required job ID can omit origin and retain the false default.
    let origin: bool = sqlx::query_scalar(
        r#"INSERT INTO spdm_machine_devices_attestation
               (machine_id, device_id, nonce, state, job_id, started_at)
           VALUES ('spdm-migration-0', 'GPU-3', gen_random_uuid(), '"FetchMetadata"'::jsonb, $1, now())
           RETURNING triggered_by_mh_controller"#,
    )
    .bind(jobs[0].1[0])
    .fetch_one(&pool)
    .await?;
    assert!(
        !origin,
        "omitting origin must not imply a controller trigger"
    );

    let error = sqlx::query(
        r#"INSERT INTO spdm_machine_devices_attestation
               (machine_id, device_id, nonce, state, started_at)
           VALUES ('spdm-migration-0', 'GPU-3', gen_random_uuid(), '"Passed"'::jsonb, now())"#,
    )
    .execute(&pool)
    .await
    .expect_err("new rows must provide a job ID instead of receiving a default");
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23502")
    );

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
