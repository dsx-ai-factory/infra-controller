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
use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;

use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use rpc::forge::forge_server::Forge;

use crate::tests::common::api_fixtures::network_segment::create_network_segment;
use crate::tests::common::api_fixtures::{FIXTURE_DHCP_RELAY_ADDRESS, create_test_env};
use crate::tests::common::rpc_builder::DhcpDiscovery;
use crate::tests::machine_dhcp::{
    RPC_MESSAGE_KIND_V6_INFO_REQUEST, RPC_MESSAGE_KIND_V6_SOLICIT, add_ipv6_prefix,
    dhcpv6_discovery, dhcpv6_discovery_with_desired_address, enable_slaac_eui64_inference,
    expected_slaac_address, interface_addresses_for_mac,
};

/// Verify one physical NIC retains one interface while gaining independent v4 and v6 leases.
///
/// Cross-family DHCP must merge identity without allowing either protocol to
/// return or overwrite the other family's address.
#[crate::sqlx_test]
async fn test_dhcp_v6_solicit_merges_with_ipv4_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:01").unwrap();

    // Make the segment dual-stack before first contact; legacy v4 must not
    // preallocate a v6 DHCP row.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:2::/64", None).await?;

    // First create the legacy DHCPv4 interface and address.
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    assert!(v4_response.address.parse::<IpAddr>()?.is_ipv4());
    assert_eq!(v4_response.prefix, "192.0.2.0/24");
    assert_eq!(
        v4_response.gateway.as_deref(),
        Some(FIXTURE_DHCP_RELAY_ADDRESS)
    );

    // Read persisted state after v4 first contact; only the v4 DHCP row should exist.
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(v4_response.machine_interface_id, Some(interface_id));
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert!(addresses[0].address.is_ipv4());

    // Request DHCPv6 later for the same MAC; it should add only the v6 family.
    let v6_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:2::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert!(v6_response.address.parse::<IpAddr>()?.is_ipv6());
    assert_eq!(v6_response.prefix, "2001:db8:2::/64");
    assert!(v6_response.gateway.is_none());
    assert_eq!(
        v4_response.machine_interface_id,
        v6_response.machine_interface_id
    );

    // Verify persistence through a fresh DB read, not only the response values.
    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 2);
    assert!(addresses.iter().any(|address| {
        address.allocation_type == AllocationType::Dhcp && address.address.is_ipv4()
    }));
    assert!(addresses.iter().any(|address| {
        address.allocation_type == AllocationType::Dhcp && address.address.is_ipv6()
    }));

    Ok(())
}

/// Verify repeated information requests infer exactly one EUI-64 SLAAC address.
///
/// Options-only DHCPv6 observations may establish addressless identity, but
/// retrying them must not create duplicate address ownership.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_infers_one_slaac_eui64_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:02").unwrap();

    // Seed exactly one IPv6 /64 on the admin segment and send an information-request.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:3::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:3::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert!(response.gateway.is_none());
    assert_eq!(response.subdomain_id, Some(env.domain.into()));
    assert!(response.last_invalidation_time.is_some());

    // Read back the persisted address and confirm it is the EUI-64 SLAAC GUA.
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(
        response.fqdn,
        format!("{}.dwrt1.com", interfaces[0].hostname)
    );
    let interface_id = interfaces[0].id;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        addresses[0].address,
        IpAddr::V6(Ipv6Addr::from_str("2001:db8:3::ff:fe00:2").unwrap())
    );
    txn.rollback().await?;

    // Repeat the same inference; the family pre-check makes it idempotent.
    env.api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:3::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?;
    let mut txn = pool.begin().await?;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    txn.rollback().await?;

    Ok(())
}

/// Verify a non-/64 IPv6 prefix remains options-only even when SLAAC inference is enabled.
///
/// The service supports the prefix for DHCPv6 routing, but must not synthesize
/// an invalid EUI-64 address or stateful lease from an information request.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_with_non_64_prefix_returns_options_only(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:0d").unwrap();

    // Seed a single IPv6 prefix that enables v6 but is not SLAAC-eligible.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:f::/80", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:f::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert!(response.gateway.is_none());
    assert_eq!(response.segment_id, Some(env.admin_segment()));
    assert_eq!(response.subdomain_id, Some(env.domain.into()));
    assert!(response.last_invalidation_time.is_some());

    // Verify the observation persisted the interface identity, but no address.
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(response.machine_interface_id, Some(interfaces[0].id));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert!(addresses.is_empty());
    txn.rollback().await?;

    Ok(())
}

