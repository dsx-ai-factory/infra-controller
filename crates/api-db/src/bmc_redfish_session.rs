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

//! Persistence layer for the `BmcSessionManager`.
//!
//! One row per outstanding Redfish session. A caller identity may hold many
//! rows for one BMC -- replicas that share a SPIFFE id each mint their own
//! session -- and the manager caps how many by revoking its oldest ones.
//! The `X-Auth-Token` itself is not persisted; it is returned to the caller
//! once and the only durable artifact is the session's `@odata.id`, which is
//! what a later revoke (cap enforcement or `flush_mac`) needs.

use mac_address::MacAddress;
use model::bmc_redfish_session::StoredSession;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, DatabaseError, DatabaseResult};

/// Returns every outstanding session row for `(spiffe_service_id, bmc_mac)`,
/// oldest first, so a caller enforcing a cap revokes from the front.
pub async fn find_by_owner(
    txn: impl DbReader<'_>,
    spiffe_service_id: &str,
    bmc_mac: MacAddress,
) -> DatabaseResult<Vec<StoredSession>> {
    let query = "SELECT spiffe_service_id, bmc_mac_address, session_odata_id, issued_at
                 FROM bmc_redfish_sessions
                 WHERE spiffe_service_id = $1 AND bmc_mac_address = $2
                 ORDER BY issued_at, session_odata_id";

    sqlx::query_as(query)
        .bind(spiffe_service_id)
        .bind(bmc_mac)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Records a newly created session. `issued_at` is set to `now()`
/// server-side so timestamps are consistent across replicas.
///
/// A conflicting row can only describe a dead session: the BMC just issued
/// this `@odata.id` to the session being recorded, so whatever the row used
/// to describe no longer exists. Ownership therefore moves to the caller
/// that minted the new session. Without this, a stale row -- retained on a
/// failed revoke or a vanished owner -- would wedge every mint against a
/// BMC that reuses session ids after a reboot.
pub async fn insert(
    txn: &mut PgConnection,
    spiffe_service_id: &str,
    bmc_mac: MacAddress,
    session_odata_id: &str,
) -> DatabaseResult<()> {
    let query = "INSERT INTO bmc_redfish_sessions
                       (spiffe_service_id, bmc_mac_address, session_odata_id, issued_at)
                       VALUES ($1, $2, $3, now())
                       ON CONFLICT (bmc_mac_address, session_odata_id) DO UPDATE
                       SET spiffe_service_id = EXCLUDED.spiffe_service_id,
                           issued_at         = EXCLUDED.issued_at";

    sqlx::query(query)
        .bind(spiffe_service_id)
        .bind(bmc_mac)
        .bind(session_odata_id)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// `SessionNotOwned` means the session row is missing or belongs to another
/// service identity. The delete does not distinguish these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionNotOwned;

/// `delete_session` removes one session row only if it belongs to this owner.
/// `NotApplied` requires leaving the remote session alone: [`insert`] may have
/// transferred the row to another identity after the BMC reused its `@odata.id`.
pub async fn delete_session(
    txn: &mut PgConnection,
    spiffe_service_id: &str,
    bmc_mac: MacAddress,
    session_odata_id: &str,
) -> DatabaseResult<ConditionalWrite<(), SessionNotOwned>> {
    let query = "DELETE FROM bmc_redfish_sessions
                       WHERE spiffe_service_id = $1
                         AND bmc_mac_address = $2
                         AND session_odata_id = $3";

    sqlx::query(query)
        .bind(spiffe_service_id)
        .bind(bmc_mac)
        .bind(session_odata_id)
        .execute(txn)
        .await
        .map(|result| {
            if result.rows_affected() > 0 {
                ConditionalWrite::Applied(())
            } else {
                ConditionalWrite::NotApplied(SessionNotOwned)
            }
        })
        .map_err(|e| DatabaseError::query(query, e))
}

/// Deletes every row whose `bmc_mac_address` matches `bmc_mac` and returns
/// the rows that were removed. The returned vector can be used by callers
/// that want to best-effort `DELETE` the corresponding sessions on the BMC.
pub async fn delete_by_mac(
    txn: &mut PgConnection,
    bmc_mac: MacAddress,
) -> DatabaseResult<Vec<StoredSession>> {
    let query = "DELETE FROM bmc_redfish_sessions
                 WHERE bmc_mac_address = $1
                 RETURNING spiffe_service_id, bmc_mac_address, session_odata_id, issued_at";

    sqlx::query_as(query)
        .bind(bmc_mac)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;
    use sqlx::PgPool;

    use super::{SessionNotOwned, delete_by_mac, delete_session, find_by_owner, insert};
    use crate::ConditionalWrite;

    const PER_SESSION_ROWS_MIGRATION: &str =
        include_str!("../migrations/20260824235713_bmc_redfish_sessions_per_session_rows.sql");

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    /// Rewinds `bmc_redfish_sessions` to its schema before the
    /// per-session-rows migration (primary key on the caller identity, plus
    /// the by-mac index), so the real migration file can be exercised
    /// against realistic predecessor rows. The `sqlx_test` harness applies
    /// every migration up front on an empty database, which never exercises
    /// the dedup semantics.
    async fn rewind_to_predecessor_schema(pool: &PgPool) {
        sqlx::raw_sql(
            "ALTER TABLE bmc_redfish_sessions
                 DROP CONSTRAINT bmc_redfish_sessions_pkey,
                 ADD CONSTRAINT bmc_redfish_sessions_pkey
                     PRIMARY KEY (spiffe_service_id, bmc_mac_address);
             CREATE INDEX bmc_redfish_sessions_by_mac
                 ON bmc_redfish_sessions USING btree (bmc_mac_address);",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_row(
        pool: &PgPool,
        spiffe: &str,
        bmc_mac: MacAddress,
        odata_id: &str,
        issued_at_offset_secs: i64,
    ) {
        sqlx::query(
            "INSERT INTO bmc_redfish_sessions
                 (spiffe_service_id, bmc_mac_address, session_odata_id, issued_at)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
        )
        .bind(spiffe)
        .bind(bmc_mac)
        .bind(odata_id)
        .bind(issued_at_offset_secs as f64)
        .execute(pool)
        .await
        .unwrap();
    }

    // Exercises the real migration file against predecessor-schema rows,
    // including the case its dedup exists for: two identities' rows naming
    // the same (mac, @odata.id) because the BMC reused a session id. The
    // newer row must win and the new primary key must then apply.
    #[crate::sqlx_test]
    async fn per_session_rows_migration_dedups_and_swaps_the_key(pool: PgPool) {
        rewind_to_predecessor_schema(&pool).await;

        let contested = mac(0x20);
        // Older and newer claims on the same reused @odata.id.
        seed_row(
            &pool,
            "svc-old",
            contested,
            "/redfish/v1/SessionService/Sessions/1",
            0,
        )
        .await;
        seed_row(
            &pool,
            "svc-new",
            contested,
            "/redfish/v1/SessionService/Sessions/1",
            60,
        )
        .await;
        // An uncontested row on the same BMC and one on another BMC survive.
        seed_row(
            &pool,
            "svc-else",
            contested,
            "/redfish/v1/SessionService/Sessions/2",
            0,
        )
        .await;
        seed_row(
            &pool,
            "svc-old",
            mac(0x21),
            "/redfish/v1/SessionService/Sessions/1",
            0,
        )
        .await;

        sqlx::raw_sql(PER_SESSION_ROWS_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT spiffe_service_id, session_odata_id FROM bmc_redfish_sessions
             ORDER BY spiffe_service_id, session_odata_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "svc-else".to_string(),
                    "/redfish/v1/SessionService/Sessions/2".to_string()
                ),
                (
                    "svc-new".to_string(),
                    "/redfish/v1/SessionService/Sessions/1".to_string()
                ),
                (
                    "svc-old".to_string(),
                    "/redfish/v1/SessionService/Sessions/1".to_string()
                ),
            ],
            "the older duplicate must be dropped and everything else kept"
        );

        // The new key: same identity, same BMC, second session is fine now.
        let mut txn = pool.begin().await.unwrap();
        insert(
            txn.as_mut(),
            "svc-new",
            contested,
            "/redfish/v1/SessionService/Sessions/3",
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
    }

    // The property this table now provides: one identity holds many
    // concurrent sessions on one BMC, returned oldest first. Under the old
    // one-row-per-identity primary key the second insert conflicted.
    #[crate::sqlx_test]
    async fn one_identity_holds_many_sessions_oldest_first(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let bmc = mac(1);

        insert(txn.as_mut(), "svc", bmc, "/sessions/1")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc", bmc, "/sessions/2")
            .await
            .unwrap();
        // Another identity and another BMC must not appear in the result.
        insert(txn.as_mut(), "other", bmc, "/sessions/3")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc", mac(2), "/sessions/4")
            .await
            .unwrap();

        let rows = find_by_owner(txn.as_mut(), "svc", bmc).await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.session_odata_id.as_str())
                .collect::<Vec<_>>(),
            vec!["/sessions/1", "/sessions/2"],
        );
    }

    #[crate::sqlx_test]
    async fn delete_session_removes_one_row_and_tolerates_absence(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let bmc = mac(3);

        insert(txn.as_mut(), "svc", bmc, "/sessions/1")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc", bmc, "/sessions/2")
            .await
            .unwrap();

        assert_eq!(
            delete_session(txn.as_mut(), "svc", bmc, "/sessions/1")
                .await
                .unwrap(),
            ConditionalWrite::Applied(())
        );
        // Deleting again is a no-op, matching best-effort revocation.
        assert_eq!(
            delete_session(txn.as_mut(), "svc", bmc, "/sessions/1")
                .await
                .unwrap(),
            ConditionalWrite::NotApplied(SessionNotOwned)
        );

        let rows = find_by_owner(txn.as_mut(), "svc", bmc).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_odata_id, "/sessions/2");
    }

    // A delete queued by one identity's cap pass must not remove the row
    // after `insert` has handed it to a new owner: that row is the only
    // revocation handle for the new owner's live session.
    #[crate::sqlx_test]
    async fn delete_session_is_a_noop_after_an_ownership_takeover(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let bmc = mac(7);

        insert(txn.as_mut(), "svc-old", bmc, "/sessions/1")
            .await
            .unwrap();
        // The BMC reused the id; the new mint took the row over.
        insert(txn.as_mut(), "svc-new", bmc, "/sessions/1")
            .await
            .unwrap();

        assert_eq!(
            delete_session(txn.as_mut(), "svc-old", bmc, "/sessions/1")
                .await
                .unwrap(),
            ConditionalWrite::NotApplied(SessionNotOwned)
        );

        let rows = find_by_owner(txn.as_mut(), "svc-new", bmc).await.unwrap();
        assert_eq!(rows.len(), 1, "the new owner's row must survive");
    }

    // A conflicting row describes a session the BMC has already replaced, so
    // recording the new session takes the row over instead of erroring --
    // otherwise a stale row would wedge every mint against a BMC that reuses
    // session ids after a reboot.
    #[crate::sqlx_test]
    async fn insert_takes_over_a_stale_row_for_a_reused_session_id(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let bmc = mac(6);

        insert(txn.as_mut(), "svc-old", bmc, "/sessions/1")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc-new", bmc, "/sessions/1")
            .await
            .unwrap();

        assert!(
            find_by_owner(txn.as_mut(), "svc-old", bmc)
                .await
                .unwrap()
                .is_empty(),
            "the stale owner's claim must be gone"
        );
        let rows = find_by_owner(txn.as_mut(), "svc-new", bmc).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_odata_id, "/sessions/1");
    }

    #[crate::sqlx_test]
    async fn delete_by_mac_returns_every_caller_row_for_that_bmc(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let bmc = mac(4);

        insert(txn.as_mut(), "svc-1", bmc, "/sessions/1")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc-2", bmc, "/sessions/2")
            .await
            .unwrap();
        insert(txn.as_mut(), "svc-1", mac(5), "/sessions/3")
            .await
            .unwrap();

        let mut removed = delete_by_mac(txn.as_mut(), bmc).await.unwrap();
        removed.sort_by(|a, b| a.session_odata_id.cmp(&b.session_odata_id));
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].spiffe_service_id, "svc-1");
        assert_eq!(removed[1].spiffe_service_id, "svc-2");

        assert!(
            find_by_owner(txn.as_mut(), "svc-1", bmc)
                .await
                .unwrap()
                .is_empty()
        );
        let survivor = find_by_owner(txn.as_mut(), "svc-1", mac(5)).await.unwrap();
        assert_eq!(survivor.len(), 1);
    }
}
