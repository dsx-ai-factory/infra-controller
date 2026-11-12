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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_authn::middleware::ConnectionAttributes;
use carbide_instrument::testing::MetricsCapture;
use carbide_uuid::machine::{
    DpuMachineId, HostMachineId, MachineId, MachineInterfaceId, StableHostMachineId,
};
use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::dpu::{create_dpu_machine, dpu_discover_dhcp, dpu_discover_machine};
use common::api_fixtures::host::{
    host_discover_dhcp, host_discover_machine, host_discover_machine_with_reporter,
};
use common::api_fixtures::{
    FIXTURE_DHCP_RELAY_ADDRESS, create_managed_host, create_managed_host_with_config,
    create_test_env,
};
use config_version::ConfigVersion;
use itertools::Itertools;
use mac_address::MacAddress;
use model::hardware_info::{HardwareInfo, PciDeviceProperties, TpmEkCertificate};
use model::machine::machine_id::from_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{HostPlatformConfigurationState, InstanceState, ManagedHostState, RetryInfo};
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::network_segment::NetworkSegmentType;
use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
use tonic::{Code, Request};

use crate::test_support::builder::TestApiBuilder;
use crate::test_support::fixture_config::{FixtureDefault, ManagedHostConfigExt as _};
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, interface_network_config_with_devices,
    single_interface_network_config,
};
use crate::tests::common::api_fixtures::tpm_attestation::{
    AK_NAME_SERIALIZED, AK_PUB_SERIALIZED, EK_PUB_SERIALIZED,
};
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::DhcpDiscovery;

fn secure_discovery_config() -> crate::cfg::file::CarbideConfig {
    let mut config = common::api_fixtures::get_config();
    config.allow_insecure_discovery = false;
    config
}

fn scout_reconciliation_config() -> crate::cfg::file::CarbideConfig {
    let mut config = common::api_fixtures::get_config();
    config.scout_boot_interface_correction_enabled = true;
    config
}

fn secure_api_for(env: &common::api_fixtures::TestEnv) -> crate::api::Api {
    TestApiBuilder::new(
        env.pool.clone(),
        env.api.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(Arc::new(secure_discovery_config()))
    .build()
}

fn discovery_request_from(
    hardware_info: &HardwareInfo,
    interface_id: Option<MachineInterfaceId>,
    remote_ip: IpAddr,
) -> Request<rpc::MachineDiscoveryInfo> {
    let mut request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: interface_id,
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(hardware_info.clone()).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });
    request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((remote_ip, 0)),
            peer_certificates: vec![],
        }));
    request
}

/// Fields these scout reconciliation tests compare across a request.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
struct ScoutPciSelectionState {
    current_primary_mac_address: Option<MacAddress>,
    primary_selection_source: Option<BootInterfaceSelectionSource>,
    primary_selection_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    current_primary_address: Option<String>,
    network_config_version: ConfigVersion,
    controller_state: String,
    controller_queued: bool,
    desired_mac_address: Option<MacAddress>,
    desired_interface_id: Option<String>,
    desired_version: Option<ConfigVersion>,
    selection_source: Option<BootInterfaceSelectionSource>,
}

/// Loads primary identity and provenance alongside address, network, state, queue and desired boot selection so independent networking decisions participate in preservation checks.
async fn load_scout_pci_selection_state(
    pool: &sqlx::PgPool,
    host_machine_id: StableHostMachineId,
) -> Result<ScoutPciSelectionState, Box<dyn std::error::Error>> {
    let state = sqlx::query_as::<_, ScoutPciSelectionState>(
        "SELECT network_primary.mac_address AS current_primary_mac_address,
                network_primary.primary_selection_source,
                network_primary.primary_selection_updated_at,
                (
                    SELECT address.address::text
                    FROM machine_interface_addresses address
                    JOIN machine_interfaces interface ON interface.id = address.interface_id
                    WHERE interface.machine_id = machine.id
                      AND interface.primary_interface
                    ORDER BY address.address::text
                    LIMIT 1
                ) AS current_primary_address,
                machine.network_config_version,
                machine.controller_state::text AS controller_state,
                EXISTS (
                    SELECT 1
                    FROM machine_state_controller_queued_objects queued
                    WHERE queued.object_id = machine.id::text
                ) AS controller_queued,
                boot_interface.desired_mac_address,
                boot_interface.desired_interface_id,
                boot_interface.desired_version,
                boot_interface.selection_source
         FROM machines machine
         LEFT JOIN machine_interfaces network_primary
           ON network_primary.machine_id = machine.id
          AND network_primary.primary_interface
          AND network_primary.interface_type != 'Bmc'
         LEFT JOIN machine_boot_interfaces boot_interface
           ON boot_interface.machine_id = machine.id
         WHERE machine.id = $1",
    )
    .bind(host_machine_id)
    .fetch_one(pool)
    .await?;
    Ok(state)
}

#[derive(Clone, Copy)]
struct ScoutPciInterfaces {
    current_id: MachineInterfaceId,
    current_mac: MacAddress,
    alternate_mac: MacAddress,
}

async fn load_scout_pci_interfaces(
    pool: &sqlx::PgPool,
    host_machine_id: StableHostMachineId,
) -> Result<ScoutPciInterfaces, Box<dyn std::error::Error>> {
    let machine = db::machine::find_one(pool, &host_machine_id, MachineSearchConfig::default())
        .await?
        .expect("managed host must exist");
    let interfaces = machine
        .status
        .interfaces
        .iter()
        .filter(|interface| {
            interface.network_segment_type == Some(NetworkSegmentType::Admin)
                && interface.attached_dpu_machine_id.is_some()
        })
        .map(|interface| {
            (
                interface.id,
                interface.mac_address,
                interface.primary_interface,
            )
        })
        .collect_vec();
    assert_eq!(
        interfaces.len(),
        2,
        "fixture must have two Admin interfaces with an attached_dpu_machine_id"
    );
    let current = interfaces
        .iter()
        .find(|(_, _, primary)| *primary)
        .expect("fixture must have one current primary interface");
    let alternate = interfaces
        .iter()
        .find(|(_, _, primary)| !primary)
        .expect("fixture must have one alternate interface");
    Ok(ScoutPciInterfaces {
        current_id: current.0,
        current_mac: current.1,
        alternate_mac: alternate.1,
    })
}

fn scout_pci_report(host_config: &ManagedHostConfig, winning_mac: MacAddress) -> HardwareInfo {
    let mut hardware_info = HardwareInfo::from(host_config);
    for (index, dpu) in host_config.dpus.iter().enumerate() {
        let interface = hardware_info
            .network_interfaces
            .iter_mut()
            .find(|interface| interface.mac_address == dpu.host_mac_address)
            .expect("each fixture DPU must have one scout interface");
        let bus = if interface.mac_address == winning_mac {
            2
        } else {
            10 + index
        };
        interface.pci_properties = Some(PciDeviceProperties {
            vendor: "0x15b3".to_string(),
            device: "0xa2dc".to_string(),
            path: format!("/devices/pci0000:00/0000:{bus:02x}:00.0/net/dpu{index}"),
            numa_node: 0,
            description: Some("DPU-backed host interface".to_string()),
            slot: Some(format!("0000:{bus:02x}:00.0")),
        });
    }
    hardware_info
}

async fn set_scout_test_source(
    pool: &sqlx::PgPool,
    host_machine_id: StableHostMachineId,
    source: BootInterfaceSelectionSource,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE machine_boot_interfaces
         SET selection_source = $1,
             selection_updated_at = TIMESTAMPTZ '2024-07-24 00:00:00+00'
         WHERE machine_id = $2",
    )
    .bind(source)
    .bind(host_machine_id)
    .execute(pool)
    .await?;
    // These fixtures give both primary and boot selections the supplied source and decision time.
    sqlx::query(
        "UPDATE machine_interfaces SET primary_selection_source = $1,
                 primary_selection_updated_at = TIMESTAMPTZ '2024-07-24 00:00:00+00'
                 WHERE machine_id = $2 AND primary_interface AND interface_type != 'Bmc'",
    )
    .bind(source)
    .bind(host_machine_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn clear_scout_test_queue(
    pool: &sqlx::PgPool,
    host_machine_id: StableHostMachineId,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_machine_id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

async fn create_scout_test_host(
    env: &common::api_fixtures::TestEnv,
    source: BootInterfaceSelectionSource,
) -> Result<(ManagedHostConfig, StableHostMachineId, ScoutPciInterfaces), Box<dyn std::error::Error>>
{
    let host_config = env.managed_host_config().with_dpu_count(2);
    let host = create_managed_host_with_config(env, host_config.clone()).await;
    let host_machine_id = host.host().id;
    let interfaces = load_scout_pci_interfaces(&env.pool, host_machine_id).await?;
    set_scout_test_source(&env.pool, host_machine_id, source).await?;
    clear_scout_test_queue(&env.pool, host_machine_id).await?;
    Ok((host_config, host_machine_id, interfaces))
}

async fn submit_scout_pci_report(
    api: &crate::api::Api,
    host_machine_id: StableHostMachineId,
    caller_interface_id: MachineInterfaceId,
    hardware_info: HardwareInfo,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut discovery_info = rpc::DiscoveryInfo::try_from(hardware_info)?;
    discovery_info.attest_key_info = Some(rpc::machine_discovery::AttestKeyInfo {
        ek_pub: EK_PUB_SERIALIZED.to_vec(),
        ak_pub: AK_PUB_SERIALIZED.to_vec(),
        ak_name: AK_NAME_SERIALIZED.to_vec(),
    });

    let (response, logs) = carbide_instrument::testing::capture_logs_async(api.discover_machine(
        Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(caller_interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(discovery_info)),
            create_machine: true,
            discovery_reporter: rpc::MachineDiscoveryReporter::Scout as i32,
            discovery_reporter_version: None,
        }),
    ))
    .await;
    let response = response?.into_inner();
    assert_eq!(response.machine_id, Some(host_machine_id.into()));
    assert_eq!(response.machine_interface_id, Some(caller_interface_id));

    Ok(scout_pci_comparisons(&logs, host_machine_id))
}

fn scout_pci_comparisons(
    logs: &[carbide_instrument::testing::CapturedLog],
    host_machine_id: StableHostMachineId,
) -> Vec<String> {
    let host_machine_id = host_machine_id.to_string();
    logs.iter()
        .filter(|log| {
            log.field("event_name") == Some("scout_pci_evaluated")
                && log.field("machine_id") == Some(host_machine_id.as_str())
        })
        .map(|log| {
            assert_eq!(
                log.field("metric_name"),
                Some("carbide_scout_pci_evaluations_total")
            );
            log.field("result")
                .expect("scout PCI comparison must record its classification")
                .to_string()
        })
        .collect()
}

async fn run_scout_pci_reconciliation(
    api: &crate::api::Api,
    host_machine_id: StableHostMachineId,
    hardware_info: &HardwareInfo,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(
        crate::handlers::update_primary_interface_from_scout_for_test(
            api,
            host_machine_id,
            hardware_info,
        )
        .await?,
    )
}

fn scout_test_allocation_request(
    host_machine_id: StableHostMachineId,
    segment_id: NetworkSegmentId,
) -> rpc::InstanceAllocationRequest {
    rpc::InstanceAllocationRequest {
        instance_id: None,
        machine_id: Some(host_machine_id),
        instance_type_id: None,
        config: Some(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        }),
        metadata: None,
        allow_unhealthy_machine: false,
        external_validation_request_id: None,
    }
}

async fn allocated_host_for_secure_discovery(
    env: &common::api_fixtures::TestEnv,
) -> (
    common::api_fixtures::TestManagedHost,
    HardwareInfo,
    IpAddr,
    MachineInterfaceId,
) {
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host_config = model::test_support::ManagedHostConfig::default();
    let hardware_info = HardwareInfo::from(&host_config);
    let managed_host = create_managed_host_with_config(env, host_config).await;
    assert_eq!(
        from_hardware_info(&hardware_info).unwrap(),
        managed_host.host().id.into()
    );

    let instance = managed_host
        .instance_builer(env)
        .config(rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        })
        .build()
        .await;

    let mut txn = env.pool.begin().await.unwrap();
    let instance_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance.id,
        &segment_id,
    )
    .await
    .unwrap();
    let [instance_address] = instance_addresses.as_slice() else {
        panic!("allocated instance must have one tenant address")
    };
    let interfaces =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[managed_host.host().id])
            .await
            .unwrap();
    let admin_interface = interfaces[&managed_host.host().id]
        .iter()
        .find(|interface| {
            interface.network_segment_type
                == Some(model::network_segment::NetworkSegmentType::Admin)
        })
        .expect("managed host must have an admin interface")
        .id;
    txn.rollback().await.unwrap();

    (
        managed_host,
        hardware_info,
        instance_address.address,
        admin_interface,
    )
}

#[crate::sqlx_test]
async fn test_machine_discovery_no_domain(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;

    let machine_interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").unwrap(),
        std::slice::from_ref(&FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()),
        None,
        None,
    )
    .await
    .expect("Unable to create machine");

    let wanted_ips: Vec<IpAddr> = vec!["192.0.2.3".parse().unwrap()]
        .into_iter()
        .sorted()
        .collect::<Vec<IpAddr>>();

    let actual_ips = machine_interface
        .addresses
        .iter()
        .copied()
        .sorted()
        .collect::<Vec<IpAddr>>();

    assert_eq!(actual_ips, wanted_ips);

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_discovery_with_domain(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");

    let machine_interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        MacAddress::from_str("ff:ff:ff:ff:ff:ff").unwrap(),
        std::slice::from_ref(&FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()),
        None,
        None,
    )
    .await
    .expect("Unable to create machine");

    let wanted_ips: Vec<IpAddr> = vec!["192.0.2.3".parse().unwrap()];

    assert_eq!(
        machine_interface
            .addresses
            .iter()
            .copied()
            .sorted()
            .collect::<Vec<IpAddr>>(),
        wanted_ips.into_iter().sorted().collect::<Vec<IpAddr>>()
    );

    assert!(
        machine_interface
            .addresses
            .iter()
            .any(|item| *item == "192.0.2.3".parse::<IpAddr>().unwrap())
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_reject_host_machine_with_disabled_tpm(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let host_machine_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

    let mut hardware_info = HardwareInfo::from(&host_config);
    hardware_info.tpm_ek_certificate = None;

    let response = env
        .api
        .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(host_machine_interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(hardware_info).unwrap(),
            )),
            create_machine: true,
            ..Default::default()
        }))
        .await;
    let err = response.expect_err("Expected DiscoverMachine request to fail");
    assert!(
        err.to_string()
            .contains("ignoring DiscoverMachine request for non-tpm enabled host")
    );

    // We shouldn't have created any machine
    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(
            rpc::forge::MachineSearchConfig::default(),
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(machine_ids.machine_ids.is_empty());

    Ok(())
}

#[crate::sqlx_test]
async fn test_discover_2_managed_hosts(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env: common::api_fixtures::TestEnv = create_test_env(pool).await;
    let (host1_id, dpu1_id) = create_managed_host(&env).await.into();
    let (host2_id, dpu2_id) = create_managed_host(&env).await.into();
    assert!(&host1_id.machine_type().is_host());
    assert!(&host2_id.machine_type().is_host());
    assert!(&dpu1_id.machine_type().is_dpu());
    assert!(&dpu2_id.machine_type().is_dpu());
    assert_ne!(host1_id, host2_id);
    assert_ne!(dpu1_id, dpu2_id);

    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: true,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;
    assert_eq!(machine_ids.len(), 4);

    Ok(())
}

