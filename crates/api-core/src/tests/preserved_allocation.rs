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

use carbide_uuid::machine::MachineInterfaceId;
use db::TransactionVending as _;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use rpc::forge::ExpectedInterfaceIpAllocation as Policy;
use rpc::forge::forge_server::Forge;
use tonic::Request;

use super::common::api_fixtures::network_segment::create_network_segment;
use super::common::api_fixtures::{FIXTURE_DHCP_RELAY_ADDRESS, create_test_env};
use super::common::rpc_builder::DhcpDiscovery;
use super::machine_dhcp::{
    RPC_MESSAGE_KIND_V6_INFO_REQUEST, RPC_MESSAGE_KIND_V6_SOLICIT, add_ipv6_prefix,
    dhcpv6_discovery, enable_slaac_eui64_inference, expected_slaac_address,
};

fn expected_machine(
    bmc_mac: MacAddress,
    mac: MacAddress,
    policy: Option<Policy>,
) -> rpc::forge::ExpectedMachine {
    rpc::forge::ExpectedMachine {
        bmc_mac_address: bmc_mac.to_string(),
        chassis_serial_number: format!("PRESERVED-{:02X}", mac.bytes()[5]),
        host_nics: vec![rpc::forge::ExpectedInterface {
            mac_address: Some(mac.to_string()),
            ip_allocation: policy.map(|policy| policy as i32),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Inspect both active and parked ownership. The active address view does not
/// expose the marker; the operator reservation list only shows parked rows.
async fn address_owner(
    pool: &sqlx::PgPool,
    address: IpAddr,
) -> Result<
    (
        Option<MachineInterfaceId>,
        AllocationType,
        Option<MacAddress>,
    ),
    sqlx::Error,
> {
    sqlx::query_as(
        "SELECT interface_id, allocation_type, reserved_by_mac
         FROM machine_interface_addresses WHERE address = $1::inet",
    )
    .bind(address)
    .fetch_one(pool)
    .await
}

#[crate::sqlx_test]
async fn preserved_dhcp_parks_and_restores_each_family(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = "02:00:00:00:00:41".parse()?;
    let bmc_mac = "02:00:00:00:01:41".parse()?;
    let expected = expected_machine(bmc_mac, mac, Some(Policy::Preserved));
    env.api
        .add_expected_machine(Request::new(expected.clone()))
        .await?;
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:41::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let discover_v4 = |relay: &str| {
        env.api
            .discover_dhcp(DhcpDiscovery::builder(mac, relay).tonic_request())
    };
    let discover_v6 = |kind| {
        env.api
            .discover_dhcp(dhcpv6_discovery(mac, "2001:db8:41::1", kind))
    };

    let v4 = discover_v4(FIXTURE_DHCP_RELAY_ADDRESS).await?.into_inner();
    let interface_id = v4.machine_interface_id.expect("DHCP created an interface");
    let v4_address: IpAddr = v4.address.parse()?;

    // Observation on the existing interface may infer a SLAAC address, but
    // only a stateful request may learn the preserved IPv6 address.
    discover_v6(RPC_MESSAGE_KIND_V6_INFO_REQUEST).await?;
    let slaac = expected_slaac_address("2001:db8:41::".parse()?, mac);
    assert_eq!(
        address_owner(&pool, slaac).await?,
        (Some(interface_id), AllocationType::Slaac, None),
    );
    let v6 = discover_v6(RPC_MESSAGE_KIND_V6_SOLICIT).await?.into_inner();
    assert_eq!(v6.machine_interface_id, Some(interface_id));
    let v6_address: IpAddr = v6.address.parse()?;
    assert!(v4_address.is_ipv4() && v6_address.is_ipv6());

    // Editing the declaration and renewing must not unmark an active address.
    let mut dynamic = expected.clone();
    dynamic.host_nics[0].ip_allocation = Some(Policy::Dynamic as i32);
    env.api
        .update_expected_machine(Request::new(dynamic))
        .await?;
    let renewed = discover_v4(FIXTURE_DHCP_RELAY_ADDRESS).await?.into_inner();
    assert_eq!(renewed.address, v4.address);
    for ip in [v4_address, v6_address] {
        assert_eq!(
            address_owner(&pool, ip).await?,
            (Some(interface_id), AllocationType::Static, Some(mac)),
        );
    }

    env.api
        .delete_interface(Request::new(rpc::forge::InterfaceDeleteQuery {
            id: Some(interface_id),
            mac_address: None,
        }))
        .await?;
    for ip in [v4_address, v6_address] {
        assert_eq!(
            address_owner(&pool, ip).await?,
            (None, AllocationType::Static, Some(mac))
        );
    }
    env.api
        .update_expected_machine(Request::new(expected))
        .await?;

    let other_segment = create_network_segment(
        &env.api,
        "OTHER_ADMIN",
        "192.0.12.0/24",
        "192.0.12.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;
    let status = discover_v4("192.0.12.1")
        .await
        .expect_err("the parked IPv4 address is outside the selected segment");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(status.message().contains(&v4.address), "{status}");
    assert!(
        status.message().contains(&other_segment.to_string()),
        "{status}"
    );
    assert_eq!(
        address_owner(&pool, v4_address).await?,
        (None, AllocationType::Static, Some(mac))
    );

    // Observe the replacement before its segment has a DNS domain. Without
    // SLAAC this row has no address to publish until a stateful restore.
    sqlx::query(
        "UPDATE network_segments SET subdomain_id = NULL, infer_slaac_eui64_addresses = false
         WHERE id = $1",
    )
    .bind(env.admin_segment())
    .execute(&pool)
    .await?;
    let observation = discover_v6(RPC_MESSAGE_KIND_V6_INFO_REQUEST)
        .await?
        .into_inner();
    let restored_id = observation
        .machine_interface_id
        .expect("the options request observed the replacement interface");
    assert_ne!(restored_id, interface_id);
    let observed = db::machine_interface::find_one(&pool, restored_id).await?;
    assert!(observed.addresses.is_empty());
    assert!(observed.domain_id.is_none());
    sqlx::query("UPDATE network_segments SET subdomain_id = $1 WHERE id = $2")
        .bind(env.domain)
        .bind(env.admin_segment())
        .execute(&pool)
        .await?;
    let zone_serial = || {
        sqlx::query_scalar::<_, i64>("SELECT (soa->>'serial')::bigint FROM domains WHERE id = $1")
            .bind(env.domain)
            .fetch_one(&pool)
    };
    let before_restore = zone_serial().await?;
    let restored_v4 = discover_v4(FIXTURE_DHCP_RELAY_ADDRESS).await?.into_inner();
    assert_eq!(restored_v4.machine_interface_id, Some(restored_id));
    assert_eq!(restored_v4.address, v4.address);
    let restored = db::machine_interface::find_one(&pool, restored_id).await?;
    assert_eq!(restored.domain_id, Some(env.domain.into()));
    assert_eq!(restored_v4.fqdn, format!("{}.dwrt1.com", restored.hostname));
    assert!(zone_serial().await? > before_restore);
    // An options request does not consume this MAC's parked IPv6 address.
    let options = discover_v6(RPC_MESSAGE_KIND_V6_INFO_REQUEST)
        .await?
        .into_inner();
    assert!(options.address.is_empty());
    assert_eq!(
        address_owner(&pool, v6_address).await?,
        (None, AllocationType::Static, Some(mac))
    );
    // The relay can still select this segment after its IPv6 prefix is removed.
    let mut txn = pool.txn_begin().await?;
    sqlx::query("DELETE FROM network_prefixes WHERE segment_id = $1 AND family(prefix) = 6")
        .bind(env.admin_segment())
        .execute(&mut *txn)
        .await?;
    sqlx::query(
        "UPDATE network_prefixes SET dhcpv6_link_address = '2001:db8:41::1' WHERE segment_id = $1",
    )
    .bind(env.admin_segment())
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;
    let status = discover_v6(RPC_MESSAGE_KIND_V6_SOLICIT).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition, "{status}");
    for expected in [
        mac.to_string(),
        v6.address.clone(),
        env.admin_segment().to_string(),
    ] {
        assert!(status.message().contains(&expected), "{status}");
    }
    assert_eq!(
        address_owner(&pool, v6_address).await?,
        (None, AllocationType::Static, Some(mac))
    );
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:41::/64", None).await?;
    let restored_v6 = discover_v6(RPC_MESSAGE_KIND_V6_SOLICIT).await?.into_inner();
    assert_eq!(restored_v6.address, v6.address);
    assert_eq!(restored_v6.machine_interface_id, Some(restored_id));
    assert_eq!(restored_v6.fqdn, restored_v4.fqdn);
    for ip in [v4_address, v6_address] {
        assert_eq!(
            address_owner(&pool, ip).await?,
            (Some(restored_id), AllocationType::Static, Some(mac))
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn failed_dhcp_reply_keeps_parked_ownership(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = "02:00:00:00:00:52".parse()?;
    let parked = "192.0.2.220".parse()?;
    let mut txn = pool.txn_begin().await?;
    db::machine_interface_address::insert_reserved(&mut txn, mac, parked, AllocationType::Static)
        .await?;
    // A segment without a DNS domain permits allocation but cannot produce
    // a stateful reply from machine_dhcp_records.
    let segment = env.admin_segment();
    let domain: carbide_uuid::domain::DomainId =
        sqlx::query_scalar("SELECT subdomain_id FROM network_segments WHERE id = $1")
            .bind(segment)
            .fetch_one(&mut *txn)
            .await?;
    sqlx::query("UPDATE network_segments SET subdomain_id = NULL WHERE id = $1")
        .bind(segment)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    env.api
        .add_expected_machine(Request::new(expected_machine(
            "02:00:00:00:01:52".parse()?,
            mac,
            Some(Policy::Dynamic),
        )))
        .await?;
    let discover = || {
        env.api
            .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
    };
    let error = discover().await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound, "{error}");
    assert!(error.message().contains("DHCP record"), "{error}");
    assert_eq!(
        address_owner(&pool, parked).await?,
        (None, AllocationType::Static, Some(mac))
    );
    assert!(
        db::machine_interface::find_by_mac_address(&pool, mac)
            .await?
            .is_empty()
    );

    sqlx::query("UPDATE network_segments SET subdomain_id = $1 WHERE id = $2")
        .bind(domain)
        .bind(segment)
        .execute(&pool)
        .await?;
    let reply = discover().await?.into_inner();
    let assigned = reply.address.parse()?;
    assert_eq!(
        address_owner(&pool, assigned).await?,
        (reply.machine_interface_id, AllocationType::Dhcp, None)
    );
    assert!(
        db::machine_interface_address::find_reserved(&pool, Some(mac), None)
            .await?
            .is_empty()
    );
    Ok(())
}

/// When an operator's release commits first, DHCP learns a new preserved address
/// instead of restoring ownership that has already been released.
#[crate::sqlx_test]
async fn preserved_dhcp_after_operator_release_learns_a_new_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = "02:00:00:00:00:53".parse()?;
    // Outside the selected segment: this address must not be restored or
    // selected by the fresh allocation after release.
    let parked = "198.51.100.53".parse()?;
    env.api
        .add_expected_machine(Request::new(expected_machine(
            "02:00:00:00:01:53".parse()?,
            mac,
            Some(Policy::Preserved),
        )))
        .await?;
    let mut setup = pool.txn_begin().await?;
    db::machine_interface_address::insert_reserved(&mut setup, mac, parked, AllocationType::Static)
        .await?;
    setup.commit().await?;

    let mut releasing = pool.txn_begin().await?;
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *releasing)
        .await?;
    assert_eq!(
        db::machine_interface_address::release_reserved(&mut releasing, Some(mac), Some(parked))
            .await?,
        vec![parked],
    );
    let discover = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request());
    tokio::pin!(discover);
    tokio::select! {
        result = &mut discover => panic!("DHCP did not wait for release: {result:?}"),
        _ = super::common::postgres::wait_for_blocked_query(&pool, pid, "SELECT address FROM machine_interface_addresses") => {}
    }
    releasing.commit().await?;
    let reply = discover.await?.into_inner();
    let assigned: IpAddr = reply.address.parse()?;
    assert_ne!(assigned, parked);
    let interface_id = reply
        .machine_interface_id
        .expect("DHCP assigned the interface");
    assert_eq!(
        address_owner(&pool, assigned).await?,
        (Some(interface_id), AllocationType::Static, Some(mac))
    );
    assert!(
        db::machine_interface_address::find_reserved(&pool, Some(mac), None)
            .await?
            .is_empty()
    );
    Ok(())
}

#[crate::sqlx_test]
async fn first_dhcp_allocation_uses_replaced_inventory(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = "02:00:00:00:00:62".parse()?;
    let bmc = "02:00:00:00:01:62".parse()?;
    env.api
        .add_expected_machine(Request::new(expected_machine(
            bmc,
            mac,
            Some(Policy::Dynamic),
        )))
        .await?;

    // Hold replace-all open after it assigns a new UUID to the same MAC.
    // A reader that selected the old UUID before waiting will find no policy.
    let mut writer = pool.txn_begin().await?;
    let mut replacement = db::expected_machine::find_all_for_replace(&mut writer)
        .await?
        .into_iter()
        .find(|machine| machine.bmc_mac_address == bmc)
        .unwrap();
    let old_id = replacement.id;
    replacement.id = None;
    replacement.data.interfaces[0].ip_allocation =
        Some(model::expected_machine::ExpectedInterfaceIpAllocation::Preserved);
    db::expected_machine::clear(&mut writer).await?;
    let replacement = db::expected_machine::create(&mut writer, replacement).await?;
    assert_ne!(replacement.id, old_id);
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let discover = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request());
    tokio::pin!(discover);
    tokio::select! {
        result = &mut discover => panic!("DHCP did not wait for inventory replacement: {result:?}"),
        _ = super::common::postgres::wait_for_blocked_query(&pool, pid, "expected_machines") => {}
    }
    writer.commit().await?;
    let reply = discover.await?.into_inner();
    assert_eq!(
        address_owner(&pool, reply.address.parse()?).await?,
        (
            reply.machine_interface_id,
            AllocationType::Static,
            Some(mac)
        )
    );
    Ok(())
}

#[crate::sqlx_test]
async fn fixed_expected_update_preserves_parked_ownership_on_conflict(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = "02:00:00:00:00:54".parse()?;
    let other_mac = "02:00:00:00:00:55".parse()?;
    let parked: IpAddr = "192.0.2.214".parse()?;
    let occupied: IpAddr = "192.0.2.215".parse()?;
    let mut expected = expected_machine("02:00:00:00:01:54".parse()?, mac, Some(Policy::Dynamic));
    env.api
        .add_expected_machine(Request::new(expected.clone()))
        .await?;
    let mut setup = pool.txn_begin().await?;
    for (mac, ip) in [(mac, parked), (other_mac, occupied)] {
        db::machine_interface_address::insert_reserved(&mut setup, mac, ip, AllocationType::Static)
            .await?;
    }
    setup.commit().await?;

    expected.host_nics[0].ip_allocation = Some(Policy::Fixed as i32);
    expected.host_nics[0].fixed_ip = Some(occupied.to_string());
    let status = env
        .api
        .update_expected_machine(Request::new(expected.clone()))
        .await
        .expect_err("the requested Fixed address is parked for another MAC");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition, "{status}");
    for (mac, ip) in [(mac, parked), (other_mac, occupied)] {
        assert_eq!(
            address_owner(&pool, ip).await?,
            (None, AllocationType::Static, Some(mac))
        );
    }
    let stored =
        db::expected_machine::find_by_bmc_mac_address(&pool, expected.bmc_mac_address.parse()?)
            .await?
            .expect("the original inventory row remains");
    assert_eq!(
        stored.data.interfaces[0].ip_allocation,
        Some(model::expected_machine::ExpectedInterfaceIpAllocation::Dynamic)
    );
    assert!(
        db::machine_interface::find_by_mac_address(&pool, mac)
            .await?
            .is_empty()
    );
    // An options request can leave an interface in place before either
    // family is assigned. Only explicit Fixed may take over its own park.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:54::/64", None).await?;
    let observed_id = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:54::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner()
        .machine_interface_id
        .expect("the options request observed the unallocated interface");
    assert!(
        db::machine_interface::find_one(&pool, observed_id)
            .await?
            .addresses
            .is_empty()
    );
    expected.host_nics[0].fixed_ip = Some(parked.to_string());
    expected.host_nics[0].ip_allocation = None;
    let status = env
        .api
        .update_expected_machine(Request::new(expected.clone()))
        .await
        .expect_err("an inferred Fixed address cannot release its parked owner");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition, "{status}");
    assert!(status.message().contains(&parked.to_string()), "{status}");
    assert_eq!(
        address_owner(&pool, parked).await?,
        (None, AllocationType::Static, Some(mac))
    );
    expected.host_nics[0].ip_allocation = Some(Policy::Fixed as i32);
    env.api
        .update_expected_machine(Request::new(expected))
        .await?;
    assert_eq!(
        address_owner(&pool, parked).await?,
        (Some(observed_id), AllocationType::Static, None)
    );
    Ok(())
}

