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

//! Persistence for deterministic DPU underlay loopback reservations.
//!
//! Reservations are a child of `expected_machines` keyed by the DPU OS/admin
//! MAC address. The table owns the site-wide uniqueness of MACs and
//! per-family loopback addresses (see the `20260914183000` migration), so the
//! functions here map those constraint violations to precondition failures for
//! the rare race that beats the handler's up-front validation.

use std::net::IpAddr;

use mac_address::MacAddress;
use model::expected_machine::DpuLoopbackReservation;
use sqlx::{PgConnection, QueryBuilder, Row};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const SQL_VIOLATION_DUPLICATE_MAC: &str = "expected_dpu_loopback_reservations_dpu_mac_address_key";
const SQL_VIOLATION_DUPLICATE_IPV4: &str = "expected_dpu_loopback_reservations_loopback_ipv4_key";
const SQL_VIOLATION_DUPLICATE_IPV6: &str = "expected_dpu_loopback_reservations_loopback_ipv6_key";

fn reservation_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<DpuLoopbackReservation, sqlx::Error> {
    let dpu_mac_address: MacAddress = row.try_get("dpu_mac_address")?;
    // The table's family CHECK constraints guarantee each column holds its own
    // address family. Surface any violation as a decode error rather than
    // silently coercing a wrong-family value to `None`, which would hydrate an
    // addressless reservation and drop the operator's intended allocation.
    let loopback_ipv4 = match row.try_get::<Option<IpAddr>, _>("loopback_ipv4")? {
        None => None,
        Some(IpAddr::V4(v4)) => Some(v4),
        Some(IpAddr::V6(addr)) => {
            return Err(sqlx::Error::Decode(
                format!("loopback_ipv4 holds a non-IPv4 address: {addr}").into(),
            ));
        }
    };
    let loopback_ipv6 = match row.try_get::<Option<IpAddr>, _>("loopback_ipv6")? {
        None => None,
        Some(IpAddr::V6(v6)) => Some(v6),
        Some(IpAddr::V4(addr)) => {
            return Err(sqlx::Error::Decode(
                format!("loopback_ipv6 holds a non-IPv6 address: {addr}").into(),
            ));
        }
    };
    Ok(DpuLoopbackReservation {
        dpu_mac_address,
        loopback_ipv4,
        loopback_ipv6,
    })
}

/// Reservations declared for one host expected machine, ordered by MAC.
pub async fn find_for_machine(
    db: impl DbReader<'_>,
    host_bmc_mac: MacAddress,
) -> DatabaseResult<Vec<DpuLoopbackReservation>> {
    let sql = "SELECT dpu_mac_address, loopback_ipv4, loopback_ipv6 \
               FROM expected_dpu_loopback_reservations \
               WHERE bmc_mac_address = $1 \
               ORDER BY dpu_mac_address";
    let rows = sqlx::query(sql)
        .bind(host_bmc_mac)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;
    rows.iter()
        .map(reservation_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| DatabaseError::query(sql, err))
}

