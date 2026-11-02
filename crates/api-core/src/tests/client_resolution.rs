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

use carbide_uuid::machine::PredictedHostMachineId;
use common::api_fixtures::{
    TEST_RMS_RACK_PROFILE_ID, TestEnv, TestEnvOverrides, create_managed_host,
    create_managed_host_with_config, create_test_env, create_test_env_with_overrides,
    get_config_with_rack_profiles,
};
use ipnetwork::IpNetwork;
use model::expected_machine::ExpectedMachineData;
use model::instance::config::network::{
    InstanceNetworkConfig, InstanceNetworkConfigUpdate, InterfaceFunctionId,
};
use model::machine::{InstanceState, ManagedHostState, SpdmMeasuringState};
use model::machine_interface::InterfaceType;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use tonic::{Code, IntoRequest};

use crate::CarbideError;
use crate::handlers::resolve_machine_interface_for_test;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::mac_address_pool::DPU_OOB_MAC_ADDRESS_POOL;
use crate::test_support::network_segment::{FIXTURE_TENANT_ORG_ID, create_default_flat_vpc};
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::default_os_config;
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

// Tenant addresses use their configured PF. A VF or an allocation without
// matching configuration keeps the host's primary-interface fallback.
#[crate::sqlx_test]
async fn test_resolve_machine_interface_via_instance_address(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh =
        create_managed_host_with_config(&env, env.managed_host_config().with_dpu_count(2)).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host = mh.host().db_machine(&mut txn).await;
    let expected_interface_id = host
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .expect("managed host must have a primary interface")
        .id;
    let secondary_dpu = host
        .status
        .interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
        })
        .and_then(|interface| interface.attached_dpu_machine_id)
        .expect("two-DPU host must have a secondary interface");
    let secondary_locator = host.get_device_locator_for_dpu_id(&secondary_dpu).unwrap();
    txn.rollback().await.unwrap();

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

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
    let network = tinstance.db_instance(&mut txn).await.config.network;
    assert!(network.interfaces[0].device_locator.is_none());
    txn.rollback().await.unwrap();

    let mut secondary_pf = network.clone();
    secondary_pf.interfaces[0].device_locator = Some(secondary_locator);
    let mut secondary_vf = secondary_pf.clone();
    secondary_vf.interfaces[0].function_id = InterfaceFunctionId::Virtual { id: 1 };
    let pending = InstanceNetworkConfigUpdate {
        old_config: secondary_pf,
        new_config: network.clone(),
    };

    // Keep the allocated address live while changing the persisted descriptions.
    // A retained old PF must participate even after the new primary config is live.
    for (case, network, pending, expected) in [
        (
            "omitted PF locator",
            network.clone(),
            None,
            Ok(expected_interface_id),
        ),
        (
            "sole VF on secondary",
            secondary_vf,
            None,
            Ok(expected_interface_id),
        ),
        (
            "owner without matching config",
            InstanceNetworkConfig::default(),
            None,
            Ok(expected_interface_id),
        ),
        (
            "retained old PF conflicts with live primary",
            network,
            Some(pending),
            Err(Code::FailedPrecondition),
        ),
    ] {
        sqlx::query(
            "UPDATE instances SET network_config = $1, update_network_config_request = $2 WHERE id = $3",
        )
        .bind(sqlx::types::Json(network))
        .bind(pending.map(sqlx::types::Json))
        .bind(tinstance.id)
        .execute(&env.pool)
        .await
        .unwrap();

        let mut txn = env.pool.begin().await.unwrap();
        let resolved = resolve_machine_interface_for_test(txn.as_mut(), tenant_ip)
            .await
            .map(|interface| interface.id)
            .map_err(|error| {
                let status = tonic::Status::from(error);
                assert!(
                    status
                        .message()
                        .contains("conflicting retained interface configurations"),
                    "{case}: {status}"
                );
                status.code()
            });
        txn.rollback().await.unwrap();
        assert_eq!(resolved, expected, "{case}");
    }
}

