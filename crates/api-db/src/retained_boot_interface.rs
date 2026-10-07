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

//! Last-known boot interface pairs that outlive their `machine_interfaces`
//! rows.
//!
//! When an interface row is deleted, its `boot_interface_id` -- the
//! vendor-named Redfish `EthernetInterface.Id` -- is the one piece of state
//! a re-ingested machine cannot always rediscover on its own: after a
//! DPU/NIC mode flip the BMC can report the interface id without its MAC,
//! so the pair can't be re-derived later. Rows here are written by
//! `machine_interface::delete` itself, keyed by MAC with no foreign keys
//! (everything they referenced is gone), and consumed once the id lands on
//! a `machine_interfaces` row again.
//!
//! Records are honored for as long as the operator-configured
//! `retained_boot_interface_window` allows. The default (`None`) is
//! forever -- if the machine eventually comes back, the pair is waiting.
//! Setting a window bounds the reach of a recycled MAC: one reappearing on
//! different hardware long after a deletion (virtual-MAC reassignment, a
//! NIC moved between slots) should not inherit an interface id that aims
//! boot-order setup at a Redfish resource that no longer exists there.
//! Migrations consume their records within minutes either way.

use mac_address::MacAddress;
use sqlx::{FromRow, PgConnection};

use crate::DatabaseError;
use crate::db_read::DbReader;

