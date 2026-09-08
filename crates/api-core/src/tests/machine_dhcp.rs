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

use carbide_network::ip::IpAddressFamily;
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY, create_host_inband_network_segment,
    create_network_segment,
};
use common::api_fixtures::{
    FIXTURE_DHCP_RELAY_ADDRESS, TestEnv, TestEnvOverrides, create_managed_host,
    create_managed_host_multi_dpu, create_managed_host_with_config, create_test_env,
    create_test_env_with_host_inband, create_test_env_with_overrides, dpu, get_config,
    site_explorer,
};
use db::{self, ObjectColumnFilter, dhcp_entry};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::expected_machine::{
    BmcIpAllocationType, ExpectedInterface, ExpectedInterfaceIpAllocation, ExpectedInterfaceRole,
    ExpectedMachine, ExpectedMachineData,
};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ExpireDhcpLeaseRequest, ManagedHostNetworkConfigRequest};

use crate::DatabaseError;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common;
use crate::tests::common::rpc_builder::DhcpDiscovery;

const RPC_ADDRESS_FAMILY_V4: i32 = rpc::forge::AddressFamily::V4 as i32;
const RPC_ADDRESS_FAMILY_V6: i32 = rpc::forge::AddressFamily::V6 as i32;
const RPC_MESSAGE_KIND_V4_DISCOVER: i32 = rpc::forge::MessageKind::V4Discover as i32;
const RPC_MESSAGE_KIND_V6_SOLICIT: i32 = rpc::forge::MessageKind::V6Solicit as i32;
const RPC_MESSAGE_KIND_V6_INFO_REQUEST: i32 = rpc::forge::MessageKind::V6InfoRequest as i32;

/// Build a DHCPv6 discovery request with explicit protocol fields.
fn dhcpv6_discovery(
    mac_address: MacAddress,
    relay_address: &str,
    message_kind: i32,
) -> tonic::Request<rpc::forge::DhcpDiscovery> {
    DhcpDiscovery::builder(mac_address, relay_address)
        .address_family(RPC_ADDRESS_FAMILY_V6)
        .message_kind(message_kind)
        .duid(vec![0x01])
        .tonic_request()
}

fn dhcpv6_discovery_with_desired_address(
    mac_address: MacAddress,
    relay_address: &str,
    message_kind: i32,
    desired_address: IpAddr,
) -> tonic::Request<rpc::forge::DhcpDiscovery> {
    let mut request = dhcpv6_discovery(mac_address, relay_address, message_kind);
    request.get_mut().desired_address = Some(desired_address.to_string());
    request
}

fn expected_slaac_address(prefix: Ipv6Addr, mac: MacAddress) -> IpAddr {
    let mac = mac.bytes();
    let mut octets = prefix.octets();
    octets[8] = mac[0] ^ 0x02;
    octets[9] = mac[1];
    octets[10] = mac[2];
    octets[11] = 0xff;
    octets[12] = 0xfe;
    octets[13] = mac[3];
    octets[14] = mac[4];
    octets[15] = mac[5];
    IpAddr::V6(Ipv6Addr::from(octets))
}

/// Add a v6 prefix to an existing test segment, optionally with a DHCPv6 link-address.
async fn add_ipv6_prefix(
    pool: &sqlx::PgPool,
    segment_id: NetworkSegmentId,
    prefix: &str,
    dhcpv6_link_address: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    add_ipv6_prefix_with_num_reserved(pool, segment_id, prefix, dhcpv6_link_address, 0).await
}

async fn add_ipv6_prefix_with_num_reserved(
    pool: &sqlx::PgPool,
    segment_id: NetworkSegmentId,
    prefix: &str,
    dhcpv6_link_address: Option<&str>,
    num_reserved: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let link_address: Option<IpAddr> = dhcpv6_link_address.map(str::parse).transpose()?;
    let mut txn = pool.begin().await?;
    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, dhcpv6_link_address, num_reserved)
         VALUES ($1, $2::cidr, $3::inet, $4)",
    )
    .bind(segment_id)
    .bind(prefix)
    .bind(link_address)
    .bind(num_reserved)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;
    Ok(())
}

async fn set_dhcpv6_link_address_on_ipv4_prefix(
    pool: &sqlx::PgPool,
    segment_id: NetworkSegmentId,
    dhcpv6_link_address: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let link_address: IpAddr = dhcpv6_link_address.parse()?;
    let mut txn = pool.begin().await?;
    sqlx::query(
        "UPDATE network_prefixes
         SET dhcpv6_link_address = $2::inet
         WHERE segment_id = $1 AND family(prefix) = 4",
    )
    .bind(segment_id)
    .bind(link_address)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;
    Ok(())
}

/// Set a test segment to reserved-only allocation.
async fn set_segment_reserved(
    pool: &sqlx::PgPool,
    segment_id: NetworkSegmentId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    sqlx::query("UPDATE network_segments SET allocation_strategy = 'reserved' WHERE id = $1")
        .bind(segment_id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

/// Allow SLAAC EUI-64 inference on a test segment.
async fn enable_slaac_eui64_inference(
    pool: &sqlx::PgPool,
    segment_id: NetworkSegmentId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    sqlx::query(
        "UPDATE network_segments
         SET infer_slaac_eui64_addresses = true
         WHERE id = $1",
    )
    .bind(segment_id)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;
    Ok(())
}

async fn create_admin_network_segment_with_id(
    env: &TestEnv,
    id: NetworkSegmentId,
    name: &str,
    prefix: &str,
    gateway: &str,
) -> Result<NetworkSegmentId, tonic::Status> {
    let response = env
        .api
        .create_network_segment(tonic::Request::new(
            rpc::forge::NetworkSegmentCreationRequest {
                id: Some(id),
                mtu: Some(1500),
                name: name.to_string(),
                prefixes: vec![rpc::forge::NetworkPrefix {
                    id: None,
                    prefix: prefix.to_string(),
                    gateway: Some(gateway.to_string()),
                    reserve_first: 3,
                    free_ip_count: 0,
                    svi_ip: None,
                    free_ip_count_v2: None,
                    free_ip_count_saturated: false,
                }],
                subdomain_id: Some(env.domain.into()),
                vpc_id: None,
                segment_type: rpc::forge::NetworkSegmentType::Admin as i32,
                infer_slaac_eui64_addresses: false,
            },
        ))
        .await?
        .into_inner();

    Ok(response.id.expect("created segment should return its id"))
}

/// Create a test environment with DHCP lease expiry handling enabled.
async fn create_test_env_with_dhcp_expiry(pool: sqlx::PgPool) -> TestEnv {
    create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            dhcp_lease_expiry_handling: Some(true),
            ..Default::default()
        },
    )
    .await
}

async fn interface_addresses_for_mac(
    pool: &sqlx::PgPool,
    mac: MacAddress,
) -> Result<
    (
        MachineInterfaceId,
        Vec<db::machine_interface_address::MachineInterfaceAddressWithType>,
    ),
    Box<dyn std::error::Error>,
> {
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    let interface_id = interfaces[0].id;
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    txn.rollback().await?;
    Ok((interface_id, addresses))
}

#[crate::sqlx_test]
async fn test_machine_dhcp(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let test_mac_address = MacAddress::from_str("ff:ff:ff:ff:ff:ff").unwrap();
    let test_gateway_address = FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap();

    db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac_address,
        std::slice::from_ref(&test_gateway_address),
        None,
        None,
    )
    .await?;

    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_dhcp_from_wrong_vlan_fails(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let test_mac_address = MacAddress::from_str("ff:ff:ff:ff:ff:ff").unwrap();
    let test_gateway_address = FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap();

    db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac_address,
        std::slice::from_ref(&test_gateway_address),
        None,
        None,
    )
    .await?;

    // Test a second time after initial creation on the same segment should not cause issues
    db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac_address,
        std::slice::from_ref(&test_gateway_address),
        None,
        None,
    )
    .await?;

    // expect this to error out
    let output = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        test_mac_address,
        &["192.0.1.1".parse().unwrap()],
        None,
        None,
    )
    .await;

    assert!(
        matches!(output, Err(DatabaseError::Internal { message, ..}) if message.starts_with("Network segment mismatch for existing MAC address"))
    );

    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_dhcp_with_api(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    // Inititially 0 addresses are allocated on the segment
    let mut txn = env.pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, env.admin_segment_ref())
            .await
            .unwrap(),
        0
    );
    txn.commit().await.unwrap();

    let mac_address = "FF:FF:FF:FF:FF:FF";
    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.segment_id.unwrap(), (env.admin_segment()));

    assert_eq!(response.mac_address, mac_address);
    assert_eq!(response.subdomain_id.unwrap(), env.domain.into());
    assert_eq!(response.address, "192.0.2.3".to_owned());
    assert_eq!(response.prefix, "192.0.2.0/24".to_owned());
    assert_eq!(response.gateway.unwrap(), "192.0.2.1".to_owned());

    // After DHCP, 1 address is allocated on the segment
    let mut txn = pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, env.admin_segment_ref())
            .await
            .unwrap(),
        1
    );
    txn.commit().await.unwrap();
    Ok(())
}