#[crate::sqlx_test]
async fn test_domainless_dpu_cloud_init_rejects_custom_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Allowing host discovery without a domain must not let a custom override
    // bypass the domain requirement for DPU provisioning.
    let env = create_test_env(pool).await;
    let dpu_id =
        common::api_fixtures::dpu::create_dpu_machine(&env, &env.managed_host_config()).await;

    let mut txn = env.pool.begin().await?;
    let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[dpu_id]).await?;
    let [interface] = interfaces[&dpu_id].as_slice() else {
        panic!("fixture DPU must have one non-BMC interface")
    };
    assert_eq!(interface.machine_id, Some(dpu_id.into()));
    assert!(interface.domain_id.is_some());
    let interface_id = interface.id;
    let client_ip = interface.addresses[0];
    let custom_user_data = "#cloud-config\ncustom-user-data";

    db::machine_boot_override::create(
        txn.as_mut(),
        interface_id,
        None,
        Some(custom_user_data.to_string()),
    )
    .await?
    .expect("fixture must install the custom cloud-init override");
    txn.commit().await?;

    let cloud_init = env
        .api
        .get_cloud_init_instructions(
            rpc::forge::CloudInitInstructionsRequest {
                ip: client_ip.to_string(),
            }
            .into_request(),
        )
        .await?
        .into_inner();
    assert_eq!(
        cloud_init.custom_cloud_init.as_deref(),
        Some(custom_user_data)
    );
    let domain = cloud_init.discovery_instructions.unwrap().domain.unwrap();
    let Some(rpc::forge::pxe_domain::Domain::NewDomain(domain)) = domain.domain else {
        panic!("DPU discovery must return its domain")
    };
    assert_eq!(domain.id, interface.domain_id);

    sqlx::query("UPDATE machine_interfaces SET domain_id = NULL WHERE id = $1")
        .bind(interface_id)
        .execute(&env.pool)
        .await?;

    let status = env
        .api
        .get_cloud_init_instructions(
            rpc::forge::CloudInitInstructionsRequest {
                ip: client_ip.to_string(),
            }
            .into_request(),
        )
        .await
        .expect_err("a custom override must not bypass the DPU domain requirement");
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(
        status.message(),
        format!(
            "internal error: machine interface did not have an associated domain {interface_id}"
        )
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_domainless_cloud_init_requires_owned_host_data(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_id = common::api_fixtures::dpu::create_dpu_machine(&env, &host_config).await;
    let host_interface_id =
        common::api_fixtures::host::host_discover_dhcp(&env, &host_config, &dpu_id).await;
    let unowned_interface_id = common::api_fixtures::dpu::dpu_discover_dhcp(
        &env,
        &DPU_OOB_MAC_ADDRESS_POOL.allocate().to_string(),
    )
    .await;

    let mut txn = env.pool.begin().await?;
    let host = db::machine_interface::find_one(txn.as_mut(), host_interface_id).await?;
    let predicted_host_id = PredictedHostMachineId::try_from(host.machine_id.unwrap())?;
    assert_eq!(host.interface_type, InterfaceType::Data);
    let bmc_interfaces =
        db::machine_interface::find_by_mac_address(txn.as_mut(), host_config.bmc_mac_address)
            .await?;
    let [bmc] = bmc_interfaces.as_slice() else {
        panic!("fixture must have one host BMC interface")
    };
    assert_eq!(bmc.machine_id, host.machine_id);
    assert_eq!(bmc.interface_type, InterfaceType::Bmc);
    let unowned = db::machine_interface::find_one(txn.as_mut(), unowned_interface_id).await?;
    assert_eq!(unowned.machine_id, None);
    assert_eq!(unowned.interface_type, InterfaceType::Data);

    let custom_user_data = "#cloud-config\nhost-discovery-override";
    db::machine_boot_override::create(
        txn.as_mut(),
        host.id,
        None,
        Some(custom_user_data.to_string()),
    )
    .await?
    .expect("fixture must install the host cloud-init override");
    txn.commit().await?;

    for (case, interface, expected_metadata_id) in [
        ("predicted host Data", &host, Some(predicted_host_id)),
        ("host BMC", bmc, None),
        ("unowned Data", &unowned, None),
    ] {
        sqlx::query("UPDATE machine_interfaces SET domain_id = NULL WHERE id = $1")
            .bind(interface.id)
            .execute(&env.pool)
            .await?;
        let response = env
            .api
            .get_cloud_init_instructions(
                rpc::forge::CloudInitInstructionsRequest {
                    ip: interface.addresses[0].to_string(),
                }
                .into_request(),
            )
            .await;

        if let Some(expected_metadata_id) = expected_metadata_id {
            let cloud_init = response.expect(case).into_inner();
            assert_eq!(
                cloud_init.custom_cloud_init.as_deref(),
                Some(custom_user_data)
            );
            let discovery = cloud_init.discovery_instructions.expect(case);
            assert_eq!(discovery.domain, None);
            assert_eq!(
                discovery.machine_interface.expect(case).id,
                Some(interface.id)
            );
            assert_eq!(
                cloud_init.metadata.expect(case).instance_id,
                expected_metadata_id.to_string()
            );
        } else {
            let status = response.expect_err(case);
            assert_eq!(status.code(), Code::Internal, "{case}");
            assert_eq!(
                status.message(),
                format!(
                    "internal error: machine interface did not have an associated domain {}",
                    interface.id
                ),
                "{case}"
            );
        }
    }
    Ok(())
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
            machine_id: Some(mh.id),
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
            machine_id: Some(mh.id),
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
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await;

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
            machine_id: Some(mh.id),
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
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await;

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
