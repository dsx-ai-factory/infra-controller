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

//! Direct-dispatch RMS job references keyed by stable device management MAC and job kind.
//!
//! Firmware and reset references share a table, with firmware operations scoped
//! to their job kinds. This stores the current reference, not operation history.

use std::collections::HashSet;

use mac_address::MacAddress;

use crate::DatabaseError;

/// Backend operation whose current job reference is tracked for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RmsJobKind {
    /// Firmware-object bundle update for a compute, switch, or power-shelf tray.
    FirmwareObject,
    /// NVOS system-image update for a switch.
    SwitchSystemImage,
    /// SDN factory reset for a switch.
    SwitchSdnFactoryReset,
    /// NVOS factory reset for a switch.
    SwitchNvosFactoryReset,
}

impl RmsJobKind {
    /// Stable database value; existing firmware references retain their kinds.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FirmwareObject => "firmware_object",
            Self::SwitchSystemImage => "switch_system_image",
            Self::SwitchSdnFactoryReset => "switch_sdn_factory_reset",
            Self::SwitchNvosFactoryReset => "switch_nvos_factory_reset",
        }
    }

    /// Unknown kinds written by another release are not interpreted by this reader.
    fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "firmware_object" => Some(Self::FirmwareObject),
            "switch_system_image" => Some(Self::SwitchSystemImage),
            "switch_sdn_factory_reset" => Some(Self::SwitchSdnFactoryReset),
            "switch_nvos_factory_reset" => Some(Self::SwitchNvosFactoryReset),
            _ => None,
        }
    }
}

/// Replace one device/kind reference, preserving every other kind.
///
/// The last successful write replaces the reference. This does not order
/// remote acceptance or provide admission for remote operations.
pub async fn save(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: RmsJobKind,
    job_id: &str,
) -> Result<(), DatabaseError> {
    let mut txn = db
        .begin()
        .await
        .map_err(|error| DatabaseError::query("firmware_job::begin", error))?;
    crate::cm_action::lock_firmware(&mut txn, bmc_mac).await?;
    let action_id = crate::cm_action::firmware(&mut txn, bmc_mac, job_kind, job_id).await?;
    let sql = "INSERT INTO direct_dispatch_rms_jobs (bmc_mac, job_kind, job_id, action_id) \
               VALUES ($1, $2, $3, $4) \
               ON CONFLICT (bmc_mac, job_kind) \
               DO UPDATE SET job_id = EXCLUDED.job_id, action_id = EXCLUDED.action_id, created = now()";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(job_kind.as_str())
        .bind(job_id)
        .bind(action_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    crate::cm_action::reconcile_firmware(&mut txn, bmc_mac).await?;
    txn.commit()
        .await
        .map_err(|error| DatabaseError::query("firmware_job::commit", error))?;
    Ok(())
}

/// Remove one device/kind reference without affecting other operations.
pub async fn delete(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: RmsJobKind,
) -> Result<(), DatabaseError> {
    let mut txn = db
        .begin()
        .await
        .map_err(|error| DatabaseError::query("firmware_job::begin", error))?;
    crate::cm_action::lock_firmware(&mut txn, bmc_mac).await?;
    let sql = "DELETE FROM direct_dispatch_rms_jobs \
               WHERE bmc_mac = $1 AND job_kind = $2";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(job_kind.as_str())
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    crate::cm_action::reconcile_firmware(&mut txn, bmc_mac).await?;
    txn.commit()
        .await
        .map_err(|error| DatabaseError::query("firmware_job::commit", error))?;
    Ok(())
}

/// Fetch the current reference for one device/kind, if recorded.
pub async fn get(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: RmsJobKind,
) -> Result<Option<String>, DatabaseError> {
    let sql = "SELECT job_id FROM direct_dispatch_rms_jobs WHERE bmc_mac = $1 AND job_kind = $2";
    sqlx::query_scalar(sql)
        .bind(bmc_mac)
        .bind(job_kind.as_str())
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))
}

/// Read the originating log trace for the exact backend job being polled.
/// Legacy jobs and jobs replaced during a poll have no matching context.
pub async fn trace_id(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: RmsJobKind,
    job_id: &str,
) -> Result<Option<trace_propagation::correlation::TraceId>, DatabaseError> {
    let metadata: Option<sqlx::types::Json<crate::cm_action::Metadata>> = sqlx::query_scalar("SELECT a.metadata FROM direct_dispatch_rms_jobs j JOIN cm_actions a ON a.id = j.action_id AND a.request_key = j.job_kind || ':' || j.job_id WHERE j.bmc_mac = $1 AND j.job_kind = $2 AND j.job_id = $3")
        .bind(bmc_mac).bind(job_kind.as_str()).bind(job_id).fetch_optional(db).await
        .map_err(|error| DatabaseError::query("firmware_job::trace_id", error))?;
    Ok(metadata.map(|metadata| metadata.0.trace_id))
}