#[crate::sqlx_test]
async fn test_non_primary_admin_interface_dhcp_is_rejected(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    // Create a multi-DPU host and find its dormant DPU-backed admin interface.
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = pool.begin().await?;
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let dormant_interface = interface_map
        .remove(&mh.id)
        .unwrap()
        .into_iter()
        .find(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
                && !interface.primary_interface
        })
        .unwrap();
    assert!(dormant_interface.addresses.is_empty());
    txn.commit().await?;

    // DHCP on the dormant admin link must be rejected before a new lease
    // is allocated or any stale record can be returned.
    let result = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(dormant_interface.mac_address, FIXTURE_DHCP_RELAY_ADDRESS)
                .tonic_request(),
        )
        .await;
    let status = result.expect_err("dormant admin DHCP should be rejected");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        status
            .message()
            .contains("dormant non-primary admin interface")
    );

    // Verify the rejected request did not allocate a replacement address.
    let mut txn = pool.begin().await?;
    let persisted_interface =
        db::machine_interface::find_one(&mut *txn, dormant_interface.id).await?;
    assert!(persisted_interface.addresses.is_empty());
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_discover_dhcp_includes_site_ntp_server_ips(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.ntp_servers = vec![
        "198.51.100.10".parse().unwrap(),
        "198.51.100.11".parse().unwrap(),
    ];
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(config)).await;

    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder("FF:FF:FF:FF:FF:FF", FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(
        response.ntp_servers,
        vec!["198.51.100.10".to_string(), "198.51.100.11".to_string()]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_discover_dhcp_returns_empty_ntp_servers_when_site_not_configured(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.ntp_servers = vec![];
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(config)).await;

    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder("FF:FF:FF:FF:FF:EE", FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await?
        .into_inner();

    assert_eq!(response.ntp_servers, Vec::<String>::new());
    Ok(())
}

#[crate::sqlx_test]
async fn test_multiple_machines_dhcp_with_api(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;

    // Inititially 0 addresses are allocated on the segment
    let mut txn = pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, env.admin_segment_ref())
            .await
            .unwrap(),
        0
    );
    txn.commit().await.unwrap();

    let mac_address = "FF:FF:FF:FF:FF:0".to_string();
    const NUM_MACHINES: usize = 6;
    for i in 0..NUM_MACHINES {
        let mac = format!("{mac_address}{i}");
        let expected_ip = format!("192.0.2.{}", i + 3); // IP starts with 3.
        let response = env
            .api
            .discover_dhcp(DhcpDiscovery::builder(&mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
            .await
            .unwrap()
            .into_inner();

        assert_eq!(response.segment_id.unwrap(), (env.admin_segment()));

        assert_eq!(response.mac_address, mac);
        assert_eq!(response.subdomain_id.unwrap(), env.domain.into());
        assert_eq!(response.address, expected_ip);
        assert_eq!(response.prefix, "192.0.2.0/24".to_owned());
        assert_eq!(response.gateway.unwrap(), "192.0.2.1".to_owned());
    }

    let mut txn = pool.begin().await?;
    assert_eq!(
        db::machine_interface::count_by_segment_id(&mut txn, env.admin_segment_ref())
            .await
            .unwrap(),
        NUM_MACHINES
    );
    txn.commit().await.unwrap();
    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_dhcp_declared_admin_nic_allocates_from_relay_admin_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.rack_management_enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    // Create a second admin segment so the relay determines which admin segment is used.
    let second_admin_segment = create_network_segment(
        &env.api,
        "ADMIN_2",
        "192.0.12.0/24",
        "192.0.12.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;

    // Register an expected host NIC declared as an admin-network NIC.
    let bmc_mac: MacAddress = "7a:7b:7c:7d:7e:10".parse().unwrap();
    let admin_nic_mac: MacAddress = "7a:7b:7c:7d:7e:11".parse().unwrap();
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-ADMIN-RELAY-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: admin_nic_mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: None,
                fixed_mask: None,
                fixed_gateway: None,
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // DHCP through the second admin relay should allocate from that segment.
    let response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(admin_nic_mac, "192.0.12.1").tonic_request())
        .await?
        .into_inner();

    let expected_address: IpAddr = "192.0.12.3".parse().unwrap();
    assert_eq!(response.segment_id.unwrap(), second_admin_segment);
    assert_eq!(response.mac_address, admin_nic_mac.to_string());
    assert_eq!(response.subdomain_id.unwrap(), env.domain.into());
    assert_eq!(response.address, expected_address.to_string());
    assert_eq!(response.prefix, "192.0.12.0/24");
    assert_eq!(response.gateway.unwrap(), "192.0.12.1");

    // Verify the persisted interface matches the DHCP response.
    let interface_id = response
        .machine_interface_id
        .expect("DHCP response should include machine_interface_id");
    let mut txn = env.pool.begin().await?;
    let persisted_interface = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(persisted_interface.segment_id, second_admin_segment);
    assert_eq!(persisted_interface.mac_address, admin_nic_mac);
    assert_eq!(persisted_interface.domain_id, Some(env.domain.into()));
    assert!(persisted_interface.primary_interface);
    assert_eq!(persisted_interface.addresses, vec![expected_address]);

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_dhcp_declared_segment_type_allocates_from_relay_admin_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = get_config();
    config.rack_management_enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    // A second admin segment, so the relay -- not the declaration -- decides
    // which admin segment is used once selection is narrowed to Admin.
    let second_admin_segment = create_network_segment(
        &env.api,
        "ADMIN_2",
        "192.0.12.0/24",
        "192.0.12.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;

    // Declare the host NIC's segment type directly -- the typed field, no
    // legacy nic_type string.
    let bmc_mac: MacAddress = "7a:7b:7c:7d:7e:20".parse().unwrap();
    let admin_nic_mac: MacAddress = "7a:7b:7c:7d:7e:21".parse().unwrap();
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-ADMIN-TYPED-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: admin_nic_mac.to_string(),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // DHCP through the second admin relay allocates from that admin segment --
    // the typed declaration narrowed selection to Admin, the relay picked which.
    let response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(admin_nic_mac, "192.0.12.1").tonic_request())
        .await?
        .into_inner();

    assert_eq!(response.segment_id.unwrap(), second_admin_segment);
    assert_eq!(response.mac_address, admin_nic_mac.to_string());
    assert_eq!(response.prefix, "192.0.12.0/24");

    let interface_id = response
        .machine_interface_id
        .expect("DHCP response should include machine_interface_id");
    let mut txn = env.pool.begin().await?;
    let persisted_interface = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
    assert_eq!(persisted_interface.segment_id, second_admin_segment);
    assert_eq!(persisted_interface.mac_address, admin_nic_mac);

    Ok(())
}

/// Every expected-interface role uses the same DHCP and allocation-policy
/// path, while the role supplies only its interface type and primary setting.
#[crate::sqlx_test]
async fn test_expected_interface_roles_and_policies_flow_through_dhcp_and_site_explorer(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let expected_bmc_mac: MacAddress = "7A:7B:7C:7D:7E:31".parse()?;
    let host_mac: MacAddress = "7A:7B:7C:7D:7E:34".parse()?;
    let dpu_os_mac: MacAddress = "7A:7B:7C:7D:7E:32".parse()?;
    let dpu_bmc_mac: MacAddress = "7A:7B:7C:7D:7E:33".parse()?;
    let underlay_segment = env
        .underlay_segment
        .expect("test environment should have an underlay segment");

    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: expected_bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DPU-EXPECTED-INTERFACES-001".into(),
            host_nics: vec![
                rpc::forge::ExpectedInterface {
                    mac_address: host_mac.to_string(),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                    role: Some(rpc::forge::ExpectedInterfaceRole::Host as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    primary: Some(true),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    mac_address: dpu_os_mac.to_string(),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuOs as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    mac_address: dpu_bmc_mac.to_string(),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                    role: Some(rpc::forge::ExpectedInterfaceRole::DpuBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                },
                rpc::forge::ExpectedInterface {
                    mac_address: expected_bmc_mac.to_string(),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                    role: Some(rpc::forge::ExpectedInterfaceRole::HostBmc as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Retained as i32),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }))
        .await?;

    // The machine-wide primary declaration belongs only to the Host role.
    // DPU OS and BMC interfaces still derive their primary settings from their
    // roles when the same ExpectedMachine declares a primary Host interface.
    struct Case {
        name: &'static str,
        mac_address: MacAddress,
        role: ExpectedInterfaceRole,
        policy: ExpectedInterfaceIpAllocation,
        interface_type: InterfaceType,
        primary: bool,
    }

    for Case {
        name,
        mac_address,
        role,
        policy,
        interface_type,
        primary,
    } in [
        Case {
            name: "Host dynamic",
            mac_address: host_mac,
            role: ExpectedInterfaceRole::Host,
            policy: ExpectedInterfaceIpAllocation::Dynamic,
            interface_type: InterfaceType::Data,
            primary: true,
        },
        Case {
            name: "DPU OS dynamic",
            mac_address: dpu_os_mac,
            role: ExpectedInterfaceRole::DpuOs,
            policy: ExpectedInterfaceIpAllocation::Dynamic,
            interface_type: InterfaceType::Data,
            primary: true,
        },
        Case {
            name: "DPU BMC retained",
            mac_address: dpu_bmc_mac,
            role: ExpectedInterfaceRole::DpuBmc,
            policy: ExpectedInterfaceIpAllocation::Retained,
            interface_type: InterfaceType::Bmc,
            primary: false,
        },
        Case {
            name: "Host BMC retained",
            mac_address: expected_bmc_mac,
            role: ExpectedInterfaceRole::HostBmc,
            policy: ExpectedInterfaceIpAllocation::Retained,
            interface_type: InterfaceType::Bmc,
            primary: false,
        },
    ] {
        let response = env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(mac_address, FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip())
                    .tonic_request(),
            )
            .await?
            .into_inner();
        let interface_id = response
            .machine_interface_id
            .expect("DHCP response should identify the interface");

        let mut txn = pool.begin().await?;
        let interface = db::machine_interface::find_one(txn.as_mut(), interface_id).await?;
        let before =
            db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
        txn.rollback().await?;
        assert_eq!(interface.segment_id, underlay_segment, "case: {name}");
        assert_eq!(interface.interface_type, interface_type, "case: {name}");
        assert_eq!(interface.primary_interface, primary, "case: {name}");
        assert_eq!(before.len(), 1, "case: {name}");
        assert_eq!(
            before[0].allocation_type,
            AllocationType::Dhcp,
            "case: {name}",
        );

        carbide_site_explorer::try_apply_expected_interface(
            &pool,
            &ExpectedInterface {
                mac_address,
                role,
                ip_allocation: Some(policy),
                network_segment_type: Some(NetworkSegmentType::Underlay),
                ..Default::default()
            },
            None,
        )
        .await;

        let mut txn = pool.begin().await?;
        let after =
            db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
        txn.rollback().await?;
        let expected_allocation = if policy == ExpectedInterfaceIpAllocation::Retained {
            AllocationType::Static
        } else {
            AllocationType::Dhcp
        };
        assert_eq!(after.len(), 1, "case: {name}");
        assert_eq!(
            after[0].allocation_type, expected_allocation,
            "case: {name}",
        );
    }

    Ok(())
}

/// The top-level BMC alternate key wins over a stale nested declaration that
/// happens to reuse the same MAC.
#[crate::sqlx_test]
async fn test_host_bmc_identity_wins_expected_interface_mac_lookup(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let bmc_mac: MacAddress = "7A:7B:7C:7D:7E:35".parse()?;

    // Write the conflicting legacy shape directly. Current API validation
    // rejects this input, but an existing row may predate the HostBmc role.
    let mut txn = pool.begin().await?;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac,
            data: ExpectedMachineData {
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                serial_number: "EM-HOST-BMC-LOOKUP-001".into(),
                bmc_ip_allocation: BmcIpAllocationType::Dynamic,
                interfaces: vec![ExpectedInterface {
                    mac_address: bmc_mac,
                    role: ExpectedInterfaceRole::Host,
                    ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
                    network_segment_type: Some(NetworkSegmentType::Underlay),
                    ..Default::default()
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    txn.commit().await?;

    // Existing conflicting rows remain updatable so introducing HostBmc does
    // not turn an unrelated read-modify-write into a breaking change.
    env.api
        .update_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "UPDATED_ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-HOST-BMC-LOOKUP-001".into(),
            bmc_ip_allocation: Some(rpc::forge::BmcIpAllocationType::Dynamic as i32),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: bmc_mac.to_string(),
                role: Some(rpc::forge::ExpectedInterfaceRole::Host as i32),
                ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Underlay as i32),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let mut txn = pool.begin().await?;
    let stored = db::expected_machine::find_by_bmc_mac_address(&mut *txn, bmc_mac)
        .await?
        .expect("expected machine should exist");
    txn.rollback().await?;
    assert!(
        stored.data.interfaces.iter().any(|interface| {
            interface.mac_address == bmc_mac && interface.role == ExpectedInterfaceRole::Host
        }),
        "the legacy conflicting Host declaration should survive the update",
    );

    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(bmc_mac, FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip())
                .tonic_request(),
        )
        .await?
        .into_inner();
    let interface_id = response
        .machine_interface_id
        .expect("DHCP response should identify the Host BMC interface");

    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.interface_type, InterfaceType::Bmc);
    assert!(!interface.primary_interface);

    Ok(())
}

#[crate::sqlx_test]
// This test exercises DHCP through the compatibility fields sent to older agents.
#[allow(deprecated)]
async fn test_machine_dhcp_with_api_for_instance_physical_virtual(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env(pool.clone()).await;
    let (segment_id_1, segment_id_2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let network = rpc::InstanceNetworkConfig {
        interfaces: vec![
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: Some(segment_id_1),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Virtual as i32,
                network_segment_id: Some(segment_id_2),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
        ],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    mh.instance_builer(&env).network(network).build().await;
    // Instance dhcp is not handled by carbide. Best way to find out allocated IP info is to read
    // data from managedhostnetworkconfig.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(mh.dpu().id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    let tenant_data = response.tenant_interfaces;
    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.ip.as_deref() == Some("192.0.4.3"))
    );
    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.ip.as_deref() == Some("192.1.4.3"))
    );

    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.prefix.as_deref() == Some("192.0.4.0/24"))
    );
    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.prefix.as_deref() == Some("192.1.4.0/24"))
    );

    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.gateway.as_deref() == Some("192.0.4.1/24"))
    );
    assert!(
        tenant_data
            .iter()
            .any(|interface| interface.gateway.as_deref() == Some("192.1.4.1/24"))
    );

    Ok(())
}

