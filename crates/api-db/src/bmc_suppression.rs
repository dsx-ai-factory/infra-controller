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

use carbide_uuid::DbTable;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::bmc_suppression::{
    BmcSuppression, BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression,
};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// Inserts or updates a suppression request for this source.
///
/// Repeated requests preserve the original request and acknowledgement
/// timestamps so retries do not restart an acknowledged handoff.
pub async fn upsert(
    txn: &mut PgConnection,
    input: &NewBmcSuppression,
) -> DatabaseResult<BmcSuppression> {
    let query = format!(
        "INSERT INTO bmc_suppressions (
        bmc_mac_address,
        subsystem,
        source,
        reason
    ) VALUES ($1, $2, $3, $4)
    ON CONFLICT (bmc_mac_address, subsystem, source) DO UPDATE SET
        reason = EXCLUDED.reason
    RETURNING {}",
        BmcSuppression::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(input.bmc_mac_address)
        .bind(input.subsystem)
        .bind(input.source)
        .bind(&input.reason)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Returns the suppression rows for the selected BMC MAC addresses in
/// `subsystem` owned by `source`. Missing MACs are simply absent from the
/// result.
pub async fn find_many(
    db: impl DbReader<'_>,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
    source: BmcSuppressionSource,
) -> DatabaseResult<Vec<BmcSuppression>> {
    let query = format!(
        "SELECT {}
    FROM bmc_suppressions
    WHERE bmc_mac_address = ANY($1) AND subsystem = $2 AND source = $3
    ORDER BY bmc_mac_address",
        BmcSuppression::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .bind(source)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Returns all suppression sources for a BMC in the selected subsystem.
pub async fn find_for_bmc(
    db: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<BmcSuppressionSource>> {
    const QUERY: &str =
        "SELECT source FROM bmc_suppressions WHERE bmc_mac_address = $1 AND subsystem = $2";
    sqlx::query_scalar(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns endpoint IPs paused by DPF provisioning, using their BMC interface identity.
pub async fn dpf_suppressed_ips(db: impl DbReader<'_>) -> DatabaseResult<Vec<std::net::IpAddr>> {
    const QUERY: &str = "SELECT DISTINCT mia.address FROM bmc_suppressions s
        JOIN machine_interfaces mi ON mi.mac_address = s.bmc_mac_address
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        WHERE s.subsystem = $1 AND s.source = $2";
    sqlx::query_scalar(QUERY)
        .bind(BmcSuppressionSubsystem::SiteExplorer)
        .bind(BmcSuppressionSource::DpfProvisioning)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns an active suppression request for this source, if one exists.
pub async fn find(
    db: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
    source: BmcSuppressionSource,
) -> DatabaseResult<Option<BmcSuppression>> {
    let query = format!(
        "SELECT {}
    FROM bmc_suppressions
    WHERE bmc_mac_address = $1 AND subsystem = $2 AND source = $3",
        BmcSuppression::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(bmc_mac_address)
        .bind(subsystem)
        .bind(source)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Returns whether decommissioning still needs a DHCP suppression acknowledgement.
///
/// A recorded DHCP acknowledgement for decommissioning satisfies the wait.
/// Otherwise, skip it only when the MAC has at least one interface, every
/// interface has a current static allocation, and none of its addresses fall
/// within a configured network prefix. NICo cannot rely on seeing DHCP for
/// these external addresses.
///
/// Check the addresses themselves: assigning another address family can move
/// a managed reservation onto `static-assignments`, and older external
/// allocations can remain on ordinary segments.
pub async fn is_dhcp_acknowledgement_pending(
    db: impl DbReader<'_>,
    mac_address: MacAddress,
) -> DatabaseResult<bool> {
    const QUERY: &str = r"
        SELECT NOT EXISTS (
            SELECT 1 FROM bmc_suppressions
            WHERE bmc_mac_address = $1 AND subsystem = $2 AND source = $3
                AND acknowledged_at IS NOT NULL
        ) AND NOT COALESCE((
            SELECT bool_and(EXISTS (
                SELECT 1 FROM machine_interface_addresses address
                WHERE address.interface_id = interface.id AND address.allocation_type = $4
            ) AND NOT EXISTS (
                SELECT 1 FROM machine_interface_addresses address
                JOIN network_prefixes prefix ON address.address <<= prefix.prefix
                WHERE address.interface_id = interface.id
            ))
            FROM machine_interfaces interface
            WHERE interface.mac_address = $1
        ), false)";

    sqlx::query_scalar(QUERY)
        .bind(mac_address)
        .bind(BmcSuppressionSubsystem::Dhcp)
        .bind(BmcSuppressionSource::Decommissioning)
        .bind(AllocationType::Static)
        .fetch_one(db)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns every BMC suppression for `subsystem`.
pub async fn find_all_by_subsystem(
    db: impl DbReader<'_>,
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<BmcSuppression>> {
    let query = format!(
        "SELECT {}
    FROM bmc_suppressions
    WHERE subsystem = $1
    ORDER BY bmc_mac_address, source",
        BmcSuppression::db_table_columns(),
    );

    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(subsystem)
        .fetch_all(db)
        .await
        .map_err(|e| DatabaseError::query(&query, e))
}

/// Acknowledges pending suppression requests for the selected BMC MAC addresses.
///
/// Every source for those MACs is acknowledged. Returns the BMC MAC addresses
/// that had at least one unacknowledged row.
pub async fn acknowledge_unacknowledged(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
) -> DatabaseResult<Vec<MacAddress>> {
    const QUERY: &str = "WITH updated AS (
        UPDATE bmc_suppressions
        SET acknowledged_at = statement_timestamp()
        WHERE bmc_mac_address = ANY($1)
            AND subsystem = $2
            AND acknowledged_at IS NULL
        RETURNING bmc_mac_address
    )
    SELECT DISTINCT bmc_mac_address FROM updated
    ORDER BY bmc_mac_address";

    sqlx::query_scalar(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Returns whether a BMC MAC is suppressed for `subsystem` by any source.
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

/// Records that `subsystem` has observed and applied suppression requests.
///
/// Every source for this MAC is acknowledged. The lookup and timestamp write
/// are atomic. The return value is `true` when at least one row exists and
/// `false` when no matching request exists. Repeated acknowledgements preserve
/// the first timestamp.
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

/// Deletes one source's suppression request for a BMC MAC.
///
/// Returns `true` when a row was removed.
pub async fn delete(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    subsystem: BmcSuppressionSubsystem,
    source: BmcSuppressionSource,
) -> DatabaseResult<bool> {
    const QUERY: &str = "DELETE FROM bmc_suppressions
        WHERE bmc_mac_address = $1 AND subsystem = $2 AND source = $3";

    sqlx::query(QUERY)
        .bind(bmc_mac_address)
        .bind(subsystem)
        .bind(source)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Deletes every source's suppression requests for a set of BMC MACs.
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

/// Deletes suppression requests owned by `source` for a set of BMC MACs.
///
/// Returns the number of rows removed.
pub async fn delete_many_for_source(
    txn: &mut PgConnection,
    bmc_mac_addresses: &[MacAddress],
    subsystem: BmcSuppressionSubsystem,
    source: BmcSuppressionSource,
) -> DatabaseResult<u64> {
    const QUERY: &str = "DELETE FROM bmc_suppressions
        WHERE bmc_mac_address = ANY($1) AND subsystem = $2 AND source = $3";

    sqlx::query(QUERY)
        .bind(bmc_mac_addresses)
        .bind(subsystem)
        .bind(source)
        .execute(txn)
        .await
        .map(|result| result.rows_affected())
        .map_err(|e| DatabaseError::query(QUERY, e))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::assert_outcome;
    use mac_address::MacAddress;
    use model::allocation_type::AllocationType;
    use model::bmc_suppression::{
        BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression,
    };

    use super::{
        acknowledge, acknowledge_unacknowledged, delete, delete_many, delete_many_for_source, find,
        find_all_by_subsystem, find_many, is_suppressed, upsert,
    };

    const SITE_EXPLORER: BmcSuppressionSubsystem = BmcSuppressionSubsystem::SiteExplorer;
    const DHCP: BmcSuppressionSubsystem = BmcSuppressionSubsystem::Dhcp;
    const DECOMMISSIONING: BmcSuppressionSource = BmcSuppressionSource::Decommissioning;
    const ROTATION: BmcSuppressionSource = BmcSuppressionSource::BmcCredentialRotation;
    const SOURCE_MIGRATION: &str =
        include_str!("../migrations/20260909213700_bmc_suppressions_source.sql");

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
            source: DECOMMISSIONING,
            reason: reason.to_string(),
        }
    }

    #[crate::sqlx_test]
    async fn dhcp_wait_uses_current_interface_assignments(pool: sqlx::PgPool) {
        struct Interface<'a> {
            segment: uuid::Uuid,
            addresses: &'a [(std::net::IpAddr, AllocationType)],
            has_dhcp_history: bool,
        }

        let mut txn = pool.begin().await.unwrap();
        let mut segments = Vec::new();
        for name in [
            "dhcp-wait-test",
            crate::network_segment::STATIC_ASSIGNMENTS_SEGMENT_NAME,
        ] {
            let id: uuid::Uuid = sqlx::query_scalar(
                "INSERT INTO network_segments (name, version, network_segment_type)
                 VALUES ($1, 'V1-T0', 'underlay') RETURNING id",
            )
            .bind(name)
            .fetch_one(txn.as_mut())
            .await
            .unwrap();
            segments.push(id);
        }
        let [managed, external]: [uuid::Uuid; 2] = segments.try_into().unwrap();
        sqlx::query(
            "INSERT INTO network_prefixes (segment_id, prefix)
             VALUES ($1, '192.0.2.0/24'), ($1, '2001:db8::/64')",
        )
        .bind(managed)
        .execute(txn.as_mut())
        .await
        .unwrap();

        // Expected inventory alone must not exempt a managed reservation.
        // Legacy external assignments can also exist on ordinary segments.
        sqlx::query(
            "INSERT INTO expected_machines
                (serial_number, bmc_mac_address, bmc_username, bmc_password, bmc_ip_address)
             VALUES
                ('managed-reservation', '02:00:00:00:00:01', 'root', 'password', '192.0.2.2'),
                ('legacy-external', '02:00:00:00:00:06', 'root', 'password', '198.51.100.12')",
        )
        .execute(txn.as_mut())
        .await
        .unwrap();

        for (scenario, last, interfaces, pending) in [
            (
                "managed reservation before its first DHCP request",
                1,
                &[Interface {
                    segment: managed,
                    addresses: &[("192.0.2.2".parse().unwrap(), AllocationType::Static)],
                    has_dhcp_history: false,
                }][..],
                true,
            ),
            (
                "external assignment with old DHCP history and no expected inventory",
                2,
                &[Interface {
                    segment: external,
                    addresses: &[("198.51.100.4".parse().unwrap(), AllocationType::Static)],
                    has_dhcp_history: true,
                }][..],
                false,
            ),
            (
                "removed external assignment awaiting DHCP reconciliation",
                3,
                &[Interface {
                    segment: external,
                    addresses: &[],
                    has_dhcp_history: false,
                }][..],
                true,
            ),
            (
                "external and managed interfaces sharing a MAC",
                4,
                &[
                    Interface {
                        segment: external,
                        addresses: &[("198.51.100.8".parse().unwrap(), AllocationType::Static)],
                        has_dhcp_history: false,
                    },
                    Interface {
                        segment: managed,
                        addresses: &[("192.0.2.9".parse().unwrap(), AllocationType::Static)],
                        has_dhcp_history: false,
                    },
                ][..],
                true,
            ),
            (
                "removed external assignment with retained DHCP outside managed prefixes",
                5,
                &[Interface {
                    segment: external,
                    addresses: &[("198.51.100.10".parse().unwrap(), AllocationType::Dhcp)],
                    has_dhcp_history: true,
                }][..],
                true,
            ),
            (
                "legacy external assignment on an ordinary segment",
                6,
                &[Interface {
                    segment: managed,
                    addresses: &[("198.51.100.12".parse().unwrap(), AllocationType::Static)],
                    has_dhcp_history: false,
                }][..],
                false,
            ),
            (
                "external static IPv4 with managed DHCP IPv6 on one interface",
                7,
                &[Interface {
                    segment: external,
                    addresses: &[
                        ("198.51.100.14".parse().unwrap(), AllocationType::Static),
                        ("2001:db8::10".parse().unwrap(), AllocationType::Dhcp),
                    ],
                    has_dhcp_history: true,
                }][..],
                true,
            ),
            (
                "external and addressless interfaces sharing a MAC",
                8,
                &[
                    Interface {
                        segment: external,
                        addresses: &[("198.51.100.16".parse().unwrap(), AllocationType::Static)],
                        has_dhcp_history: false,
                    },
                    Interface {
                        segment: managed,
                        addresses: &[],
                        has_dhcp_history: false,
                    },
                ][..],
                true,
            ),
        ] {
            upsert(txn.as_mut(), &upsert_input(last, DHCP, scenario))
                .await
                .unwrap();
            for interface in interfaces {
                let interface_id = sqlx::query_scalar(
                    "INSERT INTO machine_interfaces
                        (segment_id, mac_address, primary_interface, hostname, last_dhcp)
                     VALUES ($1, $2, false, 'dhcp-wait-test',
                        CASE WHEN $3 THEN now() ELSE NULL END)
                     RETURNING id",
                )
                .bind(interface.segment)
                .bind(mac(last))
                .bind(interface.has_dhcp_history)
                .fetch_one(txn.as_mut())
                .await
                .unwrap();
                for &(address, allocation_type) in interface.addresses {
                    crate::machine_interface_address::insert(
                        txn.as_mut(),
                        interface_id,
                        address,
                        allocation_type,
                    )
                    .await
                    .unwrap();
                }
            }
            assert_outcome(
                super::is_dhcp_acknowledgement_pending(txn.as_mut(), mac(last))
                    .await
                    .map_err(|error| error.to_string()),
                Yields(pending),
                scenario,
            );
        }
    }

    #[crate::sqlx_test]
    async fn dhcp_wait_uses_decommissioning_acknowledgement(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        for (scenario, last, subsystem, source, acknowledged, pending) in [
            (
                "unacknowledged request",
                1,
                DHCP,
                DECOMMISSIONING,
                false,
                true,
            ),
            (
                "acknowledged request",
                2,
                DHCP,
                DECOMMISSIONING,
                true,
                false,
            ),
            (
                "other subsystem",
                3,
                SITE_EXPLORER,
                DECOMMISSIONING,
                true,
                true,
            ),
            ("other source", 4, DHCP, ROTATION, true, true),
        ] {
            let mut input = upsert_input(last, subsystem, scenario);
            input.source = source;
            upsert(txn.as_mut(), &input).await.unwrap();
            if acknowledged {
                acknowledge(txn.as_mut(), mac(last), subsystem)
                    .await
                    .unwrap();
            }
            assert_eq!(
                super::is_dhcp_acknowledgement_pending(txn.as_mut(), mac(last))
                    .await
                    .unwrap(),
                pending,
                "{scenario}",
            );
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
    async fn sources_are_independent(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();

        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        upsert(
            txn.as_mut(),
            &NewBmcSuppression {
                bmc_mac_address: mac(1),
                subsystem: SITE_EXPLORER,
                source: ROTATION,
                reason: "bmc_credential_rotation".to_string(),
            },
        )
        .await
        .unwrap();

        assert!(
            is_suppressed(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
        assert_eq!(
            delete_many_for_source(txn.as_mut(), &[mac(1)], SITE_EXPLORER, ROTATION)
                .await
                .unwrap(),
            1
        );
        assert!(
            find(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            is_suppressed(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
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
                find(txn.as_mut(), mac(last), other_subsystem, DECOMMISSIONING)
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
                find(txn.as_mut(), mac(last), subsystem, DECOMMISSIONING)
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
        upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "decommissioning"),
        )
        .await
        .unwrap();
        upsert(
            txn.as_mut(),
            &NewBmcSuppression {
                bmc_mac_address: mac(1),
                subsystem: SITE_EXPLORER,
                source: ROTATION,
                reason: "bmc_credential_rotation".to_string(),
            },
        )
        .await
        .unwrap();
        for (last, subsystem) in [(2, DHCP), (3, SITE_EXPLORER)] {
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
            find(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
                .unwrap()
                .acknowledged_at
                .is_some()
        );
        assert!(
            find(txn.as_mut(), mac(1), SITE_EXPLORER, ROTATION)
                .await
                .unwrap()
                .unwrap()
                .acknowledged_at
                .is_some()
        );
        assert!(
            find(txn.as_mut(), mac(2), DHCP, DECOMMISSIONING)
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
        let initial = find(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
            .await
            .unwrap()
            .unwrap();

        assert!(
            acknowledge(txn.as_mut(), mac(1), SITE_EXPLORER)
                .await
                .unwrap()
        );
        let repeated = find(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
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
        assert_eq!(retried.bmc_mac_address, mac(1));
        assert_eq!(retried.subsystem, SITE_EXPLORER);
        assert_eq!(retried.source, DECOMMISSIONING);
        assert_eq!(retried.reason, "decommissioning retry");
        assert_eq!(retried.requested_at, initial.requested_at);
        assert_eq!(retried.acknowledged_at, initial.acknowledged_at);

        assert!(
            delete(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
        );
        let recreated = upsert(
            txn.as_mut(),
            &upsert_input(1, SITE_EXPLORER, "new decommissioning request"),
        )
        .await
        .unwrap();
        assert!(recreated.acknowledged_at.is_none());
    }

    #[crate::sqlx_test]
    async fn find_many_returns_selected_rows(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        for (last, subsystem) in [(1, SITE_EXPLORER), (2, SITE_EXPLORER), (3, DHCP)] {
            upsert(txn.as_mut(), &upsert_input(last, subsystem, "reason"))
                .await
                .unwrap();
        }

        let found = find_many(
            txn.as_mut(),
            &[mac(1), mac(2), mac(3)],
            SITE_EXPLORER,
            DECOMMISSIONING,
        )
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

        assert!(
            delete(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
        );
        assert!(
            !delete(txn.as_mut(), mac(1), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
        );
        assert!(
            find(txn.as_mut(), mac(1), DHCP, DECOMMISSIONING)
                .await
                .unwrap()
                .is_some()
        );

        assert_eq!(
            delete_many(txn.as_mut(), &[mac(2), mac(3), mac(4)], SITE_EXPLORER)
                .await
                .unwrap(),
            2
        );
        assert!(
            find(txn.as_mut(), mac(2), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            find(txn.as_mut(), mac(2), DHCP, DECOMMISSIONING)
                .await
                .unwrap()
                .is_some()
        );
    }

    // sqlx_test applies every migration on an empty table, which never runs
    // the reason backfill. Rewind to the pre-source schema first.
    #[crate::sqlx_test]
    async fn source_migration_backfills_from_reason_and_requires_source(pool: sqlx::PgPool) {
        sqlx::raw_sql(
            "ALTER TABLE bmc_suppressions
                 DROP CONSTRAINT bmc_suppressions_pkey;
             ALTER TABLE bmc_suppressions
                 DROP CONSTRAINT bmc_suppressions_source_check;
             ALTER TABLE bmc_suppressions
                 DROP COLUMN source;
             ALTER TABLE bmc_suppressions
                 ADD CONSTRAINT bmc_suppressions_pkey
                     PRIMARY KEY (bmc_mac_address, subsystem);",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO bmc_suppressions (bmc_mac_address, subsystem, reason)
             VALUES
                ($1, 'site_explorer', 'bmc_credential_rotation'),
                ($2, 'site_explorer', 'factory_reset_bmc'),
                ($3, 'site_explorer', 'managed host x is being decommissioned')",
        )
        .bind(mac(1))
        .bind(mac(2))
        .bind(mac(3))
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(SOURCE_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        let rows: Vec<(MacAddress, BmcSuppressionSource)> = sqlx::query_as(
            "SELECT bmc_mac_address, source FROM bmc_suppressions
             ORDER BY bmc_mac_address",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (mac(1), ROTATION),
                (mac(2), BmcSuppressionSource::FactoryResetBmc),
                (mac(3), DECOMMISSIONING),
            ]
        );

        let (is_nullable, column_default): (String, Option<String>) = sqlx::query_as(
            "SELECT is_nullable, column_default
             FROM information_schema.columns
             WHERE table_name = 'bmc_suppressions' AND column_name = 'source'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(is_nullable, "NO");
        assert_eq!(column_default, None);

        sqlx::query(
            "INSERT INTO bmc_suppressions (bmc_mac_address, subsystem, reason)
             VALUES ($1, 'site_explorer', 'omitted source')",
        )
        .bind(mac(4))
        .execute(&pool)
        .await
        .unwrap_err();

        let mut txn = pool.begin().await.unwrap();
        upsert(
            txn.as_mut(),
            &NewBmcSuppression {
                bmc_mac_address: mac(3),
                subsystem: SITE_EXPLORER,
                source: ROTATION,
                reason: "bmc_credential_rotation".to_string(),
            },
        )
        .await
        .unwrap();
        assert!(
            find(txn.as_mut(), mac(3), SITE_EXPLORER, DECOMMISSIONING)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            find(txn.as_mut(), mac(3), SITE_EXPLORER, ROTATION)
                .await
                .unwrap()
                .is_some()
        );
    }
    #[crate::sqlx_test]
    async fn dpf_source_migration_preserves_existing_suppressions(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        sqlx::raw_sql(
            "ALTER TABLE bmc_suppressions DROP CONSTRAINT bmc_suppressions_source_check;
            ALTER TABLE bmc_suppressions ADD CONSTRAINT bmc_suppressions_source_check
            CHECK (source IN ('decommissioning', 'bmc_credential_rotation', 'factory_reset_bmc'));",
        )
        .execute(conn.as_mut())
        .await
        .unwrap();
        for (last, source) in [
            (1, DECOMMISSIONING),
            (2, ROTATION),
            (3, BmcSuppressionSource::FactoryResetBmc),
        ] {
            let mut input = upsert_input(last, SITE_EXPLORER, "active workflow");
            input.source = source;
            upsert(conn.as_mut(), &input).await.unwrap();
        }
        let before = find_all_by_subsystem(conn.as_mut(), SITE_EXPLORER)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/20261019084224_dpf_provisioning_suppression.sql"
        ))
        .execute(conn.as_mut())
        .await
        .unwrap();
        assert_eq!(
            find_all_by_subsystem(conn.as_mut(), SITE_EXPLORER)
                .await
                .unwrap(),
            before
        );
        let mut input = upsert_input(4, SITE_EXPLORER, "DPF provisioning");
        input.source = BmcSuppressionSource::DpfProvisioning;
        upsert(conn.as_mut(), &input).await.unwrap();
        super::delete(conn.as_mut(), mac(4), SITE_EXPLORER, input.source)
            .await
            .unwrap();
        assert_eq!(
            find_all_by_subsystem(conn.as_mut(), SITE_EXPLORER)
                .await
                .unwrap(),
            before
        );
    }
}
