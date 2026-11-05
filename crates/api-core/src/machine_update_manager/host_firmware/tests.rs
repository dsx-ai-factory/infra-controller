// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use db::test_support::postgres::wait_for_blocked_query;
use figment::Figment;
use figment::providers::{Format, Toml};
use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;

use super::*;

fn updater(version: &str) -> Result<HostFirmwareUpdate, Box<dyn std::error::Error>> {
    let config: Arc<CarbideConfig> = Arc::new(
        Figment::new()
            .merge(Toml::file(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/cfg/test_data/full_config.toml"
            )))
            .extract()?,
    );
    let firmware = Firmware {
        vendor: bmc_vendor::BMCVendor::Dell,
        model: "fenced-catalog-host".into(),
        components: HashMap::from([(
            FirmwareComponentType::Bmc,
            FirmwareComponent {
                known_firmware: vec![FirmwareEntry {
                    version: version.into(),
                    default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let firmware_config = FirmwareConfig::new(
        Default::default(),
        &HashMap::from([("fixture".into(), firmware)]),
        &HashMap::new(),
    );
    Ok(HostFirmwareUpdate::new(
        config,
        opentelemetry::global::meter("host-firmware-fence-test"),
        firmware_config,
    )
    .expect("host update module is enabled"))
}

async fn catalog(pool: &sqlx::PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT versions::text FROM desired_firmware ORDER BY versions::text")
        .fetch_all(pool)
        .await
}

// A fresh updater has a changed nonempty catalog to publish. Losing ownership
// must leave the previously committed catalog intact.
#[crate::sqlx_test]
async fn stale_work_lock_cannot_publish_host_firmware(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(&mut tasks, pool.clone(), Default::default()).await?;
    let work_key = "host-firmware-admission";
    let work_lock = locks.try_acquire_lock(work_key.into()).await?;
    let updating = HashSet::new();
    let snapshots = HashMap::new();
    updater("1.0")?
        .start_updates(&pool, &work_lock, 1, &updating, &snapshots)
        .await?;
    let before = catalog(&pool).await?;
    assert_eq!(before.len(), 1);
    assert!(before[0].contains("1.0"));

    let changed =
        sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
            .bind(work_key)
            .execute(&pool)
            .await?;
    assert_eq!(changed.rows_affected(), 1);
    let fresh_updater = updater("2.0")?;
    let error = fresh_updater
        .start_updates(&pool, &work_lock, 1, &updating, &snapshots)
        .await
        .expect_err("the previous worker must not replace the catalog");
    assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    assert_eq!(catalog(&pool).await?, before);
    assert!(
        fresh_updater
            .firmware_catalog_last_read
            .lock()
            .await
            .is_none()
    );
    drop(work_lock);
    drop(locks);
    tasks.shutdown().await;
    Ok(())
}

// A failed pass must roll back its catalog and retry the same marker. On retry,
// takeover waits until the transaction that publishes the catalog commits.
#[crate::sqlx_test]
async fn catalog_retry_keeps_ownership_through_publication_commit(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let writer_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '1500ms'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let updater = updater("2.0")?;
    let mut tasks = JoinSet::new();
    let locks = db::work_lock_manager::start(&mut tasks, pool.clone(), Default::default()).await?;
    let work_key = "host-catalog-publication";
    let work_lock = locks.try_acquire_lock(work_key.into()).await?;
    let updating = HashSet::new();
    let snapshots = HashMap::new();

    // Discovery runs after catalog publication. Blocking this relation lets
    // the pass write the catalog before a later statement forces rollback.
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE explored_endpoints IN ACCESS EXCLUSIVE MODE")
        .execute(blocker.as_mut())
        .await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    {
        let admission = updater.start_updates(&writer_pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        tokio::select! {
            result = &mut admission => panic!("catalog pass did not wait for discovery: {result:?}"),
            _ = wait_for_blocked_query(&pool, blocker_pid, "FROM explored_endpoints") => {},
        }
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), admission)
            .await
            .expect("the shorter operator statement limit must be preserved")
            .expect_err("blocked discovery must abort the catalog transaction");
        assert!(matches!(error, crate::CarbideError::DBError(_)));
    }
    assert!(catalog(&pool).await?.is_empty());
    assert!(updater.firmware_catalog_last_read.lock().await.is_none());
    let limit: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&writer_pool)
        .await?;
    assert_eq!(limit, "1500ms");

    let (admitted, taken_over) = {
        let admission = updater.start_updates(&writer_pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        let admission_pid = tokio::select! {
            result = &mut admission => panic!("retry did not reach blocked discovery: {result:?}"),
            pid = wait_for_blocked_query(&pool, blocker_pid, "FROM explored_endpoints") => pid,
        };
        let takeover =
            sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
                .bind(work_key)
                .execute(&pool);
        tokio::pin!(takeover);
        tokio::select! {
            result = &mut admission => panic!("retry escaped the discovery lock: {result:?}"),
            result = &mut takeover => panic!("ownership changed before catalog commit: {result:?}"),
            _ = wait_for_blocked_query(&pool, admission_pid, "UPDATE work_locks SET worker_id") => {},
        }
        blocker.commit().await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(admission, takeover)
        })
        .await
        .expect("publication and takeover must finish after releasing discovery")
    };
    assert!(admitted?.is_empty());
    assert_eq!(taken_over?.rows_affected(), 1);
    let committed = catalog(&pool).await?;
    assert_eq!(committed.len(), 1);
    assert!(committed[0].contains("2.0"));
    assert!(updater.firmware_catalog_last_read.lock().await.is_some());
    drop(work_lock);
    drop(locks);
    writer_pool.close().await;
    tasks.shutdown().await;
    Ok(())
}