#[crate::sqlx_test]
async fn machine_interface_discovery_persists_vendor_strings(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    async fn assert_vendor_strings_equal(
        pool: &sqlx::PgPool,
        interface_id: &MachineInterfaceId,
        expected: &[&str],
    ) {
        let mut txn = pool.clone().begin().await.unwrap();
        let entry = db::dhcp_entry::find_by(
            &mut txn,
            ObjectColumnFilter::One(dhcp_entry::MachineInterfaceIdColumn, interface_id),
        )
        .await
        .unwrap();
        assert_eq!(
            entry
                .iter()
                .map(|e| e.vendor_string.as_str())
                .collect::<Vec<&str>>(),
            expected
        );

        // Also check via the MachineInterface API
        let iface = db::machine_interface::find_one(txn.as_mut(), *interface_id)
            .await
            .unwrap();
        assert_eq!(iface.vendors, expected);

        txn.rollback().await.unwrap();
    }

    async fn dhcp_with_vendor(
        env: &TestEnv,
        mac_address: MacAddress,
        vendor_string: Option<&str>,
    ) -> rpc::protos::forge::DhcpRecord {
        let builder = DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS);
        let builder = if let Some(vendor_string) = vendor_string {
            builder.vendor_string(vendor_string)
        } else {
            builder
        };
        env.api
            .discover_dhcp(builder.tonic_request())
            .await
            .unwrap()
            .into_inner()
    }

    let env = create_test_env(pool.clone()).await;
    let mac_address = MacAddress::from_str("ab:cd:ff:ff:ff:ff").unwrap();

    let response = dhcp_with_vendor(&env, mac_address, Some("vendor1")).await;
    let interface_id = response
        .machine_interface_id
        .expect("machine_interface_id must be set");
    assert_vendor_strings_equal(&pool, &interface_id, &["vendor1"]).await;

    let _ = dhcp_with_vendor(&env, mac_address, Some("vendor2")).await;
    assert_vendor_strings_equal(&pool, &interface_id, &["vendor1", "vendor2"]).await;

    let _ = dhcp_with_vendor(&env, mac_address, None).await;
    assert_vendor_strings_equal(&pool, &interface_id, &["vendor1", "vendor2"]).await;

    // DHCP with a previously known vendor string
    // This should not fail
    let _ = dhcp_with_vendor(&env, mac_address, Some("vendor2")).await;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpu_machine_dhcp_for_existing_dpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = dpu::create_dpu_machine(&env, &host_config).await;

    let machine = env.find_machine(&dpu_machine_id).await.remove(0);
    let mac = machine.status.as_ref().unwrap().interfaces[0]
        .mac_address
        .clone();

    let response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(&mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        response.address.as_str(),
        machine.status.as_ref().unwrap().interfaces[0].address[0].as_str()
    );

    Ok(())
}

// test_dhcp_record_address_family verifies that find_by_mac_address correctly
// filters by address family. In a dual-stack environment, a machine interface
// has both IPv4 and IPv6 addresses. The DHCPv4 server must receive only the
// IPv4 record, and a future DHCPv6 server must receive only the IPv6 record.
#[crate::sqlx_test]
async fn test_dhcp_record_address_family(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    // Create a machine via DHCPv4 discovery — gives us an interface with an IPv4 address.
    let mac_address = "AB:CD:EF:01:23:45";
    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let segment_id = response.segment_id.unwrap();
    let ipv4_address = response.address.clone();

    // Verify the IPv4 address is correct.
    let parsed_v4: IpAddr = ipv4_address.parse().unwrap();
    assert!(
        parsed_v4.is_ipv4(),
        "DHCPv4 discovery should return an IPv4 address"
    );

    // Insert an IPv6 address for the same interface, simulating dual-stack.
    let mut txn = pool.begin().await?;
    let parsed_mac: MacAddress = mac_address.parse().unwrap();
    let interfaces = db::machine_interface::find_by_mac_address(txn.as_mut(), parsed_mac).await?;
    let interface = &interfaces[0];

    let ipv6_addr: IpAddr = "fd00::42".parse().unwrap();
    sqlx::query("INSERT INTO machine_interface_addresses (interface_id, address) VALUES ($1, $2)")
        .bind(interface.id)
        .bind(ipv6_addr)
        .execute(&mut *txn)
        .await?;

    // The machine_dhcp_records view requires the address is contained within
    // the prefix, so we also need an IPv6 prefix on the same segment for the
    // IPv6 address to appear.
    sqlx::query(
        "INSERT INTO network_prefixes (segment_id, prefix, num_reserved) VALUES ($1, $2::cidr, 0)",
    )
    .bind(segment_id)
    .bind("fd00::/64")
    .execute(&mut *txn)
    .await?;

    txn.commit().await?;

    // Now test find_by_mac_address with IPv4 — should return only the IPv4 record.
    let mut txn = pool.begin().await?;
    let ipv4_record = db::dhcp_record::find_by_mac_address(
        &mut txn,
        &parsed_mac,
        &segment_id,
        IpAddressFamily::Ipv4,
    )
    .await?
    .expect("IPv4 DHCP record should exist");
    assert!(
        ipv4_record.address.is_ipv4(),
        "IPv4 query should return an IPv4 address, got: {}",
        ipv4_record.address
    );
    assert_eq!(ipv4_record.address.to_string(), ipv4_address);
    txn.rollback().await?;

    // And with IPv6 — should return only the IPv6 record.
    let mut txn = pool.begin().await?;
    let ipv6_record = db::dhcp_record::find_by_mac_address(
        &mut txn,
        &parsed_mac,
        &segment_id,
        IpAddressFamily::Ipv6,
    )
    .await?
    .expect("IPv6 DHCP record should exist");
    assert!(
        ipv6_record.address.is_ipv6(),
        "IPv6 query should return an IPv6 address, got: {}",
        ipv6_record.address
    );
    assert_eq!(ipv6_record.address, ipv6_addr);
    txn.rollback().await?;

    Ok(())
}

// DHCPv4 and DHCPv6 for one physical NIC should merge into one interface row,
// while each response is routed to the requested address family.
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

// A DHCPv6 information-request infers one SLAAC EUI-64 address and returns
// only site options, so it must not allocate a DHCP lease.
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

// With SLAAC EUI-64 inference left off, an Information-Request keeps the
// existing IPv4 address and DNS record without publishing a computed IPv6 one.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_default_off_keeps_ipv4_only(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:03").unwrap();

    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:4::/64", None).await?;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    assert!(v4_response.address.parse::<IpAddr>()?.is_ipv4());
    let info_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:4::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(info_response.address, "");
    assert_eq!(info_response.prefix, "");
    assert_eq!(
        info_response.machine_interface_id,
        v4_response.machine_interface_id
    );
    assert_eq!(info_response.fqdn, v4_response.fqdn);

    let computed_address = expected_slaac_address("2001:db8:4::".parse()?, mac);
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(Some(interface_id), v4_response.machine_interface_id);
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert!(addresses[0].address.is_ipv4());

    let mut txn = pool.begin().await?;
    let records =
        db::dns::resource_record::find_record(&mut *txn, &format!("{}.", v4_response.fqdn)).await?;
    assert!(
        records
            .iter()
            .any(|record| record.q_type == "A" && record.record == v4_response.address)
    );
    assert!(records.iter().all(|record| record.q_type != "AAAA"));
    assert!(
        db::dns::resource_record::find_ptr_record(&mut *txn, computed_address)
            .await?
            .is_empty()
    );
    txn.rollback().await?;

    Ok(())
}

