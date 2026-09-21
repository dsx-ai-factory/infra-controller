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

use model::expected_power_shelf::ExpectedPowerShelf;
use model::metadata::Metadata;

use super::*;
use crate as db;

fn expected_power_shelf_bmc_mac_address(index: u8) -> mac_address::MacAddress {
    mac_address::MacAddress::new([0x44, 0x44, 0x22, 0x22, 0x00, index])
}

/// create_expected_power_shelves seeds 6 expected power shelves into the
/// database, replacing the create_expected_power_shelf.sql fixture.
async fn create_expected_power_shelves(
    txn: &mut sqlx::PgConnection,
) -> Vec<model::expected_power_shelf::ExpectedPowerShelf> {
    use model::expected_power_shelf::ExpectedPowerShelf;
    use model::metadata::Metadata;

    let mut created = Vec::new();
    for i in 0..6 {
        let power_shelf = ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: expected_power_shelf_bmc_mac_address(i),
            serial_number: format!("PS-SN-{:03}", i + 1),
            bmc_username: "ADMIN".into(),
            bmc_password: "Pwd2023x0x0x0x0x7".into(),
            bmc_ip_address: if (3..=4).contains(&i) {
                Some(format!("192.168.1.{}", 100 + i - 3).parse().unwrap())
            } else {
                None
            },
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
        };
        let result = db::expected_power_shelf::create(txn, power_shelf)
            .await
            .expect("unable to create expected power shelf");
        created.push(result);
    }
    created
}

