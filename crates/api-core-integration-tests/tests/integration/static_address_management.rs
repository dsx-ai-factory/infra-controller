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

use carbide_api_core::test_support::network_segment::create_static_assignments_segment;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use rpc::forge::{
    AssignStaticAddressRequest, AssignStaticAddressStatus, DhcpDiscovery,
    FindInterfaceAddressesRequest, RemoveStaticAddressRequest, RemoveStaticAddressStatus,
};
use tonic::Request;

struct StaticAddressTestEnv {
    env: TestHarness,
    admin_segment: TestNetworkSegment,
    underlay_segment: TestNetworkSegment,
}

async fn init(pool: PgPool) -> StaticAddressTestEnv {
    let env = TestHarness::builder(pool).build().await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    create_static_assignments_segment(env.api(), Some(domain.id)).await;
    StaticAddressTestEnv {
        env,
        admin_segment,
        underlay_segment,
    }
}

#[sqlx_test]
async fn test_assign_static_address(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create an interface (via DHCP discovery to get an interface_id).
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:10").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    // Delete the DHCP address so we can assign a static one for this family.
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    // Assign a static address.
    let resp = env
        .api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.210".to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(resp.status(), AssignStaticAddressStatus::Assigned);
    assert_eq!(resp.ip_address, "192.0.2.210");

    Ok(())
}

#[sqlx_test]
async fn test_assign_replaces_existing_static(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:11").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    // Assign first static address.
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.211".to_string(),
        }))
        .await?;

    // Assign a different static address for the same family,
    // which should replace.
    let resp = env
        .api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.212".to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(resp.status(), AssignStaticAddressStatus::ReplacedStatic);
    assert_eq!(resp.ip_address, "192.0.2.212");

    // Verify only the new address exists.
    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(interface.id),
        }))
        .await?
        .into_inner();

    assert_eq!(addrs.addresses.len(), 1);
    assert_eq!(addrs.addresses[0].address, "192.0.2.212");
    assert_eq!(addrs.addresses[0].allocation_type, "static");

    Ok(())
}

#[sqlx_test]
async fn test_assign_takes_over_dhcp_allocation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create interface with DHCP-allocated IPv4.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:12").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let dhcp_ip = interface.addresses[0];
    txn.commit().await?;

    // And now assign a static IPv4 over the DHCP allocation,
    // which should take over.
    let resp = env
        .api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.213".to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(resp.status(), AssignStaticAddressStatus::ReplacedDhcp);
    assert_eq!(resp.ip_address, "192.0.2.213");

    // Verify the old DHCP address is gone and the static one is there.
    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(interface.id),
        }))
        .await?
        .into_inner();

    assert_eq!(addrs.addresses.len(), 1);
    assert_eq!(addrs.addresses[0].address, "192.0.2.213");
    assert_eq!(addrs.addresses[0].allocation_type, "static");
    assert_ne!(
        addrs.addresses[0].address,
        dhcp_ip.to_string(),
        "old DHCP address should be gone"
    );

    Ok(())
}

#[sqlx_test]
async fn test_remove_static_address(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:13").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    // Assign then remove.
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.214".to_string(),
        }))
        .await?;

    let resp = env
        .api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.214".to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(resp.status(), RemoveStaticAddressStatus::Removed);

    // Verify no addresses remain.
    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(interface.id),
        }))
        .await?
        .into_inner();

    assert!(addrs.addresses.is_empty());

    Ok(())
}

#[sqlx_test]
async fn test_remove_nonexistent_returns_not_found(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:14").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    txn.commit().await?;

    let resp = env
        .api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "10.99.99.99".to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(resp.status(), RemoveStaticAddressStatus::NotFound);

    Ok(())
}

