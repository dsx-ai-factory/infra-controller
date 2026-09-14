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

use carbide_api_core::test_support::network::FIXTURE_DHCP_RELAY_ADDRESS;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use rpc::forge::forge_server::Forge;

use crate::expected_power_shelf::create_test_env;

/// When an expected power shelf is created with a bmc_ip_address, test to make
/// sure a machine_interface is pre-allocated with a static address in the DB.
/// Site explorer then just picks it up naturally from the underlay interface query.
#[sqlx_test]
async fn test_add_with_bmc_ip_creates_static_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "4A:4B:4C:4D:4E:4F".parse().unwrap();
    let bmc_ip = "192.0.2.180";

    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-STATIC-001".into(),
            bmc_ip_address: bmc_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Add doesn't preallocate inline; mimic what site-explorer does on the next iteration --
    // materialize the static BMC interface for this entity.
    carbide_site_explorer::try_preallocate_one(
        &env.api().database_connection,
        bmc_mac,
        bmc_ip.parse().unwrap(),
        model::machine_interface::InterfaceType::Bmc,
        "expected_power_shelf BMC",
        None,
    )
    .await;

    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(
        interfaces.len(),
        1,
        "should have one interface for the BMC MAC"
    );

    let iface = &interfaces[0];
    assert!(
        iface.addresses.contains(&bmc_ip.parse().unwrap()),
        "interface should have the static BMC IP"
    );

    // Verify the address is a static allocation type.
    let addrs = db::machine_interface_address::find_for_interface(&mut txn, iface.id).await?;
    assert_eq!(addrs.len(), 1);
    assert_eq!(
        addrs[0].address,
        bmc_ip.parse::<std::net::IpAddr>().unwrap()
    );
    assert_eq!(
        addrs[0].allocation_type,
        model::allocation_type::AllocationType::Static
    );

    txn.rollback().await?;

    Ok(())
}

/// When an expected power shelf is created WITHOUT a bmc_ip_address,
/// no machine_interface should be created.
#[sqlx_test]
async fn test_add_without_bmc_ip_creates_no_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "5A:5B:5C:5D:5E:5F".parse().unwrap();

    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-NO-IP-001".into(),
            bmc_ip_address: "".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // No interface should exist for this MAC.
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert!(
        interfaces.is_empty(),
        "should not create interface without bmc_ip_address"
    );

    txn.rollback().await?;

    Ok(())
}

/// Adding an expected power shelf with an external bmc_ip_address (not
/// in any managed prefix) should create the interface on the
/// static-assignments anchor segment.
#[sqlx_test]
async fn test_add_with_external_bmc_ip_uses_static_assignments(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:84".parse().unwrap();
    let external_ip = "10.50.1.150";

    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-EXT-001".into(),
            bmc_ip_address: external_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Add doesn't preallocate inline; mimic what site-explorer does on the next iteration --
    // materialize the static BMC interface for this entity.
    carbide_site_explorer::try_preallocate_one(
        &env.api().database_connection,
        bmc_mac,
        external_ip.parse().unwrap(),
        model::machine_interface::InterfaceType::Bmc,
        "expected_power_shelf BMC",
        None,
    )
    .await;

    // Verify interface was created on the static-assignments segment
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(interfaces.len(), 1);

    let iface = &interfaces[0];
    assert!(iface.addresses.contains(&external_ip.parse().unwrap()));

    let static_seg = db::network_segment::static_assignments(&mut txn).await?;
    assert_eq!(
        iface.segment_id, static_seg.id,
        "external IP should be on the static-assignments segment"
    );

    txn.rollback().await?;

    Ok(())
}

/// Updating with bmc_ip_address that matches the existing address is a
/// no-op -- the update succeeds without modifying the interface.
#[sqlx_test]
async fn test_update_with_matching_bmc_ip_is_noop(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:81".parse().unwrap();
    let bmc_ip = "192.0.1.191";

    // Add expected power shelf with bmc_ip_address.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-NOOP-001".into(),
            bmc_ip_address: bmc_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Update with the same bmc_ip_address -- should succeed (no-op).
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN-UPDATED".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-NOOP-001".into(),
            bmc_ip_address: bmc_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    Ok(())
}