/// One raw `retained_boot_interfaces` row: the preserved boot interface id for
/// a MAC and when it was recorded, returned verbatim with no retention-window
/// filtering. Built for the boot-interface troubleshooting view, which wants to
/// surface stale records too -- the window-filtered [`find_by_mac`] and the
/// consuming `take_by_mac` would hide or remove them.
#[derive(Debug, Clone, FromRow)]
pub struct RetainedBootInterfaceRecord {
    pub mac_address: MacAddress,
    pub boot_interface_id: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

/// Record the boot interface pair for a MAC, overwriting any prior record
/// (the newest observation wins).
pub async fn upsert(
    txn: &mut PgConnection,
    mac_address: MacAddress,
    boot_interface_id: &str,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO retained_boot_interfaces (mac_address, boot_interface_id) \
                 VALUES ($1, $2) \
                 ON CONFLICT (mac_address) \
                 DO UPDATE SET boot_interface_id = EXCLUDED.boot_interface_id, recorded_at = NOW()";
    sqlx::query(query)
        .bind(mac_address)
        .bind(boot_interface_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Look up the retained boot interface id for a MAC without consuming it.
/// Records older than `window` (when one is set) are not returned. The
/// consuming `take_by_mac` is the one production flows use; this read is
/// for inspection (and test assertions).
pub async fn find_by_mac(
    txn: &mut PgConnection,
    mac_address: MacAddress,
    window: Option<chrono::Duration>,
) -> Result<Option<String>, DatabaseError> {
    let query = "SELECT boot_interface_id FROM retained_boot_interfaces \
                 WHERE mac_address = $1 \
                 AND ($2::bigint IS NULL OR recorded_at > NOW() - ($2::bigint * INTERVAL '1 second'))";
    sqlx::query_scalar(query)
        .bind(mac_address)
        .bind(window.map(|w| w.num_seconds()))
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Fetch the full retained records for a set of MACs without consuming them
/// and without any retention-window filtering -- every matching row, including
/// ones aged past the configured window, is returned with its `recorded_at`.
///
/// This is the troubleshooting read: where [`find_by_mac`] answers "would this
/// MAC's pair still apply?" (window-filtered, value only), this answers "what
/// is actually on file for these MACs, fresh or stale?" so an operator can see
/// a record that exists but has aged out. Production reuse flows must stay on
/// the window-aware `take_by_mac`/`find_by_mac`.
pub async fn find_records_by_macs(
    db: impl DbReader<'_>,
    mac_addresses: &[MacAddress],
) -> Result<Vec<RetainedBootInterfaceRecord>, DatabaseError> {
    let query = "SELECT mac_address, boot_interface_id, recorded_at \
                 FROM retained_boot_interfaces WHERE mac_address = ANY($1) \
                 ORDER BY mac_address";
    sqlx::query_as(query)
        .bind(mac_addresses)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Consume the retained record for a MAC, returning its boot interface id
/// when the record is within `window` (always, when no window is set). The
/// record is removed either way -- a `machine_interfaces` row now
/// exists for the MAC, so future explorations keep it current and the
/// retention copy is done.
pub async fn take_by_mac(
    txn: &mut PgConnection,
    mac_address: MacAddress,
    window: Option<chrono::Duration>,
) -> Result<Option<String>, DatabaseError> {
    let query = "DELETE FROM retained_boot_interfaces WHERE mac_address = $1 \
                 RETURNING boot_interface_id, \
                 ($2::bigint IS NULL OR recorded_at > NOW() - ($2::bigint * INTERVAL '1 second')) AS applicable";
    let row: Option<(String, bool)> = sqlx::query_as(query)
        .bind(mac_address)
        .bind(window.map(|w| w.num_seconds()))
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(row.and_then(|(boot_interface_id, applicable)| applicable.then_some(boot_interface_id)))
}

/// Remove records that have aged out of `window`. A no-op when no window
/// is set -- without one, every record waits forever for its machine to
/// come back. Reads already ignore expired records; this sweep keeps MACs
/// that never return from occupying table rows indefinitely.
pub async fn delete_expired(
    txn: &mut PgConnection,
    window: Option<chrono::Duration>,
) -> Result<u64, DatabaseError> {
    let Some(window) = window else {
        return Ok(0);
    };
    let query = "DELETE FROM retained_boot_interfaces \
                 WHERE recorded_at <= NOW() - ($1::bigint * INTERVAL '1 second')";
    let result = sqlx::query(query)
        .bind(window.num_seconds())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use mac_address::MacAddress;
    use sqlx::PgConnection;

    use super::{delete_expired, find_by_mac, find_records_by_macs, take_by_mac, upsert};

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    /// Backdate a record's `recorded_at` so the window / expiry paths have a
    /// stale row to act on -- production only ever writes `recorded_at = NOW()`.
    async fn age_record(txn: &mut PgConnection, mac: MacAddress, seconds_ago: i64) {
        sqlx::query(
            "UPDATE retained_boot_interfaces \
             SET recorded_at = NOW() - ($2::bigint * INTERVAL '1 second') \
             WHERE mac_address = $1",
        )
        .bind(mac)
        .bind(seconds_ago)
        .execute(txn)
        .await
        .unwrap();
    }

    #[crate::sqlx_test]
    async fn upsert_then_find_by_mac_returns_the_retained_id(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.7-1-1")
            .await
            .unwrap();

        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), None)
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.7-1-1"),
        );
        // A MAC with nothing on file resolves to None.
        assert_eq!(find_by_mac(txn.as_mut(), mac(2), None).await.unwrap(), None);
    }

    #[crate::sqlx_test]
    async fn upsert_keeps_the_newest_id_for_a_mac(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.1-1").await.unwrap();
        upsert(txn.as_mut(), mac(1), "NIC.Slot.7-1-1")
            .await
            .unwrap();

        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), None)
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.7-1-1"),
            "the newest observation wins on conflict",
        );
    }

    #[crate::sqlx_test]
    async fn take_by_mac_returns_the_id_and_consumes_the_row(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.7-1-1")
            .await
            .unwrap();

        assert_eq!(
            take_by_mac(txn.as_mut(), mac(1), None)
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.7-1-1"),
        );
        // The row is gone after a take...
        assert_eq!(find_by_mac(txn.as_mut(), mac(1), None).await.unwrap(), None);
        // ...so a second take finds nothing.
        assert_eq!(take_by_mac(txn.as_mut(), mac(1), None).await.unwrap(), None);
    }

    #[crate::sqlx_test]
    async fn find_by_mac_filters_records_aged_past_the_window(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.7-1-1")
            .await
            .unwrap();
        age_record(txn.as_mut(), mac(1), 3600).await; // an hour old

        // Inside a generous window the pair still applies.
        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), Some(Duration::hours(2)))
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.7-1-1"),
        );
        // Aged past a tight window, it is filtered out.
        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), Some(Duration::seconds(60)))
                .await
                .unwrap(),
            None,
        );
        // No window means forever, so it applies again.
        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), None)
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.7-1-1"),
        );
    }

    #[crate::sqlx_test]
    async fn take_by_mac_withholds_a_stale_id_but_still_consumes_the_row(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.7-1-1")
            .await
            .unwrap();
        age_record(txn.as_mut(), mac(1), 3600).await; // an hour old

        // Past the window the id is withheld -- a recycled MAC must not inherit it...
        assert_eq!(
            take_by_mac(txn.as_mut(), mac(1), Some(Duration::seconds(60)))
                .await
                .unwrap(),
            None,
        );
        // ...but the row is consumed regardless, so a stale record can't linger.
        assert_eq!(find_by_mac(txn.as_mut(), mac(1), None).await.unwrap(), None);
    }

    #[crate::sqlx_test]
    async fn delete_expired_sweeps_only_aged_rows_and_noops_without_a_window(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.1-1").await.unwrap(); // fresh
        upsert(txn.as_mut(), mac(2), "NIC.Slot.2-1").await.unwrap();
        age_record(txn.as_mut(), mac(2), 3600).await; // stale

        // Without a window nothing ever expires, so the sweep is a no-op.
        assert_eq!(delete_expired(txn.as_mut(), None).await.unwrap(), 0);

        // A window removes only the aged row.
        assert_eq!(
            delete_expired(txn.as_mut(), Some(Duration::seconds(60)))
                .await
                .unwrap(),
            1,
        );
        assert_eq!(find_by_mac(txn.as_mut(), mac(2), None).await.unwrap(), None);
        assert_eq!(
            find_by_mac(txn.as_mut(), mac(1), None)
                .await
                .unwrap()
                .as_deref(),
            Some("NIC.Slot.1-1"),
            "fresh records survive the sweep",
        );
    }

    #[crate::sqlx_test]
    async fn find_records_by_macs_returns_every_match_including_stale(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(txn.as_mut(), mac(1), "NIC.Slot.1-1").await.unwrap();
        upsert(txn.as_mut(), mac(2), "NIC.Slot.2-1").await.unwrap();
        age_record(txn.as_mut(), mac(1), 3600).await; // stale, but still surfaced

        let records = find_records_by_macs(txn.as_mut(), &[mac(1), mac(2)])
            .await
            .unwrap();

        // The troubleshooting read is unfiltered and ordered by MAC: both rows come
        // back, the stale one included.
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].mac_address, mac(1));
        assert_eq!(records[0].boot_interface_id, "NIC.Slot.1-1");
        assert_eq!(records[1].mac_address, mac(2));
        assert_eq!(records[1].boot_interface_id, "NIC.Slot.2-1");
    }
}