/// Trying to remove a DHCP-allocated address via remove-address should
/// return NotFound -- remove-address only operates on static allocations.
/// DHCP allocations are managed by the lease expiration flow.
#[sqlx_test]
async fn test_remove_dhcp_address_returns_not_found(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create an interface with a DHCP-allocated IP.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:25").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    let dhcp_ip = interface.addresses[0];
    txn.commit().await?;

    // Try to remove it via remove-address -- should return NotFound
    // because the address is DHCP, not static.
    let resp = env
        .api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: dhcp_ip.to_string(),
        }))
        .await?
        .into_inner();

    assert_eq!(
        resp.status(),
        RemoveStaticAddressStatus::NotFound,
        "remove-address should not delete DHCP allocations"
    );

    // Verify the DHCP address is still there.
    let mut txn = env.db_txn().await;
    let addr =
        db::machine_interface_address::find_ipv4_for_interface(&mut txn, interface.id).await?;
    assert_eq!(addr.address, dhcp_ip, "DHCP address should still exist");
    txn.rollback().await?;

    Ok(())
}

#[sqlx_test]
async fn test_find_interface_addresses_shows_types(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create interface with DHCP address.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:15").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    txn.commit().await?;

    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(interface.id),
        }))
        .await?
        .into_inner();

    assert_eq!(addrs.addresses.len(), 1);
    assert_eq!(addrs.addresses[0].allocation_type, "dhcp");

    Ok(())
}

#[sqlx_test]
async fn test_assign_remove_then_dhcp_reallocates(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:16").unwrap();
    let static_ip = "192.0.2.216";

    // First, create the interface, clear its DHCP address, and
    // a assign static one.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        mac,
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: static_ip.to_string(),
        }))
        .await?;

    // A later expected declaration must not be reapplied after an explicit
    // address removal.
    env.api()
        .add_expected_machine(Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: "aa:bb:cc:dd:ef:16".into(),
            chassis_serial_number: "EXPLICIT-REMOVE-RETURNS-TO-DHCP".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: mac.to_string(),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // Now, remove the static address.
    let remove_resp = env
        .api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: static_ip.to_string(),
        }))
        .await?
        .into_inner();
    assert_eq!(remove_resp.status(), RemoveStaticAddressStatus::Removed);

    // And then fire off a DHCPDISCOVER -- the interface has no addresses,
    // should it should re-allocate a new one that is DHCP-managed.
    let mac_str = mac.to_string();
    let discover_resp = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();

    assert!(
        !discover_resp.address.is_empty(),
        "should get a DHCP address after static was removed"
    );
    assert_eq!(
        discover_resp.machine_interface_id.unwrap(),
        interface.id,
        "should reuse the same interface"
    );

    // Moment of truth.. verify it's a DHCP allocation.
    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(interface.id),
        }))
        .await?
        .into_inner();
    assert_eq!(addrs.addresses.len(), 1);
    assert_eq!(addrs.addresses[0].allocation_type, "dhcp");

    Ok(())
}

/// Regression for #3383: assign-address and remove-address must keep the
/// interface's IP-derived hostname consistent with its address, like every
/// other allocation path. A stale hostname encoding a now-freed IP collides
/// with the fqdn_must_be_unique constraint and wedges DHCP allocation on the
/// whole segment.
#[sqlx_test]
async fn test_assign_and_remove_resync_hostname(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:17").unwrap();
    let mac_str = mac.to_string();

    // Let the interface get a DHCP address; its hostname is derived from it.
    let discover_resp = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let dhcp_ip = discover_resp.address.clone();
    let interface_id = discover_resp.machine_interface_id.unwrap();

    let mut txn = env.db_txn().await;
    let iface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(
        iface.hostname,
        dhcp_ip.replace('.', "-"),
        "hostname should be derived from the DHCP address"
    );
    txn.commit().await?;

    // Move it to a different static IP (same family, so it replaces the DHCP
    // one). The hostname must follow the new address, not stay pinned to the
    // now-freed DHCP IP.
    let static_ip = "192.0.2.217";
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface_id),
            ip_address: static_ip.to_string(),
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let iface_after_assign = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(
        iface_after_assign.hostname,
        static_ip.replace('.', "-"),
        "hostname should resync to the newly assigned static address"
    );
    txn.commit().await?;

    // Remove the static address; with no addresses left the hostname must reset
    // to the dormant placeholder rather than keep encoding the freed IP.
    env.api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface_id),
            ip_address: static_ip.to_string(),
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let iface_after_remove = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert!(
        iface_after_remove
            .hostname
            .to_lowercase()
            .starts_with("noip"),
        "hostname should reset to dormant format after removal, got: {}",
        iface_after_remove.hostname,
    );
    txn.commit().await?;

    Ok(())
}

