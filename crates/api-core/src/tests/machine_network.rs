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
use std::collections::HashMap;
use std::ops::DerefMut;
use std::time::SystemTime;

use ::rpc::forge::{
    CreateDpuExtensionServiceRequest, DpuExtensionServiceType, DpuNetworkStatus,
    InstanceDpuExtensionServiceConfig, InstanceDpuExtensionServicesConfig,
    ManagedHostNetworkConfigRequest, ManagedHostNetworkStatusRequest,
};
use carbide_instrument::testing::MetricsCapture;
use carbide_secrets::credentials::{BgpCredentialType, CredentialKey, Credentials};
use carbide_uuid::machine::DpuMachineId;
use common::api_fixtures::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, create_tenant_network_segment,
};
use common::api_fixtures::{self, create_managed_host, dpu, network_configured_with_health};
use model::machine::network::ManagedHostQuarantineMode;
use rpc::Metadata;
use rpc::forge::forge_server::Forge;

use crate::cfg::file::{
    AdminFnnConfig, FnnConfig, FnnRoutingProfileConfig, PrefixFilterPolicyEntry, RouteTargetConfig,
};
use crate::tests::common;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::api_fixtures::site_explorer::MockExploredHost;
use crate::tests::common::rpc_builder::VpcCreationRequest;

async fn set_use_admin_network_changed(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
    value: bool,
) {
    let mut txn = env.db_txn().await;
    db::machine::set_use_admin_network_changed(txn.deref_mut(), &dpu_machine_id, value)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn use_admin_network_changed(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
) -> Option<bool> {
    let mut txn = env.db_txn().await;
    let dpu = db::machine::find_one(txn.deref_mut(), &dpu_machine_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    txn.commit().await.unwrap();
    dpu.network_config.value.use_admin_network_changed
}

async fn bump_dpu_network_config_version(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
) {
    let mut txn = env.db_txn().await;
    let dpu = db::machine::find_one(txn.deref_mut(), &dpu_machine_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    let version = dpu.network_config.version;
    let value = dpu.network_config.value;
    db::machine::try_update_network_config(txn.deref_mut(), &dpu_machine_id, version, &value)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

async fn record_dpu_network_status(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
    network_config_version: Option<String>,
) {
    env.api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id.into()),
            dpu_agent_version: Some(dpu::TEST_DPU_AGENT_VERSION.to_string()),
            observed_at: Some(SystemTime::now().into()),
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                triggered_by: None,
                observed_at: None,
                successes: vec![],
                alerts: vec![],
            }),
            network_config_version,
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            fabric_interfaces: vec![],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .unwrap();
}

#[crate::sqlx_test]
async fn test_clear_use_admin_network_changed_keeps_newer_version_flag(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;

    set_use_admin_network_changed(&env, dpu_machine_id, true).await;
    let mut txn = env.db_txn().await;
    let stale_version = db::machine::find_one(txn.deref_mut(), &dpu_machine_id, Default::default())
        .await
        .unwrap()
        .unwrap()
        .network_config
        .version;
    txn.commit().await.unwrap();

    bump_dpu_network_config_version(&env, dpu_machine_id).await;

    let mut txn = env.db_txn().await;
    let cleared = db::machine::clear_use_admin_network_changed_if_version_matches(
        txn.deref_mut(),
        &dpu_machine_id,
        &stale_version,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    assert!(!cleared);
    assert_eq!(
        use_admin_network_changed(&env, dpu_machine_id).await,
        Some(true)
    );
}

#[crate::sqlx_test]
// This test verifies parity between `addresses` and the compatibility fields.
#[allow(deprecated)]
async fn test_managed_host_network_config(pool: sqlx::PgPool) {
    // The default fixture omits `lo-ip-v6`, which must preserve the existing IPv4-only response.
    let env = api_fixtures::create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let mh = dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = mh.dpu().id;

    // Fetch a Machines network config
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        response
            .managed_host_config
            .as_ref()
            .expect("managed host config")
            .loopback_ip_v6
            .is_none(),
        "sites without lo-ip-v6 must remain IPv4-only"
    );

    let admin_interface = response.admin_interface.expect("admin interface");
    assert_eq!(
        admin_interface.addresses,
        vec![rpc::forge::InterfaceAddressConfig {
            address_family: rpc::forge::AddressFamily::V4.into(),
            ip: admin_interface.ip.unwrap(),
            interface_prefix: admin_interface.interface_prefix.unwrap(),
            prefix: admin_interface.prefix.unwrap(),
            gateway: admin_interface.gateway,
            svi_ip: admin_interface.svi_ip,
            tenant_vrf_loopback_ip: admin_interface.tenant_vrf_loopback_ip,
        }]
    );
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_does_not_clear_use_admin_network_changed(
    pool: sqlx::PgPool,
) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;

    set_use_admin_network_changed(&env, dpu_machine_id, true).await;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.use_admin_network_changed, Some(true));
    assert_eq!(
        use_admin_network_changed(&env, dpu_machine_id).await,
        Some(true)
    );
}