// SLAAC EUI-64 inference uses the shared ownership boundary: if the computed
// address is already held by another interface, reject instead of creating a
// duplicate machine_interface_addresses row.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_rejects_slaac_address_owned_by_other_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let owner_mac = MacAddress::from_str("02:00:00:00:00:2a").unwrap();
    let requester_mac = MacAddress::from_str("02:00:00:00:00:2b").unwrap();

    // Give the segment a SLAAC-eligible prefix and create an unrelated owner.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:87::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let owner_response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(owner_mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await?
        .into_inner();
    let owner_interface_id = owner_response
        .machine_interface_id
        .expect("owner interface should exist");

    // Seed the requester's computed SLAAC address on the owner interface.
    let duplicate_slaac = expected_slaac_address("2001:db8:87::".parse()?, requester_mac);
    let mut txn = pool.begin().await?;
    db::machine_interface_address::insert(
        &mut txn,
        owner_interface_id,
        duplicate_slaac,
        AllocationType::Static,
    )
    .await?;
    txn.commit().await?;

    // The request must reject instead of adding duplicate address ownership.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            requester_mac,
            "2001:db8:87::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await
        .expect_err("duplicate SLAAC ownership should reject");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        status.message(),
        format!(
            "address already in use: {duplicate_slaac} by {owner_mac} in network segment {} (interface: {owner_interface_id})",
            env.admin_segment(),
        )
    );

    let mut txn = pool.begin().await?;
    let requester_interfaces =
        db::machine_interface::find_by_mac_address(&mut *txn, requester_mac).await?;
    assert!(requester_interfaces.is_empty());
    txn.rollback().await?;

    Ok(())
}

// DHCPv6 information-request on a v6-enabled but SLAAC-ineligible prefix
// returns options only and must not persist an IPv6 address.
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

// DHCPv6 SLAAC EUI-64 inference after a v4 lease expiration must restore the
// segment domain so the options-only DHCP record remains visible.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_restores_domain_after_v4_expiration(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_dhcp_expiry(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:0c").unwrap();

    // Make the segment dual-stack and create the initial IPv4 DHCP lease.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:e::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response
        .machine_interface_id
        .expect("DHCP response should include an interface id");

    // Expire the only address; the deletion path should park the row outside DNS.
    env.api
        .expire_dhcp_lease(tonic::Request::new(ExpireDhcpLeaseRequest {
            ip_address: v4_response.address,
            mac_address: Some(mac.to_string()),
        }))
        .await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert!(interface.addresses.is_empty());
    assert!(interface.domain_id.is_none());
    txn.rollback().await?;

    // Infer the SLAAC EUI-64 address on the same row; the options-only response
    // should keep its metadata.
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:e::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.machine_interface_id, Some(interface_id));
    assert_eq!(response.subdomain_id, Some(env.domain.into()));
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert!(response.gateway.is_none());
    assert!(response.last_invalidation_time.is_some());

    // Verify persistence after a fresh DB read, not just the response.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.domain_id, Some(env.domain.into()));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        addresses[0].address,
        expected_slaac_address("2001:db8:e::".parse()?, mac)
    );
    txn.rollback().await?;

    Ok(())
}

// DHCPv6 information-request on a SLAAC-ineligible segment still returns FQDN
// options after a prior lease expiration, without rejoining DNS.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_with_non_64_prefix_keeps_fqdn_after_expiration(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_dhcp_expiry(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:0e").unwrap();

    // Make the segment v6-enabled but SLAAC-ineligible, then create a v4 lease.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:10::/80", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response
        .machine_interface_id
        .expect("DHCP response should include an interface id");

    // Expire the only address so the persisted row becomes DNS-silent.
    env.api
        .expire_dhcp_lease(tonic::Request::new(ExpireDhcpLeaseRequest {
            ip_address: v4_response.address,
            mac_address: Some(mac.to_string()),
        }))
        .await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert!(interface.addresses.is_empty());
    assert!(interface.domain_id.is_none());
    txn.rollback().await?;

    // Request v6 options; no SLAAC address is eligible, but FQDN should survive.
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:10::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.machine_interface_id, Some(interface_id));
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert!(response.gateway.is_none());
    assert_eq!(response.subdomain_id, Some(env.domain.into()));
    assert!(response.last_invalidation_time.is_some());

    // Verify no address was persisted and FQDN came from the segment domain.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.domain_id, None);
    assert_eq!(response.fqdn, format!("{}.dwrt1.com", interface.hostname));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert!(addresses.is_empty());
    txn.rollback().await?;

    Ok(())
}

// A DHCPv6 relay link-address can identify the segment even when it sits
// outside the segment's IPv6 prefix.
#[crate::sqlx_test]
async fn test_dhcp_v6_link_address_matches_segment_outside_prefix(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:03").unwrap();

    // Seed a DHCPv6 link-address outside the prefix and use it as the relay.
    add_ipv6_prefix(
        &pool,
        env.admin_segment(),
        "2001:db8:5::/64",
        Some("2001:db8:ffff::1"),
    )
    .await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:ffff::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(response.segment_id.unwrap(), env.admin_segment());
    assert!(response.address.parse::<IpAddr>()?.is_ipv6());

    // Verify a nearby address with no equality match resolves to no segment.
    let mut txn = pool.begin().await?;
    let missing = db::network_segment::for_relay(&mut txn, "2001:db8:ffff::2".parse()?).await?;
    assert!(missing.is_none());
    txn.rollback().await?;

    Ok(())
}

// An exact DHCPv6 link-address match is the relay's authoritative segment,
// even when the link-address also falls inside another segment's prefix.
#[crate::sqlx_test]
async fn test_dhcp_v6_link_address_exact_match_precedes_prefix_candidate(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let prefix_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000201")?;
    let exact_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000202")?;
    let relay = "2001:db8:b::1";
    let mac = MacAddress::from_str("02:00:00:00:00:22").unwrap();

    // The lower UUID segment owns the relay by prefix containment.
    create_admin_network_segment_with_id(
        &env,
        prefix_segment,
        "ADMIN_V6_PREFIX_CONTAINS_LINK",
        "192.0.62.0/24",
        "192.0.62.1",
    )
    .await?;
    add_ipv6_prefix(&pool, prefix_segment, "2001:db8:b::/64", None).await?;

    // The higher UUID segment owns the relay by exact DHCPv6 link-address.
    create_admin_network_segment_with_id(
        &env,
        exact_segment,
        "ADMIN_V6_EXACT_LINK",
        "192.0.63.0/24",
        "192.0.63.1",
    )
    .await?;
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:a::/64", Some(relay)).await?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(mac, relay, RPC_MESSAGE_KIND_V6_SOLICIT))
        .await?
        .into_inner();
    assert_eq!(response.segment_id, Some(exact_segment));
    assert!(response.address.parse::<IpAddr>()?.is_ipv6());

    Ok(())
}

// Stateful DHCPv6 must not let a reserved prefix fallback veto a dynamic exact
// link-address candidate. IPv4 keeps the old all-candidate reserved veto.
#[crate::sqlx_test]
async fn test_dhcp_v6_solicit_exact_link_precedes_reserved_prefix_candidate(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let reserved_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000205")?;
    let exact_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000206")?;
    let relay = "2001:db8:82::1";
    let mac = MacAddress::from_str("02:00:00:00:00:25").unwrap();

    // A reserved prefix fallback contains the relay.
    create_admin_network_segment_with_id(
        &env,
        reserved_segment,
        "ADMIN_V6_RESERVED_SOLICIT_PREFIX",
        "192.0.82.0/24",
        "192.0.82.1",
    )
    .await?;
    add_ipv6_prefix(&pool, reserved_segment, "2001:db8:82::/64", None).await?;
    set_segment_reserved(&pool, reserved_segment).await?;

    // A dynamic exact link-address candidate is authoritative.
    create_admin_network_segment_with_id(
        &env,
        exact_segment,
        "ADMIN_V6_EXACT_SOLICIT_DYNAMIC",
        "192.0.83.0/24",
        "192.0.83.1",
    )
    .await?;
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:83::/64", Some(relay)).await?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(mac, relay, RPC_MESSAGE_KIND_V6_SOLICIT))
        .await?
        .into_inner();
    let response_address: IpAddr = response.address.parse()?;
    assert_eq!(response.segment_id, Some(exact_segment));
    assert!(response_address.is_ipv6());

    // Verify the dynamic allocation was persisted only on the exact segment.
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    txn.rollback().await?;
    assert_eq!(interface.segment_id, exact_segment);
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, response_address);

    Ok(())
}

// A legacy Host declaration that omits the new allocation policy keeps its
// typed segment field as a first-lease selector, so an exact DHCPv6
// link-address remains authoritative.
#[crate::sqlx_test]
async fn test_dhcp_v6_solicit_exact_link_preserves_legacy_typed_segment_behavior(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let relay = "2001:db8:90::1";
    let bmc_mac: MacAddress = "02:00:00:00:00:27".parse().unwrap();
    let host_mac: MacAddress = "02:00:00:00:00:28".parse().unwrap();

    // Create a declared-type prefix fallback and a different-type exact match.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:90::/64", None).await?;
    let exact_segment = create_network_segment(
        &env.api,
        "UNDERLAY_V6_EXACT_BEATS_EXPECTED_ADMIN",
        "192.0.90.0/24",
        "192.0.90.1",
        rpc::forge::NetworkSegmentType::Underlay,
        None,
        true,
    )
    .await;
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:91::/64", Some(relay)).await?;

    // The typed field selects Admin for prefix matching, but omission of the
    // new policy keeps it from guarding the authoritative exact link-address.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-EXACT-TYPE-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: host_mac.to_string(),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            host_mac,
            relay,
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    let response_address: IpAddr = response.address.parse()?;
    assert_eq!(response.segment_id, Some(exact_segment));
    assert!(response_address.is_ipv6());

    // Verify persistence came from the exact segment, not the declared-type fallback.
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, host_mac).await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    txn.rollback().await?;
    assert_eq!(interface.segment_id, exact_segment);
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, response_address);

    Ok(())
}

// INFORMATION-REQUEST uses the same legacy Host compatibility rule, so its
// SLAAC EUI-64 inference follows the authoritative exact link-address segment.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_exact_link_preserves_legacy_typed_segment_behavior(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let relay = "2001:db8:92::1";
    let bmc_mac: MacAddress = "02:00:00:00:00:29".parse().unwrap();
    let host_mac: MacAddress = "02:00:00:00:00:2a".parse().unwrap();

    // Create a declared-type prefix fallback and a different-type exact match.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:92::/64", None).await?;
    let exact_segment = create_network_segment(
        &env.api,
        "UNDERLAY_V6_INFO_EXACT_BEATS_EXPECTED_ADMIN",
        "192.0.92.0/24",
        "192.0.92.1",
        rpc::forge::NetworkSegmentType::Underlay,
        None,
        true,
    )
    .await;
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:93::/64", Some(relay)).await?;
    enable_slaac_eui64_inference(&pool, exact_segment).await?;

    // Omission of the new policy keeps the typed Admin field from guarding the
    // authoritative Underlay link-address.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-INFO-EXACT-TYPE-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: host_mac.to_string(),
                network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                primary: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            host_mac,
            relay,
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert_eq!(response.segment_id, Some(exact_segment));

    // Verify SLAAC EUI-64 inference used the exact segment prefix.
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, host_mac).await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    txn.rollback().await?;
    assert_eq!(interface.segment_id, exact_segment);
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        addresses[0].address,
        expected_slaac_address("2001:db8:93::".parse()?, host_mac)
    );

    Ok(())
}