/// Mark the exact backend job's action finished. A replacement job is untouched;
/// its predecessor's completion can arrive after a newer submission.
pub async fn finish(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: RmsJobKind,
    job_id: &str,
) -> Result<(), DatabaseError> {
    sqlx::query("UPDATE cm_actions a SET completed_at = statement_timestamp() WHERE completed_at IS NULL AND id = (SELECT action_id FROM direct_dispatch_rms_jobs j WHERE bmc_mac = $1 AND job_kind = $2 AND job_id = $3 AND a.request_key = j.job_kind || ':' || j.job_id)")
        .bind(bmc_mac).bind(job_kind.as_str()).bind(job_id).execute(db).await
        .map_err(|error| DatabaseError::query("firmware_job::finish", error))?;
    Ok(())
}

/// Fetch firmware references only; reset jobs must not enter firmware status aggregation.
/// Accepts a pool, connection, or transaction through `DbReader` and performs one
/// SELECT. Rows have no guaranteed order; consumers define their own order.
pub async fn get_firmware_jobs(
    db: impl crate::db_read::DbReader<'_>,
    bmc_mac: MacAddress,
) -> Result<Vec<(RmsJobKind, String)>, DatabaseError> {
    let sql = "SELECT job_kind, job_id FROM direct_dispatch_rms_jobs \
               WHERE bmc_mac = $1 AND job_kind IN ('firmware_object', 'switch_system_image')";
    let rows: Vec<(String, String)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, job_id)| RmsJobKind::from_db_value(&kind).map(|k| (k, job_id)))
        .collect())
}

/// Replace a device's firmware reference set atomically, leaving reset references untouched.
pub async fn replace_firmware_jobs(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    jobs: &[(RmsJobKind, String)],
) -> Result<(), DatabaseError> {
    if jobs.iter().any(|(kind, _)| {
        !matches!(
            kind,
            RmsJobKind::FirmwareObject | RmsJobKind::SwitchSystemImage
        )
    }) {
        return Err(DatabaseError::InvalidArgument(
            "reset jobs cannot replace firmware references".to_owned(),
        ));
    }
    let mut txn = db.begin().await.map_err(|e| {
        DatabaseError::new("direct_dispatch_rms_job::replace_firmware_jobs: begin", e)
    })?;

    crate::cm_action::lock_firmware(&mut txn, bmc_mac).await?;

    let delete_sql = "DELETE FROM direct_dispatch_rms_jobs \
                      WHERE bmc_mac = $1 AND job_kind IN ('firmware_object', 'switch_system_image')";
    sqlx::query(delete_sql)
        .bind(bmc_mac)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::new(delete_sql, e))?;

    let insert_sql = "INSERT INTO direct_dispatch_rms_jobs (bmc_mac, job_kind, job_id, action_id) \
                      VALUES ($1, $2, $3, $4)";
    for (job_kind, job_id) in jobs {
        let action_id = crate::cm_action::firmware(&mut txn, bmc_mac, *job_kind, job_id).await?;
        sqlx::query(insert_sql)
            .bind(bmc_mac)
            .bind(job_kind.as_str())
            .bind(job_id)
            .bind(action_id)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::new(insert_sql, e))?;
    }

    crate::cm_action::reconcile_firmware(&mut txn, bmc_mac).await?;

    txn.commit()
        .await
        .map_err(|e| DatabaseError::new("direct_dispatch_firmware_job::replace: commit", e))?;
    Ok(())
}

