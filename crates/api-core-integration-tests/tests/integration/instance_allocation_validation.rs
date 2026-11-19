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

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use carbide_api_core::test_support::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, FIXTURE_TENANT_ORG_ID,
};
use carbide_api_core::test_support::{default_config, metadata};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::external_validation::ExternalValidationRequestId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use model::instance::config::network::DeviceLocator;
use model::machine::ManagedHostState;
use model::metadata::Metadata;
use model::test_support::ManagedHostConfig;
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::forge::{
    InstanceAllocationRequest, InstanceConfig, ManagedHostQuarantineMode, VpcCreationRequest,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::{IntoRequest, Request};

use super::instance_common::{
    InstanceEnv, InstanceFixture, create_tenant_overlay_prefix_with_prefix, default_os_config,
    default_tenant_config, fnn_config, single_interface_network_config,
};

#[sqlx_test]
async fn test_allocate_instance_with_invalid_metadata(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(true) {
        let tenant_config = default_tenant_config();
        let config = InstanceConfig::builder()
            .tenant(tenant_config)
            .os(default_os_config())
            .network(single_interface_network_config(segment_id))
            .rpc();

        let result = env
            .api()
            .allocate_instance(
                InstanceAllocationRequest::builder(false)
                    .machine_id(host_machine_id)
                    .config(config)
                    .metadata(invalid_metadata.clone())
                    .tonic_request(),
            )
            .await;

        let err = result.expect_err(&format!(
            "Invalid metadata of type should not be accepted: {invalid_metadata:?}"
        ));

        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains(&expected_err),
            "Testcase: {:?}\nMessage is \"{}\".\nMessage should contain: \"{}\"",
            invalid_metadata,
            err.message(),
            expected_err
        );
    }
}

#[sqlx_test]
async fn test_create_instance_with_provided_id(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let instance = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("Create instance failed.")
        .into_inner();

    assert_eq!(instance.id, Some(instance_id));

    let instance = env
        .api()
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance_id],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances;
    assert_eq!(instance.len(), 1);
    assert_eq!(instance[0].id, Some(instance_id));
}

#[sqlx_test]
async fn test_create_instance_duplicate_keyset_ids(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    let config = rpc::InstanceConfig {
        os: Some(default_os_config()),
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: "Tenant1".to_string(),
            tenant_keyset_ids: vec![
                "a".to_string(),
                "bad_id".to_string(),
                "c".to_string(),
                "bad_id".to_string(),
            ],
            hostname: Some("test-instance".to_string()),
        }),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let err = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("Duplicate TenantKeyset IDs should not be accepted");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "duplicate tenant KeySet ID found: bad_id");
}

#[sqlx_test]
async fn test_create_instance_keyset_ids_max(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    let config = rpc::InstanceConfig {
        os: Some(default_os_config()),
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: "Tenant1".to_string(),
            tenant_keyset_ids: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
                "f".to_string(),
                "g".to_string(),
                "h".to_string(),
                "i".to_string(),
                "j".to_string(),
                "k".to_string(),
            ],
            hostname: Some("test-hostname".to_string()),
        }),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let err = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("More than 10 TenantKeyset IDs should not be accepted");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "more than 10 tenant KeySet IDs are not allowed"
    );
}

#[sqlx_test]
async fn test_allocate_instance_with_invalid_os_image(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;

    // Use a non-existent OS image ID
    let invalid_os_image_id = uuid::Uuid::new_v4();

    let os_config = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: None,
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OsImageId(rpc::Uuid::from(
                invalid_os_image_id,
            )),
        ),
    };

    let result = env
        .api()
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.host.id.try_into().unwrap()),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                dhcp_option_set_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(os_config),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-invalid-os-image".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
            external_validation_request_id: None,
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("does not exist"),
        "Expected error about OS image not existing, got: {}",
        err.message()
    );

    Ok(())
}