#[crate::sqlx_test]
async fn test_discover_dpu_by_source_ip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let mut txn = env.pool.begin().await?;
    db::resource_pool::define(
        &mut txn,
        model::resource_pool::common::LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/125".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await?;
    txn.commit().await?;

    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();

    let dhcp_response = env
        .api
        .discover_dhcp(Request::new(rpc::forge::DhcpDiscovery {
            mac_address: dpu.oob_mac_address.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            vendor_string: None,
            link_address: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let mut req = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(HardwareInfo::from(dpu)).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });

    let dhcp_address: IpAddr = dhcp_response.address.parse().unwrap();
    req.extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((dhcp_address, 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(req).await.unwrap().into_inner();

    assert_eq!(
        response.machine_interface_id,
        dhcp_response.machine_interface_id
    );
    let dpu_machine_id = response.machine_id.expect("DPU should be created");
    let mut txn = env.pool.begin().await?;
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig::default(),
    )
    .await?
    .expect("DPU should exist");
    assert!(dpu_machine.network_config.loopback_ip.is_some());
    assert!(dpu_machine.network_config.loopback_ip_v6.is_some());
    Ok(())
}

/// Define the IPv6 loopback pool as a small auto-assign prefix so DPU discovery
/// can allocate an IPv6 underlay loopback alongside the default IPv4 pool.
async fn define_loopback_ipv6_pool(env: &common::api_fixtures::TestEnv) {
    let mut txn = env.pool.begin().await.unwrap();
    db::resource_pool::define(
        &mut txn,
        model::resource_pool::common::LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8::/125".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
}

/// Create a host expected machine carrying one DPU loopback reservation. Direct
/// discovery resolves the reservation globally by DPU OS/admin MAC, so only the child
/// row and its foreign-key parent need to exist.
async fn seed_reservation_host(
    env: &common::api_fixtures::TestEnv,
    host_bmc_mac: MacAddress,
    reservation: model::expected_machine::DpuLoopbackReservation,
) {
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData};

    let mut txn = env.pool.begin().await.unwrap();
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: host_bmc_mac,
            data: ExpectedMachineData {
                interfaces: reservation
                    .interfaces()
                    .chain([model::expected_machine::ExpectedInterface {
                        mac_address: Some(reservation.dpu_oob_net0_mac_address),
                        role: model::expected_machine::ExpectedInterfaceRole::DpuOs,
                        ip_allocation: Some(
                            model::expected_machine::ExpectedInterfaceIpAllocation::Fixed,
                        ),
                        fixed_ip: Some("192.0.2.200".parse().unwrap()),
                        ..Default::default()
                    }])
                    .collect(),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
}

/// Directly discover a DPU by its DHCP source IP, returning the created machine.
async fn discover_dpu_by_source_ip(
    env: &common::api_fixtures::TestEnv,
    dpu: &model::test_support::DpuConfig,
    hardware_info: HardwareInfo,
) -> model::machine::Machine<carbide_uuid::machine::MachineId> {
    let dhcp_response = env
        .api
        .discover_dhcp(Request::new(rpc::forge::DhcpDiscovery {
            mac_address: dpu.oob_mac_address.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            vendor_string: None,
            link_address: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let mut req = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(hardware_info).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });
    let dhcp_address: IpAddr = dhcp_response.address.parse().unwrap();
    req.extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((dhcp_address, 0)),
            peer_certificates: vec![],
        }));

    let dpu_machine_id = env
        .api
        .discover_machine(req)
        .await
        .unwrap()
        .into_inner()
        .machine_id
        .expect("DPU should be created");
    db::machine::find_one(&env.pool, &dpu_machine_id, MachineSearchConfig::default())
        .await
        .unwrap()
        .expect("DPU should exist")
}

/// Direct discovery claims the operator's reserved IPv4 and IPv6 loopbacks for a
/// DPU matched by its OS/admin MAC, instead of auto-assigning from the pool.
#[crate::sqlx_test]
async fn test_direct_discovery_applies_dpu_loopback_reservation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::net::{Ipv4Addr, Ipv6Addr};

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    define_loopback_ipv6_pool(&env).await;

    // Reservable (non-auto-assign) addresses the operator pins to this DPU.
    let reserved_v4 = Ipv4Addr::new(172, 30, 0, 10);
    let reserved_v6: Ipv6Addr = "2001:db8:1::10".parse()?;
    let mut txn = env.pool.begin().await?;
    db::resource_pool::populate(
        env.api.common_pools.ethernet.pool_loopback_ip.as_ref(),
        &mut txn,
        vec![IpAddr::V4(reserved_v4)],
        false,
    )
    .await?;
    db::resource_pool::populate(
        env.api.common_pools.ethernet.pool_loopback_ip_v6.as_ref(),
        &mut txn,
        vec![reserved_v6],
        false,
    )
    .await?;
    txn.commit().await?;

    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    seed_reservation_host(
        &env,
        "aa:bb:cc:dd:ee:10".parse()?,
        model::expected_machine::DpuLoopbackReservation {
            dpu_oob_net0_mac_address: dpu.oob_mac_address,
            loopback_ipv4: Some(reserved_v4),
            loopback_ipv6: Some(reserved_v6),
        },
    )
    .await;

    let dpu_machine = discover_dpu_by_source_ip(&env, dpu, HardwareInfo::from(dpu)).await;
    assert_eq!(
        dpu_machine.network_config.loopback_ip,
        Some(IpAddr::V4(reserved_v4))
    );
    assert_eq!(dpu_machine.network_config.loopback_ip_v6, Some(reserved_v6));
    let asn = dpu_machine
        .asn
        .expect("prepared ASN attaches alongside declared IPv6");
    let mut txn = env.pool.begin().await?;
    assert_eq!(
        env.api
            .resource_pool_registry
            .inspect_allocations(
                &env.api.common_pools.ethernet.pool_fnn_asn,
                &mut txn,
                model::resource_pool::OwnerType::Machine,
                &dpu_machine.id.to_string(),
                carbide_site_explorer::dpu_asn::DPU_ASN_SLOT,
            )
            .await?,
        vec![carbide_resource_pool::AllocationStatus::Attached { value: asn }],
    );
    txn.rollback().await?;
    let interfaces =
        db::machine_interface::find_by_mac_address(&env.pool, dpu.oob_mac_address).await?;
    assert!(
        interfaces
            .iter()
            .flat_map(|i| &i.addresses)
            .any(|address| *address == "192.0.2.200".parse::<IpAddr>().unwrap())
    );
    assert!(
        interfaces
            .iter()
            .flat_map(|i| &i.addresses)
            .all(|address| *address != IpAddr::V4(reserved_v4))
    );
    // Changing intent does not readdress this live DPU on rediscovery.
    let mut txn = env.pool.begin().await?;
    let mut expected =
        db::expected_machine::find_by_bmc_mac_address(&mut *txn, "aa:bb:cc:dd:ee:10".parse()?)
            .await?
            .expect("reservation host exists");
    expected
        .data
        .interfaces
        .retain(|interface| !interface.role.is_dpu_loopback());
    expected.data.replace_dpu_loopbacks = true;
    db::expected_machine::update(&mut txn, &expected).await?;
    txn.commit().await?;
    let rediscovered = discover_dpu_by_source_ip(&env, dpu, HardwareInfo::from(dpu)).await;
    assert_eq!(
        rediscovered.network_config.loopback_ip,
        dpu_machine.network_config.loopback_ip
    );
    assert_eq!(
        rediscovered.network_config.loopback_ip_v6,
        dpu_machine.network_config.loopback_ip_v6
    );
    // Restoring intent for an already-discovered OS MAC is accepted because
    // the resource pool's owner is that DPU, without changing its addresses.
    let mut expected = env
        .api
        .get_expected_machine(Request::new(rpc::forge::ExpectedMachineRequest {
            bmc_mac_address: "aa:bb:cc:dd:ee:10".to_string(),
            id: None,
        }))
        .await?
        .into_inner();
    expected.bmc_username = "ADMIN".to_string();
    expected.bmc_password = "PASS".to_string();
    expected.chassis_serial_number = "LOOPBACK-HOST".to_string();
    expected.host_nics.extend(
        model::expected_machine::DpuLoopbackReservation {
            dpu_oob_net0_mac_address: dpu.oob_mac_address,
            loopback_ipv4: Some(reserved_v4),
            loopback_ipv6: Some(reserved_v6),
        }
        .interfaces()
        .map(rpc::forge::ExpectedInterface::from),
    );
    env.api
        .update_expected_machine(Request::new(expected))
        .await?;
    Ok(())
}

/// Preparation must not apply a declaration superseded before the creation
/// transaction locks intent. A retry consumes the replacement reservation.
#[crate::sqlx_test]
async fn test_direct_discovery_retries_changed_loopback_reservation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::expected_machine::DpuLoopbackReservation;
    use model::resource_pool::ResourcePoolEntryState;

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    let hardware_info = HardwareInfo::from(dpu);
    let machine_id = from_hardware_info(&hardware_info)?;
    let host_bmc_mac = "aa:bb:cc:dd:ee:30".parse()?;
    let old_address = Ipv4Addr::new(172, 30, 0, 30);
    let new_address = Ipv4Addr::new(172, 30, 0, 31);
    let loopback_pool = env.api.common_pools.ethernet.pool_loopback_ip.as_ref();
    let mut txn = env.pool.begin().await?;
    db::resource_pool::populate(
        loopback_pool,
        &mut txn,
        vec![IpAddr::V4(old_address), IpAddr::V4(new_address)],
        false,
    )
    .await?;
    txn.commit().await?;
    seed_reservation_host(
        &env,
        host_bmc_mac,
        DpuLoopbackReservation {
            dpu_oob_net0_mac_address: dpu.oob_mac_address,
            loopback_ipv4: Some(old_address),
            loopback_ipv6: None,
        },
    )
    .await;
    let interface_id = dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let remote_ip = "192.0.2.200".parse()?;

    // The Integrated preparation reads existing pool ownership after capturing
    // declaration intent. Wait for that query, then commit a replacement while
    // preparation is suspended, without relying on scheduler timing.
    let mut gate = env.pool.begin().await?;
    sqlx::query("LOCK TABLE resource_pool IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *gate)
        .await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await?;
    let discovery = env.api.discover_machine(discovery_request_from(
        &hardware_info,
        Some(interface_id),
        remote_ip,
    ));
    tokio::pin!(discovery);
    tokio::select! {
        result = &mut discovery => panic!("discovery bypassed the preparation gate: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, gate_pid, "SELECT value FROM resource_pool") => {},
    }
    let mut writer = env.pool.begin().await?;
    let mut expected = db::expected_machine::find_by_bmc_mac_address(&mut *writer, host_bmc_mac)
        .await?
        .expect("reservation host exists");
    expected
        .data
        .interfaces
        .retain(|interface| !interface.role.is_dpu_loopback());
    expected.data.interfaces.extend(
        DpuLoopbackReservation {
            dpu_oob_net0_mac_address: dpu.oob_mac_address,
            loopback_ipv4: Some(new_address),
            loopback_ipv6: None,
        }
        .interfaces(),
    );
    expected.data.replace_dpu_loopbacks = true;
    db::expected_machine::update(&mut writer, &expected).await?;
    writer.commit().await?;
    gate.commit().await?;

    let error = tokio::time::timeout(Duration::from_secs(10), &mut discovery)
        .await?
        .expect_err("changed intent must defer creation");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("DPU loopback reservation changed during preparation; retry")
    );
    assert!(
        db::machine::find_one(&env.pool, &machine_id, MachineSearchConfig::default())
            .await?
            .is_none()
    );
    for address in [old_address, new_address] {
        assert!(matches!(
            db::resource_pool::find_value_in_pool(loopback_pool, &env.pool, &IpAddr::V4(address))
                .await?
                .expect("reserved address remains in the pool")
                .state,
            ResourcePoolEntryState::Free
        ));
    }

    let retried = env
        .api
        .discover_machine(discovery_request_from(
            &hardware_info,
            Some(interface_id),
            remote_ip,
        ))
        .await?
        .into_inner();
    assert_eq!(retried.machine_id, Some(machine_id));
    let machine = db::machine::find_one(&env.pool, &machine_id, MachineSearchConfig::default())
        .await?
        .expect("retry creates the DPU");
    assert_eq!(
        machine.network_config.loopback_ip,
        Some(IpAddr::V4(new_address))
    );
    for (address, state) in [
        (old_address, ResourcePoolEntryState::Free),
        (
            new_address,
            ResourcePoolEntryState::Allocated {
                owner: machine_id.to_string(),
                owner_type: model::resource_pool::OwnerType::Machine.to_string(),
            },
        ),
    ] {
        assert_eq!(
            db::resource_pool::find_value_in_pool(loopback_pool, &env.pool, &IpAddr::V4(address))
                .await?
                .expect("reserved address remains in the pool")
                .state,
            state,
            "retry must allocate only current intent: {address}"
        );
    }
    Ok(())
}

/// Direct discovery uses the caller's MAC and leaves another DPU's reserved
/// address untouched while allocating automatically for an undeclared MAC.
#[crate::sqlx_test]
async fn test_direct_discovery_does_not_use_another_mac_reservation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::net::Ipv4Addr;

    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;

    // A reservation for a different DPU OS/admin MAC.
    let reserved_v4 = Ipv4Addr::new(172, 30, 0, 20);
    let mut txn = env.pool.begin().await?;
    db::resource_pool::populate(
        env.api.common_pools.ethernet.pool_loopback_ip.as_ref(),
        &mut txn,
        vec![IpAddr::V4(reserved_v4)],
        false,
    )
    .await?;
    txn.commit().await?;
    seed_reservation_host(
        &env,
        "aa:bb:cc:dd:ee:20".parse()?,
        model::expected_machine::DpuLoopbackReservation {
            dpu_oob_net0_mac_address: "02:00:00:00:ff:ff".parse()?,
            loopback_ipv4: Some(reserved_v4),
            loopback_ipv6: None,
        },
    )
    .await;

    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    let hardware_info = HardwareInfo::from(dpu);

    let dpu_machine = discover_dpu_by_source_ip(&env, dpu, hardware_info).await;
    let loopback = dpu_machine
        .network_config
        .loopback_ip
        .expect("DPU should still receive an automatic loopback");
    assert_ne!(
        loopback,
        IpAddr::V4(reserved_v4),
        "a different OS/admin MAC must not select another DPU's reservation"
    );
    // The automatic address comes from the default auto-assign pool prefix
    // (172.20.0.0/24), not the reservable range the reservation targeted.
    let IpAddr::V4(v4) = loopback else {
        panic!("the IPv4 loopback pool must hand out an IPv4 address");
    };
    let [a, b, c, _] = v4.octets();
    assert_eq!(
        [a, b, c],
        [172, 20, 0],
        "the automatic loopback must come from the default auto-assign pool"
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_discover_dpu_not_create_machine(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();

    let dhcp_response = env
        .api
        .discover_dhcp(Request::new(rpc::forge::DhcpDiscovery {
            mac_address: dpu.oob_mac_address.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            vendor_string: None,
            link_address: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let mut req = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(HardwareInfo::from(dpu)).unwrap(),
        )),
        create_machine: false,
        ..Default::default()
    });

    let dhcp_address: IpAddr = dhcp_response.address.parse().unwrap();
    req.extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((dhcp_address, 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(req).await;

    assert_eq!(response.unwrap_err().code(), Code::PermissionDenied);

    Ok(())
}

/// The mode of the advisory lock `pid` is waiting for.
async fn pending_advisory_lock_mode(pool: &sqlx::PgPool, pid: i32) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT mode FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND NOT granted",
    )
    .bind(pid)
    .fetch_one(pool)
    .await
}

/// Whether the transaction on `pid` has neither written nor locked a row:
/// Postgres assigns its transaction id on the first of either.
async fn has_written_nothing(pool: &sqlx::PgPool, pid: i32) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT backend_xid IS NULL FROM pg_stat_activity WHERE pid = $1")
        .bind(pid)
        .fetch_one(pool)
        .await
}

/// The OOB interface of `dpu`, which the DPU agent registers from.
async fn dpu_oob_interface(
    env: &common::api_fixtures::TestEnv,
    dpu: &model::test_support::DpuConfig,
    dpu_machine_id: MachineId,
) -> Result<model::machine::MachineInterfaceSnapshot, Box<dyn std::error::Error>> {
    Ok(
        db::machine_interface::find_by_mac_address(&env.pool, dpu.oob_mac_address)
            .await?
            .into_iter()
            .find(|interface| interface.machine_id == Some(dpu_machine_id))
            .expect("the DPU OOB interface should belong to the DPU machine"),
    )
}

/// The host link `dpu` backs: the admin interface row keyed on its factory
/// MAC, attached to `dpu_machine_id`.
async fn dpu_host_link(
    env: &common::api_fixtures::TestEnv,
    dpu: &model::test_support::DpuConfig,
    dpu_machine_id: DpuMachineId,
) -> Result<model::machine::MachineInterfaceSnapshot, Box<dyn std::error::Error>> {
    Ok(
        db::machine_interface::find_by_mac_address(&env.pool, dpu.host_mac_address)
            .await?
            .into_iter()
            .find(|interface| interface.attached_dpu_machine_id == Some(dpu_machine_id))
            .expect("the DPU should back a host link"),
    )
}

