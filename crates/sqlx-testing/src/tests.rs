// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::future::poll_fn;

use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::*;

#[tokio::test]
async fn cleanup_queues_before_connecting_and_does_not_use_test_pool_capacity() {
    let pool = POOL.get_or_init(init_pool).await;
    let options = pool.connect_options();
    let mut admin = options.connect().await.unwrap();
    let cleanup_count = MAX_CONCURRENT_CLEANUPS + 1;
    let databases: Vec<_> = (0..cleanup_count)
        .map(|index| format!("cleanup_queue_{}_{index}", std::process::id()))
        .collect();
    for database in &databases {
        admin
            .execute(sqlx::AssertSqlSafe(create_database_query(database)))
            .await
            .unwrap();
    }

    // Exhaust the parent budget used by test pools. Cleanup must still progress.
    let mut test_connections = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        test_connections.push(pool.acquire().await.unwrap());
    }

    // Hold all drops inside PostgreSQL so their connection count is observable.
    // Unlike pg_database, this catalog is not needed when opening a connection.
    let mut blocker = admin.begin().await.unwrap();
    blocker
        .execute("LOCK TABLE pg_shdepend IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let mut cleanups = JoinSet::new();
    let (started, mut starts) = mpsc::channel(cleanup_count);
    for database in &databases {
        let database = database.clone();
        let mut started = Some(started.clone());
        cleanups.spawn(async move {
            let mut cleanup = std::pin::pin!(cleanup_test(&database));
            poll_fn(|context| {
                let result = cleanup.as_mut().poll(context);
                if let Some(started) = started.take() {
                    started.try_send(()).unwrap();
                }
                result
            })
            .await
        });
    }
    for _ in 0..cleanup_count {
        starts.recv().await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_get_activity(NULL) \
                 WHERE application_name = 'carbide-sqlx-cleanup'",
            )
            .fetch_one(&mut **test_connections.first_mut().unwrap())
            .await
            .unwrap();
            assert!(
                active <= MAX_CONCURRENT_CLEANUPS as i64,
                "queued cleanup opened {active} connections"
            );
            if active == MAX_CONCURRENT_CLEANUPS as i64 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cleanup connections must start even with the test pool exhausted");
    assert_eq!(cleanups.len(), cleanup_count);
    blocker.rollback().await.unwrap();
    while let Some(result) = cleanups.join_next().await {
        result.unwrap().unwrap();
    }
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_database WHERE datname = ANY($1)")
            .bind(&databases)
            .fetch_one(&mut admin)
            .await
            .unwrap();
    assert_eq!(
        remaining, 0,
        "awaiting cleanup must await actual database deletion"
    );

    // A failed drop must return its capacity so a subsequent cleanup can retry.
    let database = &databases[0];
    admin
        .execute(sqlx::AssertSqlSafe(create_database_query(database)))
        .await
        .unwrap();
    let live = options
        .as_ref()
        .clone()
        .database(database)
        .connect()
        .await
        .unwrap();
    let error = cleanup_test(database).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55006")
    );
    assert_eq!(CLEANUP_SLOTS.available_permits(), MAX_CONCURRENT_CLEANUPS);
    live.close().await.unwrap();
    cleanup_test(database).await.unwrap();
    for connection in test_connections {
        connection.close().await.unwrap();
    }
    admin.close().await.unwrap();
}