/// Regression for #3383 (review follow-up): remove-address is scoped to the
/// caller's interface. Passing an interface_id that does not own the IP must not
/// remove another interface's address — it returns NotFound and changes nothing.
#[sqlx_test]
async fn test_remove_with_mismatched_interface_id_is_rejected(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;

    // Owner interface: give it a static address so its hostname is IP-derived.
    let owner_mac = MacAddress::from_str("aa:bb:cc:dd:ee:18")
        .unwrap()
        .to_string();
    let owner = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&owner_mac, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let owner_id = owner.machine_interface_id.unwrap();
    let owned_ip = "192.0.2.218";
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(owner_id),
            ip_address: owned_ip.to_string(),
        }))
        .await?;

    let mut txn = env.db_txn().await;
    let owner_hostname_before = db::machine_interface::find_one(&mut *txn, owner_id)
        .await?
        .hostname;
    txn.commit().await?;

    // A second, unrelated interface whose id we will (incorrectly) pass to
    // remove-address.
    let other_mac = MacAddress::from_str("aa:bb:cc:dd:ee:19")
        .unwrap()
        .to_string();
    let other = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&other_mac, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let other_id = other.machine_interface_id.unwrap();

    // Remove the owner's IP but pass the *other* interface's id.
    let resp = env
        .api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(other_id),
            ip_address: owned_ip.to_string(),
        }))
        .await?
        .into_inner();
    assert_eq!(resp.status(), RemoveStaticAddressStatus::NotFound);

    // The owner still holds its address and keeps its IP-derived hostname; the
    // mismatched interface was not touched.
    let addrs = env
        .api()
        .find_interface_addresses(Request::new(FindInterfaceAddressesRequest {
            interface_id: Some(owner_id),
        }))
        .await?
        .into_inner();
    assert_eq!(addrs.addresses.len(), 1);
    assert_eq!(addrs.addresses[0].address, owned_ip);

    let mut txn = env.db_txn().await;
    let owner_after = db::machine_interface::find_one(&mut *txn, owner_id).await?;
    txn.commit().await?;
    assert_eq!(
        owner_after.hostname, owner_hostname_before,
        "the owner's hostname must be unchanged after a mismatched remove"
    );

    Ok(())
}

/// When assigning a static IP that's within a managed segment's prefix,
/// the interface's segment_id should be updated to that segment.
#[sqlx_test]
async fn test_assign_moves_interface_to_correct_segment(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env,
        admin_segment,
        underlay_segment,
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create interface on the admin segment via DHCP.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:20").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    // Interface is on the admin segment (192.0.2.0/24).
    assert_eq!(interface.segment_id, admin_segment.id);

    // Assign a static IP in the underlay segment's range (192.0.1.0/24).
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.1.180".to_string(),
        }))
        .await?;

    // Verify the interface moved to the underlay segment.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    assert_eq!(
        updated.segment_id, underlay_segment.id,
        "interface should have moved to the underlay segment"
    );
    txn.rollback().await?;

    Ok(())
}

/// When assigning an external static IP (not in any managed prefix),
/// the interface's segment_id should be updated to static-assignments.
#[sqlx_test]
async fn test_assign_external_ip_moves_to_static_assignments(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create interface on the admin segment via DHCP.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:21").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    assert_eq!(interface.segment_id, admin_segment.id);

    // Assign an external IP (not in any managed segment).
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "10.50.1.100".to_string(),
        }))
        .await?;

    // Verify the interface moved to static-assignments with the segment's domain.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    let static_seg = db::network_segment::static_assignments(&mut txn).await?;
    assert_eq!(
        updated.segment_id, static_seg.id,
        "interface should have moved to the static-assignments segment"
    );
    assert_eq!(
        updated.domain_id, static_seg.config.subdomain_id,
        "domain_id should match the static-assignments segment's subdomain"
    );
    txn.rollback().await?;

    Ok(())
}

