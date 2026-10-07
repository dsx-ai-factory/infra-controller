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

use std::collections::HashMap;

use model::expected_switch::ExpectedSwitch;
use model::metadata::Metadata;
use model::rack::RackConfig;
use sqlx::Connection;

use super::*;
use crate as db;

#[crate::sqlx_test]
async fn bmc_credential_length_migration_preserves_rows(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The harness applies all migrations; restore the populated predecessor.
    sqlx::raw_sql(
        "ALTER TABLE expected_switches
             ALTER COLUMN bmc_username TYPE VARCHAR(16),
             ALTER COLUMN bmc_password TYPE VARCHAR(16);
         INSERT INTO expected_switches
             (serial_number, bmc_mac_address, bmc_username, bmc_password)
         VALUES ('existing-switch', '02:00:00:00:01:01', 'admin', repeat('a', 16));",
    )
    .execute(&pool)
    .await?;
    let snapshot =
        "SELECT to_jsonb(expected_switches) FROM expected_switches ORDER BY expected_switch_id";
    let before: Vec<serde_json::Value> = sqlx::query_scalar(snapshot).fetch_all(&pool).await?;

    sqlx::raw_sql(include_str!(
        "../../migrations/20261005160144_expected_switch_bmc_password_length.sql"
    ))
    .execute(&pool)
    .await?;
    let after: Vec<serde_json::Value> = sqlx::query_scalar(snapshot).fetch_all(&pool).await?;
    assert_eq!(after, before);

    let mut txn = pool.begin().await?;
    let existing = find_by_serial_number(&mut txn, "existing-switch")
        .await?
        .unwrap();
    let new_switch = ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: expected_switch_bmc_mac_address(1),
        serial_number: "long-credentials-switch".to_string(),
        bmc_username: "u".repeat(256),
        bmc_password: "b".repeat(17),
        ..existing
    };
    let mut switch = create(&mut txn, new_switch).await?;
    assert_eq!(switch.bmc_username, "u".repeat(256));
    assert_eq!(switch.bmc_password, "b".repeat(17));
    switch.bmc_username = "v".repeat(512);
    switch.bmc_password = "c".repeat(255);
    update(&mut txn, &switch).await?;
    txn.commit().await?;

    let mut connection = pool.acquire().await?;
    let stored = find_by_id(&mut connection, switch.expected_switch_id.unwrap())
        .await?
        .unwrap();
    assert_eq!(stored.bmc_username, "v".repeat(512));
    assert_eq!(stored.bmc_password, "c".repeat(255));
    let error =
        sqlx::query("UPDATE expected_switches SET bmc_password = $1 WHERE expected_switch_id = $2")
            .bind("d".repeat(256))
            .bind(switch.expected_switch_id)
            .execute(&pool)
            .await
            .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("22001")
    );
    Ok(())
}

#[crate::sqlx_test]
async fn expected_switch_queries_survive_added_columns(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut api_connection = pool.acquire().await?;
    exercise_expected_switch_queries(&mut api_connection).await?;
    assert!(api_connection.cached_statements_size() > 0);

    // Keep the API's prepared statements while another connection applies DDL.
    let mut migration = pool.begin().await?;
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '5s';
         ALTER TABLE expected_switches ADD COLUMN test_added_column text;",
    )
    .execute(&mut *migration)
    .await?;
    migration.commit().await?;

    exercise_expected_switch_queries(&mut api_connection).await?;
    Ok(())
}

