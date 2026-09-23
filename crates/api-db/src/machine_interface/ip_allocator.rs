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
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

use carbide_network::ip::IpAddressFamily;
use mac_address::MacAddress;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::allocation_type::AllocationType;
use model::dns::{Domain, NewDomain};
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    AllocationStrategy, NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
};

use crate as db;
use crate::test_support::network_segment::admin_segment;

async fn init_dwrt1_domain(txn: &mut sqlx::PgTransaction<'_>) -> db::DatabaseResult<Domain> {
    db::dns::domain::persist(NewDomain::new("dwrt1.com"), txn.as_mut()).await
}

/// Test that machine_interface::create allocates the correct IPv4 address
/// from the admin segment (192.0.2.0/24 with num_reserved=3, gateway=.1).
#[crate::sqlx_test]
async fn test_machine_interface_create_with_ipv4_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    let network_segment = db::network_segment::persist(
        admin_segment("ADMIN_TEST", "192.0.2.0/24", "192.0.2.1", 3),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let network_prefix = network_segment
        .prefixes
        .first()
        .expect("network_segment should have had at least one prefix");

    // The next IP should be .3, since num_reserved = 3.
    let expected_ip = match network_prefix.prefix.ip() {
        IpAddr::V4(ip) => {
            let [o1, o2, o3, _] = ip.octets();
            Ipv4Addr::new(
                o1,
                o2,
                o3,
                network_prefix
                    .num_reserved
                    .try_into()
                    .expect("too many reserved IPs in admin segment"),
            )
        }
        _ => panic!("admin segment should have an IPv4 prefix"),
    };

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").as_ref().unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        interface.addresses.len(),
        1,
        "interface should have 1 address allocated"
    );
    assert_eq!(
        interface.addresses[0], expected_ip,
        "interface address should be the first available IP after reserved"
    );

    // Allocate a second interface and verify it gets a different address
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("ff:ff:ff:ff:ff:fe").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await
    .unwrap();

    assert_ne!(
        interface.addresses[0], interface2.addresses[0],
        "two allocations should produce different addresses"
    );

    Ok(())
}

