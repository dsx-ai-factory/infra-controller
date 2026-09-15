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
//! Reservations are a child of `expected_machines` keyed by the DPU pairing
//! serial number. The table owns the site-wide uniqueness of serials and
//! per-family loopback addresses (see the `20260914183000` migration), so the
//! functions here map those constraint violations to precondition failures for
//! the rare race that beats the handler's up-front validation.

use std::net::IpAddr;

use mac_address::MacAddress;
use model::expected_machine::DpuLoopbackReservation;
use sqlx::{PgConnection, QueryBuilder, Row};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const SQL_VIOLATION_DUPLICATE_SERIAL: &str =
    "expected_dpu_loopback_reservations_dpu_serial_number_key";
const SQL_VIOLATION_DUPLICATE_IPV4: &str = "expected_dpu_loopback_reservations_loopback_ipv4_key";
const SQL_VIOLATION_DUPLICATE_IPV6: &str = "expected_dpu_loopback_reservations_loopback_ipv6_key";

fn reservation_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<DpuLoopbackReservation, sqlx::Error> {
    let dpu_serial_number: String = row.try_get("dpu_serial_number")?;
    let loopback_ipv4: Option<IpAddr> = row.try_get("loopback_ipv4")?;
    let loopback_ipv6: Option<IpAddr> = row.try_get("loopback_ipv6")?;
    Ok(DpuLoopbackReservation {
        dpu_serial_number,
        loopback_ipv4: loopback_ipv4.and_then(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        }),
        loopback_ipv6: loopback_ipv6.and_then(|ip| match ip {
            IpAddr::V6(v6) => Some(v6),
            IpAddr::V4(_) => None,
        }),
    })
}

/// Reservations declared for one host expected machine, ordered by serial.
pub async fn find_for_machine(
    db: impl DbReader<'_>,
    bmc_mac_address: MacAddress,
) -> DatabaseResult<Vec<DpuLoopbackReservation>> {
    let sql = "SELECT dpu_serial_number, loopback_ipv4, loopback_ipv6 \
               FROM expected_dpu_loopback_reservations \
               WHERE bmc_mac_address = $1 \
               ORDER BY dpu_serial_number";
    let rows = sqlx::query(sql)
        .bind(bmc_mac_address)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;
    rows.iter()
        .map(reservation_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| DatabaseError::query(sql, err))
}

/// The reservation for one DPU pairing serial, resolved globally.
///
/// Direct discovery uses this because Scout never sees the owning host expected
/// machine; the serial is unique site-wide, so a single row (or none) matches.
pub async fn find_by_dpu_serial(
    db: impl DbReader<'_>,
    dpu_serial_number: &str,
) -> DatabaseResult<Option<DpuLoopbackReservation>> {
    let sql = "SELECT dpu_serial_number, loopback_ipv4, loopback_ipv6 \
               FROM expected_dpu_loopback_reservations \
               WHERE dpu_serial_number = $1";
    let row = sqlx::query(sql)
        .bind(dpu_serial_number)
        .fetch_optional(db)
        .await
        .map_err(|err| DatabaseError::query(sql, err))?;
    row.as_ref()
        .map(reservation_from_row)
        .transpose()
        .map_err(|err| DatabaseError::query(sql, err))
}

/// Replace the reservations for one host expected machine.
///
/// Deletes the host's existing reservations, then inserts `reservations`. The
/// caller runs this inside the same transaction as the expected-machine write
/// so the two move atomically. An empty slice clears the host's reservations.
pub async fn replace_for_machine(
    txn: &mut PgConnection,
    bmc_mac_address: MacAddress,
    reservations: &[DpuLoopbackReservation],
) -> DatabaseResult<()> {
    let delete = "DELETE FROM expected_dpu_loopback_reservations WHERE bmc_mac_address = $1";
    sqlx::query(delete)
        .bind(bmc_mac_address)
        .execute(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(delete, err))?;

    if reservations.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::new(
        "INSERT INTO expected_dpu_loopback_reservations \
         (bmc_mac_address, dpu_serial_number, loopback_ipv4, loopback_ipv6) ",
    );
    builder.push_values(reservations, |mut b, reservation| {
        b.push_bind(bmc_mac_address)
            .push_bind(&reservation.dpu_serial_number)
            .push_bind(reservation.loopback_ipv4.map(IpAddr::V4))
            .push_bind(reservation.loopback_ipv6.map(IpAddr::V6));
    });

    builder
        .build()
        .execute(&mut *txn)
        .await
        .map_err(map_reservation_insert_error)?;
    Ok(())
}

/// Remove every reservation. Replace-all relies on the `expected_machines`
/// cascade instead; this exists for callers that clear reservations alone.
pub async fn clear(txn: &mut PgConnection) -> DatabaseResult<()> {
    let query = "DELETE FROM expected_dpu_loopback_reservations";
    sqlx::query(query)
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|err| DatabaseError::query(query, err))
}