/// Select devices with firmware references for direct-dispatch status routing.
/// Reset references never redirect firmware status from the state controller.
pub async fn find_macs_with_firmware_job(
    db: &sqlx::PgPool,
    bmc_macs: &[MacAddress],
) -> Result<HashSet<MacAddress>, DatabaseError> {
    if bmc_macs.is_empty() {
        return Ok(HashSet::new());
    }
    let sql = "SELECT bmc_mac FROM direct_dispatch_rms_jobs WHERE bmc_mac = ANY($1) \
               AND job_kind IN ('firmware_object', 'switch_system_image')";
    let macs: Vec<MacAddress> = sqlx::query_scalar(sql)
        .bind(bmc_macs)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(macs.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mac_address::MacAddress;

    use super::{
        RmsJobKind, delete, find_macs_with_firmware_job, get, get_firmware_jobs,
        replace_firmware_jobs, save,
    };

    const FW_OBJECT: RmsJobKind = RmsJobKind::FirmwareObject;
    const SYS_IMAGE: RmsJobKind = RmsJobKind::SwitchSystemImage;
    const SDN_RESET: RmsJobKind = RmsJobKind::SwitchSdnFactoryReset;
    const NVOS_RESET: RmsJobKind = RmsJobKind::SwitchNvosFactoryReset;

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    #[crate::sqlx_test]
    async fn save_then_get_round_trips_and_upserts(pool: sqlx::PgPool) {
        // Absent (MAC, kind) has no job.
        assert_eq!(get(&pool, mac(1), FW_OBJECT).await.unwrap(), None);

        // First dispatch persists the job.
        save(&pool, mac(1), FW_OBJECT, "job-a").await.unwrap();
        assert_eq!(
            get(&pool, mac(1), FW_OBJECT).await.unwrap(),
            Some("job-a".to_string())
        );

        // Re-dispatch of the same kind replaces the job rather than erroring or
        // duplicating (ON CONFLICT (bmc_mac, job_kind) DO UPDATE).
        save(&pool, mac(1), FW_OBJECT, "job-b").await.unwrap();
        assert_eq!(
            get(&pool, mac(1), FW_OBJECT).await.unwrap(),
            Some("job-b".to_string())
        );
    }

    #[crate::sqlx_test]
    async fn delete_removes_only_the_named_kind(pool: sqlx::PgPool) {
        save(&pool, mac(1), FW_OBJECT, "job-fw").await.unwrap();
        save(&pool, mac(1), SYS_IMAGE, "job-img").await.unwrap();

        // Deleting one kind leaves the device's other kinds untouched.
        delete(&pool, mac(1), FW_OBJECT).await.unwrap();
        assert_eq!(get(&pool, mac(1), FW_OBJECT).await.unwrap(), None);
        assert_eq!(
            get(&pool, mac(1), SYS_IMAGE).await.unwrap(),
            Some("job-img".to_string())
        );

        // Deleting an absent (MAC, kind) is a no-op, not an error.
        delete(&pool, mac(1), FW_OBJECT).await.unwrap();
    }

    #[crate::sqlx_test]
    async fn distinct_kinds_coexist_and_replace_sets_full_set(pool: sqlx::PgPool) {
        // Firmware and both reset actions coexist under the composite key.
        save(&pool, mac(1), FW_OBJECT, "job-fw").await.unwrap();
        save(&pool, mac(1), SYS_IMAGE, "job-img").await.unwrap();
        save(&pool, mac(1), SDN_RESET, "job-reset").await.unwrap();
        save(&pool, mac(1), NVOS_RESET, "job-nvos").await.unwrap();
        let mut all = get_firmware_jobs(&pool, mac(1)).await.unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                (FW_OBJECT, "job-fw".to_string()),
                (SYS_IMAGE, "job-img".to_string()),
            ]
        );

        // replace_firmware_jobs() installs the whole set: a kind absent from the new set is
        // dropped, and a present kind is overwritten.
        replace_firmware_jobs(&pool, mac(1), &[(SYS_IMAGE, "job-img2".to_string())])
            .await
            .unwrap();
        assert_eq!(
            get_firmware_jobs(&pool, mac(1)).await.unwrap(),
            vec![(SYS_IMAGE, "job-img2".to_string())]
        );
        assert_eq!(get(&pool, mac(1), FW_OBJECT).await.unwrap(), None);

        // An empty firmware set cannot clear either reset reference.
        replace_firmware_jobs(&pool, mac(1), &[]).await.unwrap();
        assert!(get_firmware_jobs(&pool, mac(1)).await.unwrap().is_empty());
        assert_eq!(
            get(&pool, mac(1), SDN_RESET).await.unwrap().as_deref(),
            Some("job-reset")
        );
        assert_eq!(
            get(&pool, mac(1), NVOS_RESET).await.unwrap().as_deref(),
            Some("job-nvos")
        );
    }

    #[crate::sqlx_test]
    async fn find_macs_with_firmware_job_returns_only_present_macs(pool: sqlx::PgPool) {
        save(&pool, mac(1), FW_OBJECT, "job-1").await.unwrap();
        save(&pool, mac(2), SDN_RESET, "job-reset").await.unwrap();
        save(&pool, mac(3), FW_OBJECT, "job-3").await.unwrap();
        save(&pool, mac(4), NVOS_RESET, "job-nvos").await.unwrap();

        // A reset-only device must not enter firmware direct-dispatch routing.
        let found = find_macs_with_firmware_job(&pool, &[mac(1), mac(2), mac(3), mac(4)])
            .await
            .unwrap();
        assert_eq!(found, HashSet::from([mac(1), mac(3)]));
    }

    #[crate::sqlx_test]
    async fn find_macs_with_firmware_job_short_circuits_on_empty_input(pool: sqlx::PgPool) {
        save(&pool, mac(1), FW_OBJECT, "job-1").await.unwrap();

        let found = find_macs_with_firmware_job(&pool, &[]).await.unwrap();
        assert!(found.is_empty());
    }

    const RENAME_MIGRATION: &str =
        include_str!("../migrations/20261102224844_direct_dispatch_rms_jobs.sql");

    #[crate::sqlx_test]
    async fn rename_migration_preserves_firmware_references(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        sqlx::raw_sql(
            "ALTER TABLE direct_dispatch_rms_jobs RENAME TO direct_dispatch_firmware_update_jobs;
             INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_kind, job_id, created)
             VALUES ('02:00:00:00:00:01', 'firmware_object', 'old-fw', '2026-10-01 10:00:00+00'),
                    ('02:00:00:00:00:01', 'switch_system_image', 'old-image', '2026-10-02 10:00:00+00')",
        )
        .execute(&mut *txn)
        .await
        .unwrap();
        let predecessor: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT job_kind, job_id, created::text FROM direct_dispatch_firmware_update_jobs ORDER BY job_kind",
        )
        .fetch_all(&mut *txn)
        .await
        .unwrap();
        sqlx::raw_sql(RENAME_MIGRATION)
            .execute(&mut *txn)
            .await
            .unwrap();
        let migrated: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT job_kind, job_id, created::text FROM direct_dispatch_rms_jobs ORDER BY job_kind",
        )
        .fetch_all(&mut *txn)
        .await
        .unwrap();
        assert_eq!(migrated, predecessor);
        txn.commit().await.unwrap();
    }

    // The table's create, backfill, and job_kind migrations have no Rust
    // counterpart, so exercise the real SQL files (via `include_str!`, so the
    // test cannot drift from what ships). The `sqlx_test` harness has already
    // advanced this table to the final `(bmc_mac, job_kind)` key, so the test
    // resets it to the create-migration schema and replays the three migrations
    // in production order: `create` -> `backfill` (which upserts on the
    // `bmc_mac`-only key that predates job_kind) -> `job_kind` (which widens the
    // key and defaults every backfilled row to firmware_object).
    const CREATE_MIGRATION: &str =
        include_str!("../migrations/20260904214512_direct_dispatch_firmware_update_jobs.sql");
    const BACKFILL_MIGRATION: &str = include_str!(
        "../migrations/20260904214537_backfill_direct_dispatch_firmware_update_jobs.sql"
    );
    const JOB_KIND_MIGRATION: &str = include_str!(
        "../migrations/20260908162734_direct_dispatch_firmware_update_jobs_job_kind.sql"
    );

    const SEGMENT_ID: &str = "20000000-0000-0000-0000-000000000001";

    async fn seed_segment(pool: &sqlx::PgPool) {
        sqlx::query(
            "INSERT INTO network_segments (id, name, version) \
             VALUES ($1::uuid, 'seg', 'test')",
        )
        .bind(SEGMENT_ID)
        .execute(pool)
        .await
        .unwrap();
    }

    // Inserts a machine, optionally recording a legacy direct-dispatch job ID in
    // the column this release stopped writing.
    async fn seed_machine(pool: &sqlx::PgPool, id: &str, job_id: Option<&str>) {
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        if let Some(job_id) = job_id {
            sqlx::query("UPDATE machines SET backend_firmware_object_job_id = $1 WHERE id = $2")
                .bind(job_id)
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    // Inserts a BMC interface (associated to `machine_id`, or unassociated for a
    // pre-ingestion tray) and returns its id.
    async fn seed_bmc_interface(
        pool: &sqlx::PgPool,
        mac: &str,
        machine_id: Option<&str>,
    ) -> String {
        let association_type = if machine_id.is_some() {
            "Machine"
        } else {
            "None"
        };
        sqlx::query_scalar(
            "INSERT INTO machine_interfaces \
                 (machine_id, segment_id, mac_address, primary_interface, hostname, \
                  association_type, interface_type) \
             VALUES ($1, $2::uuid, $3::macaddr, false, $4, $5::association_type, 'Bmc') \
             RETURNING id::text",
        )
        .bind(machine_id)
        .bind(SEGMENT_ID)
        .bind(mac)
        .bind(format!("{mac}-bmc"))
        .bind(association_type)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn seed_interface_address(pool: &sqlx::PgPool, interface_id: &str, address: &str) {
        sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address) \
             VALUES ($1::uuid, $2::inet)",
        )
        .bind(interface_id)
        .bind(address)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_explored_endpoint(pool: &sqlx::PgPool, address: &str, job_id: &str) {
        sqlx::query(
            "INSERT INTO explored_endpoints \
                 (address, exploration_report, version, backend_firmware_object_job_id) \
             VALUES ($1::inet, '{}'::jsonb, 'v', $2)",
        )
        .bind(address)
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn backfilled_jobs(pool: &sqlx::PgPool) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT bmc_mac::text, job_kind, job_id FROM direct_dispatch_firmware_update_jobs \
             ORDER BY bmc_mac::text",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[crate::sqlx_test]
    async fn backfill_migrates_legacy_job_ids_keyed_by_bmc_mac(pool: sqlx::PgPool) {
        seed_segment(&pool).await;

        // The harness has already renamed this table and widened its key to
        // (bmc_mac, job_kind); rebuild the create-migration schema so the backfill (which
        // upserts on the bmc_mac-only key that predates job_kind) runs exactly as
        // it did in the release that shipped it.
        sqlx::raw_sql("DROP TABLE direct_dispatch_rms_jobs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(CREATE_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        // Ingested tray with a job, resolved via its machine's BMC interface.
        seed_machine(&pool, "m-ingested", Some("job-ingested")).await;
        seed_bmc_interface(&pool, "02:00:00:00:00:10", Some("m-ingested")).await;

        // Pre-ingestion tray with a job on explored_endpoints, resolved via the
        // BMC interface that owns the endpoint's IP.
        let pre_iface = seed_bmc_interface(&pool, "02:00:00:00:00:20", None).await;
        seed_interface_address(&pool, &pre_iface, "10.0.0.20").await;
        seed_explored_endpoint(&pool, "10.0.0.20", "job-preingest").await;

        // Same MAC in both sources: the ingested machine holds the current job
        // while a stale explored_endpoints entry lingers. The machines job wins.
        seed_machine(&pool, "m-both", Some("job-both-machine")).await;
        let both_iface = seed_bmc_interface(&pool, "02:00:00:00:00:30", Some("m-both")).await;
        seed_interface_address(&pool, &both_iface, "10.0.0.30").await;
        seed_explored_endpoint(&pool, "10.0.0.30", "job-both-stale").await;

        // Machine without a job must not produce a row.
        seed_machine(&pool, "m-nojob", None).await;
        seed_bmc_interface(&pool, "02:00:00:00:00:40", Some("m-nojob")).await;

        // The backfill upserts against the bmc_mac-keyed predecessor table.
        sqlx::raw_sql(BACKFILL_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        // Re-applying the backfill is idempotent (no duplicate-key error, no
        // change), matching how the harness runs it a second time. This must
        // happen before the key is widened, since the backfill's ON CONFLICT
        // targets the bmc_mac-only key.
        sqlx::raw_sql(BACKFILL_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        // Widen the key to (bmc_mac, job_kind); every backfilled row takes the
        // firmware_object default.
        sqlx::raw_sql(JOB_KIND_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        // Every legacy job survives the full sequence as a firmware-object job.
        assert_eq!(
            backfilled_jobs(&pool).await,
            vec![
                (
                    "02:00:00:00:00:10".to_string(),
                    "firmware_object".to_string(),
                    "job-ingested".to_string()
                ),
                (
                    "02:00:00:00:00:20".to_string(),
                    "firmware_object".to_string(),
                    "job-preingest".to_string()
                ),
                (
                    "02:00:00:00:00:30".to_string(),
                    "firmware_object".to_string(),
                    "job-both-machine".to_string()
                ),
            ]
        );
    }
}