// A typed segment guard rejects an exact DHCPv6 link-address from a different
// segment type for every allocation-bearing DHCPv6 message.
#[crate::sqlx_test]
async fn test_dhcp_v6_exact_link_honors_expected_segment_type_guard(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    struct Case {
        name: &'static str,
        message_kind: i32,
        relay: &'static str,
        fallback_prefix: &'static str,
        fallback_ipv4_prefix: &'static str,
        fallback_ipv4_gateway: &'static str,
        exact_prefix: &'static str,
        exact_ipv4_prefix: &'static str,
        exact_ipv4_gateway: &'static str,
        bmc_mac: &'static str,
        host_mac: &'static str,
    }

    for case in [
        Case {
            name: "SOLICIT",
            message_kind: RPC_MESSAGE_KIND_V6_SOLICIT,
            relay: "2001:db8:92::1",
            fallback_prefix: "2001:db8:92::/64",
            fallback_ipv4_prefix: "192.0.92.0/24",
            fallback_ipv4_gateway: "192.0.92.1",
            exact_prefix: "2001:db8:93::/64",
            exact_ipv4_prefix: "192.0.93.0/24",
            exact_ipv4_gateway: "192.0.93.1",
            bmc_mac: "02:00:00:00:00:29",
            host_mac: "02:00:00:00:00:2a",
        },
        Case {
            name: "INFO_REQUEST",
            message_kind: RPC_MESSAGE_KIND_V6_INFO_REQUEST,
            relay: "2001:db8:94::1",
            fallback_prefix: "2001:db8:94::/64",
            fallback_ipv4_prefix: "192.0.94.0/24",
            fallback_ipv4_gateway: "192.0.94.1",
            exact_prefix: "2001:db8:95::/64",
            exact_ipv4_prefix: "192.0.95.0/24",
            exact_ipv4_gateway: "192.0.95.1",
            bmc_mac: "02:00:00:00:00:2b",
            host_mac: "02:00:00:00:00:2c",
        },
    ] {
        let bmc_mac: MacAddress = case.bmc_mac.parse()?;
        let host_mac: MacAddress = case.host_mac.parse()?;

        // Create a declared-type prefix fallback and a different-type exact match.
        let fallback_segment_name = format!("ADMIN_V6_{}_EXPECTED_FALLBACK", case.name);
        let fallback_segment = create_network_segment(
            &env.api,
            &fallback_segment_name,
            case.fallback_ipv4_prefix,
            case.fallback_ipv4_gateway,
            rpc::forge::NetworkSegmentType::Admin,
            None,
            true,
        )
        .await;
        add_ipv6_prefix(&pool, fallback_segment, case.fallback_prefix, None).await?;
        let exact_segment_name = format!("UNDERLAY_V6_{}_EXACT_BEATS_EXPECTED_ADMIN", case.name);
        let exact_segment = create_network_segment(
            &env.api,
            &exact_segment_name,
            case.exact_ipv4_prefix,
            case.exact_ipv4_gateway,
            rpc::forge::NetworkSegmentType::Underlay,
            None,
            true,
        )
        .await;
        add_ipv6_prefix(&pool, exact_segment, case.exact_prefix, Some(case.relay)).await?;

        // Declare the host NIC as Admin; the exact Underlay link-address must be
        // rejected.
        env.api
            .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
                id: None,
                bmc_mac_address: bmc_mac.to_string(),
                bmc_username: "ADMIN".into(),
                bmc_password: "PASS".into(),
                chassis_serial_number: format!("DHCP6-{}-GUARD", case.name),
                host_nics: vec![rpc::forge::ExpectedInterface {
                    mac_address: host_mac.to_string(),
                    network_segment_type: Some(rpc::forge::NetworkSegmentType::Admin as i32),
                    ip_allocation: Some(rpc::forge::ExpectedInterfaceIpAllocation::Dynamic as i32),
                    primary: Some(true),
                    ..Default::default()
                }],
                ..Default::default()
            }))
            .await?;

        let status = env
            .api
            .discover_dhcp(dhcpv6_discovery(host_mac, case.relay, case.message_kind))
            .await
            .expect_err("the typed segment guard should reject a different exact-link segment");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert!(
            status
                .message()
                .contains("do not identify the expected admin network segment type"),
            "{status}",
        );

        // A rejected discover must not create an observed interface.
        let mut txn = pool.begin().await?;
        let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, host_mac).await?;
        txn.rollback().await?;
        assert!(interfaces.is_empty(), "case: {}", case.name);
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_dhcp_rejects_explicit_protocol_family_mismatched_with_relay_context(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let v6_request_mac = MacAddress::from_str("02:00:00:00:00:23").unwrap();
    let v4_request_mac = MacAddress::from_str("02:00:00:00:00:24").unwrap();
    let v6_link_address = "2001:db8:ffff:18::1";

    // Make the admin segment dual-stack and addressable by DHCPv6 link-address
    // so the old path could resolve it if protocol parsing allowed the mismatch.
    add_ipv6_prefix(
        &pool,
        env.admin_segment(),
        "2001:db8:18::/64",
        Some(v6_link_address),
    )
    .await?;

    // Explicit IPv6 protocol fields cannot use an IPv4 relay context.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            v6_request_mac,
            FIXTURE_DHCP_RELAY_ADDRESS,
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await
        .expect_err("IPv6 protocol fields with an IPv4 relay should reject");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status
            .message()
            .contains("address_family must match relay/link-address family")
    );

    // Explicit IPv4 protocol fields cannot use an IPv6 link-address context.
    let mut v4_request = DhcpDiscovery::builder(v4_request_mac, FIXTURE_DHCP_RELAY_ADDRESS)
        .address_family(RPC_ADDRESS_FAMILY_V4)
        .message_kind(RPC_MESSAGE_KIND_V4_DISCOVER)
        .tonic_request();
    v4_request.get_mut().link_address = Some(v6_link_address.to_string());
    let status = env
        .api
        .discover_dhcp(v4_request)
        .await
        .expect_err("IPv4 protocol fields with an IPv6 link-address should reject");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status
            .message()
            .contains("address_family must match relay/link-address family")
    );

    // Both rejects happen before interface/address persistence.
    let mut txn = pool.begin().await?;
    for mac in [v6_request_mac, v4_request_mac] {
        let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
        assert!(interfaces.is_empty());
    }
    txn.rollback().await?;

    Ok(())
}

// Existing-machine detection uses DHCP relay semantics, including an
// off-prefix DHCPv6 link-address.
#[crate::sqlx_test]
async fn test_dhcp_v6_find_existing_machine_uses_link_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let host = create_managed_host(&env).await;
    let (host_mac, gateway) = host_interface_and_gateway(&env, host.host().id).await?;
    let host_segment = {
        let mut txn = pool.begin().await?;
        let segment = db::network_segment::for_relay(&mut txn, gateway)
            .await?
            .expect("host segment should resolve by its IPv4 gateway");
        txn.rollback().await?;
        segment.id
    };

    // The link-address is deliberately outside the segment prefix; relay lookup
    // should still find the known host machine.
    let link_address: IpAddr = "2001:db8:ffff:16::1".parse()?;
    add_ipv6_prefix(
        &pool,
        host_segment,
        "2001:db8:16::/64",
        Some("2001:db8:ffff:16::1"),
    )
    .await?;
    let mut txn = pool.begin().await?;
    let machine_id = db::machine::find_existing_machine(&mut txn, host_mac, link_address).await?;
    assert_eq!(machine_id, Some(host.host().id.into()));
    txn.rollback().await?;

    Ok(())
}

// Exact DHCPv6 link-address ownership must beat a known machine on a prefix
// fallback segment; otherwise known-machine lookup skips segment reconciliation.
#[crate::sqlx_test]
async fn test_dhcp_v6_exact_link_rejects_known_machine_on_prefix_fallback(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let host = create_managed_host(&env).await;
    let (host_mac, gateway) = host_interface_and_gateway(&env, host.host().id).await?;
    let exact_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000204")?;
    let relay = "2001:db8:80::1";

    // Put the known host on a segment whose prefix contains the relay.
    let host_segment = {
        let mut txn = pool.begin().await?;
        let segment = db::network_segment::for_relay(&mut txn, gateway)
            .await?
            .expect("host segment should resolve by gateway");
        txn.rollback().await?;
        segment.id
    };
    add_ipv6_prefix(&pool, host_segment, "2001:db8:80::/64", None).await?;

    // Put the exact DHCPv6 link-address on a different segment.
    create_admin_network_segment_with_id(
        &env,
        exact_segment,
        "ADMIN_V6_EXACT_REJECTS_KNOWN_PREFIX",
        "192.0.80.0/24",
        "192.0.80.1",
    )
    .await?;
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:81::/64", Some(relay)).await?;

    // Known-machine lookup must not accept the prefix fallback when exact exists.
    let mut txn = pool.begin().await?;
    let machine_id = db::machine::find_existing_machine(&mut txn, host_mac, relay.parse()?).await?;
    assert!(machine_id.is_none());
    txn.rollback().await?;

    // The DHCP path should hit the existing-MAC segment guard and reject.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            host_mac,
            relay,
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await
        .expect_err("known MAC on prefix fallback should reject exact-link DHCP");
    assert_eq!(status.code(), tonic::Code::Internal);
    assert!(
        status
            .message()
            .contains("Network segment mismatch for existing MAC address")
    );

    Ok(())
}

// If NICo first inferred a SLAAC EUI-64 address and the client later asks for
// stateful DHCPv6, the stateful allocation replaces the inferred row.
#[crate::sqlx_test]
async fn test_dhcp_v6_stateful_replaces_prior_inferred_slaac_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:04").unwrap();

    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:6::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    env.api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:6::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?;
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:6::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(response.machine_interface_id, Some(interface_id));
    let response_address: IpAddr = response.address.parse()?;

    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, response_address);

    Ok(())
}

// Fixed IPv6 reservations still materialize when SLAAC EUI-64 inference is
// disabled for the segment.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_materializes_fixed_reservation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let bmc_mac = MacAddress::from_str("02:00:00:00:00:09").unwrap();
    let mac = MacAddress::from_str("02:00:00:00:00:0a").unwrap();
    let fixed_ip: IpAddr = "2001:db8:a::55".parse()?;

    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:a::/64", None).await?;
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-FIXED-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.to_string()),
                fixed_mask: None,
                fixed_gateway: None,
                primary: None,
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    let info_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:a::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(info_response.address, "");
    assert_eq!(info_response.prefix, "");

    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    assert_eq!(addresses[0].address, fixed_ip);

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:a::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, fixed_ip.to_string());

    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    assert_eq!(addresses[0].address, fixed_ip);

    Ok(())
}