/// The DPU machine `dpu_machine_id` as stored.
async fn dpu_machine(
    pool: &sqlx::PgPool,
    dpu_machine_id: &MachineId,
) -> Result<model::machine::AnyMachine, Box<dyn std::error::Error>> {
    Ok(db::machine::find_one(
        pool,
        dpu_machine_id,
        MachineSearchConfig {
            include_dpus: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?
    .expect("the DPU machine should exist"))
}

/// Clears the loopback address of `dpu_machine_id`, so its next discovery
/// allocates one and writes the machines rows of its group.
async fn clear_dpu_loopback_ip(
    pool: &sqlx::PgPool,
    dpu_machine_id: &MachineId,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE machines
         SET network_config = jsonb_set(network_config, '{loopback_ip}', 'null'::jsonb)
         WHERE id = $1",
    )
    .bind(dpu_machine_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether another transaction holds the machines row `machine_id` the way an
/// `UPDATE` of its network config would. `FOR NO KEY UPDATE` leaves a foreign
/// key check's `FOR KEY SHARE` lock out of the answer.
async fn machines_row_is_held(
    pool: &sqlx::PgPool,
    machine_id: &MachineId,
) -> Result<bool, sqlx::Error> {
    let mut probe = pool.begin().await?;
    let result = sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE NOWAIT")
        .bind(machine_id)
        .fetch_one(probe.as_mut())
        .await;
    probe.rollback().await?;
    match result {
        Ok(_) => Ok(false),
        Err(error)
            if error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some("55P03") =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

/// The primary and the dormant DPU-backed admin link of a two-DPU host.
async fn primary_and_dormant_links(
    env: &common::api_fixtures::TestEnv,
    host_config: &ManagedHostConfig,
) -> Result<
    (
        model::machine::MachineInterfaceSnapshot,
        model::machine::MachineInterfaceSnapshot,
    ),
    Box<dyn std::error::Error>,
> {
    let mut links = Vec::new();
    for dpu in &host_config.dpus {
        links.push(
            db::machine_interface::find_by_mac_address(&env.pool, dpu.host_mac_address)
                .await?
                .into_iter()
                .find(|link| link.attached_dpu_machine_id.is_some())
                .expect("each DPU should back a host link"),
        );
    }
    let Ok([first, second]) = <[_; 2]>::try_from(links) else {
        panic!("a two-DPU host has two host links")
    };
    assert_ne!(first.primary_interface, second.primary_interface);
    Ok(if first.primary_interface {
        (first, second)
    } else {
        (second, first)
    })
}

/// Moves every address row of `from` to `to`: the state a host is left in
/// when its admin address was leased through a link that is now dormant.
async fn move_address_rows(
    pool: &sqlx::PgPool,
    from: MachineInterfaceId,
    to: MachineInterfaceId,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE machine_interface_addresses SET interface_id = $1 WHERE interface_id = $2")
        .bind(to)
        .bind(from)
        .execute(pool)
        .await?;
    Ok(())
}

/// The network config version of `machine_id` as stored.
async fn network_config_version(
    pool: &sqlx::PgPool,
    machine_id: &MachineId,
) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(db::machine::get_network_config(pool, machine_id)
        .await?
        .version
        .version_nr())
}

/// Asserts the admin address reconcile ran for the host under
/// `stable_host_id`: the dormant link owns no address and no domain, the
/// primary owns `admin_ip` again, and the host's network config version
/// advanced once.
async fn assert_admin_addresses_repaired_under(
    env: &common::api_fixtures::TestEnv,
    stable_host_id: MachineId,
    primary_link_id: MachineInterfaceId,
    dormant_link_id: MachineInterfaceId,
    admin_ip: IpAddr,
    version_before: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let primary_link = db::machine_interface::find_one(&env.pool, primary_link_id).await?;
    let dormant_link = db::machine_interface::find_one(&env.pool, dormant_link_id).await?;
    assert_eq!(dormant_link.machine_id, Some(stable_host_id));
    assert!(dormant_link.addresses.is_empty(), "{dormant_link:?}");
    assert_eq!(dormant_link.domain_id, None);
    assert_eq!(primary_link.machine_id, Some(stable_host_id));
    assert_eq!(primary_link.addresses, vec![admin_ip]);
    assert_eq!(
        network_config_version(&env.pool, &stable_host_id).await?,
        version_before + 1
    );
    Ok(())
}

/// Re-registering a DPU of an ingested host must not wait on the admin
/// segment locks: the discovery transaction no longer takes them, and the
/// admin address reconcile that follows its commit finds nothing to repair.
#[crate::sqlx_test]
async fn test_dpu_rediscovery_does_not_wait_on_admin_segment_locks(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    let dpu = host_config.get_and_assert_single_dpu();
    let dpu_machine_id = MachineId::from(mh.dpu_ids[0]);
    let dpu_interface = dpu_oob_interface(&env, dpu, dpu_machine_id).await?;

    // Nothing is pending on the host, so the reconcile after the rediscovery
    // has no reason to wait on the gate either.
    let mut txn = env.pool.begin().await?;
    assert!(!db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?);
    txn.commit().await?;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(&mut gate).await?;

    let rediscovered_machine_id = tokio::time::timeout(
        Duration::from_secs(30),
        dpu_discover_machine(&env, dpu, dpu_interface.id),
    )
    .await
    .expect("DPU rediscovery must not wait on the admin segment locks");
    assert_eq!(rediscovered_machine_id, dpu_machine_id);
    gate.rollback().await?;

    // The lock-free discovery still reattached the DPU's host link.
    let host_link = dpu_host_link(&env, dpu, mh.dpu_ids[0]).await?;
    assert_eq!(host_link.machine_id, Some(MachineId::from(mh.id)));

    Ok(())
}

/// A repeat discovery of a host that already carries its stable machine id
/// renames nothing, so it takes no admin segment lock and completes while an
/// exclusive holder has them.
#[crate::sqlx_test]
async fn test_repeat_host_discovery_does_not_wait_on_admin_segment_locks(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    let mut txn = env.pool.begin().await?;
    let host_interface = db::machine_interface::find_by_machine_ids(&mut txn, &[mh.id])
        .await?
        .remove(&mh.id)
        .unwrap_or_default()
        .into_iter()
        .find(|interface| interface.primary_interface)
        .expect("the ingested host should have a primary interface");
    txn.commit().await?;

    // Nothing is pending on the host, so the reconcile after the rediscovery
    // has no reason to wait on the gate either.
    let mut txn = env.pool.begin().await?;
    assert!(!db::machine_interface::reconcile_admin_addresses_for_host(&mut txn, &mh.id).await?);
    txn.commit().await?;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(&mut gate).await?;

    let rediscovered_machine_id = tokio::time::timeout(
        Duration::from_secs(30),
        host_discover_machine(&env, &host_config, host_interface.id),
    )
    .await
    .expect("a repeat host discovery must not wait on the admin segment locks");
    assert_eq!(
        MachineId::from(rediscovered_machine_id),
        MachineId::from(mh.id)
    );
    gate.rollback().await?;

    // The lock-free discovery left the primary interface on the stable host.
    let host_interface = db::machine_interface::find_one(&env.pool, host_interface.id).await?;
    assert_eq!(host_interface.machine_id, Some(MachineId::from(mh.id)));

    Ok(())
}

/// A host's first discovery requests the admin segments shared, and before
/// its row pre-lock, so an exclusive holder parks it with nothing locked or
/// written.
#[crate::sqlx_test]
async fn test_first_host_discovery_requests_the_admin_segments_shared_before_its_row_locks(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;
    let expected_machine_id = from_hardware_info(&HardwareInfo::from(&host_config))?;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(&mut gate).await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await?;

    let discovery = host_discover_machine(&env, &host_config, host_interface_id);
    tokio::pin!(discovery);
    let discovery_pid = tokio::select! {
        result = &mut discovery => {
            panic!("a host's first discovery bypassed the admin segment locks: {result:?}")
        }
        blocked = wait_for_blocked_query(
            &env.pool,
            gate_pid,
            "pg_advisory_xact_lock_shared(",
        ) => blocked,
    };
    assert_eq!(
        pending_advisory_lock_mode(&env.pool, discovery_pid).await?,
        "ShareLock"
    );
    assert!(
        has_written_nothing(&env.pool, discovery_pid).await?,
        "the segment lock request must precede the row pre-lock"
    );
    gate.rollback().await?;

    assert_eq!(MachineId::from(discovery.await), expected_machine_id);
    let host_interface = db::machine_interface::find_one(&env.pool, host_interface_id).await?;
    assert_eq!(host_interface.machine_id, Some(expected_machine_id));

    Ok(())
}

/// A host's first discovery requests the host's interface rows, ascending,
/// before it locks or writes anything else, holding the admin segments
/// shared: an allocation-mode request for them is admitted beside it and an
/// exclusive one waits.
#[crate::sqlx_test]
async fn test_first_host_discovery_locks_host_rows_first_under_shared_segment_locks(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;
    let expected_machine_id = from_hardware_info(&HardwareInfo::from(&host_config))?;
    let host_interface = db::machine_interface::find_one(&env.pool, host_interface_id).await?;
    let predicted_host_id = host_interface
        .machine_id
        .expect("the DHCP-discovered host interface should belong to the predicted host");
    let admin_segment_count = env.admin_segments.len();

    // A second row of the predicted host whose id sorts below the caller
    // interface's, so a holder of it parks the discovery on its first row lock.
    let lowest_interface_id: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
             (id, machine_id, association_type, segment_id, mac_address,
              primary_interface, hostname)
         VALUES ('00000000-0000-0000-0000-000000000001', $1, 'Machine', $2, $3::macaddr,
                 false, 'lowest-host-row')
         RETURNING id",
    )
    .bind(predicted_host_id)
    .bind(host_interface.segment_id)
    .bind("02:00:00:00:00:01")
    .fetch_one(&env.pool)
    .await?;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
        .bind(lowest_interface_id)
        .fetch_one(&mut *blocker)
        .await?;

    let discovery = host_discover_machine(&env, &host_config, host_interface_id);
    tokio::pin!(discovery);
    let discovery_pid = tokio::select! {
        result = &mut discovery => {
            panic!("the discovery finished while a row of the host was held: {result:?}")
        }
        blocked = wait_for_blocked_query(
            &env.pool,
            blocker_pid,
            "WHERE machine_id = $1 ORDER BY id FOR UPDATE",
        ) => blocked,
    };
    assert!(
        has_written_nothing(&env.pool, discovery_pid).await?,
        "the row pre-lock must precede every other row lock and write"
    );
    let advisory_locks: Vec<String> = sqlx::query_scalar(
        "SELECT mode FROM pg_locks
         WHERE pid = $1 AND locktype = 'advisory' AND granted
         ORDER BY mode",
    )
    .bind(discovery_pid)
    .fetch_all(&env.pool)
    .await?;
    assert_eq!(
        advisory_locks,
        vec!["ShareLock".to_string(); admin_segment_count],
        "a host's first discovery must hold every admin segment shared"
    );

    let mut probe = env.pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *probe)
        .await?;
    db::machine_interface::lock_all_admin_segments_for_allocation(&mut probe).await?;
    probe.rollback().await?;

    let mut probe = env.pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *probe)
        .await?;
    let error = db::machine_interface::lock_all_admin_segments(&mut probe)
        .await
        .expect_err("an exclusive request must wait for the discovery's shared locks");
    let db::DatabaseError::Sqlx(error) = error else {
        panic!("expected wrapped SQLx error, got {error:?}");
    };
    assert_eq!(
        error
            .source
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("55P03"),
        "{error}"
    );
    probe.rollback().await?;

    blocker.rollback().await?;
    assert_eq!(MachineId::from(discovery.await), expected_machine_id);
    for interface_id in [host_interface_id, lowest_interface_id] {
        let interface = db::machine_interface::find_one(&env.pool, interface_id).await?;
        assert_eq!(interface.machine_id, Some(expected_machine_id));
    }

    Ok(())
}

/// Where Site Explorer does not create machines, a DPU's first discovery may
/// create the host machine row its sibling DPUs share, so it takes the admin
/// segment locks exclusively, and before its first row write.
#[crate::sqlx_test]
async fn test_legacy_first_dpu_discovery_waits_on_admin_segment_locks_before_writing(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_legacy_first_dpu_discovery_waits_on_admin_segment_locks(pool, false).await
}

/// A host NIC that leased before its DPU registered leaves a link row with no
/// machine. The DPU's first discovery still creates the host machine, so the
/// unattached link does not let it skip the exclusive lock.
#[crate::sqlx_test]
async fn test_legacy_first_dpu_discovery_keeps_the_exclusive_lock_with_an_unattached_host_link(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_legacy_first_dpu_discovery_waits_on_admin_segment_locks(pool, true).await
}

async fn check_legacy_first_dpu_discovery_waits_on_admin_segment_locks(
    pool: sqlx::PgPool,
    host_nic_leases_first: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    assert!(
        !env.config
            .site_explorer
            .create_machines
            .load(Ordering::Relaxed),
        "this test needs the DPU agent to create machines"
    );
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    if host_nic_leases_first {
        // The host NIC leases on the admin segment like any unknown MAC.
        dpu_discover_dhcp(&env, &dpu.host_mac_address.to_string()).await;
        let host_links =
            db::machine_interface::find_by_mac_address(&env.pool, dpu.host_mac_address).await?;
        assert_eq!(host_links.len(), 1);
        assert!(host_links[0].machine_id.is_none());
    }
    let dpu_interface_id = dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let expected_dpu_machine_id = from_hardware_info(&HardwareInfo::from(dpu))?;

    let mut gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(&mut gate).await?;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *gate)
        .await?;

    let discovery = dpu_discover_machine(&env, dpu, dpu_interface_id);
    tokio::pin!(discovery);
    let discovery_pid = tokio::select! {
        result = &mut discovery => {
            panic!("a first DPU discovery bypassed the admin segment locks: {result:?}")
        }
        blocked = wait_for_blocked_query(&env.pool, gate_pid, "pg_advisory_xact_lock(") => blocked,
    };
    assert_eq!(
        pending_advisory_lock_mode(&env.pool, discovery_pid).await?,
        "ExclusiveLock"
    );
    assert!(
        has_written_nothing(&env.pool, discovery_pid).await?,
        "the lock request must precede the first row write"
    );
    gate.rollback().await?;

    assert_eq!(discovery.await, expected_dpu_machine_id);
    let host_links =
        db::machine_interface::find_by_mac_address(&env.pool, dpu.host_mac_address).await?;
    assert_eq!(host_links.len(), 1);
    assert!(host_links[0].machine_id.is_some());

    Ok(())
}

/// While a DPU rediscovery holds its row locks mid-transaction it holds no
/// advisory lock, so a creation-mode request for the admin segments is
/// admitted beside it instead of queueing behind the fleet.
#[crate::sqlx_test]
async fn test_creation_lock_request_is_admitted_while_dpu_rediscovery_is_in_flight(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    let dpu = host_config.get_and_assert_single_dpu();
    let dpu_machine_id = MachineId::from(mh.dpu_ids[0]);
    let dpu_interface = dpu_oob_interface(&env, dpu, dpu_machine_id).await?;

    // Park the rediscovery on its first row write, the caller interface row.
    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
        .bind(dpu_interface.id)
        .fetch_one(&mut *blocker)
        .await?;

    let rediscovery = dpu_discover_machine(&env, dpu, dpu_interface.id);
    tokio::pin!(rediscovery);
    let rediscovery_pid = tokio::select! {
        result = &mut rediscovery => {
            panic!("the rediscovery finished while its caller interface row was held: {result:?}")
        }
        blocked = wait_for_blocked_query(
            &env.pool,
            blocker_pid,
            "UPDATE machine_interfaces SET machine_id",
        ) => blocked,
    };
    let advisory_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE pid = $1 AND locktype = 'advisory'",
    )
    .bind(rediscovery_pid)
    .fetch_one(&env.pool)
    .await?;
    assert_eq!(
        advisory_locks, 0,
        "a DPU rediscovery must hold no segment lock"
    );

    let mut probe = env.pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *probe)
        .await?;
    db::machine_interface::lock_all_admin_segments_for_allocation(&mut probe).await?;
    probe.rollback().await?;

    blocker.rollback().await?;
    assert_eq!(rediscovery.await, dpu_machine_id);

    Ok(())
}

/// A DPU rediscovery that fills a loopback address updates the host link row
/// before it writes any machines row, the order the admin address repair and
/// the host rename lock in, so a holder of the link row parks it with every
/// machines row of the group still free.
#[crate::sqlx_test]
async fn test_dpu_rediscovery_locks_the_host_link_row_before_the_machines_rows(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    let dpu = host_config.get_and_assert_single_dpu();
    let dpu_machine_id = MachineId::from(mh.dpu_ids[0]);
    let host_machine_id = MachineId::from(mh.id);
    let dpu_interface = dpu_oob_interface(&env, dpu, dpu_machine_id).await?;
    let host_link = dpu_host_link(&env, dpu, mh.dpu_ids[0]).await?;
    let original_loopback = dpu_machine(&env.pool, &dpu_machine_id)
        .await?
        .network_config
        .loopback_ip
        .expect("the ingested DPU has an Integrated loopback");
    let used_before =
        db::resource_pool::stats(&env.pool, model::resource_pool::common::LOOPBACK_IP)
            .await?
            .used;
    // Remove only the published address. The reservation remains owned by
    // this DPU, so rediscovery must repair the field without consuming another.
    clear_dpu_loopback_ip(&env.pool, &dpu_machine_id).await?;
    let version_before = dpu_machine(&env.pool, &dpu_machine_id)
        .await?
        .network_config
        .version;

    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("SELECT id FROM machine_interfaces WHERE id = $1 FOR UPDATE")
        .bind(host_link.id)
        .fetch_one(&mut *blocker)
        .await?;

    let rediscovery = dpu_discover_machine(&env, dpu, dpu_interface.id);
    tokio::pin!(rediscovery);
    tokio::select! {
        result = &mut rediscovery => {
            panic!("the rediscovery finished while its host link row was held: {result:?}")
        }
        _ = wait_for_blocked_query(
            &env.pool,
            blocker_pid,
            "UPDATE machine_interfaces SET attached_dpu_machine_id",
        ) => {}
    }
    for machine_id in [host_machine_id, dpu_machine_id] {
        assert!(
            !machines_row_is_held(&env.pool, &machine_id).await?,
            "the machines row {machine_id} must stay free until the host link row is held"
        );
    }

    // Release the interface gate, then reload configuration and capacity to
    // distinguish reuse from a second successful allocation.
    blocker.rollback().await?;
    assert_eq!(rediscovery.await, dpu_machine_id);
    let dpu_machine = dpu_machine(&env.pool, &dpu_machine_id).await?;
    assert_eq!(
        dpu_machine.network_config.loopback_ip,
        Some(original_loopback)
    );
    assert_eq!(
        db::resource_pool::stats(&env.pool, model::resource_pool::common::LOOPBACK_IP)
            .await?
            .used,
        used_before,
        "rediscovery must reuse the DPU's existing Integrated reservation",
    );
    assert_eq!(
        dpu_machine.network_config.version.version_nr(),
        version_before.version_nr() + 1
    );

    Ok(())
}