#[crate::sqlx_test]
async fn test_lookup_by_mac(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");
    let shelves = create_expected_power_shelves(&mut txn).await;

    // This used to assert `shelves[0].serial_number == "PS-SN-001"` -- the string the
    // fixture had just built with `format!` -- so the finder this test is named for never
    // ran at all.
    let found =
        db::expected_power_shelf::find_by_bmc_mac_address(&mut txn, shelves[0].bmc_mac_address)
            .await?
            .expect("the power shelf we just created should be findable by its BMC MAC");
    assert_eq!(found.bmc_mac_address, shelves[0].bmc_mac_address);
    assert_eq!(found.serial_number, shelves[0].serial_number);

    // 0xff is past the six shelves the fixture creates.
    let unknown_mac = mac_address::MacAddress::new([0x44, 0x44, 0x22, 0x22, 0x00, 0xff]);
    assert!(
        db::expected_power_shelf::find_by_bmc_mac_address(&mut txn, unknown_mac)
            .await?
            .is_none()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_create_preserves_bmc_ip_addresses(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let shelves = create_expected_power_shelves(&mut txn).await;

    // Shelves at indices 3 and 4 are created with IP addresses
    assert_eq!(
        shelves[3].bmc_ip_address,
        Some("192.168.1.100".parse().unwrap())
    );
    assert_eq!(
        shelves[4].bmc_ip_address,
        Some("192.168.1.101".parse().unwrap())
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_duplicate_fail_create(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");
    let shelves = create_expected_power_shelves(&mut txn).await;

    let power_shelf = &shelves[0];

    let new_power_shelf = db::expected_power_shelf::create(
        &mut txn,
        ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: power_shelf.bmc_mac_address,
            bmc_username: "ADMIN3".into(),
            bmc_password: "hmm".into(),
            serial_number: "DUPLICATE".into(),
            bmc_ip_address: None,
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
        },
    )
    .await;

    assert!(matches!(
        new_power_shelf,
        Err(DatabaseError::ExpectedHostDuplicateMacAddress(_))
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_bmc_credentials(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");
    let shelves = create_expected_power_shelves(&mut txn).await;
    let mut power_shelf = shelves[0].clone();

    assert_eq!(power_shelf.serial_number, "PS-SN-001");
    assert_eq!(power_shelf.bmc_username, "ADMIN");
    assert_eq!(power_shelf.bmc_password, "Pwd2023x0x0x0x0x7");

    power_shelf.bmc_username = "ADMIN2".to_string();
    power_shelf.bmc_password = "wysiwyg".to_string();
    db::expected_power_shelf::update(&mut txn, &power_shelf)
        .await
        .expect("Error updating bmc username/password");

    txn.commit().await.expect("Failed to commit transaction");

    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");

    let power_shelf =
        db::expected_power_shelf::find_by_bmc_mac_address(&mut txn, shelves[0].bmc_mac_address)
            .await
            .unwrap()
            .expect("Expected power shelf not found");

    assert_eq!(power_shelf.bmc_username, "ADMIN2");
    assert_eq!(power_shelf.bmc_password, "wysiwyg");

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete(pool: sqlx::PgPool) -> () {
    let mut txn = pool.begin().await.unwrap();
    let shelves = create_expected_power_shelves(&mut txn).await;
    let mac = shelves[0].bmc_mac_address;
    txn.commit().await.expect("Failed to commit transaction");

    crate::test_support::expected_host::assert_delete_by_mac_removes_row(
        &pool,
        mac,
        async |txn, mac| db::expected_power_shelf::delete_by_mac(txn, mac).await,
        async |txn, mac| db::expected_power_shelf::find_by_bmc_mac_address(txn, mac).await,
    )
    .await;
}

/// Every power shelf is rack-scale, so the pre-ingestion RMS identity resolver
/// requires a declared `rack_id` and takes its rack profile from the live
/// `racks` row when present, otherwise the `expected_racks` declaration.
/// A power shelf missing a `rack_id` is a misconfiguration and is omitted.
#[crate::sqlx_test]
async fn test_find_rms_identities_by_bmc_macs(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;

    use model::rack::RackConfig;

    let mut txn = pool.begin().await?;

    let bmc_expected_only = "02:00:00:00:0e:01";
    let bmc_live_rack = "02:00:00:00:0e:02";
    let bmc_no_rack = "02:00:00:00:0e:03";

    let rack_expected: RackId = "ps-rack-expected-only".parse().unwrap();
    let rack_live: RackId = "ps-rack-live".parse().unwrap();
    let profile_expected = RackProfileId::new("NVL72-EXPECTED");
    let profile_live = RackProfileId::new("NVL72-LIVE");

    // Both racks are declared in expected_racks; only rack_live also has a
    // live racks row (with a different profile) to prove COALESCE precedence.
    sqlx::query("INSERT INTO expected_racks (rack_id, rack_profile_id) VALUES ($1, $2), ($3, $4)")
        .bind(rack_expected.to_string())
        .bind(profile_expected.to_string())
        .bind(rack_live.to_string())
        .bind("NVL72-EXPECTED-IGNORED")
        .execute(txn.as_mut())
        .await?;

    crate::rack::create(
        txn.as_mut(),
        &rack_live,
        Some(&profile_live),
        &RackConfig::default(),
        None,
    )
    .await?;

    sqlx::query(
        "INSERT INTO expected_power_shelves
             (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id)
         VALUES ('PS-EXP', $1::macaddr, 'admin', 'pw', $2),
                ('PS-LIVE', $3::macaddr, 'admin', 'pw', $4),
                ('PS-NORACK', $5::macaddr, 'admin', 'pw', NULL)",
    )
    .bind(bmc_expected_only)
    .bind(rack_expected.to_string())
    .bind(bmc_live_rack)
    .bind(rack_live.to_string())
    .bind(bmc_no_rack)
    .execute(txn.as_mut())
    .await?;

    let identities = find_rms_identities_by_bmc_macs(
        txn.as_mut(),
        &[
            bmc_expected_only.parse()?,
            bmc_live_rack.parse()?,
            bmc_no_rack.parse()?,
        ],
    )
    .await?;

    let by_mac: HashMap<_, _> = identities
        .iter()
        .map(|id| (id.bmc_mac_address, id))
        .collect();

    assert_eq!(by_mac.len(), 2, "the no-rack power shelf must be omitted");

    let expected_only = by_mac
        .get(&bmc_expected_only.parse()?)
        .expect("expected-only power shelf resolves");
    assert_eq!(expected_only.rack_id, rack_expected);
    assert_eq!(
        expected_only.rack_profile_id.as_ref(),
        Some(&profile_expected),
        "rack profile falls back to expected_racks when no live rack exists"
    );

    let live = by_mac
        .get(&bmc_live_rack.parse()?)
        .expect("live-rack power shelf resolves");
    assert_eq!(live.rack_id, rack_live);
    assert_eq!(
        live.rack_profile_id.as_ref(),
        Some(&profile_live),
        "a live racks row takes precedence over the expected_racks declaration"
    );

    assert!(
        !by_mac.contains_key(&bmc_no_rack.parse()?),
        "a power shelf without a rack_id cannot resolve an RMS identity"
    );

    txn.rollback().await?;
    Ok(())
}
