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

//! Storage for direct-dispatch (`--bypass-state-controller`) firmware-update
//! job IDs, keyed by device BMC MAC and job kind.
//!
//! The BMC MAC is a device's stable identity across ingestion, so a single
//! table serves both ingested devices (which have a machine/switch/power-shelf
//! row) and pre-ingestion devices (which do not). Persisting the backend job ID
//! here lets `get_firmware_status` recover it after a nico-api restart clears
//! the in-memory job map, without splitting the state across per-device-type
//! rows.
//!
//! A device can have several concurrent jobs of distinct kinds (a switch tracks
//! a firmware-object job and an NVOS system-image job independently), so rows
//! are keyed by `(bmc_mac, job_kind)`. `job_kind` is an opaque backend-job-class
//! label owned by the caller (the component-manager backend); this layer only
//! stores and returns it.

use std::collections::HashSet;

use mac_address::MacAddress;

use crate::DatabaseError;

/// The class of backend firmware-update job stored in a row.
///
/// A device can have one in-flight job per kind (a switch tracks a
/// firmware-object job and an NVOS system-image job independently), so the kind
/// is part of the row's identity (`PRIMARY KEY (bmc_mac, job_kind)`) and selects
/// which backend query recovers a job's status after a restart.
///
/// Stored as `text` rather than a native Postgres enum: this table is
/// device-agnostic (reused across compute, switch, and future power-shelf
/// backends), so the set of kinds is the union across backends and grows with a
/// one-line change here rather than an `ALTER TYPE` migration. The database does
/// not branch on the value, so it owns no invariant a schema enum would enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FirmwareJobKind {
    /// An RMS firmware-object bundle update (compute trays and switches).
    FirmwareObject,
    /// An NVOS system-image update (switches).
    SwitchSystemImage,
}

impl FirmwareJobKind {
    /// The value persisted in the `job_kind` column. Stable: existing rows and
    /// the backfill migration depend on these exact strings.
    pub fn as_str(self) -> &'static str {
        match self {
            FirmwareJobKind::FirmwareObject => "firmware_object",
            FirmwareJobKind::SwitchSystemImage => "switch_system_image",
        }
    }

    /// Parse a persisted `job_kind` value. Returns `None` for a kind this
    /// release does not recognize (e.g. one written by a newer release, or by a
    /// backend whose kinds this reader does not handle).
    pub fn from_db_value(value: &str) -> Option<Self> {
        match value {
            "firmware_object" => Some(FirmwareJobKind::FirmwareObject),
            "switch_system_image" => Some(FirmwareJobKind::SwitchSystemImage),
            _ => None,
        }
    }
}

/// Persist (or overwrite) the backend firmware-update job ID for one
/// `(bmc_mac, job_kind)`.
///
/// A re-dispatch of the same job kind to the same device replaces the previous
/// job, so this upserts. Jobs of other kinds for the device are untouched; use
/// [`replace`] to set a device's full job set atomically.
pub async fn save(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: FirmwareJobKind,
    job_id: &str,
) -> Result<(), DatabaseError> {
    let sql = "INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_kind, job_id) \
               VALUES ($1, $2, $3) \
               ON CONFLICT (bmc_mac, job_kind) \
               DO UPDATE SET job_id = EXCLUDED.job_id, created = now()";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(job_kind.as_str())
        .bind(job_id)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

/// Fetch the persisted job ID for one `(bmc_mac, job_kind)`, if any.
pub async fn get(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_kind: FirmwareJobKind,
) -> Result<Option<String>, DatabaseError> {
    let sql = "SELECT job_id FROM direct_dispatch_firmware_update_jobs \
               WHERE bmc_mac = $1 AND job_kind = $2";
    let row: Option<(String,)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .bind(job_kind.as_str())
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(row.map(|(job_id,)| job_id))
}

/// Fetch every persisted `(job_kind, job_id)` for `bmc_mac`.
///
/// Used by backends whose devices track multiple job kinds (e.g. a switch's
/// firmware-object and system-image jobs) to rebuild the full set after a
/// restart. Rows whose `job_kind` this release does not recognize are skipped,
/// so a reader tolerates kinds written by another backend or a newer release.
pub async fn get_all(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
) -> Result<Vec<(FirmwareJobKind, String)>, DatabaseError> {
    let sql = "SELECT job_kind, job_id FROM direct_dispatch_firmware_update_jobs \
               WHERE bmc_mac = $1";
    let rows: Vec<(String, String)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, job_id)| FirmwareJobKind::from_db_value(&kind).map(|k| (k, job_id)))
        .collect())
}

