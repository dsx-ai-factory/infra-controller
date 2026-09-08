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

use common::api_fixtures::{
    FIXTURE_DHCP_RELAY_ADDRESS, TestEnv, create_managed_host, create_managed_host_with_config,
    create_test_env, dpu,
};
use model::test_support::ManagedHostConfig;
use rpc::forge::IpType;
use rpc::forge::forge_server::Forge;

use crate::test_support::fixture_config::FixtureDefault as _;
use crate::tests::common;

/// Test searching for an IP address. Tests all the cases in a single
/// test so that we only need to create and populate the DB once.
#[crate::sqlx_test]
async fn test_ip_finder(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let host_machine = mh.host().rpc_machine().await;

    let status = host_machine
        .status
        .as_ref()
        .expect("host machine must have a status message");
    assert!(
        !status.interfaces.is_empty(),
        "status.interfaces must be populated for the host machine"
    );
    #[allow(deprecated)]
    {
        assert_eq!(
            host_machine.interfaces, status.interfaces,
            "interfaces must equal status.interfaces"
        );
    }

    mh.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .keyset_ids(&["keyset1", "keyset2"])
        .build()
        .await;

    test_not_found(&env).await;
    test_inner(
        FIXTURE_DHCP_RELAY_ADDRESS,
        IpType::StaticDataDhcpServer,
        &env,
        "test_dhcp_server",
    )
    .await;
    test_inner(
        "172.20.0.10",
        IpType::ResourcePool,
        &env,
        "test_resource_pool",
    )
    .await;
    test_inner(
        "192.0.4.3",
        IpType::InstanceAddress,
        &env,
        "test_instance_address",
    )
    .await;
    test_inner(
        "192.0.2.4",
        IpType::MachineAddress,
        &env,
        "test_machine_address",
    )
    .await;

    // The managed host above uses the default `auto` bmc_ip_allocation, which
    // retains its auto-allocated BMC address as Static -- so the finder reports
    // it as StaticBmcIp rather than plain BmcIp.
    test_inner(
        host_machine.bmc_info.as_ref().unwrap().ip(),
        IpType::StaticBmcIp,
        &env,
        "test_static_bmc_ip",
    )
    .await;

    test_inner(
        "192.0.4.1",
        IpType::NetworkSegment,
        &env,
        "test_network_segment",
    )
    .await;

    // Loopback IP is assigned at random from pool, so we need to search for the correct one
    let mut txn = db_pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let loopback_ip = dpu::loopback_ip(&mut txn, &mh.dpu().id).await;
    test_inner(
        &loopback_ip.to_string(),
        IpType::LoopbackIp,
        &env,
        "test_loopback_ip",
    )
    .await;

    Ok(())
}

/// `FindIpAddress` returns every instance owner when isolated VPCs use the
/// same overlay address. Each match also names its VPC and segment so an
/// operator can tell the otherwise-identical allocations apart.
#[crate::sqlx_test]
async fn test_ip_finder_returns_every_duplicate_overlay_address_owner(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let address = "10.20.30.40".parse().unwrap();

    let mut txn = db_pool.begin().await?;
    let owners = [
        common::overlay_address::seed_overlay_address_owner(
            txn.as_mut(),
            "finder-owner-a",
            address,
        )
        .await,
        common::overlay_address::seed_overlay_address_owner(
            txn.as_mut(),
            "finder-owner-b",
            address,
        )
        .await,
    ];
    txn.commit().await?;

    let response = env
        .api
        .find_ip_address(tonic::Request::new(rpc::forge::FindIpAddressRequest {
            ip: address.to_string(),
        }))
        .await?
        .into_inner();
    assert!(response.errors.is_empty(), "finder returned lookup errors");

    let instance_matches = response
        .matches
        .iter()
        .filter(|ip_match| ip_match.ip_type == IpType::InstanceAddress as i32)
        .collect::<Vec<_>>();
    assert_eq!(
        instance_matches.len(),
        owners.len(),
        "finder should return one match for each overlay address owner"
    );

    for owner in owners {
        let instance_id = owner.instance_id.to_string();
        let ip_match = instance_matches
            .iter()
            .find(|ip_match| ip_match.owner_id.as_deref() == Some(instance_id.as_str()))
            .unwrap_or_else(|| panic!("finder omitted instance {instance_id}"));
        assert!(
            ip_match.message.contains(&owner.vpc_id.to_string()),
            "finder match should identify VPC {}: {}",
            owner.vpc_id,
            ip_match.message,
        );
        assert!(
            ip_match.message.contains(&owner.segment_id.to_string()),
            "finder match should identify segment {}: {}",
            owner.segment_id,
            ip_match.message,
        );
    }

    Ok(())
}