/// Verify that machine_interface::create falls through to a later candidate
/// admin segment when the first candidate is exhausted.
#[crate::sqlx_test]
async fn test_machine_interface_create_falls_through_admin_segments(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create two tiny admin segments with one allocatable address each.
    let first_segment = db::network_segment::persist(
        admin_segment("ADMIN_TINY_1", "192.0.20.0/30", "192.0.20.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let second_segment = db::network_segment::persist(
        admin_segment("ADMIN_TINY_2", "192.0.21.0/30", "192.0.21.1", 2),
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let candidate_segments = [first_segment.clone(), second_segment.clone()];

    // Allocate the only usable address from the first candidate segment.
    let first_interface = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &MacAddress::from_str("aa:bb:cc:dd:ee:10").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(first_interface.segment_id, first_segment.id);
    assert_eq!(
        first_interface.addresses,
        vec![IpAddr::V4("192.0.20.2".parse()?)]
    );

    // Allocate again with the same candidates and verify it falls through.
    let second_interface = db::machine_interface::create(
        &mut txn,
        &candidate_segments,
        &MacAddress::from_str("aa:bb:cc:dd:ee:11").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let second_interface_id = second_interface.id;
    txn.commit().await?;

    // Re-read the second interface to verify the persisted segment and address.
    let mut txn = pool.begin().await?;
    let persisted_interface =
        db::machine_interface::find_one(txn.as_mut(), second_interface_id).await?;
    assert_eq!(persisted_interface.segment_id, second_segment.id);
    assert_eq!(
        persisted_interface.addresses,
        vec![IpAddr::V4("192.0.21.2".parse()?)]
    );

    Ok(())
}

/// Verify that machine_interface::create allocates from an IPv6-only segment.
#[crate::sqlx_test]
async fn test_machine_interface_create_with_ipv6_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;

    // Create an underlay segment with only an IPv6 prefix
    let new_ns = NewNetworkSegment {
        name: "IPV6-UNDERLAY-TEST".to_string(),
        subdomain_id: Some(domain.id),
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![NewNetworkPrefix {
            prefix: "2001:db8:abcd::0/112".parse().unwrap(),
            gateway: None,
            dhcpv6_link_address: None,
            num_reserved: 2,
        }],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        id: uuid::Uuid::new_v4().into(),
        can_stretch: None,
        allocation_strategy: AllocationStrategy::Dynamic,
        infer_slaac_eui64_addresses: false,
    };
    let network_segment =
        db::network_segment::persist(new_ns, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:ee:01").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    assert_eq!(
        interface.addresses.len(),
        1,
        "interface should have 1 address allocated"
    );
    let addr = interface.addresses[0];
    assert!(
        addr.is_ipv6(),
        "allocated address should be IPv6, got {addr}"
    );

    // Allocate a second interface to verify sequential allocation works
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:ee:02").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    let addr2 = interface2.addresses[0];
    assert!(
        addr2.is_ipv6(),
        "second address should be IPv6, got {addr2}"
    );
    assert_ne!(
        addr, addr2,
        "two allocations should produce different addresses"
    );

    Ok(())
}

/// An address can still belong to an interface on `static-assignments` when a
/// managed prefix is added around it later. Both initial DHCPv6 allocation and
/// family recovery must skip that address even though its owning interface
/// remains on a different segment.
#[crate::sqlx_test]
async fn test_ipv6_allocation_skips_address_owned_outside_managed_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    let static_assignments = db::network_segment::persist(
        NewNetworkSegment {
            name: db::network_segment::STATIC_ASSIGNMENTS_SEGMENT_NAME.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "169.254.254.254/32".parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Reserved,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let occupied_address: IpAddr = "2001:db8:4801::2".parse()?;
    db::machine_interface::preallocate_machine_interface(
        &mut txn,
        MacAddress::from_str("aa:bb:cc:dd:48:01")?,
        occupied_address,
        None,
    )
    .await?;

    let owner = db::machine_interface_address::find_by_address(txn.as_mut(), occupied_address)
        .await?
        .expect("static IPv6 owner should exist");
    assert_eq!(owner.segment_id, static_assignments.id);
    let owner_interface_id = owner.id;

    // Add the managed prefix after the static reservation. Its first DHCP
    // candidate is already owned by the interface on `static-assignments`.
    let managed_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-GLOBAL-OWNER".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:4801::/112".parse()?,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: Some(false),
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let next_available_address: IpAddr = "2001:db8:4801::3".parse()?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&managed_segment),
        &MacAddress::from_str("aa:bb:cc:dd:48:02")?,
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.addresses, vec![next_available_address]);

    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    let recovered = db::machine_interface::allocate_address_for_family(
        &mut txn,
        interface.id,
        &managed_segment,
        IpAddressFamily::Ipv6,
        AllocationType::Dhcp,
    )
    .await?;
    assert_eq!(recovered, vec![next_available_address]);

    let owner = db::machine_interface_address::find_by_address(txn.as_mut(), occupied_address)
        .await?
        .expect("static IPv6 owner should remain unchanged");
    assert_eq!(owner.id, owner_interface_id);

    Ok(())
}

/// Verify that a dual-stack segment (IPv4 + IPv6 prefixes) allocates one
/// address from each family, and that the hostname is derived from the IPv4
/// address (more human-readable).
#[crate::sqlx_test]
async fn test_machine_interface_create_dual_stack(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let domain = init_dwrt1_domain(&mut txn).await?;

    let new_ns = NewNetworkSegment {
        name: "DUAL-STACK-TEST".to_string(),
        subdomain_id: Some(domain.id),
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![
            NewNetworkPrefix {
                prefix: "10.99.1.0/24".parse().unwrap(),
                gateway: Some("10.99.1.1".parse().unwrap()),
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
            NewNetworkPrefix {
                prefix: "2001:db8:99::0/112".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
        ],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        id: uuid::Uuid::new_v4().into(),
        can_stretch: None,
        allocation_strategy: AllocationStrategy::Dynamic,
        infer_slaac_eui64_addresses: false,
    };
    let network_segment =
        db::network_segment::persist(new_ns, &mut txn, NetworkSegmentControllerState::Ready)
            .await?;

    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:00:00:01").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    // Dual-stack: should have one IPv4 and one IPv6 address
    assert_eq!(
        interface.addresses.len(),
        2,
        "dual-stack interface should have 2 addresses, got {:?}",
        interface.addresses
    );

    let has_v4 = interface.addresses.iter().any(|a| a.is_ipv4());
    let has_v6 = interface.addresses.iter().any(|a| a.is_ipv6());
    assert!(has_v4, "should have an IPv4 address");
    assert!(has_v6, "should have an IPv6 address");

    // Hostname should be derived from the IPv4 address (preferred for readability)
    assert!(
        !interface.hostname.contains(':'),
        "hostname should be derived from IPv4 (no colons), got: {}",
        interface.hostname
    );
    assert!(
        interface.hostname.contains('-'),
        "hostname should have dashes from IPv4 dot replacement, got: {}",
        interface.hostname
    );

    // Allocate a second interface and verify both families get new addresses
    let interface2 = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:00:00:02").unwrap(),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;

    assert_eq!(
        interface2.addresses.len(),
        2,
        "second dual-stack interface should also have 2 addresses"
    );

    // No addresses should overlap between the two interfaces
    for addr in &interface.addresses {
        assert!(
            !interface2.addresses.contains(addr),
            "address {addr} was allocated to both interfaces"
        );
    }

    Ok(())
}

/// Verifies that IPv6 single-address allocation works across representative
/// prefix widths that the old SQL fast path could not enumerate correctly.
///
/// This exercises both initial DHCP allocation and family-specific reallocation
/// so shifts around /64, /96, and wider prefixes stay IPv6-safe.
#[crate::sqlx_test]
async fn test_machine_interface_ipv6_allocation_shift_widths(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    for (index, (name, prefix)) in [
        ("IPV6-SHIFT-64", "2001:db8:6400::/64"),
        ("IPV6-SHIFT-65", "2001:db8:6500::/65"),
        ("IPV6-SHIFT-80", "2001:db8:8000::/80"),
        ("IPV6-SHIFT-96", "2001:db8:9600::/96"),
        ("IPV6-SHIFT-97", "2001:db8:9700::/97"),
        ("IPV6-SHIFT-112", "2001:db8:1120::/112"),
    ]
    .into_iter()
    .enumerate()
    {
        // Create an IPv6-only segment for the representative prefix width.
        let network_segment = db::network_segment::persist(
            NewNetworkSegment {
                name: name.to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![NewNetworkPrefix {
                    prefix: prefix.parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 2,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Underlay,
                id: uuid::Uuid::new_v4().into(),
                can_stretch: None,
                allocation_strategy: AllocationStrategy::Dynamic,
                infer_slaac_eui64_addresses: false,
            },
            &mut txn,
            NetworkSegmentControllerState::Ready,
        )
        .await?;

        // Initial DHCP allocation should succeed through the IPv6 allocator.
        let interface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&network_segment),
            &MacAddress::from_str(&format!("aa:bb:cc:dd:40:{index:02x}")).unwrap(),
            true,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await?;
        assert_eq!(interface.addresses.len(), 1);
        assert!(interface.addresses[0].is_ipv6());
        assert!(
            network_segment.prefixes[0]
                .prefix
                .contains(interface.addresses[0])
        );

        // Lease reallocation for the same family should use the same IPv6-safe path.
        db::machine_interface_address::delete(&mut txn, &interface.id).await?;
        let allocated = db::machine_interface::allocate_address_for_family(
            &mut txn,
            interface.id,
            &network_segment,
            IpAddressFamily::Ipv6,
            AllocationType::Dhcp,
        )
        .await?;
        assert_eq!(allocated.len(), 1);
        assert!(allocated[0].is_ipv6());
        assert!(network_segment.prefixes[0].prefix.contains(allocated[0]));
    }

    Ok(())
}

/// Verifies that IPv6 exhaustion on one candidate segment falls through to the
/// next candidate segment instead of aborting allocation.
///
/// This protects the fast path from treating IPv6 PrefixExhausted differently
/// than IPv4 ResourceExhausted when later segments still have capacity.
#[crate::sqlx_test]
async fn test_machine_interface_ipv6_exhausted_segment_falls_through(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create one exhausted IPv6 segment followed by one segment with capacity.
    let exhausted_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-EXHAUSTED-FIRST".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:7100::/127".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let available_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "IPV6-AVAILABLE-SECOND".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: "2001:db8:7200::/126".parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 2,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;

    // Allocate across both candidates; the exhausted first segment should not abort the request.
    let interface = db::machine_interface::create(
        &mut txn,
        &[exhausted_segment, available_segment.clone()],
        &MacAddress::from_str("aa:bb:cc:dd:41:00").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.segment_id, available_segment.id);
    assert_eq!(interface.addresses.len(), 1);
    assert!(
        available_segment.prefixes[0]
            .prefix
            .contains(interface.addresses[0])
    );
    let interface_id = interface.id;
    txn.commit().await?;

    // Re-read the interface to verify the fallback segment persisted.
    let mut txn = pool.begin().await?;
    let persisted = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(persisted.segment_id, available_segment.id);
    assert!(persisted.addresses.iter().any(|address| address.is_ipv6()));

    Ok(())
}

/// Verifies that DHCP reallocation can restore IPv4 and IPv6 independently on
/// a dual-stack segment.
///
/// This covers the family-filtered allocation wrapper used when one address
/// family must be refreshed without disturbing the other family.
#[crate::sqlx_test]
async fn test_allocate_address_for_family_dual_stack_round_trip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;

    // Create a dual-stack admin segment and allocate both families initially.
    let network_segment = db::network_segment::persist(
        NewNetworkSegment {
            name: "DUAL-STACK-REALLOCATE".to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![
                NewNetworkPrefix {
                    prefix: "10.88.1.0/24".parse().unwrap(),
                    gateway: Some("10.88.1.1".parse().unwrap()),
                    dhcpv6_link_address: None,
                    num_reserved: 1,
                },
                NewNetworkPrefix {
                    prefix: "2001:db8:8801::/64".parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 2,
                },
            ],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Underlay,
            id: uuid::Uuid::new_v4().into(),
            can_stretch: None,
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        },
        &mut txn,
        NetworkSegmentControllerState::Ready,
    )
    .await?;
    let interface = db::machine_interface::create(
        &mut txn,
        std::slice::from_ref(&network_segment),
        &MacAddress::from_str("aa:bb:cc:dd:40:ff").unwrap(),
        true,
        AddressSelectionStrategy::NextAvailableIp,
        None,
    )
    .await?;
    assert_eq!(interface.addresses.len(), 2);

    // Remove the leases, then recover each family independently.
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    let allocated_v4 = db::machine_interface::allocate_address_for_family(
        &mut txn,
        interface.id,
        &network_segment,
        IpAddressFamily::Ipv4,
        AllocationType::Dhcp,
    )
    .await?;
    let allocated_v6 = db::machine_interface::allocate_address_for_family(
        &mut txn,
        interface.id,
        &network_segment,
        IpAddressFamily::Ipv6,
        AllocationType::Dhcp,
    )
    .await?;

    // Re-read from the database to verify both family rows persisted.
    let persisted = db::machine_interface::find_one(txn.as_mut(), interface.id).await?;
    assert_eq!(allocated_v4.len(), 1);
    assert_eq!(allocated_v6.len(), 1);
    assert!(allocated_v4[0].is_ipv4());
    assert!(allocated_v6[0].is_ipv6());
    assert!(persisted.addresses.iter().any(|address| address.is_ipv4()));
    assert!(persisted.addresses.iter().any(|address| address.is_ipv6()));

    Ok(())
}