/// The reservation for one DPU OS/admin MAC, locked for the creating
/// transaction.
///
/// DPU creation reads the operator's intent through this so the row it allocates
/// from cannot be moved to another DPU or cleared before the creating
/// transaction commits: `FOR UPDATE` makes a concurrent reservation writer
/// (`replace_for_machine` or the cascade behind an expected-machine
/// delete) block until creation finishes, and vice versa. An absent row locks
/// nothing, which is correct -- a reservation declared after creation only
/// affects the next ingestion.
///
/// Call this at the same point the automatic path would read intent, before the
/// loopback-pool row locks and any admin-segment locks, so the lock order the
/// creation transaction already establishes is preserved.
pub async fn find_by_dpu_mac_for_update(
    txn: &mut PgConnection,
    dpu_mac_address: MacAddress,
) -> DatabaseResult<Option<DpuLoopbackReservation>> {
    lock_inventory(txn).await?;
    let sql = "SELECT dpu_mac_address, loopback_ipv4, loopback_ipv6 \
               FROM expected_dpu_loopback_reservations \
               WHERE dpu_mac_address = $1 \
               FOR UPDATE";
    let row = sqlx::query(sql)
        .bind(dpu_mac_address)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;
    row.as_ref()
        .map(reservation_from_row)
        .transpose()
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Lock a host's existing reservation rows for the writing transaction.
///
/// A reservation writer (add / update / replace-all / patch) calls this before
/// it validates a replacement set against the live resource pool, so it
/// serializes with any DPU creation that has locked one of those rows through
/// [`find_by_dpu_mac_for_update`]. Without it, a same-host "transfer address
/// X from DPU A to DPU B" could validate X as free while a creating transaction
/// is mid-flight allocating X to A, then persist B -> X after that creation
/// commits. Holding the row lock forces the writer to wait and re-observe the
/// committed allocation, where the owner-aware pool check rejects the move.
///
/// A host with no reservations locks nothing, which is correct: there is no
/// prior intent for a creation to have read.
pub async fn lock_for_machine(
    txn: &mut PgConnection,
    host_bmc_mac: MacAddress,
) -> DatabaseResult<()> {
    lock_inventory(txn).await?;
    let sql = "SELECT 1 FROM expected_dpu_loopback_reservations \
               WHERE bmc_mac_address = $1 \
               FOR UPDATE";
    sqlx::query(sql)
        .bind(host_bmc_mac)
        .fetch_all(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;
    Ok(())
}

/// Compatible with ordinary writes/creation, but excludes replace-all's parent
/// delete/reinsert until the creating transaction has consumed its intent.
async fn lock_inventory(txn: &mut PgConnection) -> DatabaseResult<()> {
    let sql = "LOCK TABLE expected_machines IN ROW EXCLUSIVE MODE";
    sqlx::query(sql)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(sql, error))?;
    Ok(())
}

/// Replace the reservations for one host expected machine.
///
/// Removes omitted MACs and upserts retained/new MACs in `reservations`. The
/// caller runs this inside the same transaction as the expected-machine write
/// so the two move atomically. An empty slice clears the host's reservations.
pub async fn replace_for_machine(
    txn: &mut PgConnection,
    host_bmc_mac: MacAddress,
    reservations: &[DpuLoopbackReservation],
) -> DatabaseResult<()> {
    lock_inventory(txn).await?;
    let delete = "DELETE FROM expected_dpu_loopback_reservations WHERE bmc_mac_address = $1 AND NOT (dpu_mac_address = ANY($2))";
    sqlx::query(delete)
        .bind(host_bmc_mac)
        .bind(
            reservations
                .iter()
                .map(|r| r.dpu_mac_address)
                .collect::<Vec<_>>(),
        )
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(delete, err))?;

    if reservations.is_empty() {
        return Ok(());
    }

    // A replacement may swap two free addresses between retained MACs. Defer
    // only address uniqueness for this statement, then check the final set.
    let defer = "SET CONSTRAINTS expected_dpu_loopback_reservations_loopback_ipv4_key, expected_dpu_loopback_reservations_loopback_ipv6_key DEFERRED";
    sqlx::query(defer)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(defer, error))?;
    let mut builder = QueryBuilder::new(
        "INSERT INTO expected_dpu_loopback_reservations \
         (bmc_mac_address, dpu_mac_address, loopback_ipv4, loopback_ipv6) ",
    );
    builder.push_values(reservations, |mut b, reservation| {
        b.push_bind(host_bmc_mac)
            .push_bind(reservation.dpu_mac_address)
            .push_bind(reservation.loopback_ipv4.map(IpAddr::V4))
            .push_bind(reservation.loopback_ipv6.map(IpAddr::V6));
    });

    // Keep existing selector rows in place: a creator waiting on FOR UPDATE
    // must see the new intent, not miss a replacement DELETE/INSERT snapshot.
    builder.push(" ON CONFLICT (dpu_mac_address) DO UPDATE SET loopback_ipv4 = EXCLUDED.loopback_ipv4, loopback_ipv6 = EXCLUDED.loopback_ipv6 WHERE expected_dpu_loopback_reservations.bmc_mac_address = EXCLUDED.bmc_mac_address");
    let result = builder
        .build()
        .execute(&mut *txn)
        .await
        .map_err(map_reservation_insert_error)?;
    let check = "SET CONSTRAINTS expected_dpu_loopback_reservations_loopback_ipv4_key, expected_dpu_loopback_reservations_loopback_ipv6_key IMMEDIATE";
    sqlx::query(check)
        .execute(&mut *txn)
        .await
        .map_err(map_reservation_insert_error)?;
    if result.rows_affected() != reservations.len() as u64 {
        return Err(DatabaseError::FailedPrecondition(
            "DPU OS/admin MAC is already declared by another reservation".to_string(),
        ));
    }
    Ok(())
}