async fn exercise_expected_switch_queries(
    connection: &mut PgConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = connection.begin().await?;
    let id = Uuid::new_v4();
    let rack_id = RackId::new("projection-rack");
    let nvos_mac = "02:00:00:00:02:02".parse()?;
    let expected = ExpectedSwitch {
        expected_switch_id: Some(id),
        bmc_mac_address: "02:00:00:00:02:01".parse()?,
        nvos_mac_addresses: vec![nvos_mac],
        bmc_username: "test-user".to_string(),
        bmc_password: "test-password".to_string(),
        serial_number: "projection-switch".to_string(),
        nvos_username: Some("nvos-user".to_string()),
        nvos_password: Some("nvos-password".to_string()),
        bmc_ip_address: Some("192.0.2.20".parse()?),
        nvos_ip_address: Some("192.0.2.21".parse()?),
        rack_id: Some(rack_id.clone()),
        bmc_retain_credentials: Some(true),
        metadata: Metadata {
            name: "expected switch".to_string(),
            description: "populated projection fixture".to_string(),
            labels: HashMap::from([("location".to_string(), "rack-1".to_string())]),
        },
    };
    assert_switch(&create(&mut txn, expected.clone()).await?, &expected);

    for found in [
        find_by_bmc_mac_address(&mut txn, expected.bmc_mac_address).await?,
        find_by_nvos_mac_address(&mut txn, nvos_mac).await?,
        find_by_serial_number(&mut txn, &expected.serial_number).await?,
        find_by_id(&mut txn, id).await?,
        find_by_rack_id(&mut txn, rack_id.to_string()).await?,
        find_for_update(
            &mut txn,
            &ExpectedSwitchRequest {
                expected_switch_id: Some(id),
                bmc_mac_address: None,
            },
        )
        .await?,
        find_for_update(
            &mut txn,
            &ExpectedSwitchRequest {
                expected_switch_id: None,
                bmc_mac_address: Some(expected.bmc_mac_address),
            },
        )
        .await?,
    ] {
        assert_switch(&found.expect("the expected switch exists"), &expected);
    }
    for found in [
        find_all(&mut txn).await?,
        find_all_by_rack_id(&mut txn, &rack_id).await?,
    ] {
        assert_eq!(found.len(), 1);
        assert_switch(&found[0], &expected);
    }
    let found = find_many_by_bmc_mac_address(&mut txn, &[expected.bmc_mac_address]).await?;
    assert_eq!(found.len(), 1);
    assert_switch(&found[&expected.bmc_mac_address], &expected);

    // Request a different MAC first so the fallback cannot hide a missing
    // `nvos_mac_addresses` column in either selector.
    let different_mac = "02:00:00:00:02:04".parse()?;
    for expected_switch_id in [Some(Uuid::new_v4()), None] {
        let other = ExpectedSwitch {
            expected_switch_id,
            bmc_mac_address: "02:00:00:00:02:03".parse()?,
            ..expected.clone()
        };
        assert_eq!(
            find_nvos_mac_claimed_elsewhere(&mut txn, &[different_mac, nvos_mac], &other).await?,
            Some(nvos_mac)
        );
    }

    txn.rollback().await?;
    Ok(())
}

fn assert_switch(actual: &ExpectedSwitch, expected: &ExpectedSwitch) {
    assert_eq!(actual.expected_switch_id, expected.expected_switch_id);
    assert_eq!(actual.bmc_mac_address, expected.bmc_mac_address);
    assert_eq!(actual.nvos_mac_addresses, expected.nvos_mac_addresses);
    assert_eq!(actual.bmc_username, expected.bmc_username);
    assert_eq!(actual.bmc_password, expected.bmc_password);
    assert_eq!(actual.serial_number, expected.serial_number);
    assert_eq!(actual.nvos_username, expected.nvos_username);
    assert_eq!(actual.nvos_password, expected.nvos_password);
    assert_eq!(actual.bmc_ip_address, expected.bmc_ip_address);
    assert_eq!(actual.nvos_ip_address, expected.nvos_ip_address);
    assert_eq!(actual.rack_id, expected.rack_id);
    assert_eq!(
        actual.bmc_retain_credentials,
        expected.bmc_retain_credentials
    );
    assert_eq!(actual.metadata, expected.metadata);
}

fn expected_switch_bmc_mac_address(index: u32) -> mac_address::MacAddress {
    mac_address::MacAddress::new([0x44, 0x44, 0x11, 0x11, 0x00, index as u8])
}

fn expected_switch_nvos_mac_address(index: u32) -> mac_address::MacAddress {
    mac_address::MacAddress::new([0x44, 0x44, 0x33, 0x33, 0x00, index as u8])
}

