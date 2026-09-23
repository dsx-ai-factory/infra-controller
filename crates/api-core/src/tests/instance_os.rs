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

use carbide_uuid::operating_system::OperatingSystemId;
use common::api_fixtures::instance::{default_tenant_config, single_interface_network_config};
use common::api_fixtures::{create_managed_host, create_test_env};
use config_version::ConfigVersion;
use rpc::forge::forge_server::Forge;
use rpc::forge::{IpxeTemplateArtifact, IpxeTemplateArtifactCacheStrategy, IpxeTemplateParameter};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::tests::common;

#[crate::sqlx_test]
async fn test_update_instance_operating_system(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let initial_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData1".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe1".to_string(),
            },
        )),
    };

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(initial_os.clone()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let os = instance.config().os();
    assert_eq!(os, &initial_os);
    let initial_config_version = instance.config_version();
    assert_eq!(initial_config_version.version_nr(), 1);

    let updated_os_1 = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: true,
        run_provisioning_instructions_on_every_boot: true,
        user_data: Some("SomeRandomData2".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe2".to_string(),
            },
        )),
    };

    let instance = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                os: Some(updated_os_1.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let os = instance.config.as_ref().unwrap().os.as_ref().unwrap();
    assert_eq!(os, &updated_os_1);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 2);

    let updated_os_2 = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData3".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe3".to_string(),
            },
        )),
    };

    // Start a conditional update first that specifies the wrong last version.
    // This should fail.
    let status = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: Some(initial_config_version.version_string()),
                os: Some(updated_os_2.clone()),
            },
        ))
        .await
        .expect_err("RPC call should fail with PreconditionFailed error");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        status.message(),
        format!(
            "an object of type instance was intended to be modified did not have the expected version {}",
            initial_config_version.version_string()
        ),
        "Message is {}",
        status.message()
    );

    // Using the correct current version should allow the update
    let instance = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: Some(updated_config_version.version_string()),
                os: Some(updated_os_2.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let os = instance.config.as_ref().unwrap().os.as_ref().unwrap();
    assert_eq!(os, &updated_os_2);
    let updated_config_version = instance.config_version.parse::<ConfigVersion>().unwrap();
    assert_eq!(updated_config_version.version_nr(), 3);

    // Try to update a non-existing instance
    let unknown_instance = uuid::Uuid::new_v4();
    let status = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(unknown_instance.into()),
                if_version_match: None,
                os: Some(updated_os_2.clone()),
            },
        ))
        .await
        .expect_err("RPC call should fail with NotFound error");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(
        status.message(),
        format!("instance not found: {unknown_instance}"),
        "Message is {}",
        status.message()
    );

    // Try to update to an invalid OS
    let invalid_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: true,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData2".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "".to_string(),
            },
        )),
    };

    let err = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                os: Some(invalid_os),
            },
        ))
        .await
        .expect_err("Invalid OS should not be accepted");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "invalid value: InlineIpxe::ipxe_script is empty"
    );
}

#[crate::sqlx_test]
async fn test_create_instance_with_ipxe_template_os(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let os_def = env
        .api
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "template-os".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: Some("iPXE template based OS".to_string()),
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: Some("os-level-userdata".to_string()),
                ipxe_script: None,
                ipxe_template_id: Some("ddbf83c0-a753-5fde-96c1-6b74e9c9db10".parse().unwrap()),
                ipxe_template_parameters: vec![rpc::forge::IpxeTemplateParameter {
                    name: "ipxe".to_string(),
                    value: "chain http://boot.example.com".to_string(),
                }],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        os_def.r#type,
        rpc::forge::OperatingSystemType::OsTypeTemplatedIpxe as i32
    );
    let os_id = os_def.id.unwrap();

    let instance_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("instance-userdata".to_string()),
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(os_id),
        ),
    };

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(instance_os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let os = instance.config().os();
    match &os.variant {
        Some(rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(id)) => {
            assert_eq!(*id, os_id);
        }
        other => panic!("expected OperatingSystemId variant, got {other:?}"),
    }
    assert_eq!(os.user_data.as_deref(), Some("instance-userdata"));

    // Verify that the boot flow renders the iPXE template correctly: fetch the PXE
    // instructions for the machine's host interface and assert the script contains
    // the parameter value provided via the raw-ipxe template.
    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();

    let pxe = host_interface
        .get_pxe_instructions(rpc::forge::MachineArchitecture::X86)
        .await;
    assert!(
        pxe.pxe_script.contains("chain http://boot.example.com"),
        "Expected rendered template to contain the iPXE parameter value, got: {}",
        pxe.pxe_script
    );
}

