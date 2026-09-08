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
    TEST_RMS_RACK_PROFILE_ID, TestEnv, TestEnvOverrides, create_managed_host,
    create_managed_host_with_config, create_test_env, create_test_env_with_overrides,
    get_config_with_rack_profiles,
};
use ipnetwork::IpNetwork;
use model::expected_machine::ExpectedMachineData;
use model::machine::{InstanceState, ManagedHostState, SpdmMeasuringState};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use tonic::{Code, IntoRequest};

use crate::CarbideError;
use crate::handlers::resolve_machine_interface_for_test;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::network_segment::{FIXTURE_TENANT_ORG_ID, create_default_flat_vpc};
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::{
    advance_created_instance_into_ready_state, default_os_config, default_tenant_config,
    single_interface_network_config,
};
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_HOST_INBAND_NETWORK_SEGMENT_GATEWAY,
    create_host_inband_network_segment,
};
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;

/// Verifies that neither PXE resolution nor cloud-init can select tenant data
/// when the observed address does not identify one client.
async fn assert_client_resolution_fails_closed(
    env: &TestEnv,
    client_ip: std::net::IpAddr,
    private_ids: &[String],
) {
    let mut txn = env.pool.begin().await.unwrap();
    let error = resolve_machine_interface_for_test(txn.as_mut(), client_ip)
        .await
        .expect_err("PXE resolution should reject an ambiguous client address");
    txn.rollback().await.unwrap();
    assert!(
        matches!(&error, CarbideError::FailedPrecondition(_)),
        "PXE resolution should fail with FailedPrecondition: {error:?}"
    );

    let status = env
        .api
        .get_cloud_init_instructions(
            rpc::forge::CloudInitInstructionsRequest {
                ip: client_ip.to_string(),
            }
            .into_request(),
        )
        .await
        .expect_err("cloud-init should reject an ambiguous client address");
    assert_eq!(status.code(), Code::FailedPrecondition);

    let errors = [error.to_string(), status.message().to_string()];
    for private_id in private_ids {
        assert!(
            errors.iter().all(|message| !message.contains(private_id)),
            "ambiguity errors must not identify a candidate owner: {private_id}"
        );
    }
}

/// Returns the NVConfig profile selected for a DPU cloud-init request.
async fn resolved_dpu_nvconfig_profile(env: &TestEnv, dpu_ip: &str) -> i32 {
    env.api
        .get_cloud_init_instructions(
            rpc::forge::CloudInitInstructionsRequest {
                ip: dpu_ip.to_string(),
            }
            .into_request(),
        )
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner()
        .discovery_instructions
        .expect("DPU should receive discovery instructions")
        .dpu_nvconfig_profile
}

// A client_ip that matches a row in machine_interface_addresses (the
// common admin/host case) should resolve directly to that interface.
#[crate::sqlx_test]
async fn test_resolve_machine_interface_via_direct_admin_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[mh.host().id])
        .await
        .unwrap();
    let host_iface = &interfaces[&mh.host().id][0];
    let admin_ip = host_iface.addresses[0];
    let expected_interface_id = host_iface.id;
    txn.rollback().await.unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let resolved = resolve_machine_interface_for_test(txn.as_mut(), admin_ip)
        .await
        .expect("admin IP should resolve to its machine_interface");
    txn.rollback().await.unwrap();

    assert_eq!(resolved.id, expected_interface_id);
}

// A client_ip that maps to a tenant-allocated instance_address (rather
// than a machine_interface_addresses entry) should resolve to the
// host's admin machine_interface via instance -> host_machine_id ->
// host_interfaces. This is the "PXE-booting an assigned host over its
// tenant network" path that the find_by_ip fallback was added for.
#[crate::sqlx_test]
async fn test_resolve_machine_interface_via_instance_address(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[mh.host().id])
        .await
        .unwrap();
    let expected_interface_id = interfaces[&mh.host().id][0].id;
    txn.rollback().await.unwrap();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    let tinstance = mh.instance_builer(&env).config(config).build().await;

    // Look up the tenant IP carbide-api allocated to the instance.
    let mut txn = env.pool.begin().await.unwrap();
    let instance_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &tinstance.id,
        &segment_id,
    )
    .await
    .unwrap();
    let [inst_addr] = instance_addresses.as_slice() else {
        panic!("instance should have one tenant address on the segment")
    };
    let tenant_ip = inst_addr.address;

    let resolved = resolve_machine_interface_for_test(txn.as_mut(), tenant_ip)
        .await
        .expect("tenant IP should resolve to the host's admin machine_interface");
    txn.rollback().await.unwrap();

    // The resolved interface is the host's admin interface -- the same one
    // we'd have hit if the request had come in on the admin IP directly.
    assert_eq!(resolved.id, expected_interface_id);
}