/// A busy host makes loopback attachment roll back instead of waiting with a
/// DPU lock. Admin repair and a later rediscovery must still finish together.
#[crate::sqlx_test]
async fn test_dpu_rediscovery_retries_after_busy_group_without_blocking_admin_repair(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = create_managed_host_with_config(&env, host_config.clone()).await;
    let dpu = host_config.get_and_assert_single_dpu();
    let dpu_machine_id = MachineId::from(mh.dpu_ids[0]);
    let host_machine_id = MachineId::from(mh.id);
    let dpu_interface = dpu_oob_interface(&env, dpu, dpu_machine_id).await?;
    let host_link = dpu_host_link(&env, dpu, mh.dpu_ids[0]).await?;
    assert!(host_link.primary_interface);

    // Both callers need a write, but the DPU still owns its original loopback.
    // Rejected attachment must return that value and leave the field empty.
    sqlx::query("DELETE FROM machine_interface_addresses WHERE interface_id = $1")
        .bind(host_link.id)
        .execute(&env.pool)
        .await?;
    clear_dpu_loopback_ip(&env.pool, &dpu_machine_id).await?;
    let used_before =
        db::resource_pool::stats(&env.pool, model::resource_pool::common::LOOPBACK_IP)
            .await?
            .used;
    let dpu_before = dpu_machine(&env.pool, &dpu_machine_id).await?;
    let host_before = db::machine::get_network_config(&env.pool, &host_machine_id).await?;
    let mut setup = env.pool.begin().await?;
    db::machine_topology::set_topology_update_needed(setup.as_mut(), &dpu_machine_id, true).await?;
    setup.commit().await?;
    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(host_machine_id)
        .fetch_one(&mut *blocker)
        .await?;

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        env.api.discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(dpu_interface.id),
            discovery_data: Some(DiscoveryData::Info(DiscoveryInfo::try_from(
                HardwareInfo::from(dpu),
            )?)),
            create_machine: true,
            ..Default::default()
        })),
    )
    .await?
    .expect_err("a busy group rejects this attachment");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("changed or is busy"));
    let dpu_after = dpu_machine(&env.pool, &dpu_machine_id).await?;
    let host_after = db::machine::get_network_config(&env.pool, &host_machine_id).await?;
    assert_eq!(
        dpu_after.network_config.value,
        dpu_before.network_config.value
    );
    assert_eq!(
        dpu_after.network_config.version,
        dpu_before.network_config.version
    );
    assert_eq!(host_after.value, host_before.value);
    assert_eq!(host_after.version, host_before.version);
    let topology_pending: bool = sqlx::query_scalar(
        "SELECT topology_update_needed FROM machine_topologies WHERE machine_id = $1",
    )
    .bind(dpu_machine_id)
    .fetch_one(&env.pool)
    .await?;
    assert!(
        topology_pending,
        "failed discovery must roll back its topology write"
    );
    assert_eq!(
        db::resource_pool::stats(&env.pool, model::resource_pool::common::LOOPBACK_IP,)
            .await?
            .used,
        used_before
    );
    let mut probe = env.pool.begin().await?;
    assert!(
        db::machine::try_lock_by_id_for_update(&mut probe, &dpu_machine_id).await?,
        "failed attachment must release its partial DPU lock"
    );
    probe.rollback().await?;

    // Admin repair can now hold the interface and queue on the host; the
    // failed discovery must not retain a DPU lock that would block its fanout.
    let repair =
        db::machine_interface::reconcile_admin_addresses_after_commit(&env.pool, &host_machine_id);
    tokio::pin!(repair);
    tokio::select! {
        result = &mut repair => panic!("repair bypassed the held host: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, blocker_pid, "UPDATE machines") => {},
    }
    let metrics = MetricsCapture::start();
    blocker.rollback().await?;
    let (rediscovered_machine_id, repaired) =
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                dpu_discover_machine(&env, dpu, dpu_interface.id),
                &mut repair
            )
        })
        .await?;
    assert_eq!(rediscovered_machine_id, dpu_machine_id);
    repaired?;
    assert_eq!(
        metrics.counter_delta(
            "carbide_api_host_admin_address_reconcile_failures_total",
            &[]
        ),
        0.0
    );
    let host_link = db::machine_interface::find_one(&env.pool, host_link.id).await?;
    assert_eq!(host_link.addresses.len(), 1);
    assert!(
        dpu_machine(&env.pool, &dpu_machine_id)
            .await?
            .network_config
            .loopback_ip
            .is_some()
    );
    let topology_pending: bool = sqlx::query_scalar(
        "SELECT topology_update_needed FROM machine_topologies WHERE machine_id = $1",
    )
    .bind(dpu_machine_id)
    .fetch_one(&env.pool)
    .await?;
    assert!(
        !topology_pending,
        "successful discovery must clear its topology flag"
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_dpu_discovery_rolls_back_when_a_new_group_member_is_busy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::future::maybe_done;
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::resource_pool::OwnerType;
    use model::resource_pool::common::LOOPBACK_IP_V6;

    struct RejectedAttempt {
        prefix: serde_json::Value,
        pool: model::resource_pool::ResourcePoolStats,
        reservation_count: i64,
        initial_group_held: bool,
    }

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let host = create_managed_host_with_config(&env, host_config.clone()).await;
    let target = host_config.get_and_assert_single_dpu();
    let target_id = MachineId::from(host.dpu_ids[0]);
    let host_id = MachineId::from(host.id);
    let target_interface = dpu_oob_interface(&env, target, target_id).await?;
    let target_link = dpu_host_link(&env, target, host.dpu_ids[0]).await?;
    assert!(target_link.primary_interface);

    // The target DPU (A) has no IPv6 reservation yet. The new member (B)
    // starts outside this host's group, with complete loopbacks and its own
    // associated caller interface. Its factory link L already belongs to
    // the host and is distinct from A's existing link.
    let mut member = target.clone();
    member.serial = "discovery-new-group-member".to_string();
    member.oob_mac_address = "02:53:33:01:00:01".parse()?;
    member.host_mac_address = "02:53:33:01:00:02".parse()?;
    let member_id = from_hardware_info(&HardwareInfo::from(&member))?;
    assert_ne!(member_id, target_id);
    let member_dpu_id = DpuMachineId::try_from(member_id)?;
    let member_interface_id = dpu_discover_dhcp(&env, &member.oob_mac_address.to_string()).await;
    let new_link_id = dpu_discover_dhcp(&env, &member.host_mac_address.to_string()).await;
    let mut setup = env.pool.begin().await?;
    db::resource_pool::define(
        setup.as_mut(),
        LOOPBACK_IP_V6,
        &ResourcePoolDef {
            pool_type: ResourcePoolType::Ipv6,
            prefix: Some("2001:db8:5333::/125".to_string()),
            ranges: vec![],
            delegate_prefix_len: None,
        },
    )
    .await?;
    db::machine::create(
        setup.as_mut(),
        Some(&env.common_pools),
        &member_id,
        ManagedHostState::Ready,
        None,
        2,
    )
    .await?;
    let mut member_config = db::machine::get_network_config(setup.as_mut(), &member_id).await?;
    member_config.value.loopback_ip = Some(
        db::resource_pool::allocate(
            &env.common_pools.ethernet.pool_loopback_ip,
            setup.as_mut(),
            OwnerType::Machine,
            &member_id.to_string(),
            None,
        )
        .await?,
    );
    assert_eq!(
        db::machine::try_update_network_config(
            setup.as_mut(),
            &member_id,
            member_config.version,
            &member_config.value,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    db::machine_interface::set_primary_interface(&new_link_id, false, setup.as_mut()).await?;
    for (interface_id, machine_id) in [(member_interface_id, member_id), (new_link_id, host_id)] {
        db::machine_interface::associate_interface_with_machine(
            &interface_id,
            MachineInterfaceAssociation::Machine(machine_id),
            setup.as_mut(),
        )
        .await?;
    }
    db::machine_interface::associate_interface_with_dpu_machine(
        &member_interface_id,
        &member_id,
        setup.as_mut(),
    )
    .await?;
    db::machine_topology::set_topology_update_needed(setup.as_mut(), &target_id, true).await?;
    setup.commit().await?;
    let new_link = db::machine_interface::find_one(&env.pool, new_link_id).await?;
    assert_eq!(new_link.segment_id, target_link.segment_id);
    assert_eq!(new_link.machine_id, Some(host_id));
    assert!(new_link.attached_dpu_machine_id.is_none());
    assert!(!new_link.addresses.is_empty());
    let target_before = db::machine::get_network_config(&env.pool, &target_id).await?;
    let member_before = db::machine::get_network_config(&env.pool, &member_id).await?;
    assert!(target_before.loopback_ip.is_some() && target_before.loopback_ip_v6.is_none());
    assert!(member_before.loopback_ip.is_some() && member_before.loopback_ip_v6.is_some());
    let machine_ids = [host_id, target_id, member_id];
    let history_ids = machine_ids.map(|id| id.to_string());
    let interface_ids = [target_interface.id, target_link.id, member_interface_id];
    let prefix_snapshot = || async {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT jsonb_build_object(
                'machines', (SELECT jsonb_agg(to_jsonb(m) ORDER BY m.id)
                    FROM machines m WHERE id = ANY($1)),
                'interfaces', (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id)
                    FROM machine_interfaces i WHERE id = ANY($2)),
                'addresses', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id)
                    FROM machine_interface_addresses a WHERE interface_id = ANY($2)),
                'topology', (SELECT to_jsonb(t) FROM machine_topologies t WHERE machine_id = $3),
                'history', (SELECT jsonb_agg(to_jsonb(h) ORDER BY h.id)
                    FROM machine_state_history h WHERE object_id = ANY($4)),
                'pool', (SELECT jsonb_agg(to_jsonb(p) ORDER BY p.id)
                    FROM resource_pool p WHERE name = $5),
                'reservations', (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.allocation_id)
                    FROM resource_pool_allocations r WHERE pool_name = $5))",
        )
        .bind(machine_ids.as_slice())
        .bind(interface_ids.as_slice())
        .bind(target_id)
        .bind(history_ids.as_slice())
        .bind(LOOPBACK_IP_V6)
        .fetch_one(&env.pool)
        .await
    };
    let prefix_before = prefix_snapshot().await?;
    let pool_before = db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6).await?;

    // A passes group admission before its fresh allocation INSERT waits here.
    // The separate segment gate stops B's post-commit address repair before
    // it can acquire interface or Machine locks needed by rollback probes.
    let mut allocation_gate = env.pool.begin().await?;
    sqlx::query("LOCK TABLE resource_pool_allocations IN SHARE MODE")
        .execute(allocation_gate.as_mut())
        .await?;
    let allocation_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(allocation_gate.as_mut())
        .await?;
    let mut allocation_gate = Some(allocation_gate);
    let mut repair_gate = env.pool.begin().await?;
    db::machine_interface::lock_all_admin_segments(repair_gate.as_mut()).await?;
    let repair_gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(repair_gate.as_mut())
        .await?;
    let mut member_gate = env.pool.begin().await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
        .bind(member_id)
        .fetch_one(member_gate.as_mut())
        .await?;
    let target_request = || {
        Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(target_interface.id),
            discovery_data: Some(DiscoveryData::Info(
                DiscoveryInfo::try_from(HardwareInfo::from(target)).unwrap(),
            )),
            create_machine: false,
            ..Default::default()
        })
    };
    let member_request = Request::new(MachineDiscoveryInfo {
        machine_interface_id: Some(member_interface_id),
        discovery_data: Some(DiscoveryData::Info(
            DiscoveryInfo::try_from(HardwareInfo::from(&member)).unwrap(),
        )),
        create_machine: false,
        ..Default::default()
    });
    let discovery = maybe_done(carbide_instrument::testing::capture_logs_async(
        env.api.discover_machine(target_request()),
    ));
    let membership_change = maybe_done(env.api.discover_machine(member_request));
    tokio::pin!(discovery, membership_change);
    let mut discovery_pid = None;
    let mut membership_pid = None;
    let proof = async {
        discovery_pid = Some(
            tokio::time::timeout(Duration::from_secs(20), async {
                tokio::select! {
                    () = &mut discovery => Err("A finished before its allocation gate"),
                    pid = wait_for_blocked_query(&env.pool, allocation_gate_pid,
                        "INSERT INTO resource_pool_allocations") => Ok(pid),
                }
            })
            .await??,
        );
        let mut probe = env.pool.begin().await?;
        let host_held =
            !db::machine::try_lock_by_id_for_no_key_update(probe.as_mut(), &host_id).await?;
        let target_held =
            !db::machine::try_lock_by_id_for_no_key_update(probe.as_mut(), &target_id).await?;
        probe.rollback().await?;
        membership_pid = Some(
            tokio::time::timeout(Duration::from_secs(20), async {
                tokio::select! {
                    () = &mut membership_change => Err("B finished before its post-commit repair gate"),
                    pid = wait_for_blocked_query(&env.pool, repair_gate_pid,
                        "pg_advisory_xact_lock_shared(") => Ok(pid),
                }
            })
            .await??,
        );
        let committed_link = db::machine_interface::find_one(&env.pool, new_link_id).await?;
        if committed_link.machine_id != Some(host_id)
            || committed_link.attached_dpu_machine_id != Some(member_dpu_id)
        {
            return Err("B did not commit its real Discovery association before A resumed".into());
        }
        // B changes only L's DPU association. Its unchanged host FK and B's
        // complete loopbacks let it commit beside A's existing group locks.
        allocation_gate
            .take()
            .expect("the allocator gate is held")
            .rollback()
            .await?;
        tokio::time::timeout(Duration::from_secs(10), &mut discovery).await?;
        let prefix = prefix_snapshot().await?;
        let pool = db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6).await?;
        let reservation_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource_pool_allocations WHERE pool_name = $1 AND owner_id = $2",
        )
        .bind(LOOPBACK_IP_V6)
        .bind(target_id.to_string())
        .fetch_one(&env.pool)
        .await?;
        let mut probe = env.pool.begin().await?;
        let released_locks = async {
            sqlx::query("SELECT id FROM machines WHERE id = ANY($1) FOR NO KEY UPDATE NOWAIT")
                .bind([host_id, target_id].as_slice())
                .execute(probe.as_mut())
                .await?;
            sqlx::query("SELECT id FROM machine_interfaces WHERE id = ANY($1) FOR UPDATE NOWAIT")
                .bind(interface_ids.as_slice())
                .execute(probe.as_mut())
                .await?;
            sqlx::query("SELECT id FROM resource_pool WHERE name = $1 FOR UPDATE NOWAIT")
                .bind(LOOPBACK_IP_V6)
                .execute(probe.as_mut())
                .await?;
            Ok::<(), sqlx::Error>(())
        }
        .await;
        let probe_end = probe.rollback().await;
        released_locks?;
        probe_end?;
        Ok::<_, Box<dyn std::error::Error>>(RejectedAttempt {
            prefix,
            pool,
            reservation_count,
            initial_group_held: host_held && target_held,
        })
    }
    .await;

    // Settle both real API calls even when readiness or a probe failed.
    let allocation_release = match allocation_gate.take() {
        Some(gate) => gate.rollback().await,
        None => Ok(()),
    };
    let member_release = member_gate.rollback().await;
    let repair_release = repair_gate.rollback().await;
    let completed = {
        let joined = async {
            tokio::join!(&mut discovery, &mut membership_change);
        };
        tokio::pin!(joined);
        let completed = tokio::time::timeout(Duration::from_secs(30), &mut joined)
            .await
            .is_ok();
        if !completed {
            for pid in [discovery_pid, membership_pid].into_iter().flatten() {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(pid)
                    .fetch_one(&env.pool)
                    .await?;
            }
            tokio::time::timeout(Duration::from_secs(30), &mut joined).await?;
        }
        completed
    };
    allocation_release?;
    member_release?;
    repair_release?;
    assert!(
        completed,
        "released discoveries must complete without cancellation"
    );
    let observation = proof?;
    let (result, logs) = discovery.as_mut().take_output().expect("A joined");
    let member_response = membership_change
        .as_mut()
        .take_output()
        .expect("B joined")?;
    assert_eq!(member_response.into_inner().machine_id, Some(member_id));
    let error = result.expect_err("the final group write must reject the newly attached busy B");
    let message = format!(
        "machine network group for {} is busy; retry discovery",
        host.dpu_ids[0]
    );
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(error.message(), message);
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-503"
    );
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        message.as_str()
    );
    assert_eq!(
        error.metadata().get("nico-error-mitigation").unwrap(),
        "Retry discovery with a fresh request after database contention clears."
    );
    let target_id_text = target_id.to_string();
    assert!(
        logs.iter().any(|log| {
            log.message == "discovery network group write is busy"
                && log.field("dpu_machine_id") == Some(target_id_text.as_str())
                && log.field("error").is_some_and(|error| {
                    error.contains("WITH locked_group")
                        && error.contains("NOWAIT")
                        && error.contains("machine.rs")
                        && error.contains("line=")
                })
        }),
        "the server log must retain the annotated final group-write failure"
    );
    assert!(observation.initial_group_held);
    assert_eq!(observation.prefix, prefix_before);
    assert_eq!(observation.pool, pool_before);
    assert_eq!(observation.reservation_count, 0);

    let configs_before_retry = [
        db::machine::get_network_config(&env.pool, &host_id).await?,
        db::machine::get_network_config(&env.pool, &target_id).await?,
        db::machine::get_network_config(&env.pool, &member_id).await?,
    ];
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        env.api.discover_machine(target_request()),
    )
    .await??
    .into_inner();
    assert_eq!(response.machine_id, Some(target_id));
    let host_after = db::machine::get_network_config(&env.pool, &host_id).await?;
    let target_after = db::machine::get_network_config(&env.pool, &target_id).await?;
    let member_after = db::machine::get_network_config(&env.pool, &member_id).await?;
    let mut expected_target = configs_before_retry[1].value.clone();
    expected_target.loopback_ip_v6 = Some(
        target_after
            .loopback_ip_v6
            .expect("fresh recovery reserves IPv6"),
    );
    assert_eq!(target_after.value, expected_target);
    assert_eq!(host_after.value, configs_before_retry[0].value);
    assert_eq!(member_after.value, configs_before_retry[2].value);
    assert_eq!(host_after.version, target_after.version);
    assert_eq!(member_after.version, target_after.version);
    assert_eq!(
        target_after.version.version_nr(),
        configs_before_retry[1].version.version_nr() + 1
    );
    assert_eq!(
        db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6)
            .await?
            .used,
        pool_before.used + 1
    );
    let topology_pending: bool = sqlx::query_scalar(
        "SELECT topology_update_needed FROM machine_topologies WHERE machine_id = $1",
    )
    .bind(target_id)
    .fetch_one(&env.pool)
    .await?;
    assert!(!topology_pending);
    let recovered_link = db::machine_interface::find_one(&env.pool, new_link_id).await?;
    assert_eq!(recovered_link.machine_id, Some(host_id));
    assert_eq!(recovered_link.attached_dpu_machine_id, Some(member_dpu_id));
    assert!(recovered_link.addresses.is_empty());
    Ok(())
}