/// Verifies that an external static IPv6 assignment moves the interface to the
/// static-assignments segment and that the anchor segment is dual-stack.
///
/// This covers the IPv6 counterpart to external static IPv4 assignment: static
/// addresses outside managed prefixes must still land on the durable
/// static-assignment topology.
#[sqlx_test]
async fn test_assign_external_ipv6_matching_link_address_moves_to_dual_stack_static_assignments(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create an addressless interface that can safely receive a static IPv6 assignment.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:2a").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    // Mark the external address as DHCPv6 relay context on a managed segment.
    // Static ownership must still use prefix containment only.
    let requested_ipv6_address: IpAddr = "2001:db8:ffff::100".parse().unwrap();
    let mut txn = env.db_txn().await;
    sqlx::query("UPDATE network_prefixes SET dhcpv6_link_address = $2::inet WHERE segment_id = $1")
        .bind(admin_segment.id)
        .bind(requested_ipv6_address)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    // Assign an external IPv6 address that is outside managed network prefixes.
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: requested_ipv6_address.to_string(),
        }))
        .await?;

    // Re-read the interface and static segment to verify the durable topology.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    let static_seg = db::network_segment::static_assignments(&mut txn).await?;
    assert_eq!(
        updated.segment_id, static_seg.id,
        "interface should have moved to the static-assignments segment"
    );
    assert!(
        updated.addresses.contains(&requested_ipv6_address),
        "interface should carry the assigned IPv6 address"
    );
    assert!(
        static_seg
            .prefixes
            .iter()
            .any(|prefix| prefix.prefix.is_ipv4()),
        "static-assignments should keep its IPv4 placeholder"
    );
    assert!(
        static_seg
            .prefixes
            .iter()
            .any(|prefix| prefix.prefix.is_ipv6()),
        "static-assignments should include an IPv6 placeholder"
    );
    txn.rollback().await?;

    Ok(())
}

/// When assigning a static IP within the interface's current segment,
/// the segment_id should not change.
#[sqlx_test]
async fn test_assign_within_same_segment_no_move(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let relay = admin_segment.relay_address;

    // Create interface on the admin segment via DHCP.
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:ee:22").unwrap(),
        std::slice::from_ref(&relay),
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    txn.commit().await?;

    let original_segment = interface.segment_id;

    // Assign a static IP within the same admin segment (192.0.2.0/24).
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.2.220".to_string(),
        }))
        .await?;

    // Verify the segment didn't change.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    assert_eq!(
        updated.segment_id, original_segment,
        "interface should stay on the same segment"
    );
    txn.rollback().await?;

    Ok(())
}

/// When an interface is on static-assignments (external IP was assigned
/// then removed), DHCP discover should move it back to the correct
/// segment based on the relay address and allocate a new IP.
#[sqlx_test]
async fn test_dhcp_moves_interface_back_from_static_assignments(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:23").unwrap();

    // First, the initial DHCP discover, which creates an
    // interface on the admin segment.
    let mac_str = mac.to_string();
    let response1 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let interface_id = response1.machine_interface_id.unwrap();

    // Next, assign an external static IP, which moves the interface
    // to static-assignments.
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface_id),
            ip_address: "10.50.1.200".to_string(),
        }))
        .await?;

    // Verify it's on static-assignments.
    let mut txn = env.db_txn().await;
    let iface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    let static_seg = db::network_segment::static_assignments(&mut txn).await?;
    assert_eq!(iface.segment_id, static_seg.id);
    txn.commit().await?;

    // And then remove the static address.
    env.api()
        .remove_static_address(Request::new(RemoveStaticAddressRequest {
            interface_id: Some(interface_id),
            ip_address: "10.50.1.200".to_string(),
        }))
        .await?;

    // And now re-walk the DHCP discover path again, which should move
    // the interace back to admin segment and allocate an IP.
    let response2 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();

    assert!(
        !response2.address.is_empty(),
        "should get a new IP after DHCP re-discover"
    );
    assert_eq!(
        response2.machine_interface_id.unwrap(),
        interface_id,
        "should reuse the same interface"
    );

    // Verify the interface moved back to the admin segment
    // with the correct domain_id.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(
        updated.segment_id, admin_segment.id,
        "interface should have moved back to admin segment from static-assignments"
    );
    assert!(
        updated.domain_id.is_some(),
        "domain_id should be restored when moving back from static-assignments"
    );
    txn.rollback().await?;

    Ok(())
}

