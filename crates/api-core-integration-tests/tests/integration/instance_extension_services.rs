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

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use itertools::Itertools;
use model::test_support::ManagedHostConfig;
use rpc::forge::DpuExtensionService;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::Request;

use super::instance_common::{
    InstanceEnv, default_os_config, default_tenant_config, host_id, single_interface_network_config,
};

/// Verifies allocation assigns and persists a non-nil UUIDv4 attachment ID so
/// later service-interface records can reference it without inventing identity
/// during reads or retries.
#[sqlx_test]
async fn test_allocate_instance_with_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    let _ = env
        .harness
        .api()
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Keep the service nonnetworked so allocation-time attachment identity is
    // the only new persisted behavior under test.
    let service = env
        .harness
        .api()
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id.clone(),
                version: service
                    .latest_version_info
                    .as_ref()
                    .unwrap()
                    .version
                    .clone(),
                service_vpc_ids: vec![],
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builder(&env.harness)
        .config(config.clone())
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    // Reload persisted state to prove allocation assigned a durable, non-nil UUIDv4.
    let mut txn = env.harness.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance_snapshot = snapshot.instance.unwrap();

    assert_eq!(
        instance_snapshot
            .config
            .extension_services
            .service_configs
            .len(),
        1
    );
    let attachment = &instance_snapshot.config.extension_services.service_configs[0];
    assert_eq!(attachment.service_id, service.service_id.parse().unwrap());
    let attachment_id = attachment.id.expect("new attachment has an ID");
    assert_ne!(attachment_id, uuid::Uuid::nil());
    assert_eq!(attachment_id.get_version(), Some(uuid::Version::Random));
    txn.rollback().await?;

    // A fresh public read proves the same durable identity reaches attachment status.
    let instance = tinstance.rpc_instance().await;
    let status = instance
        .status
        .expect("allocated instance has status")
        .dpu_extension_services
        .expect("allocated instance has extension-service status");
    assert_eq!(status.dpu_extension_services.len(), 1);
    assert_eq!(
        status.dpu_extension_services[0].attachment_id,
        Some(attachment_id.into())
    );

    Ok(())
}

#[sqlx_test]
async fn test_allocate_instance_with_extension_services_rejected_on_dpf_host(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let (service, _, _) = create_dpu_extension_services(&env).await?;

    let mut txn = env.harness.api().database_connection.begin().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &host_id(&mh)).await?;
    txn.commit().await?;

    let result = env
        .harness
        .api()
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(host_id(&mh)),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service.latest_version_info.unwrap().version,
                        service_vpc_ids: vec![],
                    }],
                }),
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::Metadata {
                name: "dpf-extension-service".to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
            external_validation_request_id: None,
        }))
        .await
        .expect_err("extension services must be rejected on a DPF-managed host");

    assert_eq!(result.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        result.message(),
        format!(
            "DPU extension services are not supported on DPF-managed host {}",
            host_id(&mh)
        )
    );

    Ok(())
}

#[sqlx_test]
async fn test_allocate_instance_with_duplicate_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    // Create extension services
    let (service1, _, _) = create_dpu_extension_services(&env).await.unwrap();

    let instance = env
        .harness
        .api()
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(host_id(&mh)),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                dhcp_option_set_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![
                        rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service1.service_id.clone(),
                            version: service1
                                .latest_version_info
                                .as_ref()
                                .unwrap()
                                .version
                                .clone(),
                            service_vpc_ids: vec![],
                        },
                        rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service1.service_id.clone(),
                            version: service1
                                .latest_version_info
                                .as_ref()
                                .unwrap()
                                .version
                                .clone(),
                            service_vpc_ids: vec![],
                        },
                    ],
                }),
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance".to_string(),
                description: "desc".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
            external_validation_request_id: None,
        }))
        .await;
    println!("instance: {:?}", instance);
    assert!(instance.is_err());
    let err = instance.unwrap_err();
    assert!(err.message().starts_with(
        "duplicate extension services in configuration. only one version of each service is allowed"
    ));

    Ok(())
}

