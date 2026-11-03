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

use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
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
        nvos_interfaces: vec![ExpectedNvosInterface {
            mac_address: nvos_mac,
            is_primary: true,
        }],
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
            find_nvos_mac_claimed_elsewhere(
                &mut txn,
                &[different_mac, nvos_mac],
                other.bmc_mac_address,
            )
            .await?,
            Some(nvos_mac)
        );
    }

    txn.rollback().await?;
    Ok(())
}

fn assert_switch(actual: &ExpectedSwitch, expected: &ExpectedSwitch) {
    assert_eq!(actual.expected_switch_id, expected.expected_switch_id);
    assert_eq!(actual.bmc_mac_address, expected.bmc_mac_address);
    assert_eq!(actual.nvos_interfaces, expected.nvos_interfaces);
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
        nvos_interfaces: vec![ExpectedNvosInterface {
            mac_address: expected_switch_nvos_mac_address(index),
            is_primary: true,
        }],
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
    assert_eq!(found.bmc_ip_address, None);
    assert_eq!(found.nvos_ip_address, None);

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
async fn expected_switch_lookup_rejects_non_host_prefixes(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    for (column, query) in [
        (
            "bmc_ip_address",
            "UPDATE expected_switches SET bmc_ip_address = '192.0.2.1/24'::inet
             WHERE expected_switch_id = $1",
        ),
        (
            "nvos_ip_address",
            "UPDATE expected_switches SET nvos_ip_address = '192.0.2.1/24'::inet
             WHERE expected_switch_id = $1",
        ),
    ] {
        let mut txn = pool.begin().await?;
        let switch = create_expected_switch(&mut txn, 0).await;
        let id = switch.expected_switch_id.expect("created switch has an ID");

        // PostgreSQL accepts this prefix, but SQLx decodes `IpAddr` only with
        // a /32 (IPv4) or /128 (IPv6) prefix.
        sqlx::query(query).bind(id).execute(&mut *txn).await?;
        let error = find_by_id(&mut txn, id).await.map(|_| ()).expect_err(
            "a non-host prefix must fail decoding rather than become an absent address",
        );
        let DatabaseError::Sqlx(annotated) = &error else {
            panic!("{column}: expected Sqlx, got {error:?}");
        };
        let sqlx::Error::ColumnDecode { index, source } = &annotated.source else {
            panic!("{column}: expected ColumnDecode, got {error:?}");
        };
        assert_eq!(index, &format!("{column:?}"));
        assert_eq!(source.to_string(), "lossy decode from inet/cidr");
        assert!(
            annotated
                .query
                .contains("FROM expected_switches WHERE expected_switch_id=$1"),
            "{error:?}"
        );
        txn.rollback().await?;
    }
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
            nvos_interfaces: switch.nvos_interfaces.clone(),
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
            nvos_interfaces: switches[0].nvos_interfaces.clone(),
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
    own.nvos_interfaces = switches[1].nvos_interfaces.clone();
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

/// The backfill seeds `expected_switch_nvos_interfaces` from the legacy
/// `nvos_mac_addresses` array, marking each switch's first MAC as primary. A
/// single-MAC switch gets that MAC as primary; an empty array leaves no rows.
#[crate::sqlx_test]
async fn backfill_marks_each_switchs_first_nvos_mac_primary(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Restore the predecessor state: expected_switches populated through the
    // legacy array column, with the interface table still empty for these rows.
    sqlx::raw_sql(
        "INSERT INTO expected_switches
             (serial_number, bmc_mac_address, bmc_username, bmc_password, nvos_mac_addresses)
         VALUES
             ('BF-MULTI',  '02:00:00:00:0e:01', 'admin', 'pw',
                 ARRAY['aa:aa:aa:aa:aa:02','aa:aa:aa:aa:aa:01']::macaddr[]),
             ('BF-SINGLE', '02:00:00:00:0e:02', 'admin', 'pw',
                 ARRAY['bb:bb:bb:bb:bb:01']::macaddr[]),
             ('BF-EMPTY',  '02:00:00:00:0e:03', 'admin', 'pw', '{}'::macaddr[]),
             ('BF-DUP-A',  '02:00:00:00:0e:04', 'admin', 'pw',
                 ARRAY['cc:cc:cc:cc:cc:01','cc:cc:cc:cc:cc:02']::macaddr[]),
             ('BF-DUP-B',  '02:00:00:00:0e:05', 'admin', 'pw',
                 ARRAY['cc:cc:cc:cc:cc:01','cc:cc:cc:cc:cc:03']::macaddr[]),
             ('BF-INTRA',  '02:00:00:00:0e:06', 'admin', 'pw',
                 ARRAY['dd:dd:dd:dd:dd:01','dd:dd:dd:dd:dd:01','dd:dd:dd:dd:dd:02']::macaddr[]);",
    )
    .execute(txn.as_mut())
    .await?;

    sqlx::raw_sql(include_str!(
        "../../migrations/20261009130100_backfill_expected_switch_nvos_interfaces.sql"
    ))
    .execute(txn.as_mut())
    .await?;

    // The ordinality-1 MAC is primary even though it sorts after the other one,
    // so a wrong primary would reorder this primary-first projection.
    let multi = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:01".parse()?)
        .await?
        .expect("multi-MAC switch exists");
    assert_eq!(
        multi.nvos_interfaces,
        vec![
            ExpectedNvosInterface {
                mac_address: "aa:aa:aa:aa:aa:02".parse()?,
                is_primary: true,
            },
            ExpectedNvosInterface {
                mac_address: "aa:aa:aa:aa:aa:01".parse()?,
                is_primary: false,
            },
        ]
    );

    let single = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:02".parse()?)
        .await?
        .expect("single-MAC switch exists");
    assert_eq!(
        single.nvos_interfaces,
        vec![ExpectedNvosInterface {
            mac_address: "bb:bb:bb:bb:bb:01".parse()?,
            is_primary: true,
        }]
    );

    let empty = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:03".parse()?)
        .await?
        .expect("empty-array switch exists");
    assert!(empty.nvos_interfaces.is_empty());

    // A MAC the unguarded legacy writes recorded on two switches lands on
    // exactly one -- the lower BMC MAC wins -- instead of aborting the backfill
    // on the primary-key collision.
    let dup_a = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:04".parse()?)
        .await?
        .expect("duplicate-owning switch exists");
    assert_eq!(
        dup_a.nvos_interfaces,
        vec![
            ExpectedNvosInterface {
                mac_address: "cc:cc:cc:cc:cc:01".parse()?,
                is_primary: true,
            },
            ExpectedNvosInterface {
                mac_address: "cc:cc:cc:cc:cc:02".parse()?,
                is_primary: false,
            },
        ]
    );

    // The losing switch keeps only its unique MAC. The backfill does not
    // recompute the flag after ON CONFLICT drops its first port, so the lone
    // survivor stays is_primary = false -- an accepted defensive fallback for
    // unexpected malformed legacy data.
    let dup_b = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:05".parse()?)
        .await?
        .expect("duplicate-losing switch exists");
    assert_eq!(
        dup_b.nvos_interfaces,
        vec![ExpectedNvosInterface {
            mac_address: "cc:cc:cc:cc:cc:03".parse()?,
            is_primary: false,
        }]
    );
    assert_eq!(
        dup_b.primary_nvos_mac_address(),
        Some("cc:cc:cc:cc:cc:03".parse()?),
        "the lone surviving port is still selected as primary at read time",
    );

    // A MAC repeated within one array collapses to a single port, keeping its
    // first position as primary.
    let intra = find_by_bmc_mac_address(&mut txn, "02:00:00:00:0e:06".parse()?)
        .await?
        .expect("intra-array duplicate switch exists");
    assert_eq!(
        intra.nvos_interfaces,
        vec![
            ExpectedNvosInterface {
                mac_address: "dd:dd:dd:dd:dd:01".parse()?,
                is_primary: true,
            },
            ExpectedNvosInterface {
                mac_address: "dd:dd:dd:dd:dd:02".parse()?,
                is_primary: false,
            },
        ]
    );

    txn.rollback().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn update_by_id_uses_the_stored_bmc_mac_for_nvos_ports(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let first = create_expected_switch(&mut txn, 30).await;
    let second = create_expected_switch(&mut txn, 31).await;
    let replacement: MacAddress = "55:55:55:55:55:30".parse()?;
    let mut mismatched = first.clone();
    mismatched.bmc_mac_address = second.bmc_mac_address;
    mismatched.nvos_interfaces = vec![ExpectedNvosInterface {
        mac_address: replacement,
        is_primary: true,
    }];

    update(&mut txn, &mismatched).await?;

    let first_reloaded = find_by_bmc_mac_address(&mut txn, first.bmc_mac_address)
        .await?
        .expect("first switch exists");
    assert_eq!(first_reloaded.nvos_interfaces, mismatched.nvos_interfaces);
    let second_reloaded = find_by_bmc_mac_address(&mut txn, second.bmc_mac_address)
        .await?
        .expect("second switch exists");
    assert_eq!(second_reloaded.nvos_interfaces, second.nvos_interfaces);

    txn.rollback().await?;
    Ok(())
}
