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
//! job IDs, keyed by device BMC MAC.
//!
//! The BMC MAC is a device's stable identity across ingestion, so a single
//! table serves both ingested devices (which have a machine/switch/power-shelf
//! row) and pre-ingestion devices (which do not). Persisting the backend job ID
//! here lets `get_firmware_status` recover it after a nico-api restart clears
//! the in-memory job map, without splitting the state across per-device-type
//! rows.

use std::collections::HashSet;

use mac_address::MacAddress;

use crate::DatabaseError;

/// Persist (or overwrite) the backend firmware-update job ID for `bmc_mac`.
///
/// A re-dispatch to the same device replaces the previous job, so this upserts.
pub async fn save(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    job_id: &str,
) -> Result<(), DatabaseError> {
    let sql = "INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_id) \
               VALUES ($1, $2) \
               ON CONFLICT (bmc_mac) DO UPDATE SET job_id = EXCLUDED.job_id, created = now()";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(job_id)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

/// Fetch the persisted backend firmware-update job ID for `bmc_mac`, if any.
pub async fn get(db: &sqlx::PgPool, bmc_mac: MacAddress) -> Result<Option<String>, DatabaseError> {
    let sql = "SELECT job_id FROM direct_dispatch_firmware_update_jobs WHERE bmc_mac = $1";
    let row: Option<(String,)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(row.map(|(job_id,)| job_id))
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

    use super::{find_macs_with_job, get, save};

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    #[crate::sqlx_test]
    async fn save_then_get_round_trips_and_upserts(pool: sqlx::PgPool) {
        // Absent MAC has no job.
        assert_eq!(get(&pool, mac(1)).await.unwrap(), None);

        // First dispatch persists the job.
        save(&pool, mac(1), "job-a").await.unwrap();
        assert_eq!(get(&pool, mac(1)).await.unwrap(), Some("job-a".to_string()));

        // Re-dispatch to the same MAC replaces the job rather than erroring or
        // duplicating (ON CONFLICT (bmc_mac) DO UPDATE).
        save(&pool, mac(1), "job-b").await.unwrap();
        assert_eq!(get(&pool, mac(1)).await.unwrap(), Some("job-b".to_string()));
    }

    #[crate::sqlx_test]
    async fn find_macs_with_job_returns_only_present_macs(pool: sqlx::PgPool) {
        save(&pool, mac(1), "job-1").await.unwrap();
        save(&pool, mac(3), "job-3").await.unwrap();

        // mac(2) was never dispatched, so it is excluded from the result.
        let found = find_macs_with_job(&pool, &[mac(1), mac(2), mac(3)])
            .await
            .unwrap();
        assert_eq!(found, HashSet::from([mac(1), mac(3)]));
    }

    #[crate::sqlx_test]
    async fn find_macs_with_job_short_circuits_on_empty_input(pool: sqlx::PgPool) {
        save(&pool, mac(1), "job-1").await.unwrap();

        let found = find_macs_with_job(&pool, &[]).await.unwrap();
        assert!(found.is_empty());
    }

    // The backfill migration has no Rust counterpart, so exercise the real SQL
    // file (via `include_str!`, so the test cannot drift from what ships)
    // against a database seeded at the predecessor schema. The `sqlx_test`
    // harness applies every migration first on an empty database, where the
    // backfill is a no-op; re-applying it here after seeding proves the
    // row-migration behavior, and the `ON CONFLICT DO NOTHING` in the file makes
    // the second application safe.
    const BACKFILL_MIGRATION: &str = include_str!(
        "../migrations/20260904214537_backfill_direct_dispatch_firmware_update_jobs.sql"
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

    async fn backfilled_jobs(pool: &sqlx::PgPool) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT bmc_mac::text, job_id FROM direct_dispatch_firmware_update_jobs \
             ORDER BY bmc_mac::text",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[crate::sqlx_test]
    async fn backfill_migrates_legacy_job_ids_keyed_by_bmc_mac(pool: sqlx::PgPool) {
        seed_segment(&pool).await;

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

        sqlx::raw_sql(BACKFILL_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            backfilled_jobs(&pool).await,
            vec![
                ("02:00:00:00:00:10".to_string(), "job-ingested".to_string()),
                ("02:00:00:00:00:20".to_string(), "job-preingest".to_string()),
                (
                    "02:00:00:00:00:30".to_string(),
                    "job-both-machine".to_string()
                ),
            ]
        );

        // Re-applying the backfill is idempotent (no duplicate-key error, no
        // change), matching how the harness runs it a second time.
        sqlx::raw_sql(BACKFILL_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(backfilled_jobs(&pool).await.len(), 3);
    }
}
