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

use carbide_authn::middleware::ConnectionAttributes;
use carbide_uuid::machine::{MachineInterfaceId, StableHostMachineId};
use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::dpu::create_dpu_machine;
use common::api_fixtures::host::{host_discover_dhcp, host_discover_machine_with_reporter};
use common::api_fixtures::{
    FIXTURE_DHCP_RELAY_ADDRESS, create_managed_host, create_managed_host_with_config,
    create_test_env,
};
use config_version::ConfigVersion;
use itertools::Itertools;
use mac_address::MacAddress;
use model::hardware_info::{HardwareInfo, PciDeviceProperties, TpmEkCertificate};
use model::machine::ManagedHostState;
use model::machine::machine_id::from_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_boot_interface::BootInterfaceSelectionSource;
use model::network_segment::NetworkSegmentType;
use model::resource_pool::{ResourcePoolDef, ResourcePoolType};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request};

use crate::test_support::builder::TestApiBuilder;
use crate::test_support::fixture_config::{FixtureDefault, ManagedHostConfigExt as _};
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::tpm_attestation::{
    AK_NAME_SERIALIZED, AK_PUB_SERIALIZED, EK_PUB_SERIALIZED,
};
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};
use crate::tests::common::postgres::wait_for_blocked_query;

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
    current_primary_address: Option<String>,
    network_config_version: ConfigVersion,
    controller_state: String,
    controller_queued: bool,
    desired_mac_address: Option<MacAddress>,
    desired_interface_id: Option<String>,
    desired_version: Option<ConfigVersion>,
    selection_source: Option<BootInterfaceSelectionSource>,
}

/// Loads only the primary, address, network, state, queue, and desired boot interface selection fields.
async fn load_scout_pci_selection_state(
    pool: &sqlx::PgPool,
    host_machine_id: StableHostMachineId,
) -> Result<ScoutPciSelectionState, Box<dyn std::error::Error>> {
    let state = sqlx::query_as::<_, ScoutPciSelectionState>(
        "SELECT (
                    SELECT mac_address
                    FROM machine_interfaces
                    WHERE machine_id = machine.id
                      AND primary_interface
                ) AS current_primary_mac_address,
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

#[crate::sqlx_test]
async fn test_secure_discovery_promotes_predicted_host_by_remote_ip(
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

    assert_eq!(response.machine_id, Some(expected_machine_id));
    assert_eq!(response.machine_interface_id, Some(host_interface_id));
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
    let mut expected = before;
    expected.selection_source = Some(BootInterfaceSelectionSource::ScoutReportPci);
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