/// Seeds one expected switch into the database.
async fn create_expected_switch(
    txn: &mut sqlx::PgConnection,
    index: u32,
) -> model::expected_switch::ExpectedSwitch {
    use model::expected_switch::ExpectedSwitch;
    use model::metadata::Metadata;

    let i = index as usize;
    let switch = ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: expected_switch_bmc_mac_address(index),
        nvos_mac_addresses: vec![expected_switch_nvos_mac_address(index)],
        serial_number: format!("SW-SN-{:03}", index + 1),
        bmc_username: "ADMIN".into(),
        bmc_password: "Pwd2023x0x0x0x7".into(),
        nvos_username: if (3..=4).contains(&i) {
            Some(format!("nvos_admin{}", i - 2))
        } else {
            None
        },
        nvos_password: if (3..=4).contains(&i) {
            Some(format!("nvos_pass{}", i - 2))
        } else {
            None
        },
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Switch{}", index + 1),
            description: format!("Test Switch {}", index + 1),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    };
    db::expected_switch::create(txn, switch)
        .await
        .expect("unable to create expected switch")
}

/// create_expected_switches seeds 6 expected switches into the database,
/// replacing the create_expected_switch.sql fixture.
async fn create_expected_switches(
    txn: &mut sqlx::PgConnection,
) -> Vec<model::expected_switch::ExpectedSwitch> {
    let mut created = Vec::new();
    for i in 0..6 {
        created.push(create_expected_switch(txn, i).await);
    }
    created
}