#[crate::sqlx_test]
async fn test_record_dpu_network_status_clears_use_admin_network_changed_for_matching_version(
    pool: sqlx::PgPool,
) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;

    set_use_admin_network_changed(&env, dpu_machine_id, true).await;
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    record_dpu_network_status(
        &env,
        dpu_machine_id,
        Some(response.managed_host_config_version),
    )
    .await;

    assert_eq!(
        use_admin_network_changed(&env, dpu_machine_id).await,
        Some(false)
    );
}

#[crate::sqlx_test]
async fn test_record_dpu_network_status_keeps_use_admin_network_changed_without_matching_version(
    pool: sqlx::PgPool,
) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;

    set_use_admin_network_changed(&env, dpu_machine_id, true).await;
    record_dpu_network_status(&env, dpu_machine_id, None).await;

    assert_eq!(
        use_admin_network_changed(&env, dpu_machine_id).await,
        Some(true)
    );
}

#[crate::sqlx_test]
async fn test_record_dpu_network_status_counts_failed_host_wakeup(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool.clone()).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;

    // Remove the state-handler queue table so the host wakeup enqueue fails
    // while the status report itself stays healthy.
    sqlx::query("DROP TABLE machine_state_controller_queued_objects")
        .execute(&pool)
        .await
        .unwrap();

    let metrics = MetricsCapture::start();
    // A never-before-observed network config version registers as a change,
    // which is what prompts the host state-handler wakeup.
    record_dpu_network_status(
        &env,
        dpu_machine_id,
        Some("wakeup-probe-version".to_string()),
    )
    .await;

    assert_eq!(
        metrics.counter_delta(
            "carbide_state_handler_wakeup_failures_total",
            &[("trigger", "dpu_network_status")],
        ),
        1.0
    );
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_with_sitewide_bgp_password(pool: sqlx::PgPool) {
    // Enable site-wide DPU BGP passwords in runtime config.
    let mut config = api_fixtures::get_config();
    config.bgp_leaf_session_password = Some(crate::cfg::file::BgpLeafSessionPassword::SiteWide);

    let env =
        api_fixtures::create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config))
            .await;

    // Seed the site-wide DPU BGP credential.
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::Bgp {
                credential_type: BgpCredentialType::SiteWideLeafPassword,
            },
            &Credentials::UsernamePassword {
                username: "".to_string(),
                password: "test-bgp-password".to_string(),
            },
        )
        .await
        .unwrap();

    // Create a DPU that can request managed host network config.
    let host_config = env.managed_host_config();
    let dpu_machine_id = dpu::create_dpu_machine(&env, &host_config).await;

    // Verify the handler returns the configured site-wide BGP password.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        response.bgp_leaf_session_password,
        Some("test-bgp-password".to_string())
    );
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_narrows_interface_anycast_prefixes(pool: sqlx::PgPool) {
    let profile_type = "ANYCAST_SUBSET_TEST";
    let vpc_allowed_anycast_prefixes = ["192.0.2.0/24".to_string(), "2001:db8:99::/48".to_string()];
    let interface_allowed_anycast_prefixes = vec![
        "192.0.2.64/26".to_string(),
        "2001:db8:99:1::/64".to_string(),
    ];
    let inherited_import = RouteTargetConfig {
        asn: 65001,
        vni: 123,
    };

    // Configure inherited properties and a base prefix that the VPC will replace.
    let env = api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default().with_fnn_config(Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([(
                profile_type.to_string(),
                FnnRoutingProfileConfig {
                    internal: Some(true),
                    access_tier: Some(0),
                    leak_default_route_from_underlay: Some(true),
                    route_target_imports: Some(vec![inherited_import.clone()]),
                    allowed_anycast_prefixes: Some(vec![PrefixFilterPolicyEntry {
                        prefix: "203.0.113.0/24".parse().unwrap(),
                    }]),
                    ..Default::default()
                },
            )]),
            use_vpc_vrf_loopback: false,
        })),
    )
    .await;

    // Override the VPC anycast list while inheriting the other base properties.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "anycast-subset-test".to_string(),
            routing_profile_type: Some(profile_type.to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "anycast-subset-test".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "anycast subset vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type(profile_type.to_string())
                .routing_profile_overrides(rpc::forge::VpcRoutingProfileOverrides {
                    allowed_anycast_prefixes: Some(rpc::forge::PrefixFilterPolicyEntries {
                        values: vpc_allowed_anycast_prefixes
                            .iter()
                            .map(|prefix| rpc::forge::PrefixFilterPolicyEntry {
                                prefix: prefix.to_string(),
                            })
                            .collect(),
                    }),
                    ..Default::default()
                })
                .rpc(),
        )
        .await;

    // Allocate an instance with a subset of the effective VPC override.
    let mut network_config =
        common::api_fixtures::instance::single_interface_network_config(segment_id);
    network_config.interfaces[0].routing_profile =
        Some(rpc::forge::InstanceInterfaceRoutingProfile {
            allowed_anycast_prefixes: interface_allowed_anycast_prefixes
                .iter()
                .map(|prefix| rpc::forge::PrefixFilterPolicyEntry {
                    prefix: prefix.to_string(),
                })
                .collect(),
        });
    let mh = create_managed_host(&env).await;
    mh.instance_builer(&env)
        .tenant_org(tenant.organization_id)
        .network(network_config)
        .build()
        .await;

    // Fetch the DPU network config and extract its split routing profiles.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(mh.dpu().id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    let vpc_routing_profile = response.tenant_interfaces[0]
        .vpc_routing_profile
        .clone()
        .unwrap();
    let interface_routing_profile = response.tenant_interfaces[0]
        .interface_routing_profile
        .clone()
        .unwrap();

    // Verify the VPC-level anycast prefixes remain unchanged.
    let actual_vpc_allowed_anycast_prefixes: Vec<_> = vpc_routing_profile
        .allowed_anycast_prefixes
        .clone()
        .into_iter()
        .map(|prefix| prefix.prefix)
        .collect();
    assert_eq!(
        actual_vpc_allowed_anycast_prefixes,
        vpc_allowed_anycast_prefixes.to_vec()
    );

    // Verify the interface-level anycast prefixes are sent separately.
    let actual_interface_allowed_anycast_prefixes: Vec<_> = interface_routing_profile
        .allowed_anycast_prefixes
        .into_iter()
        .map(|prefix| prefix.prefix)
        .collect();
    assert_eq!(
        actual_interface_allowed_anycast_prefixes,
        interface_allowed_anycast_prefixes
    );

    // Verify non-interface profile fields remain on the VPC profile.
    assert!(vpc_routing_profile.leak_default_route_from_underlay);
    assert_eq!(vpc_routing_profile.route_target_imports.len(), 1);
    assert_eq!(vpc_routing_profile.route_target_imports[0].asn, 65001);
    assert_eq!(vpc_routing_profile.route_target_imports[0].vni, 123);
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_includes_per_vpc_routing_profiles(pool: sqlx::PgPool) {
    let internal_import = RouteTargetConfig {
        asn: 65001,
        vni: 111,
    };
    let external_export = RouteTargetConfig {
        asn: 65002,
        vni: 222,
    };

    // Configure two distinguishable FNN routing profiles.
    let env = api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default().with_fnn_config(Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([
                (
                    "INTERNAL".to_string(),
                    FnnRoutingProfileConfig {
                        internal: Some(true),
                        access_tier: Some(1),
                        leak_default_route_from_underlay: Some(true),
                        route_target_imports: Some(vec![internal_import.clone()]),
                        ..Default::default()
                    },
                ),
                (
                    "EXTERNAL".to_string(),
                    FnnRoutingProfileConfig {
                        internal: Some(false),
                        access_tier: Some(2),
                        leak_tenant_host_routes_to_underlay: Some(true),
                        route_targets_on_exports: Some(vec![external_export.clone()]),
                        ..Default::default()
                    },
                ),
            ]),
            use_vpc_vrf_loopback: false,
        })),
    )
    .await;

    // Create a tenant that can use both routing profiles.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "per-vpc-routing-profile".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "per-vpc-routing-profile".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    // Create two FNN VPCs with different routing profiles.
    let internal_vpc = env
        .api
        .create_vpc(tonic::Request::new(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "internal profile vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        ))
        .await
        .unwrap()
        .into_inner();
    let internal_segment_id = create_tenant_network_segment(
        &env.api,
        internal_vpc.id,
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
        "TENANT_INTERNAL",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    let external_vpc = env
        .api
        .create_vpc(tonic::Request::new(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "external profile vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL".to_string())
                .rpc(),
        ))
        .await
        .unwrap()
        .into_inner();
    let external_segment_id = create_tenant_network_segment(
        &env.api,
        external_vpc.id,
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "TENANT_EXTERNAL",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    // Allocate an instance spanning both VPCs so each interface carries its own profile.
    let mh = create_managed_host(&env).await;
    mh.instance_builer(&env)
        .tenant_org(tenant.organization_id)
        .network(
            api_fixtures::instance::single_interface_network_config_with_vfs(vec![
                internal_segment_id,
                external_segment_id,
            ]),
        )
        .build()
        .await;

    // Fetch the DPU config and index profiles by the interface's actual VPC VNI.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(mh.dpu().id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    let mut txn = env.db_txn().await;
    let internal_vpc = db::vpc::find_by_segment(txn.as_mut(), internal_segment_id)
        .await
        .unwrap()
        .unwrap();
    let external_vpc = db::vpc::find_by_segment(txn.as_mut(), external_segment_id)
        .await
        .unwrap()
        .unwrap();
    let internal_vni = internal_vpc.status.vni.unwrap() as u32;
    let external_vni = external_vpc.status.vni.unwrap() as u32;
    let profiles_by_vni = response
        .tenant_interfaces
        .into_iter()
        .map(|interface| {
            (
                interface.vpc_vni,
                interface
                    .vpc_routing_profile
                    .expect("FNN interface should include a routing profile"),
            )
        })
        .collect::<HashMap<_, _>>();
    assert_eq!(profiles_by_vni.len(), 2);

    // Verify each VPC receives the profile resolved from that VPC, not the first VPC.
    let internal_profile = profiles_by_vni.get(&internal_vni).unwrap();
    assert!(internal_profile.leak_default_route_from_underlay);
    assert_eq!(
        internal_profile.route_target_imports,
        vec![rpc::common::RouteTarget {
            asn: internal_import.asn,
            vni: internal_import.vni,
        }]
    );

    let external_profile = profiles_by_vni.get(&external_vni).unwrap();
    assert!(external_profile.leak_tenant_host_routes_to_underlay);
    assert_eq!(
        external_profile.route_targets_on_exports,
        vec![rpc::common::RouteTarget {
            asn: external_export.asn,
            vni: external_export.vni,
        }]
    );
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_fnn_vrf_loopback_by_default(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides::default().with_fnn_config(None),
    )
    .await;

    // Create a tenant and FNN segment with the default disabled loopback setting.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "fnn-loopback-default".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "fnn-loopback-default".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "fnn loopback default vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;

    // Allocate a managed host on the FNN segment.
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu_ids[0];
    mh.instance_builer(&env)
        .tenant_org(tenant.organization_id)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Fetch the DPU config and verify no tenant VRF loopback is sent.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(!response.tenant_interfaces.is_empty());
    assert!(
        response
            .tenant_interfaces
            .iter()
            .all(|iface| iface.tenant_vrf_loopback_ip.is_none())
    );

    // Verify the DB did not allocate a VPC/DPU loopback row.
    let mut txn = env.db_txn().await;
    let vpc = db::vpc::find_by_segment(txn.as_mut(), segment_id)
        .await
        .unwrap()
        .unwrap();
    let loopback = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_machine_id, &vpc.id)
        .await
        .unwrap();
    assert!(loopback.is_none());
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_includes_fnn_vrf_loopback_when_enabled(
    pool: sqlx::PgPool,
) {
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().use_vpc_vrf_loopback = true;

    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;

    // Create a tenant and FNN segment with loopback allocation enabled.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "fnn-loopback-enabled".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(rpc::forge::Metadata {
                name: "fnn-loopback-enabled".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();

    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "fnn loopback enabled vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;

    // Allocate a managed host on the FNN segment.
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu_ids[0];
    mh.instance_builer(&env)
        .tenant_org(tenant.organization_id)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Fetch the DPU config and verify the tenant VRF loopback is sent.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    let tenant_interface = &response.tenant_interfaces[0];
    let loopback_ip = tenant_interface
        .tenant_vrf_loopback_ip
        .clone()
        .expect("loopback should be present when enabled");
    let loopback_address = tenant_interface
        .addresses
        .iter()
        .find(|address| address.address_family() == rpc::forge::AddressFamily::V4)
        .expect("IPv4 family entry should carry the tenant VRF loopback");
    assert_eq!(
        loopback_address.tenant_vrf_loopback_ip.as_deref(),
        Some(loopback_ip.as_str())
    );

    // Verify the DB allocation matches the response.
    let mut txn = env.db_txn().await;
    let vpc = db::vpc::find_by_segment(txn.as_mut(), segment_id)
        .await
        .unwrap()
        .unwrap();
    let loopback = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_machine_id, &vpc.id)
        .await
        .unwrap()
        .expect("loopback allocation should be persisted");
    assert_eq!(loopback.loopback_ip.to_string(), loopback_ip);
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_admin_fnn_vrf_loopback_by_default(
    pool: sqlx::PgPool,
) {
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        vpc_vni: Some(10000),
        routing_profile: FnnRoutingProfileConfig {
            leak_default_route_from_underlay: Some(true),
            route_target_imports: Some(vec![RouteTargetConfig {
                asn: 64512,
                vni: 10000,
            }]),
            ..Default::default()
        },
    });

    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;

    // Attach the FNN admin VPC because test env setup does not run production setup hooks.
    crate::db_init::create_admin_vpc(&env.pool, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();

    // Create a managed host that stays on the admin network.
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu_ids[0];

    // Fetch the DPU config and verify the FNN admin interface has no loopback.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    let admin_interface = response
        .admin_interface
        .as_ref()
        .expect("admin interface should be present");

    assert!(response.use_admin_network);
    assert_eq!(admin_interface.vpc_vni, 10000);
    assert!(admin_interface.tenant_vrf_loopback_ip.is_none());
    assert!(response.routing_profile.is_some());

    let routing_profile = admin_interface
        .vpc_routing_profile
        .as_ref()
        .expect("admin interface should include per-VPC routing profile");
    assert!(admin_interface.interface_routing_profile.is_none());
    assert!(routing_profile.leak_default_route_from_underlay);
    assert_eq!(routing_profile.route_target_imports.len(), 1);
    assert_eq!(routing_profile.route_target_imports[0].asn, 64512);
    assert_eq!(routing_profile.route_target_imports[0].vni, 10000);

    // Verify the admin VPC also did not allocate a VPC/DPU loopback row.
    let mut txn = env.db_txn().await;
    let admin_segment = db::network_segment::admin(txn.as_mut())
        .await
        .unwrap()
        .remove(0);
    let admin_vpc_id = admin_segment
        .config
        .vpc_id
        .expect("admin segment should be attached to an FNN VPC");
    let loopback = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_machine_id, &admin_vpc_id)
        .await
        .unwrap();
    assert!(loopback.is_none());
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_errors_when_sitewide_bgp_password_missing(
    pool: sqlx::PgPool,
) {
    // Enable site-wide DPU BGP passwords in runtime config without creating the credential.
    let mut config = api_fixtures::get_config();
    config.bgp_leaf_session_password = Some(crate::cfg::file::BgpLeafSessionPassword::SiteWide);

    let env =
        api_fixtures::create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config))
            .await;

    // Create a DPU without advancing to the point where the fixture fetches network config.
    // We'll fetch config next to validate the failure case.
    let host_config = env.managed_host_config();
    api_fixtures::site_explorer::register_expected_machine(&env, &host_config, None).await;

    let mock_explored_host = MockExploredHost::new(&env, host_config)
        .discover_dhcp_dpu_bmc(0, |_, _| Ok(()))
        .await
        .unwrap()
        .discover_dhcp_dpu_primary_iface(0)
        .await
        // Discover the host BMC and persist the exploration results.
        .discover_dhcp_host_bmc(|_, _| Ok(()))
        .await
        .unwrap()
        .insert_site_exploration_results()
        .unwrap()
        .run_site_explorer_iteration()
        .await
        .mark_preingestion_complete()
        .await
        .unwrap()
        .run_site_explorer_iteration()
        .await;

    let dpu_machine_id = mock_explored_host.dpu_machine_ids[&0];

    // Verify the handler fails when the site-wide BGP credential is missing.
    let err = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .expect_err("missing site-wide BGP password should fail");

    assert_eq!(err.code(), tonic::Code::Internal);
    assert!(err.message().contains("Could not find BGP credential"));
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_multi_dpu_fnn_ipv6_loopbacks(pool: sqlx::PgPool) {
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        vpc_vni: Some(10000),
        routing_profile: FnnRoutingProfileConfig::default(),
    });
    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;
    crate::db_init::create_admin_vpc(&env.pool, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    env.api
        .admin_grow_resource_pool(tonic::Request::new(rpc::forge::GrowResourcePoolRequest {
            text: r#"
[lo-ip-v6]
type = "ipv6"
prefix = "2001:db8:2390::/126"
"#
            .to_string(),
        }))
        .await
        .unwrap();

    let mh = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let host_machine = mh.host().rpc_machine().await;
    let dpu_ids = &host_machine
        .status
        .as_ref()
        .expect("host status")
        .associated_dpu_machine_ids;

    let dpu_1_network_config = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_ids[0]),
        }))
        .await
        .expect("DPU 1 network config")
        .into_inner();
    let dpu_2_network_config = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_ids[1]),
        }))
        .await
        .expect("DPU 2 network config")
        .into_inner();

    // Each response must use the requesting DPU's allocation even though both
    // responses share the host-level managed config version.
    let dpu_1_loopback_ip_v6 = dpu_1_network_config
        .managed_host_config
        .as_ref()
        .expect("DPU 1 managed host config")
        .loopback_ip_v6
        .as_deref()
        .expect("DPU 1 IPv6 loopback");
    let dpu_2_loopback_ip_v6 = dpu_2_network_config
        .managed_host_config
        .as_ref()
        .expect("DPU 2 managed host config")
        .loopback_ip_v6
        .as_deref()
        .expect("DPU 2 IPv6 loopback");
    assert_ne!(dpu_1_loopback_ip_v6, dpu_2_loopback_ip_v6);
    assert_eq!(
        dpu_1_network_config.managed_host_config_version,
        dpu_2_network_config.managed_host_config_version,
    );
}