// Fixed IPv6 reservations assigned to an existing addressless interface must
// restore the segment domain so the DHCP projection can return the lease.
#[crate::sqlx_test]
async fn test_dhcp_v6_fixed_reservation_restores_domain_after_v4_expiration(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_dhcp_expiry(pool.clone()).await;
    let bmc_mac = MacAddress::from_str("02:00:00:00:00:15").unwrap();
    let mac = MacAddress::from_str("02:00:00:00:00:16").unwrap();
    let fixed_ip: IpAddr = "2001:db8:15::55".parse()?;

    // Create then expire a v4 lease so the existing interface has no domain.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:15::/64", None).await?;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response.machine_interface_id.unwrap();
    env.api
        .expire_dhcp_lease(tonic::Request::new(ExpireDhcpLeaseRequest {
            ip_address: v4_response.address,
            mac_address: Some(mac.to_string()),
        }))
        .await?;

    // Configure the fixed IPv6 reservation after the row is addressless.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-FIXED-EXPIRED-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.to_string()),
                fixed_mask: None,
                fixed_gateway: None,
                primary: None,
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // Stateful DHCPv6 should materialize and serve the fixed reservation.
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:15::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(response.machine_interface_id, Some(interface_id));
    assert_eq!(response.address, fixed_ip.to_string());

    // Verify the address assignment restored domain membership in storage.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.domain_id, Some(env.domain.into()));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    assert_eq!(addresses[0].address, fixed_ip);
    txn.rollback().await?;

    Ok(())
}

// Fixed IPv6 reservations on reserved, SLAAC-ineligible segments still return
// options-only metadata and persist the configured static address.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_materializes_fixed_reservation_on_reserved_non_64(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let bmc_mac = MacAddress::from_str("02:00:00:00:00:0f").unwrap();
    let mac = MacAddress::from_str("02:00:00:00:00:10").unwrap();
    let fixed_ip: IpAddr = "2001:db8:11::55".parse()?;

    // Make the admin segment IPv6-enabled but SLAAC-ineligible and static-only.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:11::/80", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    set_segment_reserved(&pool, env.admin_segment()).await?;

    // Configure an expected-host reservation that should satisfy the reserved segment.
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            id: None,
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-FIXED-RESERVED-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                network_segment_type: None,
                mac_address: mac.to_string(),
                nic_type: Some("onboard".into()),
                fixed_ip: Some(fixed_ip.to_string()),
                fixed_mask: None,
                fixed_gateway: None,
                primary: None,
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    // INFORMATION-REQUEST must return options only, not allocate dynamically.
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:11::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert!(response.gateway.is_none());
    assert_eq!(response.segment_id, Some(env.admin_segment()));
    assert_eq!(response.subdomain_id, Some(env.domain.into()));

    // Verify the expected-machine fixed_ip was materialized as a static
    // reservation, and response metadata came from that interface without
    // writing a SLAAC row.
    let (interface_id, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    txn.rollback().await?;
    assert_eq!(response.machine_interface_id, Some(interface_id));
    assert_eq!(response.machine_id, interface.machine_id);
    assert_eq!(response.fqdn, format!("{}.dwrt1.com", interface.hostname));
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Static);
    assert_eq!(addresses[0].address, fixed_ip);

    Ok(())
}

// Reserved IPv6-enabled segments still deliver DHCPv6 options to anonymous and
// legacy expected interfaces, but they must not create an observed interface
// row or SLAAC address without a reservation.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_reserved_segment_returns_options_only_without_observation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let anonymous_mac = MacAddress::from_str("02:00:00:00:00:11").unwrap();
    let expected_mac = MacAddress::from_str("02:00:00:00:00:30").unwrap();
    let bmc_mac = MacAddress::from_str("02:00:00:00:00:31").unwrap();

    // Make the admin segment v6-enabled and reserved-only before first contact.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:12::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    set_segment_reserved(&pool, env.admin_segment()).await?;
    env.api
        .add_expected_machine(tonic::Request::new(rpc::forge::ExpectedMachine {
            bmc_mac_address: bmc_mac.to_string(),
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            chassis_serial_number: "EM-DHCPV6-RESERVED-OPTIONS-001".into(),
            host_nics: vec![rpc::forge::ExpectedInterface {
                mac_address: expected_mac.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }))
        .await?;

    for (case, mac) in [
        ("anonymous interface", anonymous_mac),
        ("legacy expected interface", expected_mac),
    ] {
        let response = env
            .api
            .discover_dhcp(dhcpv6_discovery(
                mac,
                "2001:db8:12::1",
                RPC_MESSAGE_KIND_V6_INFO_REQUEST,
            ))
            .await?
            .into_inner();
        assert_eq!(response.address, "", "case: {case}");
        assert_eq!(response.prefix, "", "case: {case}");
        assert!(response.gateway.is_none(), "case: {case}");
        assert_eq!(response.mac_address, mac.to_string(), "case: {case}");
        assert_eq!(response.machine_id, None, "case: {case}");
        assert_eq!(response.machine_interface_id, None, "case: {case}");
        assert_eq!(
            response.segment_id,
            Some(env.admin_segment()),
            "case: {case}",
        );
        assert_eq!(
            response.subdomain_id,
            Some(env.domain.into()),
            "case: {case}",
        );
        assert!(response.last_invalidation_time.is_some(), "case: {case}");
    }

    // Verify the options request did not persist an observed interface.
    let mut txn = pool.begin().await?;
    for (case, mac) in [
        ("anonymous interface", anonymous_mac),
        ("legacy expected interface", expected_mac),
    ] {
        let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
        assert!(interfaces.is_empty(), "case: {case}");
    }
    txn.rollback().await?;

    Ok(())
}

// Known same-segment reserved INFORMATION-REQUESTs still return options, but
// must pass through common bookkeeping such as last_dhcp updates.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_reserved_known_interface_updates_last_dhcp(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:26").unwrap();

    // Create a known same-segment interface with an IPv4 lease.
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response
        .machine_interface_id
        .expect("DHCP should create interface");

    // Make the same segment IPv6-enabled and reserved-only, then age last_dhcp.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:84::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    set_segment_reserved(&pool, env.admin_segment()).await?;
    let old_last_dhcp = chrono::Utc::now() - chrono::Duration::days(1);
    let mut txn = pool.begin().await?;
    db::machine_interface::update_last_dhcp(&mut txn, interface_id, Some(old_last_dhcp)).await?;
    txn.commit().await?;

    // The response is options-only, but the known interface path runs bookkeeping.
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:84::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert_eq!(response.machine_interface_id, Some(interface_id));

    // Verify last_dhcp advanced and no IPv6 address was persisted.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert!(interface.last_dhcp.expect("last_dhcp should be set") > old_last_dhcp);
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert_eq!(addresses.len(), 1);
    assert!(addresses[0].address.is_ipv4());
    txn.rollback().await?;

    Ok(())
}

// Known reserved INFORMATION-REQUESTs must not bypass dormant interface checks.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_reserved_dormant_admin_interface_rejects(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    // Create a multi-DPU host and find a dormant DPU-backed admin interface.
    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = pool.begin().await?;
    let mut interface_map = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id]).await?;
    let dormant_interface = interface_map
        .remove(&mh.id)
        .expect("multi-DPU host has machine interfaces")
        .into_iter()
        .find(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
                && !interface.primary_interface
        })
        .expect("multi-DPU host has a dormant admin interface");
    txn.rollback().await?;

    // Make the dormant interface's segment IPv6-enabled and reserved-only.
    add_ipv6_prefix(
        &pool,
        dormant_interface.segment_id,
        "2001:db8:85::/64",
        None,
    )
    .await?;
    set_segment_reserved(&pool, dormant_interface.segment_id).await?;

    // The request must enter the common path and reject as dormant.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            dormant_interface.mac_address,
            "2001:db8:85::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await
        .expect_err("dormant reserved INFO_REQUEST should reject");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        status
            .message()
            .contains("dormant non-primary admin interface")
    );

    Ok(())
}

// Reserved segments still enforce the global MAC guard before options delivery.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_reserved_segment_rejects_known_interface_on_other_segment(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_dhcp_expiry(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:12").unwrap();

    // Create then expire a v4 lease on another managed segment.
    let other_segment = create_network_segment(
        &env.api,
        "ADMIN_RESERVED_INFO_SRC",
        "192.0.40.0/24",
        "192.0.40.1",
        rpc::forge::NetworkSegmentType::Admin,
        None,
        true,
    )
    .await;
    let v4_response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(mac, "192.0.40.1").tonic_request())
        .await?
        .into_inner();
    let interface_id = v4_response
        .machine_interface_id
        .expect("DHCP response should include an interface id");
    env.api
        .expire_dhcp_lease(tonic::Request::new(ExpireDhcpLeaseRequest {
            ip_address: v4_response.address,
            mac_address: Some(mac.to_string()),
        }))
        .await?;
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:13::/64", None).await?;
    set_segment_reserved(&pool, env.admin_segment()).await?;

    // Request options on the reserved v6 segment; the wrong-segment MAC must
    // reject before options construction.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:13::1",
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

    // Verify the known interface stayed on its original segment without an address.
    let mut txn = pool.begin().await?;
    let interface = db::machine_interface::find_one(&mut *txn, interface_id).await?;
    assert_eq!(interface.segment_id, other_segment);
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interface_id).await?;
    assert!(addresses.is_empty());
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    txn.rollback().await?;

    Ok(())
}

// Non-reserved information-request enforces the same global MAC guard as
// IPv4/stateful DHCP before options delivery.
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

// A stateful DHCPv6 row is authoritative; later information-requests must not
// add a coexisting SLAAC row for the same interface.
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

    env.api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:7::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?;

    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, stateful_address);

    Ok(())
}