async fn test_not_found(env: &TestEnv) {
    let req = rpc::forge::FindIpAddressRequest {
        ip: "10.0.0.1".to_string(),
    };
    let res = env.api.find_ip_address(tonic::Request::new(req)).await;
    assert!(
        matches!(res, Err(status) if status.code() == tonic::Code::NotFound),
        "test_not_found"
    );
}

async fn test_inner(ip: &str, ip_type: IpType, env: &TestEnv, caller: &str) {
    let req = rpc::forge::FindIpAddressRequest { ip: ip.to_string() };
    let res = env
        .api
        .find_ip_address(tonic::Request::new(req))
        .await
        .expect(caller)
        .into_inner();
    assert!(!res.matches.is_empty(), "{caller} not found");
    // In integration testing DHCP relay is in a network segment,
    // so we get multiple matches. Wouldn't happen in live.
    if let Some(ip_match) = res
        .matches
        .into_iter()
        .find(|ip_match| ip_match.ip_type == ip_type as i32)
    {
        match ip_type {
            IpType::MachineAddress => {
                assert!(
                    ip_match.message.contains("machine address"),
                    "{caller} used incorrect Data-interface wording: {}",
                    ip_match.message,
                );
                assert!(
                    !ip_match.message.contains("BMC"),
                    "{caller} described a Data interface as BMC: {}",
                    ip_match.message,
                );
            }
            IpType::StaticBmcIp => {
                assert!(
                    ip_match.message.contains("static BMC IP"),
                    "{caller} used incorrect BMC-interface wording: {}",
                    ip_match.message,
                );
            }
            _ => {}
        }
        return;
    }
    panic!("{caller} did not have correct IPType");
}

#[crate::sqlx_test]
async fn test_identify_uuid(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .keyset_ids(&["keyset1", "keyset2"])
        .build()
        .await;
    let res = mh.host().rpc_machine().await;
    let interface_id = &res.status.as_ref().unwrap().interfaces[0].id;

    // Network segment
    let req = rpc::forge::IdentifyUuidRequest {
        uuid: Some(segment_id.into()),
    };
    let res = env
        .api
        .identify_uuid(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.object_type, rpc::forge::UuidType::NetworkSegment as i32);

    // Instance
    let req = rpc::forge::IdentifyUuidRequest {
        uuid: Some(tinstance.id.into()),
    };
    let res = env
        .api
        .identify_uuid(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.object_type, rpc::forge::UuidType::Instance as i32);

    // Machine interface
    let req = rpc::forge::IdentifyUuidRequest {
        uuid: interface_id.map(|id| id.into()),
    };
    let res = env
        .api
        .identify_uuid(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        res.object_type,
        rpc::forge::UuidType::MachineInterface as i32
    );

    // VPC
    let mut txn = db_pool
        .clone()
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    let segment = db::network_segment::find_by_name(&mut txn, "TENANT")
        .await
        .unwrap();
    let req = rpc::forge::IdentifyUuidRequest {
        uuid: Some(segment.config.vpc_id.unwrap().into()),
    };
    let res = env
        .api
        .identify_uuid(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.object_type, rpc::forge::UuidType::Vpc as i32);

    // Domain
    let req = rpc::forge::IdentifyUuidRequest {
        uuid: Some(env.domain.into()),
    };
    let res = env
        .api
        .identify_uuid(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.object_type, rpc::forge::UuidType::Domain as i32);

    Ok(())
}