#[crate::sqlx_test]
async fn test_managed_host_network_status(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Add an instance
    let instance_network = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: None,
            device: None,
            device_instance: 0u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    mh.instance_builer(&env)
        .network(instance_network)
        .build()
        .await;

    let response = env
        .api
        .get_all_managed_host_network_status(tonic::Request::new(
            ManagedHostNetworkStatusRequest {},
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.all.len(), 1);

    // Tell API about latest network config and machine health
    let dpu_health = rpc::health::HealthReport {
        source: "should-get-updated".to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![
            rpc::health::HealthProbeSuccess {
                id: "ContainerExists".to_string(),
                target: Some("c1".to_string()),
            },
            rpc::health::HealthProbeSuccess {
                id: "checkTwo".to_string(),
                target: None,
            },
        ],
        alerts: vec![],
    };
    network_configured_with_health(&env, &mh.dpu_ids[0], Some(dpu_health.clone())).await;

    // Query the aggregate health.
    let reported_health = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![mh.dpu().id.into()],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
        .status
        .unwrap()
        .health;
    let mut reported_health = reported_health.unwrap();
    assert!(reported_health.observed_at.is_some());
    reported_health.observed_at = None;
    reported_health.source = "should-get-updated".to_string();
    assert_eq!(reported_health, dpu_health);

    // Now fetch the instance and check that knows its configs have synced
    let response = env
        .api
        .find_instance_by_machine_id(tonic::Request::new(mh.id.into()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.instances.len(), 1);
    let instance = &response.instances[0];
    tracing::info!(
        network_config_version = %instance.network_config_version,
        "Instance network config version",
    );
    assert_eq!(
        instance.status.as_ref().unwrap().configs_synced,
        rpc::SyncState::Synced as i32
    );
}

fn create_extension_service_data(name: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27",
        name
    )
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_with_extension_services(pool: sqlx::PgPool) {
    let mut config = api_fixtures::get_config();
    config.dpf.enabled = true;
    let env =
        api_fixtures::create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config))
            .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let dpu_1_id = mh.dpu_ids[0];

    // Add an instance
    let instance_network = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: None,
            device: None,
            device_instance: 0u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    let default_tenant_org = "best_org";
    let _ = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: default_tenant_org.to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::forge::Metadata {
                name: default_tenant_org.to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Create extension services and add them to the instance
    let extension_service1 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "test1".to_string(),
            service_type: DpuExtensionServiceType::KubernetesPod as i32,
            tenant_organization_id: "best_org".to_string(),
            description: None,
            data: create_extension_service_data("test"),
            credential: None,
            observability: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let service1_version = extension_service1
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let extension_service2 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "test2".to_string(),
            service_type: DpuExtensionServiceType::KubernetesPod as i32,
            tenant_organization_id: "best_org".to_string(),
            description: None,
            data: create_extension_service_data("test2"),
            credential: None,
            observability: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let service2_version = extension_service2
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let es_config = InstanceDpuExtensionServicesConfig {
        service_configs: vec![
            InstanceDpuExtensionServiceConfig {
                service_id: extension_service1.service_id.clone(),
                version: service1_version.clone(),
            },
            InstanceDpuExtensionServiceConfig {
                service_id: extension_service2.service_id.clone(),
                version: service2_version.clone(),
            },
        ],
    };

    let _ = mh
        .instance_builer(&env)
        .network(instance_network)
        .extension_services(es_config)
        .build()
        .await;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_1_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.dpu_extension_services.len(), 2);
    assert_eq!(
        response.dpu_extension_services[0].service_id,
        extension_service1.service_id
    );
    assert_eq!(
        response.dpu_extension_services[0].version,
        service1_version.clone()
    );
    assert_eq!(response.dpu_extension_services[0].removed, None);

    assert_eq!(
        response.dpu_extension_services[1].service_id,
        extension_service2.service_id
    );
    assert_eq!(
        response.dpu_extension_services[1].version,
        service2_version.clone()
    );
    assert_eq!(response.dpu_extension_services[1].removed, None);

    let nested_extension_services = response
        .instance
        .as_ref()
        .and_then(|instance| instance.config.as_ref())
        .and_then(|config| config.dpu_extension_services.as_ref())
        .expect("agent-facing instance config retains the Kubernetes Pod services");
    assert_eq!(nested_extension_services.service_configs.len(), 2);
    assert!(
        nested_extension_services
            .service_configs
            .iter()
            .all(|service| {
                service.service_id == extension_service1.service_id
                    || service.service_id == extension_service2.service_id
            })
    );
}

