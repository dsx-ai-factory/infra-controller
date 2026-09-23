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

use std::net::IpAddr;
use std::str::FromStr;

use carbide_api_core::cfg::file::CarbideConfig;
use carbide_api_core::test_support::default_config;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use rpc::forge::forge_server::Forge;
use rpc::forge::{DhcpDiscovery, ExpireDhcpLeaseRequest, ExpireDhcpLeaseStatus};
use tonic::Request;

#[sqlx_test]
async fn test_expire_releases_allocation(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let relay: std::net::IpAddr = admin_segment.relay_address;

    // Create an interface with an allocated IP
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:01").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let ip = interface.addresses[0];
    txn.commit().await?;

    // Expire the lease via the RPC endpoint — currently blocked, returns FeatureDisabled.
    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: ip.to_string(),
            mac_address: None,
        }))
        .await?;

    let resp = response.into_inner();
    assert_eq!(resp.ip_address, ip.to_string());
    assert_eq!(resp.status(), ExpireDhcpLeaseStatus::FeatureDisabled);

    // Address and interface must both still exist (expiry is blocked).
    let mut txn = env.db_txn().await;
    let addr =
        db::machine_interface_address::find_ipv4_for_interface(&mut txn, interface.id).await?;
    assert_eq!(
        addr.address, ip,
        "address should still exist while expiry is blocked"
    );

    let iface = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    assert_eq!(iface.id, interface.id, "interface should still exist");
    txn.commit().await?;

    Ok(())
}

async fn init(pool: PgPool) -> (TestHarness, TestNetworkSegment) {
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(|b| {
            b.with_runtime_config(
                CarbideConfig {
                    dhcp_lease_expiry_handling: true,
                    ..default_config::get()
                }
                .into(),
            )
        })
        .build()
        .await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    (env, admin_segment)
}

#[sqlx_test]
async fn test_expire_nonexistent_address_returns_not_found(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _) = init(pool).await;

    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: "10.99.99.99".to_string(),
            mac_address: None,
        }))
        .await?;

    let resp = response.into_inner();
    assert_eq!(resp.ip_address, "10.99.99.99");
    assert_eq!(resp.status(), ExpireDhcpLeaseStatus::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_expire_invalid_address_fails(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _) = init(pool).await;

    let result = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: "not-an-ip".to_string(),
            mac_address: None,
        }))
        .await;

    assert!(result.is_err(), "invalid IP address should fail");

    Ok(())
}

#[sqlx_test]
async fn test_expire_ipv6_address(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _) = init(pool).await;

    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: "fd00::42".to_string(),
            mac_address: None,
        }))
        .await?;

    let resp = response.into_inner();
    assert_eq!(resp.ip_address, "fd00::42");
    assert_eq!(resp.status(), ExpireDhcpLeaseStatus::NotFound);

    Ok(())
}

#[sqlx_test]
async fn test_discover_reallocates_after_expiration(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let mac_address = "aa:bb:cc:dd:ee:07";

    // First, we do the initial DHCP discover, which
    // creates an interface and allocates an IP.
    let response1 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let original_ip = response1.address.clone();
    assert!(
        !original_ip.is_empty(),
        "should get an IP on first discover"
    );
    env.api()
        .add_expected_machine(Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: "aa:bb:cc:dd:ef:07".into(),
            chassis_serial_number: "EXPIRED-DHCP-STAYS-DYNAMIC".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: mac_address.into(),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;
    let expire_response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: original_ip.clone(),
            mac_address: None,
        }))
        .await?
        .into_inner();
    assert_eq!(expire_response.status(), ExpireDhcpLeaseStatus::Released);

    // The unassociated interface survives expiry. Reallocation must still
    // allow another shared IPv4 allocator on the same segment.
    let mut shared_allocation = env.db_txn().await;
    db::machine_interface::lock_network_segments_shared(
        &mut shared_allocation,
        std::slice::from_ref(&admin_segment.id),
    )
    .await?;
    let response2 = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        env.api().discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        ),
    )
    .await??
    .into_inner();
    shared_allocation.rollback().await?;
    assert!(
        !response2.address.is_empty(),
        "should get an IP after re-allocation"
    );

    // Verify the interface is be the same one (preserved across expiration).
    assert_eq!(
        response1.machine_interface_id, response2.machine_interface_id,
        "should reuse the same interface"
    );
    let mut txn = env.db_txn().await;
    let addresses = db::machine_interface_address::find_for_interface(
        &mut txn,
        response2
            .machine_interface_id
            .expect("DHCP should identify the interface"),
    )
    .await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn test_expire_does_not_delete_static_allocation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _) = init(pool).await;
    let static_ip: IpAddr = "192.0.2.200".parse().unwrap();

    // Create an interface with a static IP via the proper create path.
    let mut txn = env.db_txn().await;
    let segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &MacAddress::from_str("aa:bb:cc:dd:ee:08").unwrap(),
        true,
        AddressSelectionStrategy::StaticAddress(static_ip),
        None,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(interface.addresses[0], static_ip);

    // Try to expire it -- should NOT delete because it's static.
    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: static_ip.to_string(),
            mac_address: None,
        }))
        .await?
        .into_inner();
    assert_eq!(
        response.status(),
        ExpireDhcpLeaseStatus::NotFound,
        "static allocation should not be expired"
    );

    // Verify the address still exists.
    let mut txn = env.db_txn().await;
    let addr =
        db::machine_interface_address::find_ipv4_for_interface(&mut txn, interface.id).await?;
    txn.commit().await?;
    assert_eq!(addr.address, static_ip, "static address should still exist");

    Ok(())
}