#[crate::sqlx_test]
async fn test_dhcp_v6_solicit_exact_link_exhaustion_does_not_fallback_to_prefix_candidate(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let relay = "2001:db8:60::abcd";
    let first_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000101")?;
    let second_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000102")?;
    let first_mac = MacAddress::from_str("02:00:00:00:00:20").unwrap();
    let second_mac = MacAddress::from_str("02:00:00:00:00:21").unwrap();

    // Create the lower-sorted candidate first and give it a single IPv6 lease
    // reachable only through DHCPv6 link-address routing.
    create_admin_network_segment_with_id(
        &env,
        first_segment,
        "ADMIN_V6_FIRST_EXHAUSTED",
        "192.0.60.0/24",
        "192.0.60.1",
    )
    .await?;
    add_ipv6_prefix_with_num_reserved(&pool, first_segment, "2001:db8:61::/127", Some(relay), 1)
        .await?;
    let first_response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            first_mac,
            relay,
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await?
        .into_inner();
    assert_eq!(first_response.segment_id, Some(first_segment));

    // Add a later candidate whose prefix contains the relay. Because the first
    // candidate is an exact DHCPv6 link-address match, prefix fallback is only
    // routing context and must not receive allocation after exhaustion.
    create_admin_network_segment_with_id(
        &env,
        second_segment,
        "ADMIN_V6_SECOND_AVAILABLE",
        "192.0.61.0/24",
        "192.0.61.1",
    )
    .await?;
    add_ipv6_prefix(&pool, second_segment, "2001:db8:60::/64", None).await?;

    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            second_mac,
            relay,
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await
        .expect_err("exact DHCPv6 link-address exhaustion should not fallback");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);

    // Verify the rejected request did not persist a fallback-segment interface.
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, second_mac).await?;
    txn.rollback().await?;
    assert!(interfaces.is_empty());

    Ok(())
}

// DHCPv6 information-request must not persist an address supplied by the
// packet. Relay and desired addresses are ignored during SLAAC EUI-64 inference, so
// the requester receives only options while the computed EUI-64 row is stored.
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

// DHCPv6 information-request ignores desired_address entirely, so malformed
// allocation hints must not reject options delivery or SLAAC EUI-64 inference.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_ignores_malformed_desired_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:13").unwrap();

    // Send an otherwise valid information-request with an invalid address hint.
    add_ipv6_prefix(&pool, env.admin_segment(), "2001:db8:14::/64", None).await?;
    enable_slaac_eui64_inference(&pool, env.admin_segment()).await?;
    let mut request = dhcpv6_discovery(mac, "2001:db8:14::1", RPC_MESSAGE_KIND_V6_INFO_REQUEST);
    request.get_mut().desired_address = Some("not-an-ip".to_string());
    let response = env.api.discover_dhcp(request).await?.into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");

    // Verify the malformed hint did not suppress the computed SLAAC row.
    let (_, addresses) = interface_addresses_for_mac(&pool, mac).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        addresses[0].address,
        expected_slaac_address("2001:db8:14::".parse()?, mac)
    );

    Ok(())
}

// A relay can identify a segment through dhcpv6_link_address, but DHCPv6
// cannot be served unless the segment has an IPv6 prefix.
#[crate::sqlx_test]
async fn test_dhcp_v6_request_without_v6_prefix_is_rejected(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let mac = MacAddress::from_str("02:00:00:00:00:08").unwrap();

    set_dhcpv6_link_address_on_ipv4_prefix(&pool, env.admin_segment(), "2001:db8:9::1").await?;
    let fallback_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000109")?;

    // Add a prefix fallback that could serve IPv6. The exact link-address
    // segment remains authoritative and must reject instead of falling back.
    create_admin_network_segment_with_id(
        &env,
        fallback_segment,
        "ADMIN_V6_PREFIX_FALLBACK_WITH_V6",
        "192.0.109.0/24",
        "192.0.109.1",
    )
    .await?;
    add_ipv6_prefix(&pool, fallback_segment, "2001:db8:9::/64", None).await?;

    // Information-request must fail before returning options-only metadata.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:9::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await
        .expect_err("DHCPv6 information-request without a v6 prefix should fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(status.message().contains("without an IPv6 prefix"));

    // Stateful solicit must fail for the same segment configuration.
    let status = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:9::1",
            RPC_MESSAGE_KIND_V6_SOLICIT,
        ))
        .await
        .expect_err("DHCPv6 solicit without a v6 prefix should fail");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(status.message().contains("without an IPv6 prefix"));

    // The failed requests must not persist an observed interface row.
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert!(interfaces.is_empty());
    txn.rollback().await?;

    Ok(())
}

// Options-only first contact should consume the pending predicted interface
// even when the segment does not allow SLAAC EUI-64 inference.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_promotes_predicted_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_host_inband(pool.clone()).await;
    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    let mac = *mock_host.non_dpu_macs.first().unwrap();

    // Zero-DPU ingestion creates machine identity plus a predicted interface,
    // but intentionally does not create the runtime machine_interfaces row yet.
    let _mock = site_explorer::ingest_zero_dpu_host_awaiting_first_lease(&env, mock_host).await?;

    // Capture the predicted identity and target host-inband segment before
    // DHCPv6 first contact consumes the prediction.
    let (machine_id, host_inband_segment_id) = {
        let mut txn = pool.begin().await?;
        let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac)
            .await?
            .expect("zero-DPU ingest should have minted a predicted interface");
        let host_inband_segment = db::network_segment::for_relay(
            &mut txn,
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip(),
        )
        .await?
        .expect("host-inband segment should resolve from fixture gateway");
        assert!(
            db::machine_interface::find_by_mac_address(&mut *txn, mac)
                .await?
                .is_empty(),
            "the in-band NIC should not have a machine_interfaces row yet",
        );
        txn.rollback().await?;
        (predicted.machine_id, host_inband_segment.id)
    };

    // Give the predicted segment an IPv6 /64, but leave EUI-64 inference at
    // its default of `false`. The Information-Request should still promote the
    // interface and return options metadata.
    add_ipv6_prefix(&pool, host_inband_segment_id, "2001:db8:b::/64", None).await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:b::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert_eq!(response.machine_id, Some(machine_id));
    assert_eq!(response.segment_id, Some(host_inband_segment_id));

    // The prediction becomes a real interface, but there is no address for DNS
    // or zero-DPU instance allocation to consume.
    let mut txn = pool.begin().await?;
    let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac).await?;
    assert!(predicted.is_none());
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].machine_id, Some(machine_id));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert!(addresses.is_empty());
    txn.rollback().await?;

    Ok(())
}

// Reserved segments block anonymous observation, but a predicted interface is
// explicit identity and should still be promoted through the common path.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_on_reserved_segment_promotes_predicted_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_host_inband(pool.clone()).await;
    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    let mac = *mock_host.non_dpu_macs.first().unwrap();

    // Ingest a zero-DPU host so the INFO_REQUEST must consume a prediction.
    let _mock = site_explorer::ingest_zero_dpu_host_awaiting_first_lease(&env, mock_host).await?;

    // Capture the predicted machine and target segment before DHCP promotion.
    let (machine_id, host_inband_segment_id) = {
        let mut txn = pool.begin().await?;
        let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac)
            .await?
            .expect("zero-DPU ingest should have minted a predicted interface");
        let host_inband_segment = db::network_segment::for_relay(
            &mut txn,
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip(),
        )
        .await?
        .expect("host-inband segment should resolve from fixture gateway");
        txn.rollback().await?;
        (predicted.machine_id, host_inband_segment.id)
    };

    // Make the predicted segment IPv6-capable but reserved-only.
    add_ipv6_prefix(&pool, host_inband_segment_id, "2001:db8:86::/64", None).await?;
    enable_slaac_eui64_inference(&pool, host_inband_segment_id).await?;
    set_segment_reserved(&pool, host_inband_segment_id).await?;

    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            "2001:db8:86::1",
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert_eq!(response.machine_id, Some(machine_id));
    assert_eq!(response.segment_id, Some(host_inband_segment_id));

    // Verify prediction cleanup, interface promotion, and no SLAAC persistence.
    let mut txn = pool.begin().await?;
    let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac).await?;
    assert!(predicted.is_none());
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].machine_id, Some(machine_id));
    assert_eq!(interfaces[0].segment_id, host_inband_segment_id);
    assert!(interfaces[0].last_dhcp.is_some());
    assert_eq!(response.machine_interface_id, Some(interfaces[0].id));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert!(addresses.is_empty());
    txn.rollback().await?;

    Ok(())
}

// Exact DHCPv6 link-address routing must beat a reserved prefix-containing
// candidate, so INFORMATION-REQUEST still promotes and observes on the exact segment.
#[crate::sqlx_test]
async fn test_dhcp_v6_info_request_exact_link_precedes_reserved_prefix_candidate(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_host_inband(pool.clone()).await;
    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    let mac = *mock_host.non_dpu_macs.first().unwrap();
    let relay = "2001:db8:73::1";
    let reserved_segment = NetworkSegmentId::from_str("00000000-0000-0000-0000-000000000203")?;

    // Ingest a zero-DPU host so the INFO_REQUEST must consume a prediction.
    let _mock = site_explorer::ingest_zero_dpu_host_awaiting_first_lease(&env, mock_host).await?;

    // Capture the predicted machine and exact target segment before DHCP promotion.
    let (machine_id, exact_segment) = {
        let mut txn = pool.begin().await?;
        let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac)
            .await?
            .expect("zero-DPU ingest should have minted a predicted interface");
        let host_inband_segment = db::network_segment::for_relay(
            &mut txn,
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip(),
        )
        .await?
        .expect("host-inband segment should resolve from fixture gateway");
        txn.rollback().await?;
        (predicted.machine_id, host_inband_segment.id)
    };

    // Add a reserved prefix candidate that contains the relay address.
    create_admin_network_segment_with_id(
        &env,
        reserved_segment,
        "ADMIN_V6_RESERVED_PREFIX_COMPETES",
        "192.0.73.0/24",
        "192.0.73.1",
    )
    .await?;
    add_ipv6_prefix(&pool, reserved_segment, "2001:db8:73::/64", None).await?;
    set_segment_reserved(&pool, reserved_segment).await?;

    // Add the authoritative exact DHCPv6 link-address on the predicted segment.
    add_ipv6_prefix(&pool, exact_segment, "2001:db8:74::/64", Some(relay)).await?;
    enable_slaac_eui64_inference(&pool, exact_segment).await?;
    let response = env
        .api
        .discover_dhcp(dhcpv6_discovery(
            mac,
            relay,
            RPC_MESSAGE_KIND_V6_INFO_REQUEST,
        ))
        .await?
        .into_inner();
    assert_eq!(response.address, "");
    assert_eq!(response.prefix, "");
    assert_eq!(response.segment_id, Some(exact_segment));

    // Verify the request promoted the prediction and updated the exact segment row.
    let mut txn = pool.begin().await?;
    let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac).await?;
    assert!(predicted.is_none());
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].machine_id, Some(machine_id));
    assert_eq!(interfaces[0].segment_id, exact_segment);
    assert!(interfaces[0].last_dhcp.is_some());
    assert_eq!(response.machine_interface_id, Some(interfaces[0].id));

    // Verify SLAAC EUI-64 inference used the exact segment prefix, not the
    // reserved prefix.
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Slaac);
    assert_eq!(
        addresses[0].address,
        expected_slaac_address("2001:db8:74::".parse()?, mac)
    );
    txn.rollback().await?;

    Ok(())
}

