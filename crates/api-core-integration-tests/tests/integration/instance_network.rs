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

use std::collections::HashSet;
use std::net::Ipv4Addr;

use carbide_api_core::test_support::default_config;
use carbide_api_core::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcPrefixId;
use db::ObjectColumnFilter;
use db::instance_address::UsedOverlayNetworkIpResolver;
use db::ip_allocator::UsedIpResolver;
use db::network_segment::IdColumn;
use ipnetwork::{IpNetwork, Ipv4Network};
use itertools::Itertools;
use model::instance::config::network::{DeviceLocator, InstanceNetworkConfig};
use model::metadata::Metadata;
use model::network_segment::NetworkSegmentSearchConfig;
use model::test_support::ManagedHostConfig;
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig, VpcCreationRequest};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::IntoRequest;

use super::instance_common::{
    InstanceEnv, create_fixture_tenant, create_tenant_overlay_prefix_with_prefix,
    default_os_config, default_tenant_config, fnn_config, host_id,
};

#[sqlx_test]
// This test verifies parity between `addresses` and the compatibility fields.
#[allow(deprecated)]
async fn test_instance_address_creation(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let (segment_id_1, segment_id_2) = {
        let ids = env.tenant_segments(2).await;
        (ids[0], ids[1])
    };
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    let mut txn = env.harness.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_1)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_2)
            .await
            .unwrap(),
        0
    );
    txn.commit().await.unwrap();

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

    let tinstance = mh
        .instance_builder(&env.harness)
        .network(network)
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    let mut txn = env.harness.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_2)
            .await
            .unwrap(),
        1
    );

    // TODO(chet): This will be where I also drop prefix allocation testing!

    // Check the allocated IP for the PF/primary interface.
    let allocated_ip_resolver = UsedOverlayNetworkIpResolver {
        segment_id: segment_id_1,
        busy_ips: vec![],
    };
    let used_ips = allocated_ip_resolver.used_ips(txn.as_mut()).await.unwrap();
    let used_prefixes = allocated_ip_resolver
        .used_prefixes(txn.as_mut())
        .await
        .unwrap();
    assert_eq!(1, used_ips.len());
    assert_eq!(1, used_prefixes.len());
    assert_eq!("192.0.4.3", used_ips[0].to_string());
    assert_eq!("192.0.4.3/32", used_prefixes[0].to_string());

    // Check the allocated VF.
    let allocated_ip_resolver = UsedOverlayNetworkIpResolver {
        segment_id: segment_id_2,
        busy_ips: vec![],
    };
    let used_ips = allocated_ip_resolver.used_ips(txn.as_mut()).await.unwrap();
    let used_prefixes = allocated_ip_resolver
        .used_prefixes(txn.as_mut())
        .await
        .unwrap();
    assert_eq!(1, used_ips.len());
    assert_eq!(1, used_prefixes.len());
    assert_eq!("192.1.4.3", used_ips[0].to_string());
    assert_eq!("192.1.4.3/32", used_prefixes[0].to_string());

    txn.commit().await.unwrap();

    // The addresses should show up in the internal config - which is sent to the DPU
    let network_config = env
        .harness
        .api()
        .get_managed_host_network_config(tonic::Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(mh.first_dpu().id),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!network_config.use_admin_network);
    assert_eq!(network_config.tenant_interfaces.len(), 2);
    assert_eq!(
        network_config.tenant_interfaces[0].ip.as_deref(),
        Some("192.0.4.3")
    );
    assert_eq!(
        network_config.tenant_interfaces[1].ip.as_deref(),
        Some("192.1.4.3")
    );
    for interface in &network_config.tenant_interfaces {
        assert_eq!(
            interface.addresses,
            vec![rpc::forge::InterfaceAddressConfig {
                address_family: rpc::forge::AddressFamily::V4.into(),
                ip: interface.ip.clone().unwrap(),
                interface_prefix: interface.interface_prefix.clone().unwrap(),
                prefix: interface.prefix.clone().unwrap(),
                gateway: interface.gateway.clone(),
                svi_ip: interface.svi_ip.clone(),
                tenant_vrf_loopback_ip: None,
            }]
        );
    }
    assert_eq!(network_config.dpu_network_pinger_type, None);
    // Ensure the VPC prefixes (which in this case are the two network segment
    // IDs referenced above) are both associated with both interfaces.
    let expected_vpc_prefixes = vec!["192.0.4.0/24".to_string(), "192.1.4.0/24".to_string()];
    assert_eq!(
        network_config.tenant_interfaces[0].vpc_prefixes,
        expected_vpc_prefixes
    );
    assert_eq!(
        network_config.tenant_interfaces[1].vpc_prefixes,
        expected_vpc_prefixes
    );
}