/// Translate the child table's uniqueness violations into precondition failures
/// the API surfaces as `FailedPrecondition`. The handler validates first, so
/// this only fires for a writer that raced past that check.
fn map_reservation_insert_error(err: sqlx::Error) -> DatabaseError {
    if let sqlx::Error::Database(db_err) = &err {
        match db_err.constraint() {
            Some(SQL_VIOLATION_DUPLICATE_SERIAL) => {
                return DatabaseError::FailedPrecondition(
                    "DPU serial number is already declared by another reservation".to_string(),
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
                interfaces: vec![],
                rack_id: None,
                default_pause_ingestion_and_poweron: None,
                dpf_enabled: None,
                bmc_ip_address: None,
                bmc_retain_credentials: None,
                dpu_policy: Default::default(),
                bmc_ip_allocation: Default::default(),
                host_lifecycle_profile: Default::default(),
                dpu_loopback_reservations: Some(reservations),
            },
        }
    }

    fn reservation(serial: &str, v4: Option<&str>, v6: Option<&str>) -> DpuLoopbackReservation {
        DpuLoopbackReservation {
            dpu_serial_number: serial.to_string(),
            loopback_ipv4: v4.map(|s| s.parse::<Ipv4Addr>().unwrap()),
            loopback_ipv6: v6.map(|s| s.parse::<Ipv6Addr>().unwrap()),
        }
    }

    /// A host's reservations are persisted with its row, hydrated back in serial
    /// order, resolvable globally by DPU serial, and cleared by an empty replace.
    #[crate::sqlx_test]
    async fn reservations_persist_hydrate_and_clear(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mac = "aa:bb:cc:dd:ee:01";
        let machine = expected_machine(
            mac,
            vec![
                reservation("SER-B", Some("192.0.2.11"), None),
                reservation("SER-A", None, Some("2001:db8::1")),
            ],
        );

        let mut txn = pool.begin().await?;
        crate::expected_machine::create(&mut txn, machine).await?;
        txn.commit().await?;

        // A read hydrates every reservation, ordered by serial.
        let mut txn = pool.begin().await?;
        let hydrated = find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?).await?;
        assert_eq!(
            hydrated
                .iter()
                .map(|r| r.dpu_serial_number.as_str())
                .collect::<Vec<_>>(),
            ["SER-A", "SER-B"],
        );

        // Direct discovery resolves a reservation by its globally unique serial.
        let by_serial = find_by_dpu_serial(txn.as_mut(), "SER-B").await?.unwrap();
        assert_eq!(by_serial.loopback_ipv4, Some("192.0.2.11".parse()?));
        assert!(find_by_dpu_serial(txn.as_mut(), "absent").await?.is_none());

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

    /// `expected_machine::update` preserves stored reservations when the field
    /// is omitted (`None`) and clears them on an explicit empty list -- the
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
            expected_machine(mac, vec![reservation("SER-A", Some("192.0.2.11"), None)]),
        )
        .await?;

        // An omitted field (None) leaves the stored reservation intact.
        let mut preserving = expected_machine(mac, vec![]);
        preserving.data.dpu_loopback_reservations = None;
        crate::expected_machine::update(&mut txn, &preserving).await?;
        let after_preserve = find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?).await?;
        assert_eq!(
            after_preserve
                .iter()
                .map(|r| r.dpu_serial_number.as_str())
                .collect::<Vec<_>>(),
            ["SER-A"],
        );

        // An explicit empty list clears the stored reservations.
        let mut clearing = expected_machine(mac, vec![]);
        clearing.data.dpu_loopback_reservations = Some(vec![]);
        crate::expected_machine::update(&mut txn, &clearing).await?;
        assert!(
            find_for_machine(txn.as_mut(), mac.parse::<MacAddress>()?)
                .await?
                .is_empty()
        );

        txn.rollback().await?;
        Ok(())
    }

    /// The site-wide serial uniqueness constraint rejects the same DPU serial
    /// declared on a second host, surfaced as a precondition failure.
    #[crate::sqlx_test]
    async fn duplicate_serial_across_hosts_is_rejected(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = expected_machine(
            "aa:bb:cc:dd:ee:01",
            vec![reservation("SHARED", Some("192.0.2.11"), None)],
        );
        let second = expected_machine(
            "aa:bb:cc:dd:ee:02",
            vec![reservation("SHARED", Some("192.0.2.12"), None)],
        );

        let mut txn = pool.begin().await?;
        crate::expected_machine::create(&mut txn, first).await?;
        let error = crate::expected_machine::create(&mut txn, second)
            .await
            .map(|_| ())
            .expect_err("a DPU serial declared on another host must be rejected");
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
                reservation("SER-A", Some("192.0.2.11"), None),
                reservation("SER-B", Some("192.0.2.11"), None),
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
}