#[sqlx_test]
async fn test_expire_does_not_delete_slaac_allocation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let relay: std::net::IpAddr = admin_segment.relay_address;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:0e").unwrap();
    let slaac_ip: IpAddr = "2001:db8:2::ff:fe00:e".parse().unwrap();

    // Create a normal DHCP interface, then add an observed SLAAC address.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        mac,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::insert(&mut txn, interface.id, slaac_ip, AllocationType::Slaac)
        .await?;
    txn.commit().await?;

    // Try to expire the SLAAC address; only DHCP allocations are releasable.
    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: slaac_ip.to_string(),
            mac_address: Some(mac.to_string()),
        }))
        .await?
        .into_inner();
    assert_eq!(response.status(), ExpireDhcpLeaseStatus::NotFound);

    // Verify through a fresh DB read that the SLAAC row remained.
    let mut txn = env.db_txn().await;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface.id).await?;
    txn.commit().await?;
    assert!(
        addresses.iter().any(|address| address.address == slaac_ip
            && address.allocation_type == AllocationType::Slaac)
    );

    Ok(())
}

#[sqlx_test]
async fn test_static_address_survives_expiration_and_rediscover(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:09").unwrap();
    let static_ip: IpAddr = "192.0.2.201".parse().unwrap();

    // Create an interface with a static IP via the proper create path.
    let mut txn = env.db_txn().await;
    let segment = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&segment),
        &mac,
        true,
        AddressSelectionStrategy::StaticAddress(static_ip),
        None,
    )
    .await?;
    txn.commit().await?;

    assert_eq!(interface.addresses[0], static_ip);

    // Device goes offline, Kea expires the lease.
    let expire_response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: static_ip.to_string(),
            mac_address: None,
        }))
        .await?
        .into_inner();
    assert_eq!(
        expire_response.status(),
        ExpireDhcpLeaseStatus::NotFound,
        "static address should not be expired"
    );

    // Device comes back online, sends DHCP discover.
    let mac_str = mac.to_string();
    let discover_response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();

    // Should get the same static IP back, on the same interface.
    assert_eq!(
        discover_response.address,
        static_ip.to_string(),
        "should get the same static IP back"
    );
    assert_eq!(
        discover_response.machine_interface_id.unwrap(),
        interface.id,
        "should reuse the same interface"
    );

    Ok(())
}

#[sqlx_test]
async fn test_expire_with_matching_mac_releases(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let relay: std::net::IpAddr = admin_segment.relay_address;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:0a").unwrap();

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        mac,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let ip = interface.addresses[0];
    txn.commit().await?;
    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: ip.to_string(),
            mac_address: Some(mac.to_string()),
        }))
        .await?
        .into_inner();
    assert_eq!(response.status(), ExpireDhcpLeaseStatus::Released);

    Ok(())
}