/// Replace a device's entire persisted job set with `jobs` (pairs of
/// `(job_kind, job_id)`), atomically.
///
/// Deletes any rows for `bmc_mac` whose kind is absent from `jobs`, matching the
/// backend's in-memory "the latest update owns the whole set" semantics. An
/// empty `jobs` clears all rows for the device.
pub async fn replace(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    jobs: &[(FirmwareJobKind, String)],
) -> Result<(), DatabaseError> {
    let mut txn = db
        .begin()
        .await
        .map_err(|e| DatabaseError::new("direct_dispatch_firmware_job::replace: begin", e))?;

    let delete_sql = "DELETE FROM direct_dispatch_firmware_update_jobs WHERE bmc_mac = $1";
    sqlx::query(delete_sql)
        .bind(bmc_mac)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::new(delete_sql, e))?;

    let insert_sql = "INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_kind, job_id) \
                      VALUES ($1, $2, $3)";
    for (job_kind, job_id) in jobs {
        sqlx::query(insert_sql)
            .bind(bmc_mac)
            .bind(job_kind.as_str())
            .bind(job_id)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::new(insert_sql, e))?;
    }

    txn.commit()
        .await
        .map_err(|e| DatabaseError::new("direct_dispatch_firmware_job::replace: commit", e))?;
    Ok(())
}

/// Return the subset of `bmc_macs` that have a persisted firmware-update job.
///
/// Firmware-status routing uses this to decide, in one batched query, which
/// state-controller-managed devices have an in-flight direct-dispatch job to
/// poll from the live backend rather than the DB-only fallback.
pub async fn find_macs_with_job(
    db: &sqlx::PgPool,
    bmc_macs: &[MacAddress],
) -> Result<HashSet<MacAddress>, DatabaseError> {
    if bmc_macs.is_empty() {
        return Ok(HashSet::new());
    }
    let sql = "SELECT bmc_mac FROM direct_dispatch_firmware_update_jobs WHERE bmc_mac = ANY($1)";
    let rows: Vec<(MacAddress,)> = sqlx::query_as(sql)
        .bind(bmc_macs)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(rows.into_iter().map(|(bmc_mac,)| bmc_mac).collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mac_address::MacAddress;

    use super::{FirmwareJobKind, find_macs_with_job, get, get_all, replace, save};

    const FW_OBJECT: FirmwareJobKind = FirmwareJobKind::FirmwareObject;
    const SYS_IMAGE: FirmwareJobKind = FirmwareJobKind::SwitchSystemImage;

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
    async fn distinct_kinds_coexist_and_replace_sets_full_set(pool: sqlx::PgPool) {
        // Two kinds for one device coexist under the composite key.
        save(&pool, mac(1), FW_OBJECT, "job-fw").await.unwrap();
        save(&pool, mac(1), SYS_IMAGE, "job-img").await.unwrap();
        let mut all = get_all(&pool, mac(1)).await.unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                (FW_OBJECT, "job-fw".to_string()),
                (SYS_IMAGE, "job-img".to_string()),
            ]
        );

        // replace() installs the whole set: a kind absent from the new set is
        // dropped, and a present kind is overwritten.
        replace(&pool, mac(1), &[(SYS_IMAGE, "job-img2".to_string())])
            .await
            .unwrap();
        assert_eq!(
            get_all(&pool, mac(1)).await.unwrap(),
            vec![(SYS_IMAGE, "job-img2".to_string())]
        );
        assert_eq!(get(&pool, mac(1), FW_OBJECT).await.unwrap(), None);

        // An empty set clears the device.
        replace(&pool, mac(1), &[]).await.unwrap();
        assert!(get_all(&pool, mac(1)).await.unwrap().is_empty());
    }

    #[crate::sqlx_test]
    async fn find_macs_with_job_returns_only_present_macs(pool: sqlx::PgPool) {
        save(&pool, mac(1), FW_OBJECT, "job-1").await.unwrap();
        save(&pool, mac(3), FW_OBJECT, "job-3").await.unwrap();

        // mac(2) was never dispatched, so it is excluded from the result.
        let found = find_macs_with_job(&pool, &[mac(1), mac(2), mac(3)])
            .await
            .unwrap();
        assert_eq!(found, HashSet::from([mac(1), mac(3)]));
    }

    #[crate::sqlx_test]
    async fn find_macs_with_job_short_circuits_on_empty_input(pool: sqlx::PgPool) {
        save(&pool, mac(1), FW_OBJECT, "job-1").await.unwrap();

        let found = find_macs_with_job(&pool, &[]).await.unwrap();
        assert!(found.is_empty());
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

        // The harness has already migrated this table to the (bmc_mac, job_kind)
        // key; rebuild it at the create-migration schema so the backfill (which
        // upserts on the bmc_mac-only key that predates job_kind) runs exactly as
        // it did in the release that shipped it.
        sqlx::raw_sql("DROP TABLE direct_dispatch_firmware_update_jobs")
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