// A client_ip that isn't in either table should NotFound cleanly.
#[crate::sqlx_test]
async fn test_resolve_machine_interface_unknown_ip_returns_not_found(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.begin().await.unwrap();
    let result =
        resolve_machine_interface_for_test(txn.as_mut(), "203.0.113.99".parse().unwrap()).await;
    txn.rollback().await.unwrap();

    let err = result.expect_err("expected NotFound for unknown client IP");
    match err {
        CarbideError::NotFoundError { kind, .. } => {
            assert_eq!(kind, "Client", "expected NotFound kind=Client, got {kind}")
        }
        other => panic!("expected NotFoundError, got {other:?}"),
    }
}

/// Equal addresses on two tenant instances are not enough to choose either
/// host for PXE or cloud-init.
#[crate::sqlx_test]
async fn test_client_resolution_rejects_duplicate_overlay_addresses(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let client_ip = "10.20.30.50".parse().unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let owners = [
        common::overlay_address::seed_overlay_address_owner(
            txn.as_mut(),
            "client-resolution-a",
            client_ip,
        )
        .await,
        common::overlay_address::seed_overlay_address_owner(
            txn.as_mut(),
            "client-resolution-b",
            client_ip,
        )
        .await,
    ];
    txn.commit().await.unwrap();

    assert_client_resolution_fails_closed(
        &env,
        client_ip,
        &owners
            .iter()
            .map(|owner| owner.instance_id.to_string())
            .collect::<Vec<_>>(),
    )
    .await;
}

/// An underlay interface and an unrelated tenant instance are separate
/// owners, so the caller's preferred lookup cannot safely choose between them.
#[crate::sqlx_test]
async fn test_client_resolution_rejects_unrelated_underlay_and_overlay_owners(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let interfaces =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[managed_host.host().id])
            .await
            .unwrap();
    let client_ip = interfaces[&managed_host.host().id]
        .iter()
        .find(|interface| {
            interface.network_segment_type
                == Some(model::network_segment::NetworkSegmentType::Admin)
        })
        .and_then(|interface| interface.addresses.first())
        .copied()
        .expect("managed host should have an admin address");
    let overlay_owner = common::overlay_address::seed_overlay_address_owner(
        txn.as_mut(),
        "mixed-client-resolution",
        client_ip,
    )
    .await;
    txn.commit().await.unwrap();

    assert_client_resolution_fails_closed(
        &env,
        client_ip,
        &[
            managed_host.host().id.to_string(),
            overlay_owner.instance_id.to_string(),
        ],
    )
    .await;
}