/// Allocate instance with non-existent IB partition ID.
/// Expect: InvalidArgument error indicating partition is not created.
#[sqlx_test]
async fn test_allocate_instance_with_invalid_ib_partition(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;

    // Use a non-existent IB partition ID
    let invalid_partition_id = carbide_uuid::infiniband::IBPartitionId::new();

    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(invalid_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    };

    let result = env
        .api()
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.host.id.try_into().unwrap()),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                dhcp_option_set_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: Some(ib_config),
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-invalid-ib-partition".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
            external_validation_request_id: None,
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("IB partition") || err.message().contains("not created"),
        "Expected error about IB partition not existing, got: {}",
        err.message()
    );

    Ok(())
}

#[sqlx_test]
async fn test_can_not_create_instances_with_machine_in_quarantine(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id))
        .rpc();

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    env.api()
        .set_managed_host_quarantine_state(tonic::Request::new(
            rpc::forge::SetManagedHostQuarantineStateRequest {
                machine_id: Some(host_machine_id.into()),
                quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                    mode: ManagedHostQuarantineMode::BlockAllTraffic as i32,
                    reason: Some("test".to_string()),
                }),
            },
        ))
        .await
        .unwrap();

    let result = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config.clone())
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    // TODO: Do not leak the full database error to users
    let err = result.expect_err("Expect instance creation to fail");
    assert!(
        err.message()
            .contains("host is not available for allocation due to health probe alert")
    );
}

#[sqlx_test]
async fn test_instance_cannot_allocate_requested_ip_with_network_segment(
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
    let segment_id2 = env
        .create_tenant_segment(vpc_id, FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1], "TENANT")
        .await;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    let mut txn = env.harness.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        mh.host.db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    // Attempt to create an instance with a network segment and
    // an explicit IP request.
    let err = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(rpc::forge::InstanceOperatingSystemConfig {
                        phone_home_enabled: false,
                        run_provisioning_instructions_on_every_boot: false,
                        user_data: Some("SomeRandomData1".to_string()),
                        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
                            rpc::forge::InlineIpxe {
                                ipxe_script: "SomeRandomiPxe1".to_string(),
                            },
                        )),
                    }),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![rpc::InstanceInterfaceConfig {
                            ip_address: Some("192.168.0.1".to_string()),
                            ipv6_interface_config: None,
                            routing_profile: None,

                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: Some(
                                rpc::forge::instance_interface_config::NetworkDetails::SegmentId(
                                    segment_id2,
                                ),
                            ),
                            device: None,
                            device_instance: 0,
                            virtual_function_id: None,
                        }],
                        #[allow(deprecated)]
                        auto: false,
                        auto_config: None,
                    }),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                })
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("IP request with network segment should not be allowed");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("explicit IP requests are only supported for VPC prefixes")
    );
}

/// Verifies that non-FNN interfaces cannot span direct segments from multiple VPCs.
#[sqlx_test]
async fn test_allocate_instance_rejects_multiple_non_fnn_network_segments(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let mh = env
        .create_managed_host(ManagedHostConfig::default().with_dpu_count(2))
        .await;

    // Create two non-FNN tenant segments across two VPCs.
    let first_segment_id = env.segment_id;
    let peer_vpc_id = env
        .harness
        .api()
        .create_vpc(
            VpcCreationRequest::builder("e65a9d69-39d2-4872-a53e-e5cb87c84e75")
                .metadata(Metadata {
                    name: "test peer vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(
                    rpc::forge::VpcVirtualizationType::EthernetVirtualizer as i32,
                )
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let second_segment_id = env
        .create_tenant_segment(
            peer_vpc_id,
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
            "TENANT2",
        )
        .await;

    // Non-FNN segments must still reject cross-VPC interface configs.
    let err = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(interface_network_config_with_devices(
                            &[first_segment_id, second_segment_id],
                            &[
                                DeviceLocator {
                                    device: "BlueField SoC".to_string(),
                                    device_instance: 0,
                                },
                                DeviceLocator {
                                    device: "BlueField SoC".to_string(),
                                    device_instance: 1,
                                },
                            ],
                        )),
                )
                .tonic_request(),
        )
        .await
        .expect_err("non-FNN cross-VPC segment allocation should fail");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("found segments attached to multiple VPCs")
    );
}