/// Verifies extension-service updates reject duplicate service IDs, preserve terminating history,
/// permit endpoint-free same-version reattachment, and distinguish missing, deleted, and invalid
/// registrations so one service cannot map ambiguously to multiple attachments.
#[sqlx_test]
async fn test_update_instance_with_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    // Create extension services
    let (service1, service2, service3) = create_dpu_extension_services(&env).await.unwrap();

    let service1_version2 = service1.active_versions[0].clone();
    let service1_version1 = service1.active_versions[1].clone();
    // Derive an unregistered successor to service1 V2 so version validation exercises absence.
    let missing_service1_version = service1_version2
        .parse::<ConfigVersion>()?
        .increment()
        .to_string();
    let service2_version = service2
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();
    let service3_version = service3
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: service1_version1.clone(),
                service_vpc_ids: vec![],
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builder(&env.harness)
        .config(config.clone())
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    let instance = tinstance.rpc_instance().await;
    assert!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state
            == rpc::forge::TenantState::Ready as i32
    );

    let instance_id = tinstance.id;

    // Submit both registered versions before any attachment terminates so lifecycle validation
    // cannot obscure the request-local duplicate-service invariant.
    let duplicate_error = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    nvlink: None,
                    spxconfig: None,
                    dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![
                            rpc::forge::InstanceDpuExtensionServiceConfig {
                                service_id: service1.service_id.clone(),
                                version: service1_version1.clone(),
                                service_vpc_ids: vec![],
                            },
                            rpc::forge::InstanceDpuExtensionServiceConfig {
                                service_id: service1.service_id.clone(),
                                version: service1_version2.clone(),
                                service_vpc_ids: vec![],
                            },
                        ],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("duplicate service identities must be rejected during update");

    // A service ID may select only one active version in an instance configuration.
    assert!(duplicate_error.message().starts_with(
        "duplicate extension services in configuration. only one version of each service is allowed"
    ));

    // Update the extension service config
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service1.service_id.clone(),
                    version: service1_version2.clone(),
                    service_vpc_ids: vec![],
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service2.service_id.clone(),
                    version: service2_version.clone(),
                    service_vpc_ids: vec![],
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service3.service_id.clone(),
                    version: service3_version.clone(),
                    service_vpc_ids: vec![],
                },
            ],
        }),
        power_profile: None,
    };
    let instance = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state
            == rpc::forge::TenantState::Configuring as i32
    );

    // The public response exposes the three requested active attachments and omits terminating
    // service1 v1.
    let extension_services_config = instance
        .config
        .unwrap()
        .dpu_extension_services
        .unwrap()
        .service_configs;
    assert_eq!(extension_services_config.len(), 3);

    // However, internally we should track all services (including terminating ones) in status
    let status = instance.status.unwrap().dpu_extension_services.unwrap();

    // We expect 4 services total:
    // - service1 v1 (terminating, was replaced by v2)
    // - service1 v2 (active)
    // - service2 v1 (active)
    // - service3 v1 (active)
    assert_eq!(
        status.dpu_extension_services.len(),
        4,
        "Status should track all 4 services (including terminating ones)"
    );

    // Verify the services exist with correct versions (order-independent check)
    let mut service_versions: Vec<(String, u64, bool)> = status
        .dpu_extension_services
        .iter()
        .map(|s| {
            let version = s.version.parse::<ConfigVersion>().unwrap();
            (
                s.service_id.clone(),
                version.version_nr(),
                s.removed.is_some(),
            )
        })
        .collect();
    service_versions.sort();

    let mut expected_versions = vec![
        (service1.service_id.clone(), 1_u64, true),
        (service1.service_id.clone(), 2_u64, false),
        (service2.service_id.clone(), 1_u64, false),
        (service3.service_id.clone(), 1_u64, false),
    ];
    expected_versions.sort();

    assert_eq!(
        service_versions, expected_versions,
        "All service versions should be tracked in status"
    );

    // Record service2's attachment ID before removal so reattachment must revive it.
    let service2_attachment_id =
        db::instance::find_by_id(&env.harness.api().database_connection, instance_id)
            .await?
            .expect("instance remains stored before extension-service removal")
            .config
            .extension_services
            .service_configs
            .into_iter()
            .find(|attachment| attachment.service_id.to_string() == service2.service_id)
            .and_then(|attachment| attachment.id)
            .expect("active service2 attachment has durable identity");

    // Update the extension service config with no services
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![],
        }),
        power_profile: None,
    };
    let instance = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // The extension services config in the instance rpc response should be empty because
    // we only return active services to users.
    let extension_services_config = instance.config.unwrap().dpu_extension_services;
    assert!(extension_services_config.is_none());

    // However, internally we should track all services (including terminating ones) in status
    let status = instance.status.unwrap().dpu_extension_services.unwrap();

    // We expect 4 services total:
    // - service1 v1 (terminating, was replaced by v2)
    // - service1 v2 (terminating, being removed)
    // - service2 v1 (terminating, being removed)
    // - service3 v1 (terminating, being removed)
    assert_eq!(
        status.dpu_extension_services.len(),
        4,
        "Status should track all 4 services (including terminating ones)"
    );

    // Verify the services exist with correct versions (order-independent check)
    let mut service_versions: Vec<(String, u64, bool)> = status
        .dpu_extension_services
        .iter()
        .map(|s| {
            let version = s.version.parse::<ConfigVersion>().unwrap();
            (
                s.service_id.clone(),
                version.version_nr(),
                s.removed.is_some(),
            )
        })
        .collect();
    service_versions.sort();

    let mut expected_versions = vec![
        (service1.service_id.clone(), 1_u64, true),
        (service1.service_id.clone(), 2_u64, true),
        (service2.service_id.clone(), 1_u64, true),
        (service3.service_id.clone(), 1_u64, true),
    ];
    expected_versions.sort();

    assert_eq!(
        service_versions, expected_versions,
        "All service versions should be tracked in status"
    );

    // Preserve the exact unrelated tombstones so reattachment cannot discard cleanup history.
    let terminating_attachments =
        db::instance::find_by_id(&env.harness.api().database_connection, instance_id)
            .await?
            .expect("instance remains stored after extension-service removal")
            .config
            .extension_services
            .service_configs;
    assert_eq!(terminating_attachments.len(), 4);

    // Re-add an ordinary endpoint-free service at the same version while its retained attachment
    // is terminating to exercise the public asynchronous-removal contract.
    let reattached = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    nvlink: None,
                    spxconfig: None,
                    dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service2.service_id.clone(),
                            version: service2_version.clone(),
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect("endpoint-free same-version reattachment remains supported")
        .into_inner();

    // The response and a fresh database read must expose the same revived attachment ID.
    let public_reattachment = reattached
        .status
        .as_ref()
        .expect("reattached instance has status")
        .dpu_extension_services
        .as_ref()
        .expect("reattached instance has extension-service status")
        .dpu_extension_services
        .iter()
        .find(|attachment| attachment.service_id == service2.service_id)
        .expect("service2 attachment is active in the public status");
    assert_eq!(
        public_reattachment.attachment_id,
        Some(service2_attachment_id.into())
    );
    assert!(public_reattachment.removed.is_none());
    assert_eq!(
        reattached
            .config
            .as_ref()
            .expect("reattached instance has a config")
            .dpu_extension_services
            .as_ref()
            .expect("reattached instance exposes active services")
            .service_configs
            .len(),
        1
    );
    let persisted_attachments =
        db::instance::find_by_id(&env.harness.api().database_connection, instance_id)
            .await?
            .expect("reattached instance remains stored")
            .config
            .extension_services
            .service_configs;
    let persisted_reattachment = persisted_attachments
        .iter()
        .find(|attachment| attachment.service_id.to_string() == service2.service_id)
        .expect("service2 attachment is active again");
    assert_eq!(persisted_reattachment.id, Some(service2_attachment_id));
    assert!(persisted_reattachment.removed.is_none());
    assert_eq!(persisted_attachments.len(), 4);
    assert_eq!(
        persisted_attachments
            .iter()
            .filter(|attachment| attachment.service_id.to_string() != service2.service_id)
            .collect_vec(),
        terminating_attachments
            .iter()
            .filter(|attachment| attachment.service_id.to_string() != service2.service_id)
            .collect_vec(),
    );

    // Update the extension service config with non-existing service version, expect error
    // Update the extension service config with fewer new service
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: missing_service1_version,
                service_vpc_ids: vec![],
            }],
        }),
        power_profile: None,
    };
    let instance = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await;
    assert!(instance.is_err());
    let err = instance.unwrap_err();
    assert!(err.to_string().contains("does not exist or is deleted"));

    // Select an unknown registration so the API exposes entity absence distinctly from an invalid
    // version or a lifecycle precondition.
    let missing_service_id = ExtensionServiceId::new();
    let missing_error = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    nvlink: None,
                    spxconfig: None,
                    dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: missing_service_id.to_string(),
                            version: ConfigVersion::initial().to_string(),
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unknown extension service must return not found");

    // Typed not-found lets clients distinguish a missing registration from a lifecycle precondition.
    assert_eq!(missing_error.code(), tonic::Code::NotFound);

    // Create and delete an otherwise unused Pod registration so its durable row remains
    // distinguishable from an identifier that never existed.
    let deleted_service = env
        .harness
        .api()
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "deleted-instance-service".to_string(),
                deployment_type: rpc::forge::DpuExtensionServiceDeploymentType::DemandDriven as i32,
                description: Some("Deleted service classification fixture".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("deleted-instance-service-v1"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();
    let deleted_version = deleted_service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .clone();
    env.harness
        .api()
        .delete_dpu_extension_service(Request::new(rpc::forge::DeleteDpuExtensionServiceRequest {
            service_id: deleted_service.service_id.clone(),
            versions: vec![],
        }))
        .await?;
    let deletion_baseline =
        db::instance::find_by_id(&env.harness.api().database_connection, instance_id)
            .await?
            .expect("instance remains stored before deleted-service rejection");

    // Selecting a known deleted registration is a failed lifecycle precondition, not entity absence.
    let deleted_error = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    infiniband: None,
                    network_security_group_id: None,
                    dhcp_option_set_id: None,
                    nvlink: None,
                    spxconfig: None,
                    dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: deleted_service.service_id,
                            version: deleted_version,
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("a deleted extension service must fail as a precondition");
    assert_eq!(deleted_error.code(), tonic::Code::FailedPrecondition);

    // A fresh read proves classification changed neither attachment configuration nor either
    // configuration version.
    let persisted = db::instance::find_by_id(&env.harness.api().database_connection, instance_id)
        .await?
        .expect("instance remains stored after deleted-service rejection");
    assert_eq!(
        persisted.config.extension_services,
        deletion_baseline.config.extension_services
    );
    assert_eq!(
        persisted.extension_services_config_version,
        deletion_baseline.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        deletion_baseline.network_config_version
    );

    Ok(())
}

#[sqlx_test]
async fn test_attach_extension_service_rejected_on_dpf_host(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    let (service, _, _) = create_dpu_extension_services(&env).await?;
    let tinstance = mh
        .instance_builder(&env.harness)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    let mut txn = env.harness.api().database_connection.begin().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &host_id(&mh)).await?;
    txn.commit().await?;

    let result = env
        .harness
        .api()
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service.latest_version_info.unwrap().version,
                        service_vpc_ids: vec![],
                    }],
                }),
                power_profile: None,
            }),
            metadata: Some(rpc::Metadata {
                name: "dpf-extension-service".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect_err("attaching an extension service must fail on a DPF-managed host");

    assert_eq!(result.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        result.message(),
        format!(
            "DPU extension services are not supported on DPF-managed host {}",
            host_id(&mh)
        )
    );

    Ok(())
}

#[sqlx_test]
async fn test_extension_services_status_observation(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    // Create an extension service
    let (service1, _, _) = create_dpu_extension_services(&env).await.unwrap();
    let versions = service1
        .active_versions
        .iter()
        .map(|v| v.parse::<ConfigVersion>().unwrap())
        .collect::<Vec<_>>();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: versions[0].version_string(),
                service_vpc_ids: vec![],
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builder(&env.harness)
        .config(config.clone())
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    // Verify the status is correctly updated
    let mut txn = env.harness.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance_snapshot = snapshot.instance.unwrap();

    // Check that the observation was stored
    assert_eq!(instance_snapshot.observations.extension_services.len(), 1,);

    let dpu_observation = instance_snapshot
        .observations
        .extension_services
        .get(&mh.first_dpu().id)
        .and_then(|observations| {
            observations
                .for_service_type(model::extension_service::ExtensionServiceType::KubernetesPod)
        })
        .unwrap();

    assert_eq!(
        dpu_observation.config_version,
        instance_snapshot.extension_services_config_version,
    );

    assert_eq!(dpu_observation.extension_service_statuses.len(), 1,);

    let service_status = &dpu_observation.extension_service_statuses[0];
    assert_eq!(
        service_status.service_id.to_string(),
        service1.service_id.clone()
    );
    assert_eq!(service_status.version, versions[0].clone());
    assert_eq!(
        service_status.overall_state,
        model::instance::status::extension_service::ExtensionServiceDeploymentStatus::Running
    );

    txn.rollback().await?;

    // Now verify the RPC instance status
    let instance = tinstance.rpc_instance().await;
    let ext_status = instance
        .status
        .as_ref()
        .unwrap()
        .dpu_extension_services
        .as_ref()
        .unwrap();

    // Since we have matching config version observation, status should be synced
    assert_eq!(
        ext_status.configs_synced,
        rpc::forge::SyncState::Synced as i32
    );

    // Verify the service status
    assert_eq!(ext_status.dpu_extension_services.len(), 1,);

    let service_status = &ext_status.dpu_extension_services[0];
    assert_eq!(service_status.service_id, service1.service_id.clone());
    assert_eq!(service_status.version, versions[0].to_string());
    assert_eq!(
        service_status.deployment_status,
        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32,
    );

    // Verify DPU status details
    assert_eq!(service_status.dpu_statuses.len(), 1,);

    let dpu_status = &service_status.dpu_statuses[0];
    assert_eq!(dpu_status.dpu_machine_id, Some(mh.first_dpu().id));
    assert_eq!(
        dpu_status.status,
        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32,
    );

    Ok(())
}

async fn create_dpu_extension_services(
    env: &InstanceEnv,
) -> Result<
    (
        DpuExtensionService,
        DpuExtensionService,
        DpuExtensionService,
    ),
    Box<dyn std::error::Error>,
> {
    let _ = env
        .harness
        .api()
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    let service1 = env
        .harness
        .api()
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service1".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service1-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    // Update the extension service with a new version
    let service1 = env
        .harness
        .api()
        .update_dpu_extension_service(tonic::Request::new(
            rpc::forge::UpdateDpuExtensionServiceRequest {
                service_vpc_interfaces: None,
                service_id: service1.service_id.clone(),
                service_name: None,
                description: Some("Test service for instance".to_string()),
                data: create_dpu_extension_service_data("test-service1-v2"),
                credential: None,
                if_version_ctr_match: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    let service2 = env
        .harness
        .api()
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service2".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service2-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    let service3 = env
        .harness
        .api()
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service3".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service3-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    Ok((service1, service2, service3))
}

fn create_dpu_extension_service_data(name: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27",
        name
    )
}
