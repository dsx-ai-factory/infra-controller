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

use carbide_api_core::test_support::metadata;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use model::test_support::ManagedHostConfig;
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig, ManagedHostQuarantineMode};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

struct AllocationFixture {
    harness: TestHarness,
    mh: TestManagedHost,
    segment_id: NetworkSegmentId,
}

impl AllocationFixture {
    async fn new(pool: PgPool) -> Self {
        let harness = TestHarness::builder(pool)
            .with_resource_pools(
                ResourcePoolBuilder::default()
                    .with_vlan_ids(1, 5)
                    .with_vnis(10_001, 10_005)
                    .build(),
            )
            .build()
            .await;
        let domain = harness.create_test_domain("dwrt1.com").await;
        let network_controller = harness.network_controller();
        let admin_segment = network_controller.create_admin_segment(&domain).await;
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        let vpc_id = network_controller.create_vpc("test vpc 1").await;
        let segment_id = network_controller
            .create_tenant_segment(&domain, vpc_id)
            .await
            .id;
        let site_explorer = harness.default_test_site_explorer();
        let mut mh = harness
            .managed_host_builder(&site_explorer, underlay_segment)
            .with_config(ManagedHostConfig::default())
            .with_dpu_network_status_reported()
            .build()
            .await
            .0;
        mh.host.discover_primary_iface(admin_segment).await;
        mh.advance_to_converged_ready().await;
        Self {
            harness,
            mh,
            segment_id,
        }
    }
}

fn default_os_config() -> rpc::forge::InstanceOperatingSystemConfig {
    rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe".to_string(),
            },
        )),
    }
}

fn default_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: "Tenant1".to_string(),
        tenant_keyset_ids: vec![],
        hostname: None,
    }
}

fn single_interface_network_config(segment_id: NetworkSegmentId) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: Some(NetworkDetails::SegmentId(segment_id)),
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

#[sqlx_test]
async fn test_allocate_instance_with_invalid_metadata(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;
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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;
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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;
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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;
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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;

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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;

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
    let AllocationFixture {
        harness: env,
        mh,
        segment_id,
    } = AllocationFixture::new(pool).await;
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