#[crate::sqlx_test]
async fn test_lookup_by_mac(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    // This used to assert `switches[0].serial_number == "SW-SN-001"` -- the string the
    // fixture had just built with `format!` -- so the finder this test is named for never
    // ran at all.
    let found = db::expected_switch::find_by_bmc_mac_address(&mut txn, switches[0].bmc_mac_address)
        .await?
        .expect("the switch we just created should be findable by its BMC MAC");
    assert_eq!(found.bmc_mac_address, switches[0].bmc_mac_address);
    assert_eq!(found.serial_number, switches[0].serial_number);

    // 0xff is past the six switches the fixture creates.
    let unknown_mac = mac_address::MacAddress::new([0x44, 0x44, 0x11, 0x11, 0x00, 0xff]);
    assert!(
        db::expected_switch::find_by_bmc_mac_address(&mut txn, unknown_mac)
            .await?
            .is_none()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_duplicate_fail_create(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    let switch = &switches[0];
    let new_switch = db::expected_switch::create(
        &mut txn,
        ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address: switch.bmc_mac_address,
            nvos_mac_addresses: switch.nvos_mac_addresses.clone(),
            bmc_username: "ADMIN3".into(),
            bmc_password: "hmm".into(),
            serial_number: "DUPLICATE".into(),
            bmc_ip_address: None,
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
            nvos_ip_address: None,
            nvos_username: None,
            nvos_password: None,
        },
    )
    .await;

    assert!(matches!(
        new_switch,
        Err(DatabaseError::ExpectedHostDuplicateMacAddress(_))
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_create_rejects_nvos_mac_claimed_by_another_switch(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    let result = db::expected_switch::create(
        &mut txn,
        ExpectedSwitch {
            expected_switch_id: None,
            bmc_mac_address: expected_switch_bmc_mac_address(200),
            nvos_mac_addresses: switches[0].nvos_mac_addresses.clone(),
            bmc_username: "ADMIN".into(),
            bmc_password: "hmm".into(),
            serial_number: "NVOS-DUP".into(),
            bmc_ip_address: None,
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
            nvos_ip_address: None,
            nvos_username: None,
            nvos_password: None,
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(_))
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_nvos_mac_conflicts_exclude_own_switch(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    // Re-asserting a switch's own NVOS MACs is not a conflict.
    let mut own = switches[0].clone();
    own.bmc_username = "NEWADMIN".into();
    db::expected_switch::update(&mut txn, &own).await?;

    // Claiming another switch's NVOS MAC is.
    own.nvos_mac_addresses = switches[1].nvos_mac_addresses.clone();
    let result = db::expected_switch::update(&mut txn, &own).await;

    assert!(matches!(
        result,
        Err(DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(_))
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_missing_switch_reports_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    // A target that doesn't exist reports NotFound even when the payload's
    // NVOS MACs are claimed by some existing switch.
    let mut ghost = switches[0].clone();
    ghost.expected_switch_id = None;
    ghost.bmc_mac_address = expected_switch_bmc_mac_address(201);
    let result = db::expected_switch::update(&mut txn, &ghost).await;

    assert!(matches!(result, Err(DatabaseError::NotFoundError { .. })));

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_tolerates_preexisting_nvos_mac_overlap(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;

    // site-explorer's hardware-truth path can record an overlap that predates
    // the conflict guard: switch 0 ends up holding switch 1's MAC too.
    let mut overlapped = switches[0].nvos_mac_addresses.clone();
    overlapped.extend(switches[1].nvos_mac_addresses.iter().copied());
    db::expected_switch::update_nvos_mac_addresses(
        &mut txn,
        switches[0].bmc_mac_address,
        &overlapped,
    )
    .await?;

    // Re-sending the list the row already holds must stay updatable -- only
    // newly claimed MACs are checked.
    let mut own = switches[0].clone();
    own.nvos_mac_addresses = overlapped;
    own.bmc_username = "NEWADMIN".into();
    db::expected_switch::update(&mut txn, &own).await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_bmc_credentials(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    let mut switch = switches[0].clone();

    assert_eq!(switch.serial_number, "SW-SN-001");
    assert_eq!(switch.bmc_username, "ADMIN");
    assert_eq!(switch.bmc_password, "Pwd2023x0x0x0x7");
    switch.bmc_username = "ADMIN2".to_string();
    switch.bmc_password = "wysiwyg".to_string();
    db::expected_switch::update(&mut txn, &switch)
        .await
        .expect("Error updating bmc username/password");

    txn.commit().await.expect("Failed to commit transaction");

    let mut txn = pool
        .begin()
        .await
        .expect("unable to create transaction on database pool");

    let switch =
        db::expected_switch::find_by_bmc_mac_address(&mut txn, switches[0].bmc_mac_address)
            .await
            .unwrap()
            .expect("Expected switch not found");

    assert_eq!(switch.bmc_username, "ADMIN2");
    assert_eq!(switch.bmc_password, "wysiwyg");

    Ok(())
}

#[crate::sqlx_test]
async fn test_delete(pool: sqlx::PgPool) -> () {
    let mut txn = pool.begin().await.unwrap();
    let switches = create_expected_switches(&mut txn).await;
    let mac = switches[0].bmc_mac_address;
    txn.commit().await.expect("Failed to commit transaction");

    crate::test_support::expected_host::assert_delete_by_mac_removes_row(
        &pool,
        mac,
        async |txn, mac| db::expected_switch::delete_by_mac(txn, mac).await,
        async |txn, mac| db::expected_switch::find_by_bmc_mac_address(txn, mac).await,
    )
    .await;
}

/// Every switch is rack-scale, so the pre-ingestion RMS identity resolver
/// requires a declared `rack_id` and takes its rack profile from the live
/// `racks` row when present, otherwise the `expected_racks` declaration.
/// A switch missing a `rack_id` is a misconfiguration and is omitted.
#[crate::sqlx_test]
async fn test_find_rms_identities_by_bmc_macs(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    let bmc_expected_only = "02:00:00:00:0d:01";
    let bmc_live_rack = "02:00:00:00:0d:02";
    let bmc_no_rack = "02:00:00:00:0d:03";

    let rack_expected: RackId = "rack-expected-only".parse().unwrap();
    let rack_live: RackId = "rack-live".parse().unwrap();
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
        "INSERT INTO expected_switches
             (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id)
         VALUES ('SW-EXP', $1::macaddr, 'admin', 'pw', $2),
                ('SW-LIVE', $3::macaddr, 'admin', 'pw', $4),
                ('SW-NORACK', $5::macaddr, 'admin', 'pw', NULL)",
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

    assert_eq!(by_mac.len(), 2, "the no-rack switch must be omitted");

    let expected_only = by_mac
        .get(&bmc_expected_only.parse()?)
        .expect("expected-only switch resolves");
    assert_eq!(expected_only.rack_id, rack_expected);
    assert_eq!(
        expected_only.rack_profile_id.as_ref(),
        Some(&profile_expected),
        "rack profile falls back to expected_racks when no live rack exists"
    );

    let live = by_mac
        .get(&bmc_live_rack.parse()?)
        .expect("live-rack switch resolves");
    assert_eq!(live.rack_id, rack_live);
    assert_eq!(
        live.rack_profile_id.as_ref(),
        Some(&profile_live),
        "a live racks row takes precedence over the expected_racks declaration"
    );

    assert!(
        !by_mac.contains_key(&bmc_no_rack.parse()?),
        "a switch without a rack_id cannot resolve an RMS identity"
    );

    txn.rollback().await?;
    Ok(())
}