#[crate::sqlx_test]
async fn test_dpu_discovery_releases_busy_predicted_host_dependencies(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::HashMap;

    use db::state_history::{StateHistoryTableId, find_by_object_ids};
    use futures_util::future::maybe_done;
    use model::machine::{DpuInitState, DpuInitStates, MachineLastRebootRequestedMode};
    use model::resource_pool::common::LOOPBACK_IP_V6;

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    common::api_fixtures::site_explorer::new_mock_host_awaiting_discovery(
        &env,
        host_config.clone(),
    )
    .await
    .expect("fixture retains its predicted host until first host discovery");
    let dpu = host_config.get_and_assert_single_dpu();
    let dpu_machine_id = from_hardware_info(&HardwareInfo::from(dpu))?;
    let dpu_id = DpuMachineId::try_from(dpu_machine_id)?;
    let dpu_interface = dpu_oob_interface(&env, dpu, dpu_machine_id).await?;
    let old_link = dpu_host_link(&env, dpu, dpu_id).await?;
    let host_machine_id = old_link
        .machine_id
        .expect("the original host link has an owner");
    assert!(host_machine_id.machine_type().is_predicted_host());
    assert!(old_link.primary_interface);
    let host_id = HostMachineId::try_from(host_machine_id)?;
    let machine_ids = [host_machine_id, dpu_machine_id];

    // These phases are intentionally sequential. The history phase's retry
    // fills IPv6. `update_reboot_requested_time` then holds the host while
    // complete loopbacks let attachment skip its group write.
    for (scenario, busy_history, factory_mac) in [
        ("host history with missing IPv6", true, "02:53:33:00:00:02"),
        (
            "update_reboot_requested_time with complete loopbacks",
            false,
            "02:53:33:00:00:03",
        ),
    ] {
        let mut changed_dpu = dpu.clone();
        changed_dpu.host_mac_address = factory_mac.parse()?;
        let hardware = HardwareInfo::from(&changed_dpu);
        assert_eq!(from_hardware_info(&hardware)?, dpu_machine_id);
        assert_eq!(
            model::machine::machine_id::host_id_from_dpu_hardware_info(&hardware)?,
            host_machine_id,
        );
        let new_link_id = dpu_discover_dhcp(&env, factory_mac).await;
        let mut setup = env.pool.begin().await?;
        db::machine_interface::set_primary_interface(&new_link_id, true, setup.as_mut()).await?;
        if busy_history {
            db::resource_pool::define(
                setup.as_mut(),
                LOOPBACK_IP_V6,
                &ResourcePoolDef {
                    pool_type: ResourcePoolType::Ipv6,
                    prefix: Some("2001:db8::/125".to_string()),
                    ranges: vec![],
                    delegate_prefix_len: None,
                },
            )
            .await?;
        }
        let initial_state = ManagedHostState::DPUInit {
            dpu_states: DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::PollingBiosSetup)]),
            },
        };
        db::machine::update_state(setup.as_mut(), &host_id, &initial_state).await?;
        db::machine_topology::set_topology_update_needed(setup.as_mut(), &dpu_machine_id, true)
            .await?;
        setup.commit().await?;
        let new_link = db::machine_interface::find_one(&env.pool, new_link_id).await?;
        assert!(new_link.machine_id.is_none() && new_link.attached_dpu_machine_id.is_none());
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT jsonb_agg(to_jsonb(m) ORDER BY m.id) FROM machines m WHERE m.id = ANY($1)",
        )
        .bind(machine_ids.as_slice())
        .fetch_one(&env.pool)
        .await?;
        let host_before = db::machine::find_one(&env.pool, &host_id, Default::default())
            .await?
            .expect("the existing predicted host");
        let dpu_before = dpu_machine(&env.pool, &dpu_machine_id).await?;
        assert_eq!(
            dpu_before.network_config.loopback_ip_v6.is_none(),
            busy_history
        );
        let pool_before = db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6).await?;
        let mut read = env.pool.begin().await?;
        let history_before =
            find_by_object_ids(read.as_mut(), StateHistoryTableId::Machine, &machine_ids).await?;
        read.commit().await?;

        // Use the real history trigger or `update_reboot_requested_time`.
        // Association's KEY SHARE is compatible with that function's row lock.
        let mut gate = env.pool.begin().await?;
        if busy_history {
            db::state_history::persist(
                gate.as_mut(),
                StateHistoryTableId::Machine,
                &host_id,
                &initial_state,
                host_before.state.version.increment(),
            )
            .await?;
        } else {
            db::machine::update_reboot_requested_time(
                &host_machine_id,
                gate.as_mut(),
                MachineLastRebootRequestedMode::PowerOff,
            )
            .await?;
        }
        let request = || {
            Request::new(MachineDiscoveryInfo {
                machine_interface_id: Some(dpu_interface.id),
                discovery_data: Some(DiscoveryData::Info(
                    DiscoveryInfo::try_from(hardware.clone()).unwrap(),
                )),
                create_machine: false,
                ..Default::default()
            })
        };
        let discovery = maybe_done(env.api.discover_machine(request()));
        tokio::pin!(discovery);
        let completed = tokio::time::timeout(Duration::from_secs(10), &mut discovery)
            .await
            .is_ok();
        let at_gate = async {
            let machines: serde_json::Value = sqlx::query_scalar(
                "SELECT jsonb_agg(to_jsonb(m) ORDER BY m.id) FROM machines m WHERE m.id = ANY($1)",
            )
            .bind(machine_ids.as_slice())
            .fetch_one(&env.pool)
            .await?;
            let new_interface = db::machine_interface::find_one(&env.pool, new_link_id).await?;
            let old_interface = db::machine_interface::find_one(&env.pool, old_link.id).await?;
            let stats = db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6).await?;
            let pending: bool = sqlx::query_scalar(
                "SELECT topology_update_needed FROM machine_topologies WHERE machine_id = $1",
            )
            .bind(dpu_machine_id)
            .fetch_one(&env.pool)
            .await?;
            let mut probe = env.pool.begin().await?;
            let history =
                find_by_object_ids(probe.as_mut(), StateHistoryTableId::Machine, &machine_ids)
                    .await?;
            let dpu_released =
                db::machine::try_lock_by_id_for_update(probe.as_mut(), &dpu_machine_id).await?;
            let host_released = if busy_history {
                Some(
                    db::machine::try_lock_by_id_for_no_key_update(probe.as_mut(), &host_machine_id)
                        .await?,
                )
            } else {
                None
            };
            let interfaces_released = sqlx::query(
                "SELECT id FROM machine_interfaces WHERE id = ANY($1) FOR UPDATE NOWAIT",
            )
            .bind([dpu_interface.id, old_link.id, new_link_id].as_slice())
            .execute(probe.as_mut())
            .await;
            probe.rollback().await?;
            interfaces_released?;
            Ok::<_, Box<dyn std::error::Error>>((
                machines,
                new_interface,
                old_interface,
                stats,
                pending,
                history,
                dpu_released,
                host_released,
            ))
        }
        .await;
        gate.rollback().await?;
        tokio::time::timeout(Duration::from_secs(30), &mut discovery).await?;
        let rejected = discovery
            .as_mut()
            .take_output()
            .expect("discovery completed");
        assert!(
            completed,
            "{scenario}: discovery waited for the held dependency"
        );
        let error = rejected.expect_err("busy selected-host state must reject discovery");
        assert_eq!(error.code(), Code::Unavailable, "{scenario}");
        let expected_message = if busy_history {
            format!("discovery state history for host {host_machine_id} is busy; retry discovery")
        } else {
            format!(
                "discovery state for host {host_machine_id} is busy or missing; retry discovery"
            )
        };
        assert_eq!(error.message(), expected_message, "{scenario}");
        let (
            machines,
            new_interface,
            old_interface,
            stats,
            pending,
            history,
            dpu_released,
            host_released,
        ) = at_gate?;
        assert_eq!(machines, before, "{scenario}");
        assert_eq!(
            serde_json::to_value(new_interface)?,
            serde_json::to_value(new_link)?
        );
        assert_eq!(
            serde_json::to_value(old_interface)?,
            serde_json::to_value(&old_link)?
        );
        assert_eq!(stats, pool_before);
        assert_eq!(
            serde_json::to_value(history)?,
            serde_json::to_value(&history_before)?
        );
        assert!(pending && dpu_released, "{scenario}");
        if let Some(host_released) = host_released {
            assert!(host_released, "{scenario}");
        }

        let response =
            tokio::time::timeout(Duration::from_secs(30), env.api.discover_machine(request()))
                .await??
                .into_inner();
        assert_eq!(response.machine_id, Some(dpu_machine_id));
        let topology_pending: bool = sqlx::query_scalar(
            "SELECT topology_update_needed FROM machine_topologies WHERE machine_id = $1",
        )
        .bind(dpu_machine_id)
        .fetch_one(&env.pool)
        .await?;
        assert!(
            !topology_pending,
            "successful discovery must clear its topology flag"
        );
        let reused_link = db::machine_interface::find_one(&env.pool, new_link_id).await?;
        assert_eq!(reused_link.machine_id, Some(host_machine_id));
        assert_eq!(reused_link.attached_dpu_machine_id, Some(dpu_id));
        assert!(!reused_link.primary_interface && reused_link.addresses.is_empty());
        let host_after = db::machine::find_one(&env.pool, &host_id, Default::default())
            .await?
            .expect("the reused predicted host");
        let dpu_after = dpu_machine(&env.pool, &dpu_machine_id).await?;
        let expected_state = ManagedHostState::DPUInit {
            dpu_states: DpuInitStates {
                states: HashMap::from([(dpu_id, DpuInitState::Init)]),
            },
        };
        assert_eq!(host_after.state.value, expected_state);
        assert_eq!(dpu_after.state.value, expected_state);
        assert_eq!(
            host_after.state.version.version_nr(),
            host_before.state.version.version_nr() + 1
        );
        assert_eq!(dpu_after.state.version, host_after.state.version);
        let mut expected_config = dpu_before.network_config.value;
        expected_config.loopback_ip_v6 = dpu_after.network_config.loopback_ip_v6;
        assert!(expected_config.loopback_ip_v6.is_some());
        assert_eq!(dpu_after.network_config.value, expected_config);
        assert_eq!(
            host_after.network_config.value,
            host_before.network_config.value
        );
        assert_eq!(
            host_after.network_config.version,
            dpu_after.network_config.version
        );
        assert_eq!(
            dpu_after.network_config.version.version_nr(),
            dpu_before.network_config.version.version_nr() + u64::from(busy_history)
        );
        assert_eq!(
            db::resource_pool::stats(&env.pool, LOOPBACK_IP_V6)
                .await?
                .used,
            pool_before.used + usize::from(busy_history)
        );
        let mut read = env.pool.begin().await?;
        let history_after =
            find_by_object_ids(read.as_mut(), StateHistoryTableId::Machine, &machine_ids).await?;
        read.commit().await?;
        for id in machine_ids {
            assert_eq!(
                history_after[&id.to_string()].len(),
                history_before[&id.to_string()].len() + 1
            );
            assert_eq!(
                history_after[&id.to_string()].last().unwrap().state_version,
                host_after.state.version
            );
        }
    }
    Ok(())
}

/// A host's first discovery with nothing to repair leaves the host's admin
/// links and network config version as they were: the reconcile after the
/// rename decides from an unlocked read that the host needs no repair and
/// writes nothing.
#[crate::sqlx_test]
async fn test_first_host_discovery_with_nothing_to_repair_writes_no_admin_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config().with_dpu_count(2);
    common::api_fixtures::site_explorer::new_mock_host_awaiting_discovery(
        &env,
        host_config.clone(),
    )
    .await
    .expect("the host should ingest up to its first discovery");
    let (primary_link, dormant_link) = primary_and_dormant_links(&env, &host_config).await?;
    let predicted_host_id = primary_link
        .machine_id
        .expect("the host links should belong to the predicted host");
    assert_eq!(primary_link.addresses.len(), 1, "{primary_link:?}");
    assert!(dormant_link.addresses.is_empty(), "{dormant_link:?}");
    let version_before = network_config_version(&env.pool, &predicted_host_id).await?;

    let metrics = MetricsCapture::start();
    let stable_host_id =
        MachineId::from(host_discover_machine(&env, &host_config, primary_link.id).await);

    assert_eq!(
        metrics.counter_delta(
            "carbide_api_host_admin_address_reconcile_failures_total",
            &[]
        ),
        0.0
    );
    for link_before in [&primary_link, &dormant_link] {
        let link = db::machine_interface::find_one(&env.pool, link_before.id).await?;
        assert_eq!(link.machine_id, Some(stable_host_id));
        assert_eq!(link.addresses, link_before.addresses, "{link:?}");
        assert_eq!(link.hostname, link_before.hostname);
        assert_eq!(link.domain_id, link_before.domain_id);
    }
    assert_eq!(
        network_config_version(&env.pool, &stable_host_id).await?,
        version_before
    );

    Ok(())
}

/// A DPU discovery that reads its host's predicted machine id and then waits
/// on the host's first discovery for its link row reconciles under that id
/// once both have committed, and finds no rows. The host's first discovery
/// reconciles under the stable id after its own commit, so the repair is
/// made anyway and neither reconcile counts as a failure.
#[crate::sqlx_test]
async fn test_first_host_discovery_repairs_what_an_overlapping_dpu_discovery_missed(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config().with_dpu_count(2);
    common::api_fixtures::site_explorer::new_mock_host_awaiting_discovery(
        &env,
        host_config.clone(),
    )
    .await
    .expect("the host should ingest up to its first discovery");
    let (primary_link, dormant_link) = primary_and_dormant_links(&env, &host_config).await?;
    let predicted_host_id = primary_link
        .machine_id
        .expect("the host links should belong to the predicted host");
    let &[admin_ip] = primary_link.addresses.as_slice() else {
        panic!("the primary link should hold the host's admin address: {primary_link:?}")
    };
    move_address_rows(&env.pool, primary_link.id, dormant_link.id).await?;
    let version_before = network_config_version(&env.pool, &predicted_host_id).await?;
    let dormant_dpu = host_config
        .dpus
        .iter()
        .find(|dpu| dpu.host_mac_address == dormant_link.mac_address)
        .expect("the dormant link should be keyed on a DPU's host MAC");
    let dormant_dpu_machine_id = MachineId::from(
        dormant_link
            .attached_dpu_machine_id
            .expect("the dormant link should be DPU-backed"),
    );
    let dpu_interface = dpu_oob_interface(&env, dormant_dpu, dormant_dpu_machine_id).await?;

    // The host discovery parks on the host machines row with the host's
    // interface rows already locked for the rename.
    let mut blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(predicted_host_id)
        .fetch_one(&mut *blocker)
        .await?;

    let metrics = MetricsCapture::start();
    let host_discovery = host_discover_machine(&env, &host_config, primary_link.id);
    tokio::pin!(host_discovery);
    let host_discovery_pid = tokio::select! {
        result = &mut host_discovery => {
            panic!("the host discovery finished while the host machines row was held: {result:?}")
        }
        blocked = wait_for_blocked_query(
            &env.pool,
            blocker_pid,
            "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
        ) => blocked,
    };

    // The DPU discovery reads the predicted host id from its link row, then
    // queues on that row behind the rename.
    let dpu_discovery = dpu_discover_machine(&env, dormant_dpu, dpu_interface.id);
    tokio::pin!(dpu_discovery);
    tokio::select! {
        result = &mut host_discovery => {
            panic!("the host discovery finished while the host machines row was held: {result:?}")
        }
        result = &mut dpu_discovery => {
            panic!("the DPU discovery finished while the host discovery held its link row: {result:?}")
        }
        _ = wait_for_blocked_query(
            &env.pool,
            host_discovery_pid,
            "UPDATE machine_interfaces SET attached_dpu_machine_id",
        ) => {}
    }

    blocker.rollback().await?;
    let ((stable_host_id, rediscovered_dpu_machine_id), logs) =
        carbide_instrument::testing::capture_logs_async(async {
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(host_discovery, dpu_discovery)
            })
            .await
            .expect("both discoveries must finish once the host machines row is free")
        })
        .await;
    assert_eq!(rediscovered_dpu_machine_id, dormant_dpu_machine_id);
    // The DPU discovery's reconcile looked the host up under the predicted id
    // and found no rows: a skip, not a failure.
    let predicted_host = predicted_host_id.to_string();
    assert!(
        logs.iter().any(|log| {
            log.message == "host has no admin interfaces to reconcile"
                && log.field("host_machine_id") == Some(predicted_host.as_str())
        }),
        "the DPU discovery's reconcile should have found no rows under the predicted id"
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_api_host_admin_address_reconcile_failures_total",
            &[]
        ),
        0.0
    );
    assert_admin_addresses_repaired_under(
        &env,
        MachineId::from(stable_host_id),
        primary_link.id,
        dormant_link.id,
        admin_ip,
        version_before,
    )
    .await
}