/// Translate the child table's uniqueness violations into precondition failures
/// the API surfaces as `FailedPrecondition`. The handler validates first, so
/// this only fires for a writer that raced past that check.
fn map_reservation_insert_error(err: sqlx::Error) -> DatabaseError {
    if let sqlx::Error::Database(db_err) = &err {
        match db_err.constraint() {
            Some(SQL_VIOLATION_DUPLICATE_MAC) => {
                return DatabaseError::FailedPrecondition(
                    "DPU OS/admin MAC is already declared by another reservation".to_string(),
                );
            }
            Some(SQL_VIOLATION_DUPLICATE_IPV4) => {
                return DatabaseError::FailedPrecondition(
                    "IPv4 loopback address is already reserved for another DPU".to_string(),
                );
            }
            Some(SQL_VIOLATION_DUPLICATE_IPV6) => {
                return DatabaseError::FailedPrecondition(
                    "IPv6 loopback address is already reserved for another DPU".to_string(),
                );
            }
            _ => {}
        }
    }
    DatabaseError::query("INSERT INTO expected_dpu_loopback_reservations", err)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
    use model::metadata::Metadata;

    use super::*;

    /// Minimal host expected machine carrying the given reservations. The child
    /// rows are written by `expected_machine::create`, which delegates to
    /// [`replace_for_machine`].
    fn expected_machine(mac: &str, reservations: Vec<DpuLoopbackReservation>) -> ExpectedMachine {
        ExpectedMachine {
            id: None,
            bmc_mac_address: mac.parse().unwrap(),
            data: ExpectedMachineData {
                bmc_username: "user".to_string(),
                bmc_password: "pass".to_string(),
                serial_number: format!("serial-{mac}"),
                fallback_dpu_serial_numbers: vec![],
                sku_id: None,
                metadata: Metadata::new_with_default_name(),
                interfaces: reservations
                    .iter()
                    .flat_map(DpuLoopbackReservation::interfaces)
                    .collect(),
                rack_id: None,
                default_pause_ingestion_and_poweron: None,
                dpf_enabled: None,
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
                replace_dpu_loopbacks: true,
                server_name: None,
            },
        }
    }

    fn reservation(mac: &str, v4: Option<&str>, v6: Option<&str>) -> DpuLoopbackReservation {
        DpuLoopbackReservation {
            dpu_mac_address: mac.parse().unwrap(),
            loopback_ipv4: v4.map(|s| s.parse::<Ipv4Addr>().unwrap()),
            loopback_ipv6: v6.map(|s| s.parse::<Ipv6Addr>().unwrap()),
        }
    }

    /// A host's reservations are persisted with its row, hydrated back in MAC
    /// order, resolvable globally by DPU OS/admin MAC, and cleared by an empty replace.
    #[crate::sqlx_test]
    async fn reservations_persist_hydrate_and_clear(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mac = "aa:bb:cc:dd:ee:01";
        let machine = expected_machine(
            mac,
            vec![
                reservation("02:00:00:00:00:02", Some("192.0.2.11"), None),
                reservation("02:00:00:00:00:01", None, Some("2001:db8::1")),
            ],
        );

        let mut txn = pool.begin().await?;
        crate::expected_machine::create(&mut txn, machine).await?;
        txn.commit().await?;

        // A read hydrates every reservation, ordered by MAC.
        let mut txn = pool.begin().await?;
        let hydrated = find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?).await?;
        assert_eq!(
            hydrated
                .iter()
                .map(|r| r.dpu_mac_address.to_string())
                .collect::<Vec<_>>(),
            ["02:00:00:00:00:01", "02:00:00:00:00:02"],
        );

        // Direct discovery resolves a reservation by its globally unique MAC.
        let by_mac = find_by_dpu_mac_for_update(txn.as_mut(), "02:00:00:00:00:02".parse()?)
            .await?
            .unwrap();
        assert_eq!(by_mac.loopback_ipv4, Some("192.0.2.11".parse()?));
        assert!(
            find_by_dpu_mac_for_update(txn.as_mut(), "02:00:00:00:00:07".parse()?)
                .await?
                .is_none()
        );

        // An empty replace clears the host's reservations.
        replace_for_machine(txn.as_mut(), mac.parse()?, &[]).await?;
        assert!(
            find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?)
                .await?
                .is_empty()
        );
        txn.rollback().await?;
        Ok(())
    }

    /// `expected_machine::update` preserves stored reservations when loopback intent
    /// is omitted and clears them on an explicit replacement with no loopbacks -- the
    /// compatibility contract an older client relies on during a mixed-version
    /// read-modify-write.
    #[crate::sqlx_test]
    async fn update_preserves_omitted_and_clears_empty_reservations(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mac = "aa:bb:cc:dd:ee:01";
        let mut txn = pool.begin().await?;
        crate::expected_machine::create(
            &mut txn,
            expected_machine(
                mac,
                vec![reservation("02:00:00:00:00:01", Some("192.0.2.11"), None)],
            ),
        )
        .await?;

        // Omitted loopback intent leaves the stored reservation intact.
        let mut preserving = expected_machine(mac, vec![]);
        preserving.data.replace_dpu_loopbacks = false;
        crate::expected_machine::update(&mut txn, &preserving).await?;
        let after_preserve = find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?).await?;
        assert_eq!(
            after_preserve
                .iter()
                .map(|r| r.dpu_mac_address.to_string())
                .collect::<Vec<_>>(),
            ["02:00:00:00:00:01"],
        );

        // An explicit empty list clears the stored reservations.
        let mut clearing = expected_machine(mac, vec![]);
        clearing.data.replace_dpu_loopbacks = true;
        crate::expected_machine::update(&mut txn, &clearing).await?;
        assert!(
            find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?)
                .await?
                .is_empty()
        );

        txn.rollback().await?;
        Ok(())
    }

    /// When `expected_machine::update` selects a row by `id`, reservation
    /// replacement must key on that row's own BMC MAC, not the caller-supplied
    /// MAC. A direct caller passing host A's `id` with host B's MAC must update
    /// host A's reservations and leave host B's untouched, since the FK only
    /// requires the supplied MAC to identify *some* host.
    #[crate::sqlx_test]
    async fn update_by_id_keys_reservations_on_the_matched_rows_mac(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mac_a = "aa:bb:cc:dd:ee:01";
        let mac_b = "aa:bb:cc:dd:ee:02";

        let mut txn = pool.begin().await?;
        let host_a = crate::expected_machine::create(
            &mut txn,
            expected_machine(
                mac_a,
                vec![reservation("02:00:00:00:00:01", Some("192.0.2.11"), None)],
            ),
        )
        .await?;
        crate::expected_machine::create(
            &mut txn,
            expected_machine(
                mac_b,
                vec![reservation("02:00:00:00:00:02", Some("192.0.2.22"), None)],
            ),
        )
        .await?;

        // Select host A by id, but supply host B's MAC and a new reservation
        // set. The replacement must land on host A. (Use a distinct serial so
        // the parent UPDATE does not collide with host B's serial.)
        let mut update = expected_machine(
            mac_b,
            vec![reservation("02:00:00:00:00:03", Some("192.0.2.33"), None)],
        );
        update.id = host_a.id;
        update.data.serial_number = "serial-a-updated".to_string();
        crate::expected_machine::update(&mut txn, &update).await?;

        // Host B's reservation is untouched: its MAC was never the write target.
        assert_eq!(
            find_for_machine(txn.as_mut(), mac_b.parse::<MacAddress>()?)
                .await?
                .iter()
                .map(|r| r.dpu_mac_address.to_string())
                .collect::<Vec<_>>(),
            ["02:00:00:00:00:02"],
        );

        // Host A received the new set, keyed on its own MAC.
        assert_eq!(
            find_for_machine(txn.as_mut(), mac_a.parse::<MacAddress>()?)
                .await?
                .iter()
                .map(|r| r.dpu_mac_address.to_string())
                .collect::<Vec<_>>(),
            ["02:00:00:00:00:03"],
        );

        txn.rollback().await?;
        Ok(())
    }

    /// The site-wide MAC uniqueness constraint rejects the same DPU OS/admin MAC
    /// declared on a second host, surfaced as a precondition failure.
    #[crate::sqlx_test]
    async fn duplicate_mac_across_hosts_is_rejected(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = expected_machine(
            "aa:bb:cc:dd:ee:01",
            vec![reservation("02:00:00:00:00:04", Some("192.0.2.11"), None)],
        );
        let second = expected_machine(
            "aa:bb:cc:dd:ee:02",
            vec![reservation("02:00:00:00:00:04", Some("192.0.2.12"), None)],
        );

        let mut txn = pool.begin().await?;
        crate::expected_machine::create(&mut txn, first).await?;
        let error = crate::expected_machine::create(&mut txn, second)
            .await
            .map(|_| ())
            .expect_err("a DPU OS/admin MAC declared on another host must be rejected");
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
        txn.rollback().await?;
        Ok(())
    }

    /// The partial per-family uniqueness constraint rejects the same loopback
    /// address reserved for two different DPUs.
    #[crate::sqlx_test]
    async fn duplicate_loopback_address_is_rejected(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine = expected_machine(
            "aa:bb:cc:dd:ee:01",
            vec![
                reservation("02:00:00:00:00:01", Some("192.0.2.11"), None),
                reservation("02:00:00:00:00:02", Some("192.0.2.11"), None),
            ],
        );

        let mut txn = pool.begin().await?;
        let error = crate::expected_machine::create(&mut txn, machine)
            .await
            .map(|_| ())
            .expect_err("one loopback address cannot belong to two DPUs");
        assert!(matches!(error, DatabaseError::FailedPrecondition(_)));
        txn.rollback().await?;
        Ok(())
    }

    /// The table's family CHECK constraints reject an address stored in the
    /// wrong column, so the reader never has to reconcile a wrong-family value.
    /// The Rust write path always binds the correct family, so this exercises a
    /// raw insert to prove the database, not the caller, owns the invariant.
    #[crate::sqlx_test]
    async fn wrong_family_address_is_rejected(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mac = "aa:bb:cc:dd:ee:01";
        let mut txn = pool.begin().await?;
        // A host row is required for the reservation's foreign key.
        crate::expected_machine::create(&mut txn, expected_machine(mac, vec![])).await?;

        let insert = "INSERT INTO expected_dpu_loopback_reservations \
                      (bmc_mac_address, dpu_mac_address, loopback_ipv4, loopback_ipv6) \
                      VALUES ($1, $2, $3, $4)";

        // An IPv6 address in the IPv4 column violates family(loopback_ipv4) = 4.
        let v6_in_v4 = sqlx::query(insert)
            .bind(mac.parse::<MacAddress>()?)
            .bind("02:00:00:00:00:05".parse::<MacAddress>()?)
            .bind(IpAddr::V6("2001:db8::1".parse()?))
            .bind(Option::<IpAddr>::None)
            .execute(txn.as_mut())
            .await;
        assert!(
            matches!(v6_in_v4, Err(sqlx::Error::Database(_))),
            "an IPv6 address in the IPv4 column must be rejected",
        );

        txn.rollback().await?;

        // An IPv4 address in the IPv6 column violates family(loopback_ipv6) = 6.
        let mut txn = pool.begin().await?;
        crate::expected_machine::create(&mut txn, expected_machine(mac, vec![])).await?;
        let v4_in_v6 = sqlx::query(insert)
            .bind(mac.parse::<MacAddress>()?)
            .bind("02:00:00:00:00:06".parse::<MacAddress>()?)
            .bind(Option::<IpAddr>::None)
            .bind(IpAddr::V4("192.0.2.11".parse()?))
            .execute(txn.as_mut())
            .await;
        assert!(
            matches!(v4_in_v6, Err(sqlx::Error::Database(_))),
            "an IPv4 address in the IPv6 column must be rejected",
        );

        txn.rollback().await?;
        Ok(())
    }
    #[crate::sqlx_test]
    async fn replacement_can_swap_addresses_between_retained_macs(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host = "aa:bb:cc:dd:ee:01";
        let mut txn = pool.begin().await?;
        crate::expected_machine::create(
            &mut txn,
            expected_machine(
                host,
                vec![
                    reservation(
                        "02:00:00:00:00:01",
                        Some("192.0.2.10"),
                        Some("2001:db8::10"),
                    ),
                    reservation(
                        "02:00:00:00:00:02",
                        Some("192.0.2.11"),
                        Some("2001:db8::11"),
                    ),
                ],
            ),
        )
        .await?;
        let swapped = vec![
            reservation(
                "02:00:00:00:00:01",
                Some("192.0.2.11"),
                Some("2001:db8::11"),
            ),
            reservation(
                "02:00:00:00:00:02",
                Some("192.0.2.10"),
                Some("2001:db8::10"),
            ),
        ];
        replace_for_machine(&mut txn, host.parse()?, &swapped).await?;
        assert_eq!(find_for_machine(&mut *txn, host.parse()?).await?, swapped);
        txn.commit().await?;
        Ok(())
    }

    /// A creator that waits for an edit must observe the new value for the same
    /// MAC. DELETE/INSERT replacement would let its statement miss that row.
    #[crate::sqlx_test]
    async fn creation_waiting_for_reservation_update_reads_new_intent(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host = "aa:bb:cc:dd:ee:01";
        let mac: MacAddress = "02:00:00:00:00:01".parse()?;
        let mut setup = pool.begin().await?;
        crate::expected_machine::create(
            &mut setup,
            expected_machine(
                host,
                vec![reservation(&mac.to_string(), Some("192.0.2.10"), None)],
            ),
        )
        .await?;
        setup.commit().await?;

        for (replace_all, address) in [(false, "192.0.2.11"), (true, "192.0.2.12")] {
            let mut writer = pool.begin().await?;
            let query_fragment = if replace_all {
                crate::expected_machine::find_all_for_replace(&mut writer).await?;
                "LOCK TABLE expected_machines"
            } else {
                lock_for_machine(&mut writer, host.parse()?).await?;
                "expected_dpu_loopback_reservations"
            };
            let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(writer.as_mut())
                .await?;
            let mut creator = pool.begin().await?;
            let lookup = find_by_dpu_mac_for_update(creator.as_mut(), mac);
            tokio::pin!(lookup);
            tokio::select! {
                result = &mut lookup => panic!("creation did not wait for reservation writer: {result:?}"),
                _ = crate::test_support::postgres::wait_for_blocked_query(
                    &pool, blocker_pid, query_fragment,
                ) => {}
            }
            let next = reservation(&mac.to_string(), Some(address), None);
            if replace_all {
                crate::expected_machine::clear(&mut writer).await?;
                crate::expected_machine::create(&mut writer, expected_machine(host, vec![next]))
                    .await?;
            } else {
                replace_for_machine(&mut writer, host.parse()?, &[next]).await?;
            }
            writer.commit().await?;
            let actual = lookup
                .await?
                .expect("replacement remains visible to waiting creator");
            assert_eq!(actual.loopback_ipv4, Some(address.parse()?));
        }
        Ok(())
    }
}
