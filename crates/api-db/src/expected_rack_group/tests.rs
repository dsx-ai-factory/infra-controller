/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::rack::RackId;
use model::expected_rack_group::ExpectedRackGroupMember;

use super::*;

#[crate::sqlx_test]
async fn expected_rack_group_migration_requires_empty_table(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use sqlx::Acquire;

    let mut txn = pool.begin().await?;
    sqlx::query("DROP TABLE expected_rack_groups")
        .execute(&mut *txn)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/20260919210839_expected_rack_groups.sql"
    ))
    .execute(&mut *txn)
    .await?;
    let migration = include_str!("../../migrations/20260922172535_expected_rack_group_racks.sql");

    for (rack_ids, members) in [
        (serde_json::json!([]), serde_json::json!([])),
        (
            serde_json::json!(["rack-01"]),
            serde_json::json!([
                {"type": "Switch", "manufacturer": "NVIDIA", "id": "switch-01"}
            ]),
        ),
    ] {
        sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, rack_ids, members) VALUES ('legacy', 'topology', $1, $2)")
            .bind(&rack_ids).bind(&members).execute(&mut *txn).await?;
        // Match the migration runner's transaction boundary.
        let mut attempt = txn.begin().await?;
        let error = sqlx::raw_sql(migration)
            .execute(&mut *attempt)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23502")
        );
        attempt.rollback().await?;
        let retained: (serde_json::Value, serde_json::Value) = sqlx::query_as(
            "SELECT rack_ids, members FROM expected_rack_groups WHERE rack_group_id='legacy'",
        )
        .fetch_one(&mut *txn)
        .await?;
        assert_eq!(retained, (rack_ids, members));
        let schema: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name::text, is_nullable::text FROM information_schema.columns WHERE table_schema='public' AND table_name='expected_rack_groups' AND column_name IN ('racks', 'rack_ids', 'members') ORDER BY column_name"
        ).fetch_all(&mut *txn).await?;
        assert_eq!(
            schema,
            vec![
                ("members".into(), "NO".into()),
                ("rack_ids".into(), "NO".into())
            ]
        );
        sqlx::query("DELETE FROM expected_rack_groups WHERE rack_group_id='legacy'")
            .execute(&mut *txn)
            .await?;
    }

    sqlx::raw_sql(migration).execute(&mut *txn).await?;
    let mut old_writer = txn.begin().await?;
    let error = sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, rack_ids, members) VALUES ('old-writer', 'topology', '[]', '[]')")
        .execute(&mut *old_writer).await.unwrap_err();
    let error = error.as_database_error().unwrap();
    assert_eq!(error.code().as_deref(), Some("23502"));
    assert_eq!(
        error
            .downcast_ref::<sqlx::postgres::PgDatabaseError>()
            .column(),
        Some("racks")
    );
    old_writer.rollback().await?;

    for empty in [false, true] {
        let mut boundary = group(if empty { "empty" } else { "populated" });
        boundary.metadata.name = "n".repeat(256);
        boundary.metadata.description = "d".repeat(1024);
        if empty {
            boundary.racks.clear();
        }
        create(&mut txn, &boundary).await?;
        let untouched: bool = sqlx::query_scalar("SELECT rack_ids IS NULL AND members IS NULL AND racks IS NOT NULL FROM expected_rack_groups WHERE rack_group_id=$1")
            .bind(&boundary.rack_group_id).fetch_one(&mut *txn).await?;
        assert!(untouched);
        assert_eq!(
            find_by_rack_group_id(&mut txn, &boundary.rack_group_id).await?,
            Some(boundary)
        );
    }
    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn expected_rack_group_concurrent_replacements(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    // Exercise an empty table (no row to lock), then an existing snapshot.
    for populated in [false, true] {
        let mut first = pool.begin().await?;
        clear(&mut first).await?;
        if populated {
            create(&mut first, &group("old")).await?;
        }
        first.commit().await?;

        let mut first = pool.begin().await?;
        clear(&mut first).await?;
        let mut second = pool.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *second)
            .await?;
        let writer = tokio::spawn(async move {
            clear(&mut second).await.unwrap();
            create(&mut second, &group("second")).await.unwrap();
            second.commit().await.unwrap();
        });

        let blocked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid=$1 AND NOT granted)",
                )
                .bind(pid)
                .fetch_one(&pool)
                .await
                .unwrap();
                if waiting {
                    break;
                }
                if writer.is_finished() {
                    panic!("replacement bypassed the writer lock");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if blocked.is_err() {
            writer.abort();
        }
        blocked.expect("second replacement must wait for the first transaction");
        create(&mut first, &group("first")).await?;
        first.commit().await?;
        tokio::time::timeout(Duration::from_secs(5), writer).await??;
        let mut reader = pool.begin().await?;
        assert_eq!(find_all(&mut reader).await?, vec![group("second")]);
        reader.rollback().await?;
    }
    Ok(())
}

fn group(id: &str) -> ExpectedRackGroup {
    ExpectedRackGroup {
        rack_group_id: RackGroupId::new(id),
        topology: RackGroupTopology::new("gb200_nvl72r1_c2g4"),
        racks: vec![
            ExpectedRackGroupRack {
                rack_id: RackId::new("rack-02"),
                members: vec![ExpectedRackGroupMember {
                    device_type: model::rack_type::RackCapabilityType::Compute,
                    manufacturer: "NVIDIA".to_string(),
                    id: "device-01".to_string(),
                }],
            },
            ExpectedRackGroupRack {
                rack_id: RackId::new("rack-01"),
                members: vec![],
            },
        ],
        metadata: Metadata {
            name: "nvl5-gp1-jhb01".to_string(),
            description: String::new(),
            labels: [("location.datacenter".to_string(), "JHB01".to_string())]
                .into_iter()
                .collect(),
        },
    }
}

#[crate::sqlx_test]
async fn expected_rack_group_persistence(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let mut expected = group("group-b");
    create(&mut txn, &expected).await?;
    assert_eq!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id).await?,
        Some(expected.clone())
    );
    // Declaration is independent of device discovery and rack ingestion.
    create(&mut txn, &group("group-a")).await?;
    assert_eq!(
        find_ids(&mut txn).await?,
        vec![RackGroupId::new("group-a"), RackGroupId::new("group-b")]
    );
    assert_eq!(
        find_by_ids(
            &mut txn,
            &[
                RackGroupId::new("missing"),
                RackGroupId::new("group-b"),
                RackGroupId::new("group-b")
            ]
        )
        .await?,
        vec![expected.clone()]
    );
    assert!(find_by_ids(&mut txn, &[]).await?.is_empty());
    let all = find_all(&mut txn).await?;
    assert_eq!(
        all.iter()
            .map(|g| g.rack_group_id.as_str())
            .collect::<Vec<_>>(),
        ["group-a", "group-b"]
    );
    expected.racks.clear();
    expected.topology = RackGroupTopology::new("future-topology");
    update(&mut txn, &expected).await?;
    assert_eq!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id).await?,
        Some(expected.clone())
    );
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    delete(&mut txn, &expected.rack_group_id).await?;
    txn.rollback().await?;
    let mut txn = pool.begin().await?;
    assert!(
        find_by_rack_group_id(&mut txn, &expected.rack_group_id)
            .await?
            .is_some()
    );
    clear(&mut txn).await?;
    assert!(find_all(&mut txn).await?.is_empty());
    assert!(matches!(
        update(&mut txn, &expected).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    assert!(matches!(
        delete(&mut txn, &expected.rack_group_id).await,
        Err(DatabaseError::NotFoundError { .. })
    ));
    Ok(())
}