// Stateful DHCPv6 first contact should promote the predicted interface through
// the selected link-address without allocating an IPv4 DHCP row.
#[crate::sqlx_test]
async fn test_dhcp_v6_solicit_promotes_predicted_interface_by_link_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_host_inband(pool.clone()).await;
    let mock_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    };
    let mac = *mock_host.non_dpu_macs.first().unwrap();

    // Ingest a zero-DPU host so the DHCP request must consume a prediction.
    let _mock = site_explorer::ingest_zero_dpu_host_awaiting_first_lease(&env, mock_host).await?;

    // Capture the predicted machine and target segment before DHCP promotion.
    let (machine_id, host_inband_segment_id) = {
        let mut txn = pool.begin().await?;
        let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac)
            .await?
            .expect("zero-DPU ingest should have minted a predicted interface");
        let host_inband_segment = db::network_segment::for_relay(
            &mut txn,
            FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.ip(),
        )
        .await?
        .expect("host-inband segment should resolve from fixture gateway");
        assert!(
            db::machine_interface::find_by_mac_address(&mut *txn, mac)
                .await?
                .is_empty(),
            "the in-band NIC should not have a machine_interfaces row yet",
        );
        txn.rollback().await?;
        (predicted.machine_id, host_inband_segment.id)
    };

    // Make the predicted segment dual-stack and identify it by DHCPv6 link-address.
    add_ipv6_prefix(
        &pool,
        host_inband_segment_id,
        "2001:db8:c::/64",
        Some("2001:db8:d::1"),
    )
    .await?;
    // Send an unmatchable raw relay with the configured link-address selected.
    let mut request = dhcpv6_discovery(mac, "2001:db8:ffff::1", RPC_MESSAGE_KIND_V6_SOLICIT);
    request.get_mut().link_address = Some("2001:db8:d::1".to_string());
    let response = env.api.discover_dhcp(request).await?.into_inner();
    let response_address: IpAddr = response.address.parse()?;
    assert!(response_address.is_ipv6());

    // Verify prediction cleanup and the single persisted DHCPv6 allocation.
    let mut txn = pool.begin().await?;
    let predicted = db::predicted_machine_interface::find_by_mac_address(&mut txn, mac).await?;
    assert!(predicted.is_none());
    let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].machine_id, Some(machine_id));
    assert_eq!(response.machine_interface_id, Some(interfaces[0].id));
    let addresses =
        db::machine_interface_address::find_for_interface(&mut txn, interfaces[0].id).await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].allocation_type, AllocationType::Dhcp);
    assert_eq!(addresses[0].address, response_address);
    txn.rollback().await?;

    Ok(())
}

// test_dhcp_record_missing_address_family_is_none verifies that
// find_by_mac_address reports a missing record as Ok(None) rather than an
// error. An IPv4-only interface has no IPv6 row in the machine_dhcp_records
// view, and the lookup treats that as an ordinary "no record for this family"
// outcome the DHCP path can act on, not a query failure.
#[crate::sqlx_test]
async fn test_dhcp_record_missing_address_family_is_none(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    // Create a machine via DHCPv4 discovery — gives us an interface with an
    // IPv4 address only; no IPv6 address is ever allocated.
    let mac_address = "AB:CD:EF:67:89:AB";
    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    let segment_id = response.segment_id.unwrap();
    let parsed_mac: MacAddress = mac_address.parse().unwrap();

    // An IPv6 lookup for the same interface finds no row in the view: the
    // query succeeds and the miss surfaces as None.
    let mut txn = pool.begin().await?;
    let ipv6_record = db::dhcp_record::find_by_mac_address(
        &mut txn,
        &parsed_mac,
        &segment_id,
        IpAddressFamily::Ipv6,
    )
    .await?;
    assert!(
        ipv6_record.is_none(),
        "IPv6 lookup on an IPv4-only interface should find no record, got: {ipv6_record:?}"
    );
    txn.rollback().await?;

    Ok(())
}

// test_discover_dhcp_dangling_address_is_not_found verifies the RPC-level
// contract for a dangling allocation: the interface holds an address, but no
// prefix on the segment contains it, so the machine_dhcp_records view has no
// row to answer with. discover_dhcp reports that miss as NotFound -- a state
// the caller can act on -- rather than an internal error.
#[crate::sqlx_test]
async fn test_discover_dhcp_dangling_address_is_not_found(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    // A normal DHCPv4 discovery allocates an in-prefix address.
    let mac_address = "AB:CD:EF:13:57:9B";
    env.api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .unwrap();

    // Strand the allocation: move the interface's address outside every prefix
    // on the segment. The interface still holds an IPv4 address (so discovery
    // does not re-allocate), but the machine_dhcp_records view no longer has a
    // row for it.
    let parsed_mac: MacAddress = mac_address.parse().unwrap();
    let mut txn = pool.begin().await?;
    let interfaces = db::machine_interface::find_by_mac_address(txn.as_mut(), parsed_mac).await?;
    let interface = &interfaces[0];
    let dangling: IpAddr = "198.51.100.42".parse().unwrap();
    sqlx::query(
        "UPDATE machine_interface_addresses
         -- Strand the allocation outside every segment prefix for this test.
         SET address = $1
         WHERE interface_id = $2",
    )
    .bind(dangling)
    .bind(interface.id)
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    // Re-discovery surfaces the miss as a clean NotFound, not a server fault.
    let result = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await;
    let status = result.expect_err("a dangling address should not produce a DHCP record");
    assert_eq!(status.code(), tonic::Code::NotFound);

    Ok(())
}

/// Resolve a machine_interface + its segment gateway for the given host, so
/// the test can drive a DHCP request with the same relay the real host would
/// see in production.
async fn host_interface_and_gateway(
    env: &TestEnv,
    host_machine_id: carbide_uuid::machine::HostMachineId,
) -> Result<(MacAddress, IpAddr), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    let interfaces_by_machine =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_machine_id]).await?;
    let interface = interfaces_by_machine
        .get(&host_machine_id)
        .and_then(|ifaces| ifaces.first())
        .ok_or("host has no machine_interfaces")?;
    let prefix = db::network_prefix::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::network_prefix::SegmentIdColumn, &interface.segment_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or("no network_prefix for segment")?;
    let gateway = prefix.gateway.ok_or("segment prefix has no gateway")?;
    let mac = interface.mac_address;
    txn.rollback().await?;
    Ok((mac, gateway))
}

/// Insert an `instances` row directly, bypassing the allocator (which today
/// requires DPUs + VPCs). All the DHCP branch under test reads is
/// `instances.machine_id`, so a minimal INSERT is enough.
async fn attach_bare_instance(
    env: &TestEnv,
    machine_id: carbide_uuid::machine::HostMachineId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = env.pool.begin().await?;
    sqlx::query("INSERT INTO instances (machine_id) VALUES ($1)")
        .bind(machine_id)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;
    Ok(())
}

// A host with DPUs must have its DHCP rejected once an instance is attached:
// the DPUs are expected to proxy the DHCP on the host's behalf. This preserves
// the long-standing behavior that predates zero-DPU support.
#[crate::sqlx_test]
async fn test_dhcp_rejects_dpu_host_with_instance(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    attach_bare_instance(&env, mh.host().id).await?;

    let (host_mac, gateway) = host_interface_and_gateway(&env, mh.host().id).await?;

    let result = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(host_mac, FIXTURE_DHCP_RELAY_ADDRESS)
                .link_address(gateway.to_string())
                .tonic_request(),
        )
        .await;

    let status = result.expect_err("DHCP for DPU-ful host with instance should be rejected");
    assert!(
        status
            .message()
            .contains("DHCP request received for instance"),
        "unexpected error: {}",
        status.message()
    );

    Ok(())
}

// Host BMC DHCP must continue to work after an instance is allocated on a
// DPU-backed host. The instance DHCP rejection only applies to host data/admin
// DHCP that the DPU proxies, not to out-of-band BMC management.
#[crate::sqlx_test]
async fn test_dhcp_allows_host_bmc_with_instance_on_dpu_host(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    // Find the already-linked host BMC interface.
    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_all(txn.as_mut()).await?;
    let bmc_interface = interfaces
        .iter()
        .find(|interface| {
            interface.machine_id == Some(mh.host().id.into())
                && interface.interface_type == InterfaceType::Bmc
        })
        .ok_or("host has no BMC machine_interface")?;
    let bmc_mac = bmc_interface.mac_address;
    let bmc_segment_id = bmc_interface.segment_id;

    // Resolve the BMC segment gateway for the follow-up DHCP request.
    let prefix = db::network_prefix::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::network_prefix::SegmentIdColumn, &bmc_segment_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or("no network_prefix for BMC segment")?;
    let gateway = prefix.gateway.ok_or("BMC segment prefix has no gateway")?;
    txn.rollback().await?;

    // Allocate an instance after the BMC interface is linked.
    attach_bare_instance(&env, mh.host().id).await?;

    // A later BMC DHCP request should still return the BMC lease.
    let response = env
        .api
        .discover_dhcp(DhcpDiscovery::builder(bmc_mac, gateway.to_string()).tonic_request())
        .await
        .expect("host BMC DHCP should not be rejected because the host has an instance")
        .into_inner();

    assert_eq!(response.mac_address, bmc_mac.to_string());

    Ok(())
}

// A zero-DPU host with an instance attached has no DPU intermediary, so its
// own DHCP request must be allowed through instead of being rejected on the
// assumption that a DPU will handle it.
#[crate::sqlx_test]
async fn test_dhcp_allows_zero_dpu_host_with_instance(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            site_prefixes: Some(vec![
                IpNetwork::new(
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
                IpNetwork::new(
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.network(),
                    FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY.prefix(),
                )
                .unwrap(),
            ]),
            ..Default::default()
        },
    )
    .await;
    create_host_inband_network_segment(&env.api, None).await;

    let mh = create_managed_host_with_config(&env, ManagedHostConfig::zero_dpu()).await;
    assert!(
        mh.dpu_ids.is_empty(),
        "zero-DPU fixture should produce no DPU machines"
    );

    attach_bare_instance(&env, mh.host().id).await?;

    let (host_mac, gateway) = host_interface_and_gateway(&env, mh.host().id).await?;

    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(host_mac, FIXTURE_DHCP_RELAY_ADDRESS)
                .link_address(gateway.to_string())
                .tonic_request(),
        )
        .await
        .expect("DHCP for zero-DPU host with instance should succeed")
        .into_inner();

    assert_eq!(response.mac_address, host_mac.to_string());

    Ok(())
}