#[sqlx_test]
async fn test_allocate_instance_with_old_network_segemnt(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let vpc_id = db::vpc::find_by_segment(&env.harness.api().database_connection, segment_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    let txn = env
        .harness
        .api()
        .database_connection
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    txn.commit().await.unwrap();

    let instance_metadata = rpc::forge::Metadata {
        name: "test_instance_with_labels".to_string(),
        description: "this instance does not have labels.".to_string(),
        labels: vec![],
    };

    let device_locator = DeviceLocator {
        device: "DPU1".to_string(),
        device_instance: 0,
    };
    let mut nw_config =
        interface_network_config_with_devices(&[segment_id], std::slice::from_ref(&device_locator));
    for interface in &mut nw_config.interfaces {
        interface.network_details = None;
    }

    let tinstance = mh
        .instance_builder(&env.harness)
        .network(nw_config)
        .metadata(instance_metadata.clone())
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    // Test searching based on instance id.
    let mut instance_matched_by_id = tinstance.rpc_instance().await;

    instance_matched_by_id.metadata = instance_matched_by_id.metadata.take().map(|mut metadata| {
        metadata.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
        metadata
    });

    assert_eq!(
        instance_matched_by_id.metadata,
        Some(instance_metadata.clone())
    );

    let mut txn = env.harness.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(fetched_instance.machine_id, host_id(&mh).into());

    let network_config = fetched_instance.config.network;
    assert_eq!(fetched_instance.network_config_version.version_nr(), 1);
    let mut network_config_no_addresses = network_config;
    for iface in network_config_no_addresses.interfaces.iter_mut() {
        assert_eq!(iface.ip_addrs.len(), 1);
        assert_eq!(iface.interface_prefixes.len(), 1);
        iface.ip_addrs.clear();
        iface.interface_prefixes.clear();
        iface.network_segment_gateways.clear();
        iface.internal_uuid = uuid::Uuid::nil();
    }

    assert_eq!(
        network_config_no_addresses,
        InstanceNetworkConfig::for_segment_ids(&[segment_id], &[device_locator], &[vpc_id])
    );
    txn.rollback().await.unwrap();
}

#[sqlx_test]
async fn test_allocate_instance_with_multiple_fnn_vpc_prefixes(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, fnn_config()).await;
    let mh = env
        .create_managed_host(ManagedHostConfig::default().with_dpu_count(2))
        .await;

    // Register the tenant required by both FNN VPCs used in this allocation scenario.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

    // Create two FNN VPCs and prefixes to exercise cross-VPC allocation.
    let first_vpc = env
        .harness
        .api()
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "fnn vpc 1".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let second_vpc = env
        .harness
        .api()
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "fnn vpc 2".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let first_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        first_vpc,
        "fnn vpc prefix 1",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 5, 224), 27).unwrap()),
    )
    .await;
    let second_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        second_vpc,
        "fnn vpc prefix 2",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 6, 224), 27).unwrap()),
    )
    .await;

    // Allocate an instance whose interfaces draw from both VPC prefixes.
    let instance = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_id(&mh))
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(dual_physical_network_config_with_vpc_prefixes(
                            first_prefix_id,
                            second_prefix_id,
                        )),
                )
                .metadata(rpc::Metadata {
                    name: "multi-fnn-vpc".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("FNN instance allocation across multiple VPCs should succeed")
        .into_inner();

    // Verify the response resolved both interfaces that selected prefixes.
    let interfaces = &instance
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces;
    assert_eq!(interfaces.len(), 2);
    assert!(
        interfaces
            .iter()
            .all(|iface| iface.network_segment_id.is_some())
    );

    // Fetch through the API to verify the persisted config, not just the create response.
    let mut persisted_instances = env
        .harness
        .api()
        .find_instances_by_ids(
            rpc::forge::InstancesByIdsRequest {
                instance_ids: vec![instance.id.unwrap()],
            }
            .into_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .instances;
    assert_eq!(persisted_instances.len(), 1);
    let persisted = persisted_instances.remove(0);
    let persisted_interfaces = &persisted
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces;
    let segment_ids = persisted_interfaces
        .iter()
        .map(|iface| iface.network_segment_id.unwrap())
        .collect_vec();
    assert_eq!(segment_ids.iter().copied().collect::<HashSet<_>>().len(), 2);

    // Verify the allocated segments are attached to both requested FNN VPCs.
    let mut txn = env.harness.db_txn().await;
    let segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::List(IdColumn, &segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.config.vpc_id.unwrap())
            .collect::<HashSet<_>>(),
        HashSet::from([first_vpc, second_vpc])
    );
    txn.rollback().await.unwrap();
}

fn dual_physical_network_config_with_vpc_prefixes(
    first_prefix_id: VpcPrefixId,
    second_prefix_id: VpcPrefixId,
) -> rpc::InstanceNetworkConfig {
    // Put each PF on a distinct BlueField device instance so validation treats
    // them as separate physical interfaces.
    let interfaces = [first_prefix_id, second_prefix_id]
        .into_iter()
        .enumerate()
        .map(
            |(device_instance, vpc_prefix_id)| rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: None,
                network_details: Some(
                    rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                        vpc_prefix_id,
                    ),
                ),
                device: Some("BlueField SoC".to_string()),
                device_instance: device_instance as u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
        )
        .collect();

    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

fn interface_network_config_with_devices(
    segment_ids: &[NetworkSegmentId],
    device_locators: &[DeviceLocator],
) -> rpc::InstanceNetworkConfig {
    let interfaces = device_locators
        .iter()
        .zip(segment_ids)
        .map(|(dl, segment_id)| rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(*segment_id),
            network_details: Some(NetworkDetails::SegmentId(*segment_id)),
            device: Some(dl.device.clone()),
            device_instance: dl.device_instance as u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        })
        .collect();
    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}