/// Updating with a different bmc_ip_address succeeds (updates expected
/// data) but does not touch the interface if it already has addresses.
/// Expected data is decoupled from managed state -- the interface IP
/// can only be changed via assign-address / remove-address.
#[sqlx_test]
async fn test_update_with_different_bmc_ip_leaves_interface_alone(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:82".parse().unwrap();
    let original_ip = "192.0.1.192";

    // Add expected power shelf with bmc_ip_address, then run the sweep so the static
    // machine_interface row exists (the sweep is what materializes it).
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-LEAVE-001".into(),
            bmc_ip_address: original_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;
    carbide_site_explorer::try_preallocate_one(
        &env.api().database_connection,
        bmc_mac,
        original_ip.parse().unwrap(),
        model::machine_interface::InterfaceType::Bmc,
        "expected_power_shelf BMC",
        None,
    )
    .await;

    // Update with a DIFFERENT bmc_ip_address -- should succeed but
    // not touch the interface (it already has an address).
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-LEAVE-001".into(),
            bmc_ip_address: "192.0.1.193".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Verify the interface still has the ORIGINAL IP.
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert!(
        interfaces[0]
            .addresses
            .contains(&original_ip.parse().unwrap()),
        "interface should still have the original IP, not the updated expected data IP"
    );

    txn.rollback().await?;

    Ok(())
}

/// Updating with bmc_ip_address should succeed if the interface exists
/// but has no addresses (e.g., the address was expired/removed).
#[sqlx_test]
async fn test_update_with_bmc_ip_assigns_to_empty_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:83".parse().unwrap();
    let relay: std::net::IpAddr = FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap();

    // Create interface via DHCP, then remove its address.
    let mut txn = env.api().database_connection.begin().await?;
    let iface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        bmc_mac,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &iface.id).await?;
    txn.commit().await?;

    // Add expected power shelf WITHOUT bmc_ip_address.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-EMPTY-001".into(),
            bmc_ip_address: "".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Update with bmc_ip_address -- should succeed since interface has no addresses.
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-EMPTY-001".into(),
            bmc_ip_address: "192.0.1.194".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Verify the interface now has the static IP.
    let mut txn = env.api().database_connection.begin().await?;
    let addrs = db::machine_interface_address::find_for_interface(&mut txn, iface.id).await?;
    assert_eq!(addrs.len(), 1);
    assert_eq!(
        addrs[0].address,
        "192.0.1.194".parse::<std::net::IpAddr>().unwrap()
    );
    assert_eq!(
        addrs[0].allocation_type,
        model::allocation_type::AllocationType::Static
    );

    txn.rollback().await?;

    Ok(())
}

/// Updating with bmc_ip_address when no interface exists yet (device
/// hasn't DHCP'd) should create a new interface with the static IP.
#[sqlx_test]
async fn test_update_with_bmc_ip_creates_interface_if_none_exists(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:85".parse().unwrap();
    let bmc_ip = "192.0.1.195";

    // Add expected power shelf without bmc_ip_address.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-CREATE-001".into(),
            bmc_ip_address: "".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // No interface should exist yet.
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert!(interfaces.is_empty());
    txn.commit().await?;

    // Update with bmc_ip_address -- should create a new interface.
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-CREATE-001".into(),
            bmc_ip_address: bmc_ip.into(),

            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Verify interface was created with the static IP.
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert!(interfaces[0].addresses.contains(&bmc_ip.parse().unwrap()));

    txn.rollback().await?;

    Ok(())
}

/// Updating without bmc_ip_address should not touch any machine
/// interface -- only the expected device record is updated.
#[sqlx_test]
async fn test_update_without_bmc_ip_does_not_touch_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:86".parse().unwrap();
    let bmc_ip = "192.0.1.196";

    // Add with bmc_ip_address -- creates an interface.
    env.api()
        .add_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            shelf_serial_number: "PS-NOTOUCH-001".into(),
            bmc_ip_address: bmc_ip.into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;
    carbide_site_explorer::try_preallocate_one(
        &env.api().database_connection,
        bmc_mac,
        bmc_ip.parse().unwrap(),
        model::machine_interface::InterfaceType::Bmc,
        "expected_power_shelf BMC",
        None,
    )
    .await;

    // Update without bmc_ip_address (just changing credentials).
    env.api()
        .update_expected_power_shelf(tonic::Request::new(rpc::forge::ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "NEW-ADMIN".into(),
            bmc_password: "NEW-PASS".into(),
            shelf_serial_number: "PS-NOTOUCH-001".into(),
            bmc_ip_address: "".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            rack_id: None,
            bmc_retain_credentials: None,
        }))
        .await?;

    // Verify the interface still has the original static IP.
    let mut txn = env.api().database_connection.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, bmc_mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert!(
        interfaces[0].addresses.contains(&bmc_ip.parse().unwrap()),
        "interface should still have the original IP after update without bmc_ip_address"
    );

    txn.rollback().await?;

    Ok(())
}