/// When an interface is on static-assignments and still has a static
/// address, DHCP discover should NOT move it back -- the operator's
/// static assignment takes priority over DHCP.
#[sqlx_test]
async fn test_dhcp_does_not_move_interface_with_static_address(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let mac = MacAddress::from_str("aa:bb:cc:dd:ee:24").unwrap();

    // First, the DHCP discover flow -- create an interface on
    // the admin segment.
    let mac_str = mac.to_string();
    let response1 = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();
    let interface_id = response1.machine_interface_id.unwrap();

    // And now assign external static IP -- this will move the
    // interface to static-assignments.
    env.api()
        .assign_static_address(Request::new(AssignStaticAddressRequest {
            interface_id: Some(interface_id),
            ip_address: "10.50.1.201".to_string(),
        }))
        .await?;

    // Verify it's on static-assignments with the static IP.
    let mut txn = env.db_txn().await;
    let static_seg = db::network_segment::static_assignments(&mut txn).await?;
    let iface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(iface.segment_id, static_seg.id);
    assert!(
        !iface.addresses.is_empty(),
        "should have the static address"
    );
    txn.commit().await?;

    // Hit the DHCP discover again -- the interface has an external static
    // IP on static-assignments. The discover flow finds the interface but
    // doesn't move it (static takes priority). The DHCP record view will
    // fail because the external IP isn't within any managed prefix, which
    // is correct -- we don't serve DHCP for external networks.
    let result = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await;

    // The discover fails (external IP can't be in the DHCP records view),
    // but the important thing is the interface was NOT moved.
    assert!(
        result.is_err(),
        "DHCP discover should fail for external static IP"
    );

    // Verify the interface is still on static-assignments, untouched.
    let mut txn = env.db_txn().await;
    let updated = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(
        updated.segment_id, static_seg.id,
        "interface should stay on static-assignments when it has a static address"
    );
    assert!(
        updated.addresses.contains(&"10.50.1.201".parse().unwrap()),
        "static address should still be intact"
    );
    txn.rollback().await?;

    Ok(())
}

/// On a reserved segment, a device with a static reservation gets
/// its reserved IP via DHCP.
#[sqlx_test]
async fn test_reserved_segment_serves_static_reservation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;
    let bmc_mac = MacAddress::from_str("aa:bb:cc:dd:ee:30").unwrap();
    let reserved_ip: IpAddr = "192.0.2.230".parse().unwrap();

    // Set the admin segment to reserved allocation strategy.
    let mut txn = env.db_txn().await;
    sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
        .bind(admin_segment.id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    // Create a static reservation for this MAC on the admin segment.
    let mut txn = env.db_txn().await;
    let admin_seg = db::network_segment::admin(&mut txn)
        .await?
        .into_iter()
        .next()
        .unwrap();
    db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&admin_seg),
        &bmc_mac,
        true,
        model::address_selection_strategy::AddressSelectionStrategy::StaticAddress(reserved_ip),
        None,
    )
    .await?;
    txn.commit().await?;

    // DHCP discover -- should get the reserved IP.
    let mac_str = bmc_mac.to_string();
    let response = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder(&mac_str, admin_segment.relay_address).tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.address,
        reserved_ip.to_string(),
        "should get the reserved IP"
    );

    Ok(())
}

/// On a reserved segment, a device without a static reservation
/// gets no DHCP response (the discover fails).
#[sqlx_test]
async fn test_reserved_segment_rejects_unknown_mac(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let StaticAddressTestEnv {
        env, admin_segment, ..
    } = init(pool).await;

    // Set the admin segment to reserved allocation strategy.
    let mut txn = env.db_txn().await;
    sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
        .bind(admin_segment.id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    // DHCP discover with an unknown MAC -- should fail.
    let result = env
        .api()
        .discover_dhcp(
            DhcpDiscovery::builder("aa:bb:cc:dd:ee:31", admin_segment.relay_address)
                .tonic_request(),
        )
        .await;

    assert!(
        result.is_err(),
        "reserved segment should reject unknown MAC"
    );

    Ok(())
}
