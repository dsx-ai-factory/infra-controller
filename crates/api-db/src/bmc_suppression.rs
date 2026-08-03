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

//! Per-subsystem suppression requests for BMC MAC addresses.

use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppression, BmcSuppressionSubsystem, NewBmcSuppression};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Inserts or updates a suppression request.
///
/// Repeated requests preserve the original request and acknowledgement
/// timestamps so retries do not restart an acknowledged handoff.
pub async fn upsert(
    txn: &mut PgConnection,
    input: &NewBmcSuppression,
) -> DatabaseResult<BmcSuppression> {
    const QUERY: &str = "INSERT INTO bmc_suppressions (
        bmc_mac_address,
        subsystem,
        reason
    ) VALUES ($1, $2, $3)
    ON CONFLICT (bmc_mac_address, subsystem) DO UPDATE SET
        reason = EXCLUDED.reason
    RETURNING
        bmc_mac_address,
        subsystem,
        reason,
        requested_at,
        acknowledged_at";

    sqlx::query_as(QUERY)
        .bind(input.bmc_mac_address)
        .bind(input.subsystem)
        .bind(&input.reason)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Ensures a suppression request exists for the MAC without clobbering an
/// existing one.
///
/// Unlike [`upsert`], a conflicting row is preserved verbatim -- including its
/// `reason`, `requested_at`, and `acknowledged_at` -- so a rotation-owned
/// request never overwrites (and can never later delete) an operator's
/// decommissioning request for the same BMC. When no row exists, one is
/// inserted with the supplied reason.
pub async fn ensure_present(
    txn: &mut PgConnection,
    input: &NewBmcSuppression,
) -> DatabaseResult<BmcSuppression> {
    const QUERY: &str = "INSERT INTO bmc_suppressions (
        bmc_mac_address,
        subsystem,
        reason
    ) VALUES ($1, $2, $3)
    ON CONFLICT (bmc_mac_address, subsystem) DO UPDATE SET
        reason = bmc_suppressions.reason
    RETURNING
        bmc_mac_address,
        subsystem,
        reason,
        requested_at,
        acknowledged_at";

    sqlx::query_as(QUERY)
        .bind(input.bmc_mac_address)
        .bind(input.subsystem)
        .bind(&input.reason)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns the suppression rows for the selected BMC MAC addresses in