#[crate::sqlx_test]
async fn test_discover_dpu_does_not_create_machine_when_site_explorer_creates_machines(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = secure_discovery_config();
    config
        .site_explorer
        .create_machines
        .store(true, Ordering::Relaxed);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let expected_machine_id = from_hardware_info(&HardwareInfo::from(&dpu))?;

    let dhcp_response = env
        .api
        .discover_dhcp(Request::new(rpc::forge::DhcpDiscovery {
            mac_address: dpu.oob_mac_address.to_string(),
            relay_address: FIXTURE_DHCP_RELAY_ADDRESS.to_string(),
            vendor_string: None,
            link_address: None,
            circuit_id: None,
            remote_id: None,
            desired_address: None,
            address_family: None,
            message_kind: None,
            duid: None,
        }))
        .await?
        .into_inner();

    let remote_ip: IpAddr = dhcp_response.address.parse()?;
    let mut request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(rpc::DiscoveryInfo::try_from(
            HardwareInfo::from(&dpu),
        )?)),
        create_machine: true,
        ..Default::default()
    });
    request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((remote_ip, 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(request).await;

    assert_eq!(response.unwrap_err().code(), Code::PermissionDenied);
    let mut txn = env.pool.begin().await?;
    let interface = db::machine_interface::find_one(
        &mut *txn,
        dhcp_response
            .machine_interface_id
            .expect("DHCP discovery must return an interface ID"),
    )
    .await?;
    assert_eq!(interface.machine_id, None);
    assert!(
        db::machine::find_one(
            &mut *txn,
            &expected_machine_id,
            MachineSearchConfig {
                include_dpus: true,
                ..Default::default()
            },
        )
        .await?
        .is_none()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_released_instance_discovers_through_configured_secondary_dpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let host_config = env.managed_host_config().with_dpu_count(2);
    let hardware_info = HardwareInfo::from(&host_config);
    let managed_host = create_managed_host_with_config(&env, host_config).await;

    let mut txn = env.pool.begin().await?;
    let host = managed_host.host().db_machine(&mut txn).await;
    let primary = host
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("managed host must have a primary interface");
    let secondary = host
        .status
        .interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
        })
        .expect("two-DPU host must have a secondary interface");
    let secondary_device =
        host.get_device_locator_for_dpu_id(&secondary.attached_dpu_machine_id.unwrap())?;
    txn.rollback().await?;

    let instance = managed_host
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &[segment_id],
            &[secondary_device],
        ))
        .build()
        .await;
    let mut txn = env.pool.begin().await?;
    let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance.id,
        &segment_id,
    )
    .await?;
    let [address] = addresses.as_slice() else {
        panic!("instance must have one tenant address")
    };
    let tenant_ip = address.address;
    txn.rollback().await?;

    // Release boots Scout over the tenant network before switching to the
    // admin network. Stop there, while the secondary PF still owns this IP.
    env.api
        .release_instance(Request::new(rpc::InstanceReleaseRequest {
            id: Some(instance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await?;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        8,
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state: HostPlatformConfigurationState::CheckHostConfig,
            },
        },
    )
    .await;
    managed_host.network_configured(&env).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForDpusToUp,
        },
    )
    .await;
    managed_host.network_configured(&env).await;
    let discovery_state = ManagedHostState::Assigned {
        instance_state: InstanceState::BootingWithDiscoveryImage {
            retry: RetryInfo { count: 0 },
        },
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        1,
        discovery_state.clone(),
    )
    .await;

    let mut txn = env.pool.begin().await?;
    let released_host = managed_host.host().db_machine(&mut txn).await;
    assert_eq!(released_host.current_state(), &discovery_state);
    assert_eq!(released_host.network_config.use_admin_network, Some(false));
    let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance.id,
        &segment_id,
    )
    .await?;
    assert_eq!(retained_addresses.len(), 1);
    assert_eq!(retained_addresses[0].address, tenant_ip);
    let released_secondary = db::machine_interface::find_one(txn.as_mut(), secondary.id).await?;
    assert_eq!(
        released_secondary.machine_id,
        Some(managed_host.host().id.into())
    );
    assert!(released_secondary.addresses.is_empty());
    assert_eq!(released_secondary.domain_id, None);
    assert!(!released_secondary.primary_interface);
    txn.rollback().await?;

    let pxe = env
        .api
        .get_pxe_instructions(Request::new(rpc::forge::PxeInstructionRequest {
            arch: rpc::forge::MachineArchitecture::X86 as i32,
            client_ip: Some(tenant_ip.to_string()),
            ..Default::default()
        }))
        .await?
        .into_inner();
    assert!(
        pxe.pxe_script
            .contains(&format!("mac={} ", secondary.mac_address)),
        "tenant PXE must select the configured secondary MAC: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script
            .contains(&format!("machine_id={} ", secondary.id))
    );
    assert!(
        !pxe.pxe_script
            .contains(&format!("mac={} ", primary.mac_address))
    );

    let custom_user_data = "#cloud-config\nsecondary-interface-override";
    let mut txn = env.pool.begin().await?;
    db::machine_boot_override::create(
        txn.as_mut(),
        secondary.id,
        None,
        Some(custom_user_data.to_string()),
    )
    .await?
    .expect("fixture must install the secondary cloud-init override");
    txn.commit().await?;

    let cloud_init = env
        .api
        .get_cloud_init_instructions(Request::new(rpc::forge::CloudInitInstructionsRequest {
            ip: tenant_ip.to_string(),
        }))
        .await?
        .into_inner();
    assert_eq!(
        cloud_init.custom_cloud_init.as_deref(),
        Some(custom_user_data)
    );
    assert_eq!(
        cloud_init.metadata,
        Some(rpc::forge::CloudInitMetaData {
            instance_id: managed_host.host().id.to_string(),
            cloud_name: "nvidia".to_string(),
            platform: "forge".to_string(),
            local_hostname: None,
        })
    );
    let discovery = cloud_init.discovery_instructions.unwrap();
    assert_eq!(discovery.domain, None);
    let discovered_interface = discovery.machine_interface.unwrap();
    assert_eq!(discovered_interface.id, Some(secondary.id));
    assert_eq!(
        discovered_interface.mac_address,
        secondary.mac_address.to_string()
    );

    let response = secure_api_for(&env)
        .discover_machine(discovery_request_from(
            &hardware_info,
            discovered_interface.id,
            tenant_ip,
        ))
        .await?
        .into_inner();
    assert_eq!(response.machine_id, Some(managed_host.host().id.into()));
    assert_eq!(response.machine_interface_id, Some(secondary.id));
    Ok(())
}

#[crate::sqlx_test]
async fn test_secure_discovery_from_instance_address_accepts_same_machine_interfaces(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (managed_host, hardware_info, instance_ip, admin_interface_id) =
        allocated_host_for_secure_discovery(&env).await;
    let secure_api = secure_api_for(&env);

    let alternate_admin_interface_id: MachineInterfaceId = sqlx::query_scalar(
        r#"
        INSERT INTO machine_interfaces (
            segment_id, mac_address, hostname, domain_id, machine_id,
            primary_interface, interface_type, association_type
        )
        SELECT
            segment_id, $1, hostname || '-alternate', domain_id, machine_id,
            false, 'Data', 'Machine'
        FROM machine_interfaces
        WHERE id = $2
        RETURNING id
        "#,
    )
    .bind(MacAddress::from_str("02:00:00:00:20:01")?)
    .bind(admin_interface_id)
    .fetch_one(&env.pool)
    .await?;

    let non_admin_interface_id: MachineInterfaceId = sqlx::query_scalar(
        r#"
        INSERT INTO machine_interfaces (
            segment_id, mac_address, hostname, machine_id,
            primary_interface, interface_type, association_type
        )
        SELECT
            ia.segment_id, $1, 'instance-discovery-interface', i.machine_id,
            false, 'Data', 'Machine'
        FROM instance_addresses ia
        JOIN instances i ON i.id = ia.instance_id
        WHERE ia.address = $2
        RETURNING id
        "#,
    )
    .bind(MacAddress::from_str("02:00:00:00:20:04")?)
    .bind(instance_ip)
    .fetch_one(&env.pool)
    .await?;

    let bmc_interface_id: MachineInterfaceId = sqlx::query_scalar(
        r#"
        INSERT INTO machine_interfaces (
            segment_id, mac_address, hostname, domain_id, machine_id,
            primary_interface, interface_type, association_type
        )
        SELECT
            segment_id, $1, hostname || '-bmc', domain_id, machine_id,
            false, 'Bmc', 'Machine'
        FROM machine_interfaces
        WHERE id = $2
        RETURNING id
        "#,
    )
    .bind(MacAddress::from_str("02:00:00:00:20:02")?)
    .bind(admin_interface_id)
    .fetch_one(&env.pool)
    .await?;

    for (case, interface_id) in [
        ("alternate admin interface", alternate_admin_interface_id),
        ("non-admin data interface", non_admin_interface_id),
        ("BMC interface", bmc_interface_id),
    ] {
        let response = secure_api
            .discover_machine(discovery_request_from(
                &hardware_info,
                Some(interface_id),
                instance_ip,
            ))
            .await?
            .into_inner();

        assert_eq!(response.machine_id, Some(managed_host.host().id.into()));
        assert_eq!(
            response.machine_interface_id,
            Some(interface_id),
            "discovery must preserve Scout's same-machine interface selection: {case}"
        );
    }
    Ok(())
}

#[crate::sqlx_test]
async fn test_secure_discovery_from_instance_address_rejects_missing_or_foreign_interface(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (managed_host, hardware_info, instance_ip, _) =
        allocated_host_for_secure_discovery(&env).await;

    let foreign_host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    let foreign_interfaces =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[foreign_host.host().id]).await?;
    let foreign_interface_id = foreign_interfaces[&foreign_host.host().id][0].id;
    txn.rollback().await?;

    let secure_api = secure_api_for(&env);

    let cases = [
        ("missing interface ID", None, Code::InvalidArgument),
        (
            "interface owned by another host",
            Some(foreign_interface_id),
            Code::PermissionDenied,
        ),
    ];
    for (case, interface_id, expected_code) in cases {
        let response = secure_api
            .discover_machine(discovery_request_from(
                &hardware_info,
                interface_id,
                instance_ip,
            ))
            .await;
        assert_eq!(
            response.expect_err(case).code(),
            expected_code,
            "case: {case}; host: {}",
            managed_host.host().id,
        );
    }

    Ok(())
}

/// A caller-provided interface cannot disambiguate an overlay address shared
/// by separate instances. Secure discovery must reject the request before it
/// uses either host's identity.
#[crate::sqlx_test]
async fn test_secure_discovery_rejects_duplicate_overlay_address_owners(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (managed_host, hardware_info, instance_ip, admin_interface_id) =
        allocated_host_for_secure_discovery(&env).await;

    let mut txn = env.pool.begin().await?;
    let other_owner = common::overlay_address::seed_overlay_address_owner(
        txn.as_mut(),
        "secure-discovery-other-owner",
        instance_ip,
    )
    .await;
    txn.commit().await?;

    let status = secure_api_for(&env)
        .discover_machine(discovery_request_from(
            &hardware_info,
            Some(admin_interface_id),
            instance_ip,
        ))
        .await
        .expect_err("secure discovery should reject a duplicate overlay address");
    assert_eq!(status.code(), Code::PermissionDenied);
    assert!(
        !status
            .message()
            .contains(&managed_host.host().id.to_string())
            && !status
                .message()
                .contains(&other_owner.instance_id.to_string()),
        "discovery ambiguity must not identify either candidate owner"
    );

    Ok(())
}

/// A globally unique underlay address cannot override an unrelated tenant
/// allocation with the same numeric IP during secure discovery.
#[crate::sqlx_test]
async fn test_secure_discovery_rejects_unrelated_underlay_and_overlay_owners(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (managed_host, hardware_info, _, admin_interface_id) =
        allocated_host_for_secure_discovery(&env).await;

    let mut txn = env.pool.begin().await?;
    let admin_interface = db::machine_interface::find_one(txn.as_mut(), admin_interface_id).await?;
    let admin_ip = admin_interface
        .addresses
        .first()
        .copied()
        .expect("the managed host admin interface should have an address");
    let other_owner = common::overlay_address::seed_overlay_address_owner(
        txn.as_mut(),
        "secure-discovery-underlay-collision",
        admin_ip,
    )
    .await;
    txn.commit().await?;

    let status = secure_api_for(&env)
        .discover_machine(discovery_request_from(
            &hardware_info,
            Some(admin_interface_id),
            admin_ip,
        ))
        .await
        .expect_err("secure discovery should reject unrelated underlay and overlay owners");
    assert_eq!(status.code(), Code::PermissionDenied);
    assert!(
        !status
            .message()
            .contains(&managed_host.host().id.to_string())
            && !status
                .message()
                .contains(&other_owner.instance_id.to_string()),
        "discovery ambiguity must not identify either candidate owner"
    );

    Ok(())
}

/// A zero-DPU HostInband address is intentionally present in both address
/// tables. Secure discovery should accept that exact shared representation
/// without requiring a caller-provided interface ID.
#[crate::sqlx_test]
async fn test_secure_discovery_accepts_zero_dpu_host_inband_owner(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = common::api_fixtures::create_test_env_with_host_inband(pool).await;
    let vpc_id = common::api_fixtures::network_segment::create_default_flat_vpc(
        &env.api,
        "secure-discovery-zero-dpu",
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let host_config = model::test_support::ManagedHostConfig::zero_dpu();
    let hardware_info = HardwareInfo::from(&host_config);
    let managed_host = create_managed_host_with_config(&env, host_config).await;
    let instance = managed_host
        .instance_builer(&env)
        .tenant_org(crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID)
        .network(rpc::InstanceNetworkConfig {
            interfaces: vec![],
            #[allow(deprecated)]
            auto: true,
            auto_config: Some(rpc::forge::InstanceNetworkAutoConfig {
                vpc_id: Some(vpc_id),
            }),
        })
        .build()
        .await;

    let mut txn = env.pool.begin().await?;
    let interfaces =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[managed_host.host().id]).await?;
    let host_inband_interface = interfaces[&managed_host.host().id]
        .iter()
        .find(|interface| {
            interface.network_segment_type
                == Some(model::network_segment::NetworkSegmentType::HostInband)
        })
        .expect("zero-DPU host should have a HostInband interface");
    let host_ip = host_inband_interface
        .addresses
        .first()
        .copied()
        .expect("HostInband interface should have an address");
    let owners = db::instance_address::find_all_by_address(txn.as_mut(), host_ip).await?;
    let [owner] = owners.as_slice() else {
        panic!("HostInband address should have one overlay owner")
    };
    assert_eq!(owner.instance_id, instance.id);
    assert_eq!(owner.segment_id, host_inband_interface.segment_id);
    let expected_interface_id = host_inband_interface.id;
    txn.rollback().await?;

    let response = secure_api_for(&env)
        .discover_machine(discovery_request_from(&hardware_info, None, host_ip))
        .await?
        .into_inner();
    assert_eq!(response.machine_id, Some(managed_host.host().id.into()));
    assert_eq!(response.machine_interface_id, Some(expected_interface_id));

    Ok(())
}

#[crate::sqlx_test]
async fn test_secure_discovery_requires_remote_ip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let interface_id =
        common::api_fixtures::dpu::dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;

    let response = env
        .api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
            )),
            create_machine: true,
            ..Default::default()
        }))
        .await;

    assert_eq!(response.unwrap_err().code(), Code::InvalidArgument);
    Ok(())
}