#[crate::sqlx_test]
async fn test_identify_mac(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let res = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![host_machine_id.into()],
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0);
    let interface_id = res.status.as_ref().unwrap().interfaces[0]
        .id
        .as_ref()
        .unwrap()
        .to_string();
    let mac_address = &res.status.as_ref().unwrap().interfaces[0].mac_address;

    let req = rpc::forge::IdentifyMacRequest {
        mac_address: mac_address.to_string(),
    };
    let res = env
        .api
        .identify_mac(tonic::Request::new(req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(res.primary_key, *interface_id);
    assert_eq!(
        res.object_type,
        rpc::forge::MacOwner::MachineInterface as i32
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_identify_serial(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let config = ManagedHostConfig::default();
    let dpu_config = config.get_and_assert_single_dpu().clone();
    let mh = create_managed_host_with_config(&env, config).await;
    let host_machine_id = mh.host().id;
    let dpu_machine_id = mh.dpu().id;

    let res = mh.dpu().rpc_machine().await;
    assert_eq!(
        res.status
            .as_ref()
            .unwrap()
            .discovery_info
            .as_ref()
            .unwrap()
            .dmi_data
            .as_ref()
            .unwrap()
            .product_serial,
        dpu_config.serial
    );

    // Host, exact match, success
    {
        let req = rpc::forge::IdentifySerialRequest {
            // src/model/hardware_info/test_data/x86_info.json
            serial_number: "HostBoard123".to_string(),
            exact: true,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            res.machine_id.unwrap().to_string(),
            host_machine_id.to_string()
        );
    }

    // Host, exact match, failure
    {
        let req = rpc::forge::IdentifySerialRequest {
            // src/model/hardware_info/test_data/x86_info.json
            serial_number: "tBoard123".to_string(),
            exact: true,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert!(res.machine_id.is_none());
    }

    // Host, fuzzy match
    {
        let req = rpc::forge::IdentifySerialRequest {
            // src/model/hardware_info/test_data/x86_info.json
            serial_number: "tBoard123".to_string(),
            exact: false,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            res.machine_id.unwrap().to_string(),
            host_machine_id.to_string()
        );
    }

    // DPU, exact match, success
    {
        let req = rpc::forge::IdentifySerialRequest {
            serial_number: dpu_config.serial.clone(),
            exact: true,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            res.machine_id.unwrap().to_string(),
            dpu_machine_id.to_string()
        );
    }

    // DPU, exact match, failure
    {
        let req = rpc::forge::IdentifySerialRequest {
            // Lop off the first 4 characters
            serial_number: dpu_config.serial.replace(&dpu_config.serial[0..4], ""),
            exact: true,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert!(res.machine_id.is_none());
    }

    // DPU, fuzzy match
    {
        let req = rpc::forge::IdentifySerialRequest {
            // Lop off the first 4 characters
            serial_number: dpu_config.serial.replace(&dpu_config.serial[0..4], ""),
            exact: false,
        };
        let res = env
            .api
            .identify_serial(tonic::Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            res.machine_id.unwrap().to_string(),
            dpu_machine_id.to_string()
        );
    }

    Ok(())
}

/// `FindIpAddress` returns `IpTypeStaticBmcIp` when the address is a static/operator BMC
/// allocation.
#[crate::sqlx_test]
async fn test_static_bmc_ip_finder(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    use std::net::IpAddr;

    let env = create_test_env(db_pool.clone()).await;

    let static_ip: IpAddr = "10.178.160.100".parse().unwrap();
    let bmc_mac = "AA:BB:CC:DD:EE:99".parse().unwrap();

    let mut txn = db_pool.begin().await.unwrap();
    db::machine_interface::preallocate_bmc_machine_interface(
        txn.as_mut(),
        bmc_mac,
        static_ip,
        None,
    )
    .await
    .expect("preallocate static BMC interface");
    txn.commit().await.unwrap();

    // Query the IP via finder
    let req = rpc::forge::FindIpAddressRequest {
        ip: "10.178.160.100".to_string(),
    };
    let res = env
        .api
        .find_ip_address(tonic::Request::new(req))
        .await
        .expect("find_ip_address should succeed")
        .into_inner();

    assert!(!res.matches.is_empty(), "Should find at least one match");

    let static_bmc_match = res
        .matches
        .into_iter()
        .find(|ip_match| ip_match.ip_type == IpType::StaticBmcIp as i32)
        .expect("static BMC IP should be classified as IpTypeStaticBmcIp");
    assert!(
        static_bmc_match.message.contains("static BMC IP"),
        "static BMC wording should match the interface type: {}",
        static_bmc_match.message,
    );

    Ok(())
}

/// A static NVOS/Data address remains a MachineAddress even when it lives on the synthetic
/// `static-assignments` segment. When the interface is associated with a switch, finder output
/// identifies that switch.
#[crate::sqlx_test]
async fn test_static_data_ip_finder(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    use std::net::IpAddr;

    use carbide_uuid::switch::SwitchId;
    use model::machine_interface_address::MachineInterfaceAssociation;
    use model::switch::{NewSwitch, SwitchConfig};

    let env = create_test_env(db_pool.clone()).await;
    let static_ip: IpAddr = "10.86.241.49".parse().unwrap();
    let nvos_mac = "AA:BB:CC:DD:EE:98".parse().unwrap();
    let switch_id = SwitchId::from(uuid::Uuid::new_v4());

    let mut txn = db_pool.begin().await.unwrap();
    db::machine_interface::preallocate_machine_interface(txn.as_mut(), nvos_mac, static_ip, None)
        .await
        .expect("preallocate static NVOS Data interface");
    let interface = db::machine_interface::find_by_mac_address(txn.as_mut(), nvos_mac)
        .await
        .expect("find static NVOS Data interface")
        .into_iter()
        .next()
        .expect("static NVOS Data interface should exist");
    db::switch::create(
        txn.as_mut(),
        &NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: "gb-nvl-136-switch02".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: None,
            tray_index: None,
        },
    )
    .await
    .expect("create associated switch");
    db::machine_interface::associate_interface_with_machine(
        &interface.id,
        MachineInterfaceAssociation::Switch(switch_id),
        txn.as_mut(),
    )
    .await
    .expect("associate NVOS Data interface with switch");
    txn.commit().await.unwrap();

    let req = rpc::forge::FindIpAddressRequest {
        ip: static_ip.to_string(),
    };
    let res = env
        .api
        .find_ip_address(tonic::Request::new(req))
        .await
        .expect("find_ip_address should succeed")
        .into_inner();

    assert!(
        res.matches
            .iter()
            .all(|ip_match| ip_match.ip_type != IpType::StaticBmcIp as i32),
        "static NVOS Data IP must never be classified as StaticBmcIp: {:?}",
        res.matches,
    );
    let machine_address_match = res
        .matches
        .into_iter()
        .find(|ip_match| ip_match.ip_type == IpType::MachineAddress as i32)
        .expect("static NVOS Data IP should remain a MachineAddress");
    assert!(
        machine_address_match.owner_id.is_none(),
        "static NVOS Data address must not expose a switch ID as a machine owner",
    );
    assert!(
        machine_address_match.message.contains("machine address")
            && machine_address_match
                .message
                .contains(&switch_id.to_string())
            && !machine_address_match.message.contains("BMC"),
        "static NVOS Data wording should identify the switch without calling it BMC: {}",
        machine_address_match.message,
    );

    Ok(())
}