#[crate::sqlx_test]
// This test reports health with the compatibility interface fields.
#[allow(deprecated)]
async fn test_dpu_health_is_required(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let (_host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id.into()),
        }))
        .await
        .unwrap()
        .into_inner();

    let admin_if = response.admin_interface.as_ref().unwrap();

    // dpu-health is not updated here
    let err = env
        .api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id.into()),
            dpu_agent_version: Some(dpu::TEST_DPU_AGENT_VERSION.to_string()),
            observed_at: Some(SystemTime::now().into()),
            dpu_health: None,
            network_config_version: Some(response.managed_host_config_version.clone()),
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![rpc::InstanceInterfaceStatusObservation {
                function_type: admin_if.function_type,
                virtual_function_id: None,
                mac_address: None,
                addresses: admin_if.ip.clone().into_iter().collect(),
                prefixes: admin_if.interface_prefix.clone().into_iter().collect(),
                gateways: admin_if.gateway.clone().into_iter().collect(),
                network_security_group: None,
                internal_uuid: None,
            }],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            fabric_interfaces: vec![],
            last_dhcp_requests: vec![],
            dpu_extension_service_version: Some("V1-T1".to_string()),
            dpu_extension_services: vec![],
            astra_config_status: None,
        }))
        .await
        .expect_err("Should fail");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "dpu_health");
}