#[crate::sqlx_test]
async fn test_secure_discovery_does_not_fall_back_to_interface_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let interface_id =
        common::api_fixtures::dpu::dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let mut request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: Some(interface_id),
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });

    request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((Ipv4Addr::new(203, 0, 113, 252), 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(request).await;

    assert_eq!(response.unwrap_err().code(), Code::PermissionDenied);
    Ok(())
}

/// Verifies that discovery replaces the predicted machine ID in the database
/// and that the machine controller reads the replacement ID and counts one machine.
#[crate::sqlx_test]
async fn test_secure_discovery_promotes_predicted_host_by_remote_ip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const MACHINE_TOTAL_METRIC: &str = "test_discovery_promoted_machines_total{fresh=\"true\"}";

    // Setup: opt this fixture into filtering machine metrics by IDs read from
    // the database, isolate its aggregate metric, and create one predicted host.
    let mut overrides = TestEnvOverrides::with_config(secure_discovery_config());
    overrides.machine_filter_metrics_by_existing_object_ids_enabled = Some(true);
    overrides.machine_metric_prefix = Some("test_discovery_promoted_machines");
    let env = create_test_env_with_overrides(pool, overrides).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;
    let mut txn = env.pool.begin().await?;
    let host_interface = db::machine_interface::find_one(&mut *txn, host_interface_id).await?;
    txn.commit().await?;
    let predicted_machine_id = HostMachineId::try_from(
        host_interface
            .machine_id
            .expect("DHCP discovery should create a predicted host"),
    )?;
    let remote_ip = host_interface.addresses[0];
    let expected_machine_id = from_hardware_info(&HardwareInfo::from(&host_config))?;
    let stable_machine_id = HostMachineId::try_from(expected_machine_id)?;

    // Setup assertion: the database and isolated aggregate both contain only
    // the predicted identity before discovery promotes the row.
    let database_host_ids = db::managed_host::load_host_ids(&env.pool).await?;
    assert_eq!(database_host_ids.len(), 1);
    assert!(database_host_ids.contains(&predicted_machine_id));
    env.run_machine_state_controller_iteration().await;
    assert_eq!(
        env.test_meter.formatted_metric(MACHINE_TOTAL_METRIC),
        Some("1".to_string())
    );

    // Action: report stable hardware identity through the real secure
    // discovery handler, promoting the existing predicted row.
    let mut request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: Some(uuid::Uuid::new_v4().into()),
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(HardwareInfo::from(&host_config)).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });

    request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((remote_ip, 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(request).await?.into_inner();

    // Assertion: discovery returns the stable identity selected from hardware.
    assert_eq!(response.machine_id, Some(expected_machine_id));
    assert_eq!(response.machine_interface_id, Some(host_interface_id));

    // Assertion: promotion replaced predicted A with stable B in the database
    // before the controller reads the next complete database ID list.
    let database_host_ids = db::managed_host::load_host_ids(&env.pool).await?;
    assert_eq!(database_host_ids.len(), 1);
    assert!(database_host_ids.contains(&stable_machine_id));
    assert!(!database_host_ids.contains(&predicted_machine_id));

    // Action: run the public manual controller helper so the promoted row is
    // observed and aggregate metrics are emitted through production paths.
    env.run_machine_state_controller_iteration().await;

    // Assertion: the ID list published after commit contains stable B, excludes
    // predicted A, and emits one machine. The helper clears retained metrics,
    // so the generic processor regression separately proves A/B cache filtering.
    assert_eq!(
        env.machine_object_ids_from_database_contains(&stable_machine_id)
            .await,
        Some(true)
    );
    assert_eq!(
        env.machine_object_ids_from_database_contains(&predicted_machine_id)
            .await,
        Some(false)
    );
    assert_eq!(
        env.test_meter.formatted_metric(MACHINE_TOTAL_METRIC),
        Some("1".to_string())
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_secure_discovery_rejects_stable_host_identity_mismatch_without_mutation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;
    let mut txn = env.pool.begin().await?;
    let host_interface = db::machine_interface::find_one(&mut *txn, host_interface_id).await?;
    txn.commit().await?;
    let remote_ip = host_interface.addresses[0];
    let expected_machine_id = from_hardware_info(&HardwareInfo::from(&host_config))?;

    let mut initial_request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(rpc::DiscoveryInfo::try_from(
            HardwareInfo::from(&host_config),
        )?)),
        create_machine: true,
        ..Default::default()
    });
    initial_request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((remote_ip, 0)),
            peer_certificates: vec![],
        }));
    let initial_response = env
        .api
        .discover_machine(initial_request)
        .await?
        .into_inner();
    assert_eq!(initial_response.machine_id, Some(expected_machine_id));

    let mut mismatching_hardware = HardwareInfo::from(&host_config);
    mismatching_hardware.tpm_ek_certificate = Some(TpmEkCertificate::from(vec![0x5a; 512]));
    let mismatching_machine_id = from_hardware_info(&mismatching_hardware)?;
    assert_ne!(mismatching_machine_id, expected_machine_id);
    let mut mismatching_request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: None,
        discovery_data: Some(rpc::DiscoveryData::Info(rpc::DiscoveryInfo::try_from(
            mismatching_hardware,
        )?)),
        create_machine: true,
        ..Default::default()
    });
    mismatching_request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((remote_ip, 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(mismatching_request).await;

    assert_eq!(response.unwrap_err().code(), Code::PermissionDenied);
    let mut txn = env.pool.begin().await?;
    let host_interface = db::machine_interface::find_one(&mut *txn, host_interface_id).await?;
    assert_eq!(host_interface.machine_id, Some(expected_machine_id));
    let topology_count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM machine_topologies WHERE machine_id = $1")
            .bind(mismatching_machine_id)
            .fetch_one(&mut *txn)
            .await?;
    assert_eq!(topology_count.0, 0);

    Ok(())
}

#[crate::sqlx_test]
async fn test_insecure_discovery_uses_interface_id_and_ignores_remote_ip(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let interface_id =
        common::api_fixtures::dpu::dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let mut request = Request::new(rpc::MachineDiscoveryInfo {
        machine_interface_id: Some(interface_id),
        discovery_data: Some(rpc::DiscoveryData::Info(
            rpc::DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
        )),
        create_machine: true,
        ..Default::default()
    });
    request
        .extensions_mut()
        .insert::<Arc<ConnectionAttributes>>(Arc::new(ConnectionAttributes {
            peer_address: SocketAddr::from((Ipv4Addr::new(203, 0, 113, 254), 0)),
            peer_certificates: vec![],
        }));

    let response = env.api.discover_machine(request).await?.into_inner();

    assert_eq!(response.machine_interface_id, Some(interface_id));
    Ok(())
}

#[crate::sqlx_test]
async fn test_insecure_discovery_requires_interface_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let response = env
        .api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: None,
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(HardwareInfo::from(&dpu)).unwrap(),
            )),
            create_machine: true,
            ..Default::default()
        }))
        .await;

    assert_eq!(response.unwrap_err().code(), Code::InvalidArgument);
    Ok(())
}

#[crate::sqlx_test]
async fn test_discovery_ip_lookup_rejects_missing_mapping(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mut txn = env.pool.begin().await?;
    let missing_address = "203.0.113.253".parse().unwrap();
    let missing = db::machine_interface::find_for_update_by_ip(&mut txn, missing_address).await;
    assert!(matches!(
        missing,
        Err(db::DatabaseError::NotFoundError {
            kind: "machine_interface for discovery IP",
            id,
        }) if id == missing_address.to_string()
    ));
    Ok(())
}

#[crate::sqlx_test]
async fn test_discovery_rejects_interface_owned_by_different_stable_identity(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu = env
        .managed_host_config()
        .get_and_assert_single_dpu()
        .clone();
    let interface_id =
        common::api_fixtures::dpu::dpu_discover_dhcp(&env, &dpu.oob_mac_address.to_string()).await;
    let first_hardware = HardwareInfo::from(&dpu);
    let first_machine_id =
        common::api_fixtures::dpu::dpu_discover_machine(&env, &dpu, interface_id).await;

    let mut other_hardware = first_hardware;
    other_hardware
        .dmi_data
        .as_mut()
        .expect("DPU fixture must contain DMI data")
        .product_serial
        .push_str("-different");
    let other_machine_id = from_hardware_info(&other_hardware)?;
    assert_ne!(first_machine_id, other_machine_id);

    let response = env
        .api
        .discover_machine(Request::new(rpc::MachineDiscoveryInfo {
            machine_interface_id: Some(interface_id),
            discovery_data: Some(rpc::DiscoveryData::Info(
                rpc::DiscoveryInfo::try_from(other_hardware).unwrap(),
            )),
            create_machine: true,
            ..Default::default()
        }))
        .await;

    assert_eq!(response.unwrap_err().code(), Code::PermissionDenied);
    let topology_count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM machine_topologies WHERE machine_id = $1")
            .bind(other_machine_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(topology_count.0, 0);
    Ok(())
}

/// With reconciliation disabled by default, a differing fallback selection does not change the host.
#[crate::sqlx_test]
async fn test_scout_pci_default_off_observes_fallback_selection(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (host_config, host_machine_id, interfaces) =
        create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishChassisId).await?;
    let before = load_scout_pci_selection_state(&env.pool, host_machine_id).await?;
    let comparisons = submit_scout_pci_report(
        &env.api,
        host_machine_id,
        interfaces.current_id,
        scout_pci_report(&host_config, interfaces.alternate_mac),
    )
    .await?;
    assert_eq!(comparisons, ["differs_from_stored"]);
    assert_eq!(
        load_scout_pci_selection_state(&env.pool, host_machine_id).await?,
        before,
        "disabled reconciliation must not change primary_interface state",
    );

    Ok(())
}

/// A matching comparison records scout as the source without replacing the stored Redfish pair.
#[crate::sqlx_test]
async fn test_scout_pci_same_target_updates_only_source(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(scout_reconciliation_config()),
    )
    .await;
    let (host_config, host_machine_id, interfaces) =
        create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishSerialNumber).await?;
    let refreshed_interface_id = "NIC.Slot.99-1-1";
    let updated_interfaces = sqlx::query(
        "UPDATE machine_interfaces
         SET boot_interface_id = $1
         WHERE id = $2",
    )
    .bind(refreshed_interface_id)
    .bind(interfaces.current_id)
    .execute(&env.pool)
    .await?;
    assert_eq!(updated_interfaces.rows_affected(), 1);
    let before = load_scout_pci_selection_state(&env.pool, host_machine_id).await?;
    let stored_desired_interface_id = before
        .desired_interface_id
        .as_deref()
        .expect("the fixture must have a stored desired interface pair");
    assert_ne!(
        stored_desired_interface_id, refreshed_interface_id,
        "the current interface row must carry newer Redfish identity than the stored desired pair",
    );
    assert!(!before.controller_queued);

    let comparisons = submit_scout_pci_report(
        &env.api,
        host_machine_id,
        interfaces.current_id,
        scout_pci_report(&host_config, interfaces.current_mac),
    )
    .await?;
    assert_eq!(comparisons, ["matches_stored"]);
    let after = load_scout_pci_selection_state(&env.pool, host_machine_id).await?;
    // A matching report must update primary attribution and decision time without changing the target or controller work.
    assert!(
        after
            .primary_selection_updated_at
            .expect("Scout primary decision time")
            > before
                .primary_selection_updated_at
                .expect("automatic primary decision time")
    );
    let mut expected = before;
    expected.selection_source = Some(BootInterfaceSelectionSource::ScoutReportPci);
    expected.primary_selection_source = Some(BootInterfaceSelectionSource::ScoutReportPci);
    expected.primary_selection_updated_at = after.primary_selection_updated_at;
    assert_eq!(
        after, expected,
        "a matching comparison must preserve the target, generations, and controller work",
    );

    Ok(())
}

/// A different complete scout candidate moves the coupled state once and queues the controller
/// after the transaction commits.
#[crate::sqlx_test]
async fn test_scout_pci_different_target_updates_ready_host_and_queues_controller(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(scout_reconciliation_config()),
    )
    .await;
    let (host_config, host_machine_id, interfaces) =
        create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishChassisId).await?;

    let before = load_scout_pci_selection_state(&env.pool, host_machine_id).await?;

    let comparisons = submit_scout_pci_report(
        &env.api,
        host_machine_id,
        interfaces.current_id,
        scout_pci_report(&host_config, interfaces.alternate_mac),
    )
    .await?;
    assert_eq!(comparisons, ["differs_from_stored"]);
    let after = load_scout_pci_selection_state(&env.pool, host_machine_id).await?;
    assert_eq!(
        after.current_primary_mac_address,
        Some(interfaces.alternate_mac)
    );
    // Moving the primary must attribute the new row to Scout rather than inheriting its predecessor's decision time.
    assert_eq!(
        after.primary_selection_source,
        Some(BootInterfaceSelectionSource::ScoutReportPci)
    );
    assert!(
        after
            .primary_selection_updated_at
            .expect("new primary decision time")
            > before
                .primary_selection_updated_at
                .expect("previous primary decision time")
    );
    assert!(after.controller_queued);
    assert_eq!(after.desired_mac_address, Some(interfaces.alternate_mac));
    assert_eq!(
        after.selection_source,
        Some(BootInterfaceSelectionSource::ScoutReportPci),
    );

    Ok(())
}

/// Instance allocation and scout reconciliation serialize on the Machine row. Whichever commits
/// first leaves the other request a complete state to validate.
#[crate::sqlx_test]
async fn test_scout_pci_reconciliation_serializes_with_instance_allocation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(scout_reconciliation_config()),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Queue reconciliation first behind a locked Machine row, then queue allocation behind it.
    let (reconciliation_host_config, reconciliation_host_id, reconciliation_interfaces) =
        create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishChassisId).await?;
    let mut reconciliation_machine_blocker = env.pool.begin().await?;
    let reconciliation_blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reconciliation_machine_blocker)
        .await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(reconciliation_host_id)
        .fetch_one(&mut *reconciliation_machine_blocker)
        .await?;

    let reconciliation_api = env.api.clone();
    let reconciliation_report = scout_pci_report(
        &reconciliation_host_config,
        reconciliation_interfaces.alternate_mac,
    );
    let reconciliation_task = tokio::spawn(async move {
        run_scout_pci_reconciliation(
            &reconciliation_api,
            reconciliation_host_id,
            &reconciliation_report,
        )
        .await
        .expect("scout reconciliation must succeed")
    });
    let reconciliation_pid = wait_for_blocked_query(
        &env.pool,
        reconciliation_blocker_pid,
        "machine.version AS machine_version",
    )
    .await;

    let reconciliation_allocation_api = env.api.clone();
    let reconciliation_allocation_request =
        scout_test_allocation_request(reconciliation_host_id, segment_id);
    let reconciliation_allocation_task = tokio::spawn(async move {
        reconciliation_allocation_api
            .allocate_instance(Request::new(reconciliation_allocation_request))
            .await
    });
    wait_for_blocked_query(&env.pool, reconciliation_pid, "SELECT row_to_json(m.*)").await;
    reconciliation_machine_blocker.commit().await?;

    assert!(
        reconciliation_task.await?,
        "scout reconciliation must commit before allocation resumes",
    );
    let allocation_error = reconciliation_allocation_task
        .await?
        .expect_err("allocation must reject the committed pending boot configuration");
    assert_eq!(allocation_error.code(), Code::FailedPrecondition);
    assert!(
        allocation_error
            .message()
            .contains("pending boot configuration")
    );
    let reconciliation_host_instances: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instances WHERE machine_id = $1")
            .bind(reconciliation_host_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(reconciliation_host_instances, 0);

    // Reverse the queue: allocation waits first, and scout waits behind allocation.
    let (allocation_host_config, allocation_host_id, allocation_interfaces) =
        create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishSerialNumber).await?;
    let allocation_before = load_scout_pci_selection_state(&env.pool, allocation_host_id).await?;

    let mut allocation_machine_blocker = env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *allocation_machine_blocker)
        .await?;
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(allocation_host_id)
        .fetch_one(&mut *allocation_machine_blocker)
        .await?;

    let allocation_api = env.api.clone();
    let allocation_request = scout_test_allocation_request(allocation_host_id, segment_id);
    let allocation_task = tokio::spawn(async move {
        allocation_api
            .allocate_instance(Request::new(allocation_request))
            .await
    });
    let allocation_pid =
        wait_for_blocked_query(&env.pool, blocker_pid, "SELECT row_to_json(m.*)").await;

    let scout_api = env.api.clone();
    let scout_report =
        scout_pci_report(&allocation_host_config, allocation_interfaces.alternate_mac);
    let scout_task = tokio::spawn(async move {
        run_scout_pci_reconciliation(&scout_api, allocation_host_id, &scout_report)
            .await
            .expect("scout reconciliation must succeed")
    });
    wait_for_blocked_query(
        &env.pool,
        allocation_pid,
        "machine.version AS machine_version",
    )
    .await;
    allocation_machine_blocker.commit().await?;

    allocation_task
        .await?
        .expect("allocation that owns the Machine lock must commit first");
    let reconciliation_needed = scout_task.await?;
    assert!(!reconciliation_needed);
    let allocation_after = load_scout_pci_selection_state(&env.pool, allocation_host_id).await?;
    let mut expected = allocation_before;
    expected.controller_state = allocation_after.controller_state.clone();
    expected.controller_queued = allocation_after.controller_queued;
    assert_eq!(
        allocation_after, expected,
        "the live Instance must leave primary_interface selection unchanged",
    );
    // Allocation persists the Instance before machine-controller advances Ready to Assigned.
    // The live Instance must remain protected from scout during that intermediate state.
    assert!(matches!(
        serde_json::from_str::<ManagedHostState>(&allocation_after.controller_state)?,
        ManagedHostState::Ready
    ));
    let allocation_host_instances: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instances WHERE machine_id = $1")
            .bind(allocation_host_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(allocation_host_instances, 1);

    Ok(())
}