/// Matching the physical machine is not enough to treat an underlay address
/// as the zero-DPU instance interface. A tenant segment on that same host is
/// still a separate address owner.
#[crate::sqlx_test]
async fn test_client_resolution_rejects_same_machine_different_segment_owners(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let interfaces =
        db::machine_interface::find_by_machine_ids(txn.as_mut(), &[managed_host.host().id])
            .await
            .unwrap();
    let client_ip = interfaces[&managed_host.host().id]
        .iter()
        .find(|interface| {
            interface.network_segment_type
                == Some(model::network_segment::NetworkSegmentType::Admin)
        })
        .and_then(|interface| interface.addresses.first())
        .copied()
        .expect("managed host should have an admin address");
    let overlay_owner = common::overlay_address::seed_overlay_address_owner(
        txn.as_mut(),
        "same-machine-different-segment",
        client_ip,
    )
    .await;
    sqlx::query("UPDATE instances SET machine_id = $1 WHERE id = $2")
        .bind(managed_host.host().id)
        .bind(overlay_owner.instance_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    assert_client_resolution_fails_closed(
        &env,
        client_ip,
        &[
            managed_host.host().id.to_string(),
            overlay_owner.instance_id.to_string(),
        ],
    )
    .await;
}

#[crate::sqlx_test]
async fn test_zero_dpu_cloud_init_prefers_instance_when_ip_matches_host_interface(
    pool: sqlx::PgPool,
) {
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
    let vpc_id = create_default_flat_vpc(&env.api, "flat-vpc").await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let mh = create_managed_host_with_config(&env, ManagedHostConfig::zero_dpu()).await;
    assert!(
        mh.dpu_ids.is_empty(),
        "zero-DPU fixture should produce no DPU machines"
    );

    let mut txn = env.pool.begin().await.unwrap();
    let host_interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[mh.host().id])
        .await
        .unwrap();
    let host_ip = host_interfaces[&mh.host().id][0].addresses[0];
    txn.rollback().await.unwrap();

    let tenant_user_data = "#cloud-config\ntenant-user-data";
    let instance = env
        .api
        .allocate_instance(tonic::Request::new(rpc::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            instance_type_id: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(rpc::TenantConfig {
                    tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
                    tenant_keyset_ids: vec![],
                    hostname: None,
                }),
                os: Some(rpc::forge::InstanceOperatingSystemConfig {
                    user_data: Some(tenant_user_data.to_string()),
                    ..default_os_config()
                }),
                network: Some(rpc::forge::InstanceNetworkConfig {
                    interfaces: vec![],
                    #[allow(deprecated)]
                    auto: true,
                    auto_config: Some(rpc::forge::InstanceNetworkAutoConfig {
                        vpc_id: Some(vpc_id),
                    }),
                }),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: None,
            metadata: None,
            allow_unhealthy_machine: false,
        }))
        .await
        .expect("zero-DPU instance allocation should succeed")
        .into_inner();
    let instance_id = instance.id.expect("allocated instance should have an ID");

    let instance_addresses = db::instance_address::find_all_by_address(&env.pool, host_ip)
        .await
        .unwrap();
    let instance_address = instance_addresses
        .iter()
        .find(|address| address.instance_id == instance_id)
        .expect("zero-DPU instance should reuse the host interface IP");
    assert_eq!(instance_address.instance_id, instance_id);

    // When not ready yet, we should get discovery cloud-init instructions
    {
        env.run_machine_state_controller_iteration_until_state_matches(
            &mh.host().id,
            50,
            ManagedHostState::PreAssignedMeasuring {
                spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
            },
        )
        .await;

        let cloud_init = env
            .api
            .get_cloud_init_instructions(
                rpc::forge::CloudInitInstructionsRequest {
                    ip: host_ip.to_string(),
                }
                .into_request(),
            )
            .await
            .expect("get_cloud_init_instructions returned an error")
            .into_inner();

        assert!(
            cloud_init.custom_cloud_init.is_none(),
            "Should not get tenant instructions when machine is in PreAssignedMeasuring"
        );
        assert!(
            cloud_init.discovery_instructions.is_some(),
            "Should get discovery instructions when machine is in PreAssignedMeasuring"
        );
    }

    let stored_mac: Option<String> = sqlx::query_scalar(
        "SELECT network_config #>> '{interfaces,0,host_inband_mac_address}'
         FROM instances
         WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(
        stored_mac.is_some(),
        "the fixture should start with Core's HostInband MAC"
    );

    // Resolution uses the stored address owner and segment, so older configs
    // without Core's HostInband MAC still identify the same shared NIC.
    sqlx::query(
        "UPDATE instances
         SET network_config = jsonb_set(
             network_config,
             '{interfaces,0,host_inband_mac_address}',
             'null'::jsonb
         )
         WHERE id = $1",
    )
    .bind(instance_id)
    .execute(&env.pool)
    .await
    .unwrap();
    let stored_mac: Option<String> = sqlx::query_scalar(
        "SELECT network_config #>> '{interfaces,0,host_inband_mac_address}'
         FROM instances
         WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(stored_mac.is_none(), "the legacy MAC setup should apply");

    // When the instance is ready, we should get tenant cloud-init instructions
    for instance_state in [InstanceState::WaitingForRebootToReady, InstanceState::Ready] {
        let reboot_pending = instance_state == InstanceState::WaitingForRebootToReady;
        env.run_machine_state_controller_iteration_until_state_matches(
            &mh.host().id,
            10,
            ManagedHostState::Assigned { instance_state },
        )
        .await;

        let stored_mac: Option<String> = sqlx::query_scalar(
            "SELECT network_config #>> '{interfaces,0,host_inband_mac_address}'
             FROM instances
             WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert!(
            stored_mac.is_none(),
            "the controller should preserve a legacy config without the HostInband MAC"
        );

        let cloud_init = env
            .api
            .get_cloud_init_instructions(tonic::Request::new(
                rpc::forge::CloudInitInstructionsRequest {
                    ip: host_ip.to_string(),
                },
            ))
            .await
            .expect("get_cloud_init_instructions returned an error")
            .into_inner();

        assert_eq!(
            cloud_init.custom_cloud_init.as_deref(),
            Some(tenant_user_data)
        );
        assert!(
            cloud_init.discovery_instructions.is_none(),
            "tenant cloud-init must not render discovery instructions for the shared zero-DPU IP"
        );
        assert_eq!(
            cloud_init
                .metadata
                .expect("tenant cloud-init should include metadata")
                .instance_id,
            instance_id.to_string()
        );

        if reboot_pending {
            env.run_machine_state_controller_iteration().await;
            mh.host().reboot_completed().await;
        }
    }
}

#[crate::sqlx_test]
async fn test_cloud_init_local_hostname_set_from_instance_name(pool: sqlx::PgPool) {
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
    let vpc_id = create_default_flat_vpc(&env.api, "flat-vpc").await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let mh = create_managed_host_with_config(&env, ManagedHostConfig::zero_dpu()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host_interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[mh.host().id])
        .await
        .unwrap();
    let host_ip = host_interfaces[&mh.host().id][0].addresses[0];
    txn.rollback().await.unwrap();

    let instance_name = "worker-0";
    let instance = env
        .api
        .allocate_instance(tonic::Request::new(rpc::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            instance_type_id: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(rpc::TenantConfig {
                    tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
                    tenant_keyset_ids: vec![],
                    hostname: None,
                }),
                os: Some(default_os_config()),
                network: Some(rpc::forge::InstanceNetworkConfig {
                    interfaces: vec![],
                    #[allow(deprecated)]
                    auto: true,
                    auto_config: Some(rpc::forge::InstanceNetworkAutoConfig {
                        vpc_id: Some(vpc_id),
                    }),
                }),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: instance_name.to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .expect("instance allocation with name should succeed")
        .into_inner();
    let instance_id = instance.id.expect("allocated instance should have an ID");

    // Advance to Assigned/Ready so the Instance path is taken
    advance_created_instance_into_ready_state(&env, &mh).await;

    let cloud_init = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(
            rpc::forge::CloudInitInstructionsRequest {
                ip: host_ip.to_string(),
            },
        ))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    let meta = cloud_init
        .metadata
        .expect("tenant cloud-init should include metadata");
    assert_eq!(meta.instance_id, instance_id.to_string());
    assert_eq!(
        meta.local_hostname.as_deref(),
        Some(instance_name),
        "local_hostname must match the instance name so cloud-init sets the OS hostname"
    );
}

#[crate::sqlx_test]
async fn test_cloud_init_local_hostname_omitted_when_instance_name_is_not_a_valid_hostname(
    pool: sqlx::PgPool,
) {
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
    let vpc_id = create_default_flat_vpc(&env.api, "flat-vpc").await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let mh = create_managed_host_with_config(&env, ManagedHostConfig::zero_dpu()).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host_interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[mh.host().id])
        .await
        .unwrap();
    let host_ip = host_interfaces[&mh.host().id][0].addresses[0];
    txn.rollback().await.unwrap();

    // Instance metadata names are free-form (unlike TenantConfig::hostname),
    // so a name like this is accepted at allocation time but is not a legal
    // DNS hostname.
    let instance_name = "Worker Zero!";
    let instance = env
        .api
        .allocate_instance(tonic::Request::new(rpc::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            instance_type_id: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(rpc::TenantConfig {
                    tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
                    tenant_keyset_ids: vec![],
                    hostname: None,
                }),
                os: Some(default_os_config()),
                network: Some(rpc::forge::InstanceNetworkConfig {
                    interfaces: vec![],
                    #[allow(deprecated)]
                    auto: true,
                    auto_config: Some(rpc::forge::InstanceNetworkAutoConfig {
                        vpc_id: Some(vpc_id),
                    }),
                }),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: instance_name.to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .expect("instance allocation with name should succeed")
        .into_inner();
    let instance_id = instance.id.expect("allocated instance should have an ID");

    // Advance to Assigned/Ready so the Instance path is taken
    advance_created_instance_into_ready_state(&env, &mh).await;

    let cloud_init = env
        .api
        .get_cloud_init_instructions(tonic::Request::new(
            rpc::forge::CloudInitInstructionsRequest {
                ip: host_ip.to_string(),
            },
        ))
        .await
        .expect("get_cloud_init_instructions returned an error")
        .into_inner();

    let meta = cloud_init
        .metadata
        .expect("tenant cloud-init should include metadata");
    assert_eq!(meta.instance_id, instance_id.to_string());
    assert_eq!(
        meta.local_hostname, None,
        "local_hostname must be omitted when the instance name is not a legal DNS hostname"
    );
}

#[crate::sqlx_test]
async fn dpu_nvconfig_profile_resolution_uses_report_or_rack_and_dpu_identity(pool: sqlx::PgPool) {
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(get_config_with_rack_profiles()),
    )
    .await;

    let mut txn = env.pool.begin().await.unwrap();
    let rack_id = TestRackDbBuilder::new()
        .with_rack_profile_id(TEST_RMS_RACK_PROFILE_ID)
        .persist(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let managed_host = create_managed_host_with_config(
        &env,
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        }),
    )
    .await;
    let dpu = managed_host.dpu();

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = dpu.db_machine(&mut txn).await;
    let mut hardware_info = dpu_machine
        .status
        .hardware_info
        .expect("fixture DPU should have hardware information");
    hardware_info
        .dpu_info
        .as_mut()
        .expect("fixture DPU should have DPU information")
        .part_number = "900-9D3B6-00CN-PA0".to_string();
    // The topology helper only replaces existing inventory after this flag is
    // set, matching the production discovery update contract.
    db::machine_topology::set_topology_update_needed(txn.as_mut(), &dpu.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(txn.as_mut(), &dpu.id, &hardware_info)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[dpu.id])
        .await
        .unwrap();
    let dpu_interface = dpu_interfaces
        .get(&dpu.id)
        .and_then(|interfaces| interfaces.first())
        .expect("fixture DPU should have a non-BMC interface");
    let dpu_interface_ip = dpu_interface
        .addresses
        .first()
        .expect("fixture DPU interface should have an address")
        .to_string();
    txn.rollback().await.unwrap();

    assert_eq!(
        resolved_dpu_nvconfig_profile(&env, &dpu_interface_ip).await,
        rpc::forge::DpuNvConfigProfile::Gb200B3240V1 as i32,
    );

    // Non-DPF provisioning sees the same early ingestion window as DPF: the
    // Redfish report is present, but the host does not have a rack yet.
    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE machines SET rack_id = NULL WHERE id = $1")
        .bind(managed_host.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    managed_host
        .host()
        .set_exploration_model(&mut txn, "DGX GB200 Compute Tray")
        .await;
    assert!(
        managed_host
            .host()
            .db_machine(&mut txn)
            .await
            .rack_id
            .is_none()
    );
    txn.commit().await.unwrap();

    assert_eq!(
        resolved_dpu_nvconfig_profile(&env, &dpu_interface_ip).await,
        rpc::forge::DpuNvConfigProfile::Gb200B3240V1 as i32,
    );

    // A recognized report is more specific than the rack fallback. A GB300
    // report must not inherit the GB200 profile from stale rack metadata.
    let mut txn = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE machines SET rack_id = $1 WHERE id = $2")
        .bind(rack_id.as_str())
        .bind(managed_host.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    managed_host
        .host()
        .set_exploration_model(&mut txn, "DGX GB300 Compute Tray")
        .await;
    txn.commit().await.unwrap();

    assert_eq!(
        resolved_dpu_nvconfig_profile(&env, &dpu_interface_ip).await,
        rpc::forge::DpuNvConfigProfile::Unspecified as i32,
    );
}