#[sqlx_test]
async fn test_expire_resets_hostname_and_discover_restores_it(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let mac_address = "aa:bb:cc:dd:ee:0d";

    // Initial discover: interface is created and an IP-derived hostname is set.
    let response1 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let original_ip = response1.address.clone();
    assert!(
        !original_ip.is_empty(),
        "should get an IP on first discover"
    );

    // The hostname must match the IP (dots replaced with dashes for IPv4).
    let interface_id = response1.machine_interface_id.unwrap();
    let mut txn = env.db_txn().await;
    let iface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    let expected_hostname = original_ip.replace('.', "-");
    assert_eq!(
        iface.hostname, expected_hostname,
        "hostname should be derived from the allocated IP"
    );
    txn.commit().await?;

    // Expire the lease: address is removed and hostname resets to dormant format.
    let expire_response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: original_ip.clone(),
            mac_address: None,
        }))
        .await?
        .into_inner();
    assert_eq!(expire_response.status(), ExpireDhcpLeaseStatus::Released);

    // Hostname should have reset to the dormant n-<mac> placeholder.
    let mut txn = env.db_txn().await;
    let iface_after_expiry = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert!(
        iface_after_expiry
            .hostname
            .to_lowercase()
            .starts_with("noip"),
        "hostname should reset to dormant format after expiry, got: {}",
        iface_after_expiry.hostname,
    );
    txn.commit().await?;

    // Re-discover: a new IP is allocated and the hostname must be updated.
    let response2 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let new_ip = response2.address.clone();
    assert!(!new_ip.is_empty(), "should get an IP after re-allocation");

    let mut txn = env.db_txn().await;
    let iface_after_rediscover = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    let expected_new_hostname = new_ip.replace('.', "-");
    assert_eq!(
        iface_after_rediscover.hostname.to_lowercase(),
        expected_new_hostname.to_lowercase(),
        "hostname should match the newly allocated IP after rediscover"
    );
    txn.commit().await?;

    Ok(())
}

#[sqlx_test]
async fn test_expire_with_mismatched_mac_is_no_op(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Simulates a race where MacAddressA expires after the IP was already
    // re-allocated to MacAddressB. This late expiration hook should not
    // delete MacAddressB's record/row.
    let (env, admin_segment) = init(pool).await;
    let relay: std::net::IpAddr = admin_segment.relay_address;
    let mac_b = MacAddress::from_str("aa:bb:cc:dd:ee:0b").unwrap();
    let mac_a_stale = MacAddress::from_str("aa:bb:cc:dd:ee:0c").unwrap();

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        mac_b,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let ip = interface.addresses[0];
    txn.commit().await?;

    // Slow expire hook for the MacAddressA at this IP.
    let response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: ip.to_string(),
            mac_address: Some(mac_a_stale.to_string()),
        }))
        .await?
        .into_inner();
    assert_eq!(
        response.status(),
        ExpireDhcpLeaseStatus::NotFound,
        "late expire hook with previous MAC must not delete new record"
    );

    // Verify MacAddressB's record is still in tact.
    let mut txn = env.db_txn().await;
    let addr =
        db::machine_interface_address::find_ipv4_for_interface(&mut txn, interface.id).await?;
    assert_eq!(addr.address, ip, "MacAddressB address should still exist");
    txn.commit().await?;

    Ok(())
}

/// Regression for #3383 (review follow-up): the MAC-scoped expiry path must
/// resync the interface that actually owned the deleted (ip, mac) row — derived
/// from the delete itself — and leave other interfaces on the segment untouched.
#[sqlx_test]
async fn test_mac_scoped_expiry_resyncs_only_the_owner(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, admin_segment) = init(pool).await;
    let owner_mac = "aa:bb:cc:dd:ee:20";
    let other_mac = "aa:bb:cc:dd:ee:21";

    // Two interfaces, each with its own DHCP address and IP-derived hostname.
    let owner = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(owner_mac, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let owner_ip = owner.address.clone();
    let owner_id = owner.machine_interface_id.unwrap();

    let other = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(other_mac, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let other_id = other.machine_interface_id.unwrap();

    let mut txn = env.db_txn().await;
    let other_hostname_before = db::machine_interface::find_one(&mut *txn, other_id)
        .await?
        .hostname;
    txn.commit().await?;

    // Expire the owner's lease scoped by (ip, mac).
    let expire_response = env
        .api()
        .expire_dhcp_lease(Request::new(ExpireDhcpLeaseRequest {
            ip_address: owner_ip,
            mac_address: Some(owner_mac.to_string()),
        }))
        .await?
        .into_inner();
    assert_eq!(expire_response.status(), ExpireDhcpLeaseStatus::Released);

    // The owner is resynced to the dormant placeholder; the other interface is
    // left exactly as it was.
    let mut txn = env.db_txn().await;
    let owner_after = db::machine_interface::find_one(&mut *txn, owner_id).await?;
    let other_after = db::machine_interface::find_one(&mut *txn, other_id).await?;
    txn.commit().await?;
    assert!(
        owner_after.hostname.to_lowercase().starts_with("noip"),
        "the (ip, mac) owner should be resynced to dormant, got: {}",
        owner_after.hostname,
    );
    assert_eq!(
        other_after.hostname, other_hostname_before,
        "an unrelated interface must not be resynced"
    );

    Ok(())
}