/// Verifies that dual-stack prefixes on one interface cannot cross VPC boundaries.
#[sqlx_test]
async fn test_allocate_instance_rejects_dual_stack_prefixes_from_different_vpcs(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let config = fnn_config();
    let env = InstanceEnv::new(pool, config).await;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    // Register the tenant required by both FNN VPCs used in this validation scenario.
    env.harness
        .api()
        .create_tenant(
            rpc::forge::CreateTenantRequest {
                organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
                routing_profile_type: None,
                metadata: Some(rpc::Metadata {
                    name: FIXTURE_TENANT_ORG_ID.to_string(),
                    ..Default::default()
                }),
            }
            .into_request(),
        )
        .await
        .unwrap();

    // Create two FNN VPCs so the global multi-FNN check passes.
    let first_vpc = env
        .harness
        .api()
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "dual-stack fnn vpc 1".to_string(),
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
                    name: "dual-stack fnn vpc 2".to_string(),
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

    // Put the IPv4 and IPv6 prefixes in different VPCs on the same interface.
    let primary_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        first_vpc,
        "dual-stack primary prefix",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 9, 224), 27).unwrap()),
    )
    .await;
    let ipv6_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        second_vpc,
        "dual-stack ipv6 prefix",
        IpNetwork::V6(Ipv6Network::new(Ipv6Addr::from_str("fd00:217:9::").unwrap(), 120).unwrap()),
    )
    .await;

    // Reject creating a single dual-stack segment that crosses VPC boundaries.
    let err = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(rpc::InstanceNetworkConfig {
                            interfaces: vec![rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: Some(
                                rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                                    primary_prefix_id,
                                ),
                            ),
                            device: Some("BlueField SoC".to_string()),
                            device_instance: 0,
                            virtual_function_id: None,
                            ip_address: None,
                            ipv6_interface_config: Some(rpc::forge::InstanceInterfaceIpv6Config {
                                vpc_prefix_id: Some(ipv6_prefix_id),
                                ip_address: None,
                            }),
                            routing_profile: None,
                        }],
                            #[allow(deprecated)]
                            auto: false,
                            auto_config: None,
                        }),
                )
                .tonic_request(),
        )
        .await
        .expect_err("dual-stack prefixes from different VPCs should fail");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("dual-stack VPC prefixes must belong to the same VPC")
    );
}

/// Unsupported bindings must fail before legacy hydration acquires a connection.
#[sqlx_test]
async fn test_external_validation_binding_rejected_before_hydration(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = TestHarness::builder(pool.clone()).build().await;
    // Hydration must not acquire a connection for an unsupported binding.
    pool.close().await;

    #[allow(deprecated)]
    let legacy_request = rpc::InstanceAllocationRequest {
        machine_id: Some(
            "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                .parse()
                .unwrap(),
        ),
        config: Some(rpc::InstanceConfig {
            network: Some(rpc::forge::InstanceNetworkConfig {
                auto: true,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bound_request = rpc::InstanceAllocationRequest {
        external_validation_request_id: Some(ExternalValidationRequestId::new()),
        ..legacy_request.clone()
    };

    for (scenario, batch) in [
        ("single legacy allocation", false),
        ("batch with an earlier unbound legacy allocation", true),
    ] {
        let error = if batch {
            env.api()
                .allocate_instances(Request::new(rpc::forge::BatchInstanceAllocationRequest {
                    instance_requests: vec![legacy_request.clone(), bound_request.clone()],
                }))
                .await
                .unwrap_err()
        } else {
            env.api()
                .allocate_instance(Request::new(bound_request.clone()))
                .await
                .unwrap_err()
        };
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert!(
            error.message().contains("external validation allocation"),
            "{scenario}"
        );
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