/// Helper: creates an OS definition via the API and returns its OS id.
async fn create_os_definition(
    env: &crate::tests::common::api_fixtures::TestEnv,
    name: &str,
    is_active: bool,
) -> OperatingSystemId {
    env.api
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: name.to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: None,
                is_active,
                allow_override: false,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ddbf83c0-a753-5fde-96c1-6b74e9c9db10".parse().unwrap()),
                ipxe_template_parameters: vec![IpxeTemplateParameter {
                    name: "ipxe".to_string(),
                    value: "chain http://boot.example.com".to_string(),
                }],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap()
}

#[crate::sqlx_test]
async fn test_allocate_instance_rejects_inactive_os(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let os_id = create_os_definition(&env, "inactive-os", false).await;

    let result = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(rpc::forge::InstanceOperatingSystemConfig {
                    phone_home_enabled: false,
                    run_provisioning_instructions_on_every_boot: false,
                    user_data: None,
                    variant: Some(
                        rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(
                            os_id,
                        ),
                    ),
                }),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-inactive-os".to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await;

    let err = result.expect_err("allocating with inactive OS should fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("is not active"),
        "Expected 'is not active', got: {}",
        err.message()
    );
}

#[crate::sqlx_test]
async fn test_allocate_instance_rejects_not_ready_os(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // CachedOnly artifact without cached_url → OS status is PROVISIONING
    let os = env
        .api
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "not-ready-os".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: None,
                is_active: true,
                allow_override: false,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ddbf83c0-a753-5fde-96c1-6b74e9c9db10".parse().unwrap()),
                ipxe_template_parameters: vec![IpxeTemplateParameter {
                    name: "ipxe".to_string(),
                    value: "chain http://boot.example.com".to_string(),
                }],
                ipxe_template_artifacts: vec![IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "https://example.com/kernel".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: rpc::forge::IpxeTemplateArtifactCacheStrategy::CachedOnly
                        as i32,
                    cached_url: None,
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let os_id = os.id.unwrap();
    assert_eq!(os.status, rpc::forge::TenantState::Provisioning as i32);

    let result = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(rpc::forge::InstanceOperatingSystemConfig {
                    phone_home_enabled: false,
                    run_provisioning_instructions_on_every_boot: false,
                    user_data: None,
                    variant: Some(
                        rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(
                            os_id,
                        ),
                    ),
                }),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-not-ready-os".to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await;

    let err = result.expect_err("allocating with not-ready OS should fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("is not ready"),
        "Expected 'is not ready', got: {}",
        err.message()
    );
}

#[crate::sqlx_test]
async fn test_update_instance_os_rejects_inactive_os(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create an instance with an inline iPXE OS first
    let initial_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: None,
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "chain http://example.com".to_string(),
            },
        )),
    };
    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(initial_os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    let tinstance = mh.instance_builer(&env).config(config).build().await;

    // Create an inactive OS definition
    let os_id = create_os_definition(&env, "inactive-os-for-update", false).await;

    let err = env
        .api
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: Some(tinstance.id),
                if_version_match: None,
                os: Some(rpc::forge::InstanceOperatingSystemConfig {
                    phone_home_enabled: false,
                    run_provisioning_instructions_on_every_boot: false,
                    user_data: None,
                    variant: Some(
                        rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(
                            os_id,
                        ),
                    ),
                }),
            },
        ))
        .await
        .expect_err("updating instance OS to inactive OS should fail");

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("is not active"),
        "Expected 'is not active', got: {}",
        err.message()
    );
}

#[crate::sqlx_test]
async fn test_create_instance_with_os_image_and_verify_pxe_rendering(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let os_image_id = uuid::Uuid::new_v4();
    let source_url = "https://images.example.com/ubuntu-22.04.qcow2";
    let digest = "sha256:abcdef1234567890";

    let image = env
        .api
        .create_os_image(tonic::Request::new(rpc::forge::OsImageAttributes {
            id: Some(rpc::Uuid::from(os_image_id)),
            source_url: source_url.to_string(),
            digest: digest.to_string(),
            tenant_organization_id: "test-org".to_string(),
            create_volume: false,
            name: Some("test-qcow-image".to_string()),
            description: Some("Test qcow2 OS image".to_string()),
            auth_type: Some("Bearer".to_string()),
            auth_token: Some("my-secret-token".to_string()),
            rootfs_id: Some("root-uuid-1234".to_string()),
            rootfs_label: None,
            boot_disk: None,
            capacity: Some(1024 * 1024 * 1024),
            bootfs_id: None,
            efifs_id: None,
        }))
        .await
        .unwrap()
        .into_inner();

    let actual_id =
        uuid::Uuid::try_from(image.attributes.as_ref().unwrap().id.clone().unwrap()).unwrap();
    assert_eq!(actual_id, os_image_id);

    let instance_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("os-image-userdata".to_string()),
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OsImageId(rpc::Uuid::from(
                os_image_id,
            )),
        ),
    };

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(instance_os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance = tinstance.rpc_instance().await;
    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();

    let pxe = host_interface
        .get_pxe_instructions(rpc::forge::MachineArchitecture::X86)
        .await;

    assert!(
        pxe.pxe_script.contains("qcow-imager.efi"),
        "Expected qcow-imager chain command, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains(source_url),
        "Expected image_url={source_url} in script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains(digest),
        "Expected image_sha={digest} in script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains("image_auth_token=my-secret-token"),
        "Expected auth_token in script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains("image_auth_type=Bearer"),
        "Expected auth_type in script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains("rootfs_uuid=root-uuid-1234"),
        "Expected rootfs_uuid in script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains("ds=nocloud-net;"),
        "Expected cloud-init data source when userdata is set, got: {}",
        pxe.pxe_script
    );
}