/// Tests whether the in_alert_since field will be correctly populated
/// in case the DPU sends multiple reports using the same alarm
#[crate::sqlx_test]
async fn test_retain_in_alert_since(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let (_host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let dpu_health = rpc::health::HealthReport {
        source: "should-get-updated".to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![rpc::health::HealthProbeSuccess {
            id: "SuccessA".to_string(),
            target: None,
        }],
        alerts: vec![rpc::health::HealthProbeAlert {
            id: "AlertA".to_string(),
            target: None,
            in_alert_since: None,
            message: "AlertA".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_host_state_changes().to_string(),
            ],
        }],
    };

    network_configured_with_health(&env, &dpu_machine_id, Some(dpu_health.clone())).await;

    // Query the new HealthReport format
    let reported_health = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![dpu_machine_id.into()],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
        .status
        .unwrap()
        .health;

    let reported_health = reported_health.unwrap();
    assert!(reported_health.observed_at.is_some());
    assert_eq!(reported_health.successes.len(), 1);
    assert_eq!(reported_health.alerts.len(), 1);
    let mut reported_alert = reported_health.alerts[0].clone();
    assert!(reported_alert.in_alert_since.is_some());
    let in_alert_since = reported_alert.in_alert_since.unwrap();
    reported_alert.in_alert_since = None;
    assert_eq!(reported_alert, dpu_health.alerts[0].clone());

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Report health again. The in_alert_since date should not have been updated
    network_configured_with_health(&env, &dpu_machine_id, Some(dpu_health.clone())).await;
    let reported_health = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![dpu_machine_id.into()],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
        .status
        .unwrap()
        .health;
    let reported_health = reported_health.unwrap();
    assert!(reported_health.observed_at.is_some());
    assert_eq!(reported_health.successes.len(), 1);
    assert_eq!(reported_health.alerts.len(), 1);
    let mut reported_alert = reported_health.alerts[0].clone();
    assert_eq!(reported_alert.in_alert_since.unwrap(), in_alert_since);
    reported_alert.in_alert_since = None;
    assert_eq!(reported_alert, dpu_health.alerts[0].clone());
}