#[crate::sqlx_test]
async fn explicit_policy_supersedes_parked_ownership_but_omission_does_not(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use AllocationType::{Dhcp, Static};
    use Policy::{Dynamic, Fixed, Retained};

    let env = create_test_env(pool.clone()).await;
    for (index, (name, declared, policy, allocation_type)) in [
        ("no declaration", false, None, Dhcp),
        ("inferred dynamic", true, None, Dhcp),
        ("explicit dynamic", true, Some(Dynamic), Dhcp),
        ("explicit retained", true, Some(Retained), Static),
        ("explicit fixed", true, Some(Fixed), Static),
    ]
    .into_iter()
    .enumerate()
    {
        let mac = MacAddress::new([2, 0, 0, 0, 0, index as u8]);
        let bmc_mac = MacAddress::new([2, 0, 0, 0, 1, index as u8]);
        let discover = || {
            env.api.discover_dhcp(
                DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
            )
        };
        let parked = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 210 + index as u8));
        let sibling = IpAddr::V6(std::net::Ipv6Addr::new(
            0x2001,
            0xdb8,
            0x43,
            0,
            0,
            0,
            0,
            index as u16 + 1,
        ));
        let mut txn = pool.txn_begin().await?;
        db::machine_interface_address::insert_reserved(
            &mut txn,
            mac,
            parked,
            AllocationType::Static,
        )
        .await?;
        if policy.is_some() {
            db::machine_interface_address::insert_reserved(&mut txn, mac, sibling, Static).await?;
        }
        txn.commit().await?;

        if declared {
            let mut expected = expected_machine(bmc_mac, mac, policy);
            if policy == Some(Fixed) {
                // Assign the parked address itself, so Fixed must release
                // parked ownership before trying to claim it.
                expected.host_nics[0].fixed_ip = Some(parked.to_string());
            }
            env.api.add_expected_machine(Request::new(expected)).await?;
        }
        let response = discover().await?.into_inner();
        let allocated = response.address.parse()?;
        let owner = address_owner(&pool, allocated).await?;
        assert_eq!(
            owner,
            (response.machine_interface_id, allocation_type, None),
            "{name}"
        );
        if policy.is_none() {
            assert_ne!(allocated, parked, "{name}");
            assert_eq!(
                address_owner(&pool, parked).await?,
                (None, AllocationType::Static, Some(mac)),
                "{name}"
            );
        } else {
            assert!(
                db::machine_interface_address::find_reserved(&pool, Some(mac), Some(parked))
                    .await?
                    .is_empty(),
                "{name}"
            );
            assert_eq!(
                address_owner(&pool, sibling).await?,
                (None, Static, Some(mac)),
                "{name}"
            );
            if policy == Some(Fixed) {
                assert_eq!(allocated, parked, "{name}");
            }
        }
        if declared && policy.is_none() {
            // The first v6 lease uses the edited policy even after v4 discovery
            // and machine association. It must not retag the live v4 lease.
            let interface_id = response
                .machine_interface_id
                .expect("DHCP assigned the interface");
            let host_id = model::test_support::machine_snapshot::host_machine_id();
            let mut txn = pool.txn_begin().await?;
            db::machine::create(
                &mut txn,
                None,
                &host_id,
                model::machine::ManagedHostState::Ready,
                None,
                model::machine::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            db::machine_interface::associate_interface_with_machine(
                &interface_id,
                model::machine_interface_address::MachineInterfaceAssociation::Machine(
                    host_id.into(),
                ),
                &mut txn,
            )
            .await?;
            txn.commit().await?;
            add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:42::/64", None).await?;
            env.api
                .update_expected_machine(Request::new(expected_machine(
                    bmc_mac,
                    mac,
                    Some(Policy::Preserved),
                )))
                .await?;
            let v6 = env
                .api
                .discover_dhcp(dhcpv6_discovery(
                    mac,
                    "2001:db8:42::1",
                    RPC_MESSAGE_KIND_V6_SOLICIT,
                ))
                .await?
                .into_inner();
            assert_eq!(v6.machine_interface_id, Some(interface_id));
            let v6_address: IpAddr = v6.address.parse()?;
            assert!(v6_address.is_ipv6());
            assert_eq!(
                address_owner(&pool, v6_address).await?,
                (Some(interface_id), Static, Some(mac))
            );
            assert_eq!(discover().await?.into_inner().address, response.address);
            assert_eq!(address_owner(&pool, allocated).await?, owner);
            assert_eq!(
                address_owner(&pool, parked).await?,
                (None, Static, Some(mac))
            );
        }
    }
    Ok(())
}