#[crate::sqlx_test]
async fn test_create_instance_with_raw_ipxe_os_and_verify_pxe_rendering(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let raw_script = "chain --autofree https://boot.netboot.xyz";
    let os_def = env
        .api
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "raw-ipxe-os".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: Some("raw iPXE OS for instance test".to_string()),
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: Some("os-level-userdata".to_string()),
                ipxe_script: Some(raw_script.to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        os_def.r#type,
        rpc::forge::OperatingSystemType::OsTypeIpxe as i32
    );
    let os_id = os_def.id.unwrap();

    let instance_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("instance-userdata".to_string()),
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(os_id),
        ),
    };

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(instance_os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance = tinstance.rpc_instance().await;
    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    match &instance.config().os().variant {
        Some(rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(id)) => {
            assert_eq!(*id, os_id);
        }
        other => panic!("expected OperatingSystemId variant, got {other:?}"),
    }

    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();

    let pxe = host_interface
        .get_pxe_instructions(rpc::forge::MachineArchitecture::X86)
        .await;
    assert!(
        pxe.pxe_script.contains(raw_script),
        "Expected raw iPXE script in PXE instructions, got: {}",
        pxe.pxe_script
    );
}

#[crate::sqlx_test]
async fn test_create_instance_with_templated_ipxe_os_with_artifacts_and_verify_pxe_rendering(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Use the qcow-image template (ea756ddd) which requires image_url and supports {{extra}}.
    // Add a CachedOnly artifact that must be resolved via cached_url during rendering.
    let os_def = env
        .api
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "templated-os-with-artifacts".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: Some("templated iPXE OS with artifacts".to_string()),
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: Some("os-level-userdata".to_string()),
                ipxe_script: None,
                ipxe_template_id: Some("ea756ddd-add3-5e42-a202-44bfc2d5aac2".parse().unwrap()),
                ipxe_template_parameters: vec![IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://images.example.com/my-os.qcow2".to_string(),
                }],
                ipxe_template_artifacts: vec![IpxeTemplateArtifact {
                    name: "firmware".to_string(),
                    url: "https://remote.example.com/firmware.bin".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                    cached_url: None,
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        os_def.r#type,
        rpc::forge::OperatingSystemType::OsTypeTemplatedIpxe as i32,
    );
    assert_eq!(
        os_def.status,
        rpc::forge::TenantState::Provisioning as i32,
        "OS with CachedOnly artifact and no cached_url must start as PROVISIONING"
    );
    let os_id = os_def.id.unwrap();

    // Set the cached_url for the CachedOnly artifact so the OS becomes READY.
    env.api
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "firmware".to_string(),
                    cached_url: Some("http://local-cache.site/firmware.bin".to_string()),
                }],
            },
        ))
        .await
        .unwrap();

    let fetched = env
        .api
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        fetched.status,
        rpc::forge::TenantState::Ready as i32,
        "OS should be READY after setting all CachedOnly cached_urls"
    );

    // Allocate an instance referencing this OS.
    let instance_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("instance-userdata".to_string()),
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OperatingSystemId(os_id),
        ),
    };

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(instance_os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance = tinstance.rpc_instance().await;
    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();

    let pxe = host_interface
        .get_pxe_instructions(rpc::forge::MachineArchitecture::X86)
        .await;

    assert!(
        pxe.pxe_script
            .contains("http://images.example.com/my-os.qcow2"),
        "Expected image_url parameter value in rendered script, got: {}",
        pxe.pxe_script
    );
    assert!(
        pxe.pxe_script.contains("qcow-imager.efi"),
        "Expected qcow-imager.efi chain from qcow-image template, got: {}",
        pxe.pxe_script
    );
}