/// Verify an information request cannot reuse a MAC already owned by another segment.
///
/// The cross-family identity guard must run before options delivery and leave
/// the existing IPv4 interface unchanged without creating partial v6 state.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_non_reserved_segment_rejects_known_interface_on_other_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:17").unwrap();

    // Create an addressed v4 identity on another managed segment.
    let other_segment = create_network_segment(
        &env.api,
        "ADMIN_INFO_SRC",
        "192.0.41.0/24",
        "192.0.41.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, "192.0.41.1").tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response
        .machine_interface_id
        .expect("DHCP response should include an interface id");
    let v4_address: IpAddr = v4_response.address.parse()?;

    // Request v6 options on the original admin segment; the wrong-segment MAC
    // must reject before options construction.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:17::/64", None).await?;
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:17::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await
        .expect_err("wrong-segment known MAC should reject before options");
    assert_eq!(status.code(), tonic::Code::Internal);
    assert!(
        status
            .message()
            .contains("Network segment mismatch for existing MAC address")
    );

    // Verify the known interface stayed on its original segment with its v4 lease.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.segment_id, other_segment);
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].address, v4_address);

    // Verify no second managed interface or SLAAC row was created.
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    txn.rollback().await?;

    Ok(())
}

/// Verify an existing stateful DHCPv6 lease suppresses later SLAAC inference.
///
/// One interface must not accumulate mixed DHCP and SLAAC allocation types for
/// the same address family when a client changes message style.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_does_not_add_slaac_after_stateful(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:05").unwrap();

    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:7::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:7::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    let stateful_address: IpAddr = response.address.parse()?;

    let info_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:7::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();

    // The second exchange must return options only, not the stateful record.
    assert_eq!(info_response.address, "");
    assert_eq!(info_response.prefix, "");
    assert!(info_response.gateway.is_none());
    assert_eq!(
        info_response.machine_interface_id,
        response.machine_interface_id
    );
    assert_eq!(info_response.segment_id, Some(env.admin_segment()));
    assert_eq!(info_response.subdomain_id, Some(env.domain.into()));
    assert_eq!(info_response.fqdn, response.fqdn);
    assert_eq!(info_response.mtu, response.mtu);
    assert_eq!(info_response.ntp_servers, response.ntp_servers);
    assert!(info_response.last_invalidation_time.is_some());

    // A fresh persistence read must still expose exactly the stateful lease.
    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, stateful_address);

    Ok(())
}

/// Verify client-controlled IPv6 fields cannot redirect SLAAC ownership.
///
/// An information request must ignore an adversarial desired/relay address,
/// preserve the victim lease, and persist only the attacker's computed EUI-64.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_ignores_adversarial_ipv6_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let victim_mac = MacAddress::from_str("02:00:00:00:00:06").unwrap();
    let attacker_mac = MacAddress::from_str("02:00:00:00:00:07").unwrap();

    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:8::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let victim_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            victim_mac,
            "2001:db8:8::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    let victim_address: IpAddr = victim_response.address.parse()?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery_with_desired_address(
            attacker_mac,
            &victim_address.to_string(),
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
            victim_address,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");

    // Verify the victim's stateful row was not disturbed.
    let (_, victim_addresses) = interface_addresses_for_mac(&pool, victim_mac).await?;
    assert_eq!(victim_addresses.len(), 1);
    assert_eq!(victim_addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(victim_addresses[0].address, victim_address);

    // Verify the attacker persisted only its server-computed SLAAC address.
    let (_, attacker_addresses) = interface_addresses_for_mac(&pool, attacker_mac).await?;
    assert_eq!(attacker_addresses.len(), 1);
    assert_eq!(attacker_addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        attacker_addresses[0].address,
        expected_slaac_address("2001:db8:8::".parse()?, attacker_mac)
    );

    Ok(())
}
