/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES.
 * SPDX-License-Identifier: Apache-2.0
 */

use super::*;

#[crate::sqlx_test]
async fn expected_rack_group_migration_defaults(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    // Reapply the new table migration to the predecessor shape (no group table).
    sqlx::query("DROP TABLE expected_rack_groups")
        .execute(&mut *txn)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/20260919210839_expected_rack_groups.sql"
    ))
    .execute(&mut *txn)
    .await?;
    sqlx::query("INSERT INTO expected_rack_groups (rack_group_id, topology, rack_ids, members) VALUES ('defaults', 'topology', '[]', '[]')")
        .execute(&mut *txn).await?;
    let loaded = find_by_rack_group_id(&mut txn, &RackGroupId::new("defaults"))
        .await?
        .unwrap();
    assert_eq!(loaded.metadata, Metadata::default());
    let mut boundary = group("boundary");
    boundary.metadata.name = "n".repeat(256);
    boundary.metadata.description = "d".repeat(1024);
    create(&mut txn, &boundary).await?;
    assert_eq!(
        find_by_rack_group_id(&mut txn, &boundary.rack_group_id).await?,
        Some(boundary)
    );
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
        rack_ids: vec![RackId::new("rack-02"), RackId::new("rack-01")],
        members: vec![ExpectedRackGroupMember {
            device_type: model::rack_type::RackCapabilityType::Compute,
            manufacturer: "NVIDIA".to_string(),
            id: "device-01".to_string(),
        }],
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
    expected.members.clear();
    expected.rack_ids.clear();
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