/// A Scout-reported discovery records the reporter version on the machine and
/// persists it in the database.
#[crate::sqlx_test]
async fn test_discovery_records_scout_version(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

    let machine_id = host_discover_machine_with_reporter(
        &env,
        &host_config,
        host_machine_interface_id,
        rpc::MachineDiscoveryReporter::Scout,
        Some("v0.11.0-pr-11-g14586866e"),
    )
    .await;

    // The version is exposed on the Machine resource over gRPC.
    let rpc_machine = env
        .api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![machine_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0);
    assert_eq!(
        rpc_machine
            .status
            .as_ref()
            .unwrap()
            .last_scout_observed_version
            .as_deref(),
        Some("v0.11.0-pr-11-g14586866e")
    );

    Ok(())
}

/// A version reported by the DPU agent (rather than Scout) is not recorded as
/// the last seen Scout version.
#[crate::sqlx_test]
async fn test_discovery_ignores_version_from_dpu_agent(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

    let machine_id = host_discover_machine_with_reporter(
        &env,
        &host_config,
        host_machine_interface_id,
        rpc::MachineDiscoveryReporter::DpuAgent,
        Some("v0.11.0-pr-11-g14586866e"),
    )
    .await;

    let machine = db::machine::find_one(&env.pool, &machine_id, MachineSearchConfig::default())
        .await?
        .expect("machine must exist");

    assert!(machine.status.last_scout_observed_version.is_none());

    Ok(())
}

/// A subsequent Scout discovery overwrites the previously recorded version.
#[crate::sqlx_test]
async fn test_discovery_updates_scout_version_on_rediscovery(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_interface_id = host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

    let machine_id = host_discover_machine_with_reporter(
        &env,
        &host_config,
        host_machine_interface_id,
        rpc::MachineDiscoveryReporter::Scout,
        Some("v0.11.0-pr-11-g14586866e"),
    )
    .await;
    let machine = db::machine::find_one(&env.pool, &machine_id, MachineSearchConfig::default())
        .await?
        .expect("machine must exist");
    assert_eq!(
        machine.status.last_scout_observed_version.as_deref(),
        Some("v0.11.0-pr-11-g14586866e")
    );
    let rediscovered_machine_id = host_discover_machine_with_reporter(
        &env,
        &host_config,
        host_machine_interface_id,
        rpc::MachineDiscoveryReporter::Scout,
        Some("v0.12.0-pr-42-gabcdef012"),
    )
    .await;
    assert_eq!(rediscovered_machine_id, machine_id);
    let machine = db::machine::find_one(&env.pool, &machine_id, MachineSearchConfig::default())
        .await?
        .expect("machine must exist");
    assert_eq!(
        machine.status.last_scout_observed_version.as_deref(),
        Some("v0.12.0-pr-42-gabcdef012")
    );

    Ok(())
}

/// Verifies secure source-IP registration rejects a partial BF4 report, accepts fresh PF facts, and exposes them through FindMachinesByIds.
/// Generic BOM refresh may retain the first payload, so accepted PF facts need independent persistence.
#[crate::sqlx_test]
async fn dpu_agent_registration_recovers_partial_host_pf_inventory(pool: sqlx::PgPool) {
    // Lease the reporting DPU's address so discovery must resolve its interface from the source IP.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    let host_config = env.managed_host_config();
    let dpu = host_config.get_and_assert_single_dpu();
    let reporting_lease = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(dpu.oob_mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .expect("DPU source lease")
        .into_inner();
    let source_ip = reporting_lease.address.parse().expect("DPU source address");

    // Keep DMI identity unchanged while VPD classifies this DPU as BF4.
    let mut hardware_info = HardwareInfo::from(dpu);
    let dpu_info = hardware_info.dpu_info.as_mut().expect("fixture DPU");
    dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string();
    dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
        pci_controller_id: 2, // A populated inventory still lacks the configured controller.
        pf_id: 0,
        mac_address: "02:00:00:00:00:20".parse().expect("nonselected MAC"),
    }];
    let request = |hardware_info: HardwareInfo| {
        let mut request = discovery_request_from(&hardware_info, None, source_ip);
        request.get_mut().discovery_reporter = rpc::MachineDiscoveryReporter::DpuAgent as i32;
        request
    };

    // Reject first binding while retaining enough committed bootstrap state for another attempt.
    let missing = env
        .api
        .discover_machine(request(hardware_info.clone()))
        .await
        .expect_err("configured PF is missing");
    assert_eq!(missing.code(), tonic::Code::FailedPrecondition);
    assert!(missing.message().contains("c1pf0"));

    // Submit newly acquired facts instead of reusing the partial report.
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU")
        .host_pfs
        .push(model::hardware_info::DpuHostPf {
            pci_controller_id: 1, // This is the API-selected controller.
            pf_id: 0,
            mac_address: "02:00:00:00:00:10".parse().expect("selected MAC"),
        });
    let accepted = env
        .api
        .discover_machine(request(hardware_info))
        .await
        .expect("fresh report accepted")
        .into_inner();
    assert_eq!(
        accepted.machine_interface_id,
        reporting_lease.machine_interface_id
    );
    let machine_id = accepted.machine_id.expect("registered DPU ID");

    // Find the machine afresh to prove the accepted PF facts persisted through snapshot conversion.
    let found = env
        .api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![machine_id],
            include_history: false,
        }))
        .await
        .expect("find registered DPU")
        .into_inner()
        .machines
        .remove(0);
    let discovery = found
        .status
        .expect("machine status")
        .discovery_info
        .expect("discovery info");
    let observed = discovery.dpu_info.expect("DPU info").host_pfs;
    assert_eq!(
        observed
            .iter()
            .map(|pf| (pf.pci_controller_id, pf.mac_address.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "02:00:00:00:00:10"), (2, "02:00:00:00:00:20")]
    );
}

/// Verifies secure Scout, unspecified and unknown reporters cannot establish BF4 PF authority, so generic discovery cannot satisfy the initialization checkpoint.
#[crate::sqlx_test]
async fn non_agent_discovery_does_not_establish_host_pf_authority(pool: sqlx::PgPool) {
    // Authenticate each report by its own DPU lease while varying only the reporter's authority.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(secure_discovery_config()),
    )
    .await;
    for (scenario, reporter) in [
        // Scout may report DPU hardware without supplying the agent's accepted PF inventory.
        (
            "Scout reporter",
            rpc::MachineDiscoveryReporter::Scout as i32,
        ),
        // Legacy clients omit the reporter and must not gain PF authority by default.
        (
            "unspecified reporter",
            rpc::MachineDiscoveryReporter::Unspecified as i32,
        ),
        // Future or unrecognized enum values must retain the non-agent default instead of granting authority.
        ("unknown reporter", i32::MAX),
    ] {
        // Supply complete selected-PF facts so reporter gating is the only reason authority is absent.
        let host_config = env.managed_host_config();
        let dpu = host_config.get_and_assert_single_dpu();
        let reporting_lease = env
            .api
            .discover_dhcp(
                DhcpDiscovery::builder(dpu.oob_mac_address, FIXTURE_DHCP_RELAY_ADDRESS)
                    .tonic_request(),
            )
            .await
            .expect("DPU source lease")
            .into_inner();
        let source_ip = reporting_lease.address.parse().expect("DPU source address");
        let mut hardware_info = HardwareInfo::from(dpu);
        let dpu_info = hardware_info.dpu_info.as_mut().expect("fixture DPU");
        dpu_info.part_number = "900-9D4B4-CWAA-TSA".to_string();
        dpu_info.host_pfs = vec![model::hardware_info::DpuHostPf {
            pci_controller_id: env.config.dpu_host_pf_pci_controller_id, // Supplies the required controller.
            pf_id: 0,
            mac_address: "02:00:00:00:00:10".parse().expect("selected PF MAC"),
        }];

        // Accept ordinary discovery without trusting the caller's reporter as the DPU agent.
        let mut request = discovery_request_from(&hardware_info, None, source_ip);
        request.get_mut().discovery_reporter = reporter;
        let discovered = env
            .api
            .discover_machine(request)
            .await
            .expect("non-agent discovery remains supported")
            .into_inner();
        assert_eq!(
            discovered.machine_interface_id,
            reporting_lease.machine_interface_id
        );
        let dpu_id = DpuMachineId::try_from(discovered.machine_id.expect("registered DPU ID"))
            .expect("DPU machine ID");

        // Read the authority column itself because generic discovery JSON can also contain host_pfs.
        let mut read = env.db_txn().await;
        let topology = db::machine_topology::find_latest_by_machine_ids(&mut read, &[dpu_id])
            .await
            .expect("registered topology")
            .remove(&dpu_id)
            .expect("DPU topology");
        assert!(topology.dpu_host_pf_inventory.is_none(), "{scenario}");
        read.rollback().await.expect("close authority read");
    }
}

/// Corrects automatic networking without replacing independent Operator or ExpectedMachine boot selections, so primary correction preserves boot-source precedence.
#[crate::sqlx_test]
async fn test_scout_pci_preserves_independent_boot_selection(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(scout_reconciliation_config()),
    )
    .await;
    for (boot_source, needs_new_generation) in [
        // A pending boot-only Operator request retains its generation when Scout corrects automatic networking.
        (BootInterfaceSelectionSource::Operator, false),
        // A stable declaration retains its boot choice while the new network primary requires controller reconciliation.
        (BootInterfaceSelectionSource::ExpectedMachine, true),
    ] {
        // Model independent boot attribution over a replaceable automatic networking primary.
        let (host_config, host_machine_id, interfaces) =
            create_scout_test_host(&env, BootInterfaceSelectionSource::RedfishChassisId).await?;
        if boot_source == BootInterfaceSelectionSource::Operator {
            // A public boot-only request establishes pending work without changing networking provenance.
            env.api
                .set_dpu_first_boot_order(Request::new(rpc::forge::SetDpuFirstBootOrderRequest {
                    machine_id: Some(host_machine_id.to_string()),
                    bmc_endpoint_request: None,
                    boot_interface_mac: Some(interfaces.current_mac.to_string()),
                }))
                .await?;
        } else {
            // Attribute the verified boot target to a declaration while retaining the working primary's automatic source.
            let mut setup = env.pool.begin().await?;
            let boot_target =
                db::machine_desired_boot_interface::get(&mut *setup, &host_machine_id.into())
                    .await?
                    .expect("boot target")
                    .value;
            db::machine_desired_boot_interface::set(
                &mut setup,
                &host_machine_id.into(),
                &boot_target,
                boot_source,
            )
            .await?;
            setup.commit().await?;
        }
        // Clear setup's queue entry so only the subsequent Scout report can establish a new enqueue.
        clear_scout_test_queue(&env.pool, host_machine_id).await?;
        assert!(
            !load_scout_pci_selection_state(&env.pool, host_machine_id)
                .await?
                .controller_queued
        );
        let before = db::machine::find_one(&env.pool, &host_machine_id, Default::default())
            .await?
            .expect("host");

        // Check whether existing boot work already provides the controller reconciliation trigger.
        assert_eq!(
            before.pending_boot_interface_config_version().is_none(),
            needs_new_generation
        );

        // Exercise the public discovery handler with a different, complete Scout PCI winner.
        submit_scout_pci_report(
            &env.api,
            host_machine_id,
            interfaces.current_id,
            scout_pci_report(&host_config, interfaces.alternate_mac),
        )
        .await?;

        // Reload both decisions to prove networking changed and reconciliation was queued without changing boot target, source, or time.
        let after = db::machine::find_one(&env.pool, &host_machine_id, Default::default())
            .await?
            .expect("host after Scout");
        assert_eq!(
            after
                .status
                .interfaces
                .iter()
                .find(|row| row.primary_interface)
                .expect("primary")
                .mac_address,
            interfaces.alternate_mac
        );
        assert_eq!(
            after
                .config
                .primary_interface_selection
                .expect("primary source")
                .source,
            BootInterfaceSelectionSource::ScoutReportPci
        );
        let previous_boot = before
            .config
            .desired_boot_interface
            .as_ref()
            .expect("previous boot target");
        let retained_boot = after
            .config
            .desired_boot_interface
            .as_ref()
            .expect("retained boot target");
        assert_eq!(retained_boot.value, previous_boot.value);
        assert_eq!(
            retained_boot.version.version_nr(),
            previous_boot.version.version_nr() + if needs_new_generation { 1 } else { 0 }
        );
        assert_eq!(
            after.pending_boot_interface_config_version(),
            Some(retained_boot.version)
        );
        assert_eq!(
            after.config.boot_interface_selection,
            before.config.boot_interface_selection
        );
        assert_ne!(after.network_config.version, before.network_config.version);

        // Pending boot work does not replace the controller wakeup needed for the new networking primary.
        assert!(
            load_scout_pci_selection_state(&env.pool, host_machine_id)
                .await?
                .controller_queued
        );
    }
    Ok(())
}

/// Recovers Scout correction for an old writer's unattributed BF3 primary only with matching chassis/serial boot evidence, preserving explicit choices and unrelated boot intent.
#[crate::sqlx_test]
async fn test_scout_pci_recovers_missing_primary_provenance(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(scout_reconciliation_config()),
    )
    .await;
    for (boot_source, same_mac, known_operator_primary, corrects) in [
        // A postmigration old writer's matching chassis choice remains eligible for automatic correction.
        (
            BootInterfaceSelectionSource::RedfishChassisId,
            true,
            false,
            true,
        ),
        // Serial fallback supplies the other supported automatic provenance without inventing networking intent.
        (
            BootInterfaceSelectionSource::RedfishSerialNumber,
            true,
            false,
            true,
        ),
        // An independent boot MAC cannot attribute an otherwise unknown networking decision.
        (
            BootInterfaceSelectionSource::RedfishChassisId,
            false,
            false,
            false,
        ),
        // Historical Operator intent stays conservatively protected when its networking attribution is missing.
        (BootInterfaceSelectionSource::Operator, true, false, false),
        // A declaration may independently select boot, so matching MAC alone is insufficient.
        (
            BootInterfaceSelectionSource::ExpectedMachine,
            true,
            false,
            false,
        ),
        // Known explicit networking always wins over matching automatic boot evidence.
        (
            BootInterfaceSelectionSource::RedfishChassisId,
            true,
            true,
            false,
        ),
    ] {
        // Build an ordinary two-DPU BF3 host, then represent the metadata left absent by a predecessor API.
        let (host_config, host_id, interfaces) = create_scout_test_host(&env, boot_source).await?;
        sqlx::query("UPDATE machine_interfaces SET primary_selection_source = $2,
                    primary_selection_updated_at = CASE WHEN $2::boot_interface_selection_source IS NULL THEN NULL ELSE clock_timestamp() END
                    WHERE machine_id = $1 AND primary_interface")
            .bind(host_id).bind(known_operator_primary.then_some(BootInterfaceSelectionSource::Operator)).execute(&env.pool).await?;
        if !same_mac {
            // Keep automatic boot provenance while making its MAC independent of the current networking primary.
            sqlx::query(
                "UPDATE machine_boot_interfaces SET desired_mac_address = $2 WHERE machine_id = $1",
            )
            .bind(host_id)
            .bind(interfaces.alternate_mac)
            .execute(&env.pool)
            .await?;
        }
        env.run_site_explorer_iteration().await;
        // Isolate Scout's queue publication from work performed by the preceding Explorer iteration.
        clear_scout_test_queue(&env.pool, host_id).await?;
        let before = db::machine::find_one(&env.pool, &host_id, Default::default())
            .await?
            .expect("old-writer host");
        assert_eq!(
            before.config.primary_interface_selection.is_some(),
            known_operator_primary
        );
        let original_boot = before
            .config
            .desired_boot_interface
            .as_ref()
            .expect("old boot decision");

        // Submit a different complete PCI winner through public discovery after ordinary Explorer reconciliation.
        submit_scout_pci_report(
            &env.api,
            host_id,
            interfaces.current_id,
            scout_pci_report(&host_config, interfaces.alternate_mac),
        )
        .await?;

        // Reload primary, provenance and queued work so silently declining the old-writer case cannot pass.
        let after = db::machine::find_one(&env.pool, &host_id, Default::default())
            .await?
            .expect("host after Scout");
        assert_eq!(
            after
                .status
                .interfaces
                .iter()
                .find(|interface| interface.primary_interface)
                .expect("primary")
                .mac_address,
            if corrects {
                interfaces.alternate_mac
            } else {
                interfaces.current_mac
            }
        );
        if corrects {
            assert_eq!(
                after
                    .config
                    .primary_interface_selection
                    .expect("corrected source")
                    .source,
                BootInterfaceSelectionSource::ScoutReportPci
            );
            assert_eq!(
                after
                    .config
                    .desired_boot_interface
                    .as_ref()
                    .expect("corrected boot")
                    .value
                    .mac_address(),
                interfaces.alternate_mac
            );
            assert!(
                load_scout_pci_selection_state(&env.pool, host_id)
                    .await?
                    .controller_queued
            );
        } else {
            assert_eq!(
                after.config.primary_interface_selection,
                before.config.primary_interface_selection
            );
            let boot = after
                .config
                .desired_boot_interface
                .as_ref()
                .expect("retained boot");
            assert_eq!(
                (&boot.value, boot.version),
                (&original_boot.value, original_boot.version)
            );
            assert_eq!(
                after.config.boot_interface_selection,
                before.config.boot_interface_selection
            );
            assert!(
                !load_scout_pci_selection_state(&env.pool, host_id)
                    .await?
                    .controller_queued
            );
        }
    }
    Ok(())
}