#[crate::sqlx_test]
async fn test_quarantine_state_crud(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = api_fixtures::create_test_env(pool).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let network_config_version =
        db::machine::get_network_config(env.pool.begin().await?.deref_mut(), &host_machine_id)
            .await?
            .version;

    // Get, make sure it's not set yet
    {
        let quarantine_state = env
            .api
            .get_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::GetManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                },
            ))
            .await?
            .into_inner()
            .quarantine_state;

        assert!(
            quarantine_state.is_none(),
            "new host should not be quarantined"
        );
    }

    // Make sure finding machine ID's in quarantine state does not include anything yet
    {
        let ids = env
            .api
            .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
                only_quarantine: true,
                ..Default::default()
            }))
            .await?
            .into_inner()
            .machine_ids;
        assert!(
            ids.is_empty(),
            "No machine ID's should be found in quarantine state yet"
        );
    }

    // Set it, make sure we get None back for prior state
    {
        let set_result = env
            .api
            .set_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::SetManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                    quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                        mode: rpc::forge::ManagedHostQuarantineMode::BlockAllTraffic as i32,
                        reason: Some("test reason 1".to_string()),
                    }),
                },
            ))
            .await?
            .into_inner();

        assert!(
            set_result.prior_quarantine_state.is_none(),
            "prior quarantine state should be None"
        );
    }

    // Make sure finding machine ID's in quarantine state includes the machine ID
    {
        let ids = env
            .api
            .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
                only_quarantine: true,
                ..Default::default()
            }))
            .await?
            .into_inner()
            .machine_ids;
        assert_eq!(
            ids,
            vec![host_machine_id.into()],
            "Finding machine ID's with only_quarantine should have returned the quarantined host"
        );
    }

    // Make sure the version got bumped
    let network_config =
        db::machine::get_network_config(env.pool.begin().await?.deref_mut(), &host_machine_id)
            .await?;
    assert_eq!(
        network_config.version.version_nr(),
        network_config_version.version_nr() + 1,
        "Setting quarantine should have bumped the network config version"
    );
    let network_config_version = network_config.version;

    // Make sure the DPU will see a mode saying to block all traffic
    assert_eq!(
        network_config.quarantine_state.as_ref().unwrap().mode,
        ManagedHostQuarantineMode::BlockAllTraffic
    );

    // Make sure we get back what we just set
    {
        let quarantine_state = env
            .api
            .get_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::GetManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                },
            ))
            .await?
            .into_inner()
            .quarantine_state;

        assert_eq!(
            quarantine_state
                .expect("we should get a quarantine state back after setting")
                .reason
                .expect("reason should be set")
                .as_str(),
            "test reason 1",
            "getting quarantine state should return the value we just set"
        );
    }

    // Set again, make sure the prior version matches what we set last time
    {
        let set_result = env
            .api
            .set_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::SetManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                    quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                        mode: rpc::forge::ManagedHostQuarantineMode::BlockAllTraffic as i32,
                        reason: Some("test reason 2".to_string()),
                    }),
                },
            ))
            .await?
            .into_inner();

        assert_eq!(
            set_result
                .prior_quarantine_state
                .expect("prior quarantine state should now be set")
                .reason,
            Some("test reason 1".to_string()),
            "prior quarantine state should match the first state we set"
        );
    }

    // Make sure the version got bumped again
    let network_config =
        db::machine::get_network_config(env.pool.begin().await?.deref_mut(), &host_machine_id)
            .await?;
    assert_eq!(
        network_config.version.version_nr(),
        network_config_version.version_nr() + 1,
        "Setting quarantine should have bumped the network config version"
    );
    let network_config_version = network_config.version;

    // Make sure the DPU will (still) see a mode saying to block all traffic
    assert_eq!(
        network_config.quarantine_state.as_ref().unwrap().mode,
        ManagedHostQuarantineMode::BlockAllTraffic
    );

    // Make sure we get back what we set again
    {
        let quarantine_state = env
            .api
            .get_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::GetManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                },
            ))
            .await?
            .into_inner()
            .quarantine_state;

        assert_eq!(
            quarantine_state
                .expect("we should get a quarantine state back after setting")
                .reason
                .expect("reason should be set")
                .as_str(),
            "test reason 2",
            "getting quarantine state should return the value we just set"
        );
    }

    // Clear, making sure we got back what we set last time
    {
        let clear_result = env
            .api
            .clear_managed_host_quarantine_state(tonic::Request::new(
                rpc::forge::ClearManagedHostQuarantineStateRequest {
                    machine_id: Some(host_machine_id.into()),
                },
            ))
            .await?
            .into_inner();

        assert_eq!(
            clear_result
                .prior_quarantine_state
                .expect("prior quarantine state should be set when clearing")
                .reason,
            Some("test reason 2".to_string()),
            "prior quarantine state should match the second state we set"
        );
    }

    // Make sure the network config version bumps again on clear
    let network_config =
        db::machine::get_network_config(env.pool.begin().await?.deref_mut(), &host_machine_id)
            .await?;
    assert_eq!(
        network_config.version.version_nr(),
        network_config_version.version_nr() + 1,
        "Clearing quarantine should have bumped the network config version"
    );

    // Make sure the DPU no longer sees a quarantine state
    assert!(network_config.quarantine_state.is_none());

    // Make sure finding machine ID's in quarantine state does not include anything any more
    {
        let ids = env
            .api
            .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
                only_quarantine: true,
                ..Default::default()
            }))
            .await?
            .into_inner()
            .machine_ids;
        assert!(
            ids.is_empty(),
            "No machine ID's should be found in quarantine state after clearing"
        );
    }

    Ok(())
}