/// `subsystem`. Missing MACs are simply absent from the result.
pub async fn find_many(
    db: impl DbReader<'_>,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<BmcSuppression>> {
    const QUERY: &str = "SELECT
        bmc_mac_address,
        subsystem,
        reason,
        requested_at,
        acknowledged_at
    FROM bmc_suppressions
    WHERE bmc_mac_address = ANY($1) AND subsystem = $2
    ORDER BY bmc_mac_address";

    sqlx::query_as(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns an active suppression request, if one exists.
pub async fn find(
    db: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Option<BmcSuppression>> {
    const QUERY: &str = "SELECT
        bmc_mac_address,
        subsystem,
        reason,
        requested_at,
        acknowledged_at
    FROM bmc_suppressions
    WHERE bmc_mac_address = $1 AND subsystem = $2";

    sqlx::query_as(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns every BMC suppression for `subsystem`.
pub async fn find_all_by_subsystem(
    db: impl DbReader<'_>,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<BmcSuppression>> {
    const QUERY: &str = "SELECT
        bmc_mac_address,
        subsystem,
        reason,
        requested_at,
        acknowledged_at
    FROM bmc_suppressions
    WHERE subsystem = $1
    ORDER BY bmc_mac_address";

    sqlx::query_as(QUERY)
        .bind(subsystem)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Acknowledges pending suppression requests for the selected BMC MAC addresses.
///
/// Returns the BMC MAC addresses acknowledged by this call.
pub async fn acknowledge_unacknowledged(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<MacAddress>> {
    const QUERY: &str = "UPDATE bmc_suppressions
        SET acknowledged_at = statement_timestamp()
        WHERE bmc_mac_address = ANY($1)
            AND subsystem = $2
            AND acknowledged_at IS NULL
        RETURNING bmc_mac_address";

    sqlx::query_scalar(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns whether a BMC MAC is suppressed for `subsystem`.
pub async fn is_suppressed(
    db: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<bool> {
    const QUERY: &str = "SELECT EXISTS(
        SELECT 1
        FROM bmc_suppressions
        WHERE bmc_mac_address = $1 AND subsystem = $2
    )";

    sqlx::query_scalar(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .fetch_one(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Records that `subsystem` has observed and applied a suppression request.
///
/// The lookup and timestamp write are atomic. The return value is `true` when
/// suppression is active and `false` when no matching request exists. Repeated
/// acknowledgements preserve the first timestamp.
pub async fn acknowledge(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<bool> {
    const QUERY: &str = "UPDATE bmc_suppressions SET
        acknowledged_at = COALESCE(acknowledged_at, statement_timestamp())
    WHERE bmc_mac_address = $1 AND subsystem = $2";

    sqlx::query(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Deletes one subsystem's suppression request for a BMC MAC.
///
/// Returns `true` when a row was removed.
pub async fn delete(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<bool> {
    const QUERY: &str = "DELETE FROM bmc_suppressions
        WHERE bmc_mac_address = $1 AND subsystem = $2";

    sqlx::query(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Deletes a subsystem's suppression requests for a set of BMC MACs.
///
/// Returns the number of rows removed.
pub async fn delete_many(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<u64> {
    const QUERY: &str = "DELETE FROM bmc_suppressions
        WHERE bmc_mac_address = ANY($1) AND subsystem = $2";

    sqlx::query(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .execute(txn)
        .await
        .map(|result| result.rows_affected())
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Deletes a subsystem's suppression requests for a set of BMC MACs, but only
/// rows whose `reason` matches.
///
/// Scoping the delete by reason lets an automated owner (e.g. BMC credential
/// rotation) clean up exactly the rows it created without removing a request
/// another owner (e.g. an operator decommissioning) may hold for the same BMC.
/// Returns the number of rows removed.
pub async fn delete_many_with_reason(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
    reason: &str,
) -> DatabaseResult<u64> {
    const QUERY: &str = "DELETE FROM bmc_suppressions
        WHERE bmc_mac_address = ANY($1) AND subsystem = $2 AND reason = $3";

    sqlx::query(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .bind(reason)
        .execute(txn)
        .await
        .map(|result| result.rows_affected())
        .map_err(|e| DatabaseError::query(QUERY, e))
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;
    use model::bmc_suppression::{BmcSuppressionSubsystem, NewBmcSuppression};

    use super::{
        acknowledge, acknowledge_unacknowledged, delete, delete_many, delete_many_with_reason,
        ensure_present, find, find_all_by_subsystem, find_many, is_suppressed, upsert,
    };

    const SITE_EXPLORER: BmcSuppressionSubsystem = BmcSuppressionSubsystem::SiteExplorer;
    const DHCP: BmcSuppressionSubsystem = BmcSuppressionSubsystem::Dhcp;

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    fn upsert_input(
        last: u8,
        subsystem: BmcSuppressionSubsystem,
        reason: &str,
    ) -> NewBmcSuppression {
        NewBmcSuppression {
            bmc_mac_address: mac(last),
            subsystem,
            reason: reason.to_string(),
        }
    }

    #[crate::sqlx_test]
    async fn subsystems_are_independent(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        upsert(txn.as_mut(), &upsert_input(2, DHCP, "decommissioning"))
            .await
            .unwrap();
        for subsystem in [SITE_EXPLORER, DHCP] {
            upsert(txn.as_mut(), &upsert_input(3, subsystem, "decommissioning"))
                .await
                .unwrap();
        }

        assert_eq!(
            find_all_by_subsystem(txn.as_mut(), SITE_EXPLORER)
                .await
                .unwrap()
                .into_iter()
                .map(|suppression| suppression.bmc_mac_address)
                .collect::<Vec<_>>(),
            vec![mac(1), mac(3)],
        );
        assert_eq!(
            find_all_by_subsystem(txn.as_mut(), DHCP)
                .await
                .unwrap()
                .into_iter()
                .map(|suppression| suppression.bmc_mac_address)
                .collect::<Vec<_>>(),
            vec![mac(2), mac(3)],
        );
        assert!(!is_suppressed(txn.as_mut(), mac(1), DHCP).await.unwrap());
        assert!(
            is_suppressed(txn.as_mut(), mac(3), SITE_EXPLORER)
                .await
                .unwrap()
        );
        assert!(!is_suppressed(txn.as_mut(), mac(4), DHCP).await.unwrap());
    }

    #[crate::sqlx_test]
    async fn acknowledgements_are_scoped_to_subsystem(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        for (last, subsystem, other_subsystem) in
            [(1, SITE_EXPLORER, DHCP), (2, DHCP, SITE_EXPLORER)]
        {
            upsert(
                txn.as_mut(),
                &upsert_input(last, subsystem, "decommissioning"),
            )
            .await
            .unwrap();

            assert!(
                !acknowledge(txn.as_mut(), mac(last), other_subsystem)
                    .await
                    .unwrap()
            );
            assert!(
                find(txn.as_mut(), mac(last), other_subsystem)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                acknowledge(txn.as_mut(), mac(last), subsystem)
                    .await
                    .unwrap()
            );
            assert!(
                find(txn.as_mut(), mac(last), subsystem)
                    .await
                    .unwrap()
                    .unwrap()
                    .acknowledged_at
                    .is_some()
            );
        }
    }

    #[crate::sqlx_test]
    async fn acknowledge_unacknowledged_is_batched_and_idempotent(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        for (last, subsystem) in [(1, SITE_EXPLORER), (2, DHCP), (3, SITE_EXPLORER)] {
            upsert(
                txn.as_mut(),
                &upsert_input(last, subsystem, "decommissioning"),
            )
            .await
            .unwrap();
        }

        let acknowledged =
            acknowledge_unacknowledged(txn.as_mut(), &[mac(1), mac(3), mac(4)], SITE_EXPLORER)
                .await
                .unwrap();
        assert_eq!(acknowledged.len(), 2);
        assert!(acknowledged.contains(&mac(1)));
        assert!(acknowledged.contains(&mac(3)));
        assert!(
            find(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
                .unwrap()
                .acknowledged_at
                .is_some()
        );
        assert!(
            find(txn.as_mut(), mac(2), DHCP)
                .await
                .unwrap()
                .unwrap()
                .acknowledged_at
                .is_none()
        );
        assert!(
            acknowledge_unacknowledged(txn.as_mut(), &[mac(1), mac(3)], SITE_EXPLORER)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[crate::sqlx_test]
    async fn repeated_requests_and_acknowledgements_are_idempotent(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        assert!(
            acknowledge(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
        let initial = find(txn.as_mut(), mac(1), SITE_EXPLORER)
            .await
            .unwrap()
            .unwrap();

        assert!(
            acknowledge(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
        let repeated = find(txn.as_mut(), mac(1), SITE_EXPLORER)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repeated, initial);

        let retried = upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning retry"),
        )
        .await
        .unwrap();
        assert_eq!(retried.requested_at, initial.requested_at);
        assert_eq!(retried.acknowledged_at, initial.acknowledged_at);

        assert!(delete(txn.as_mut(), mac(1), SITE_EXPLORER).await.unwrap());
        let recreated = upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "new decommissioning request"),
        )
        .await
        .unwrap();
        assert!(recreated.acknowledged_at.is_none());
    }

    #[crate::sqlx_test]
    async fn ensure_present_preserves_an_existing_request(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        // An operator decommissioning request already exists and is acknowledged.
        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        assert!(
            acknowledge(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
        let operator = find(txn.as_mut(), mac(1), SITE_EXPLORER)
            .await
            .unwrap()
            .unwrap();

        // A rotation-owned ensure must not clobber reason or acknowledgement.
        let ensured = ensure_present(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "bmc_credential_rotation"),
        )
        .await
        .unwrap();
        assert_eq!(ensured, operator);
        assert_eq!(ensured.reason, "decommissioning");
        assert!(ensured.acknowledged_at.is_some());

        // When absent, ensure_present inserts with the supplied reason.
        let created = ensure_present(
            txn.as_mut(),
            &upsert_input(2, SITE_EXPLORER, "bmc_credential_rotation"),
        )
        .await
        .unwrap();
        assert_eq!(created.reason, "bmc_credential_rotation");
        assert!(created.acknowledged_at.is_none());
    }

    #[crate::sqlx_test]
    async fn find_many_returns_selected_rows(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        for (last, subsystem) in [(1, SITE_EXPLORER), (2, SITE_EXPLORER), (3, DHCP)] {
            upsert(txn.as_mut(), &upsert_input(last, subsystem, "reason"))
                .await
                .unwrap();
        }

        let found = find_many(txn.as_mut(), &[mac(1), mac(2), mac(3)], SITE_EXPLORER)
            .await
            .unwrap();
        assert_eq!(
            found
                .iter()
                .map(|suppression| suppression.bmc_mac_address)
                .collect::<Vec<_>>(),
            vec![mac(1), mac(2)],
        );
        assert!(found.iter().all(|s| s.acknowledged_at.is_none()));
    }

    #[crate::sqlx_test]
    async fn delete_many_with_reason_only_removes_matching_rows(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        // mac(1): operator-owned; mac(2) and mac(3): rotation-owned.
        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        for last in [2, 3] {
            ensure_present(
                txn.as_mut(),
                &upsert_input(last, SITE_EXPLORER, "bmc_credential_rotation"),
            )
            .await
            .unwrap();
        }

        let removed = delete_many_with_reason(
            txn.as_mut(),
            &[mac(1), mac(2), mac(3)],
            SITE_EXPLORER,
            "bmc_credential_rotation",
        )
        .await
        .unwrap();
        assert_eq!(removed, 2);

        // Operator request survives; rotation requests are gone.
        assert!(
            find(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            find_many(txn.as_mut(), &[mac(2), mac(3)], SITE_EXPLORER)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[crate::sqlx_test]
    async fn delete_helpers_are_scoped_to_subsystem(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        for last in 1..=3 {
            for subsystem in [SITE_EXPLORER, DHCP] {
                upsert(
                    txn.as_mut(),
                    &upsert_input(last, subsystem, "decommissioning"),
                )
                .await
                .unwrap();
            }
        }

        assert!(delete(txn.as_mut(), mac(1), SITE_EXPLORER).await.unwrap());
        assert!(!delete(txn.as_mut(), mac(1), SITE_EXPLORER).await.unwrap());
        assert!(find(txn.as_mut(), mac(1), DHCP).await.unwrap().is_some());

        assert_eq!(
            delete_many(txn.as_mut(), &[mac(2), mac(3), mac(4)], SITE_EXPLORER)
                .await
                .unwrap(),
            2
        );
        assert!(
            find(txn.as_mut(), mac(2), SITE_EXPLORER)
                .await
                .unwrap()
                .is_none()
        );
        assert!(find(txn.as_mut(), mac(2), DHCP).await.unwrap().is_some());
    }
}
