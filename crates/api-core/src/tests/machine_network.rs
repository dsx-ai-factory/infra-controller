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
use std::collections::{BTreeMap, HashMap};
use std::ops::DerefMut;
use std::time::SystemTime;

use ::rpc::forge::{
    CreateDpuExtensionServiceRequest, DpuExtensionServiceType, DpuNetworkStatus,
    InstanceDpuExtensionServiceConfig, InstanceDpuExtensionServicesConfig,
    ManagedHostNetworkConfigRequest, ManagedHostNetworkStatusRequest,
};
use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_dpf::DpuDeploymentType;
use carbide_instrument::testing::MetricsCapture;
use carbide_network::ip::IdentifyAddressFamily;
use carbide_secrets::credentials::{BgpCredentialType, CredentialKey, Credentials};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{AsMachineId, DpuMachineId, StableHostMachineId};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use common::api_fixtures::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY,
    create_tenant_network_segment, create_underlay_network_segment,
};
use common::api_fixtures::{self, create_managed_host, dpu, network_configured_with_health};
use config_version::ConfigVersion;
use db::ConditionalWrite;
use db::machine::AdminNetworkChangeNotPending;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedInterface, ExpectedInterfaceRole};
use model::instance::config::network::InstanceServiceInterfaceConfig;
use model::machine::network::ManagedHostQuarantineMode;
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
use rpc::Metadata;
use rpc::forge::forge_server::Forge;

use crate::cfg::file::{
    AdminFnnConfig, DpfBlueFieldSoftwareConfig, DpfDeploymentConfig, FnnConfig,
    FnnRoutingProfileConfig, PrefixFilterPolicyEntry, RouteTargetConfig,
};
use crate::network_segment::allocate::PrefixAllocator;
use crate::tests::common;
use crate::tests::common::api_fixtures::TestEnvOverrides;
use crate::tests::common::api_fixtures::site_explorer::MockExploredHost;
use crate::tests::common::rpc_builder::{DhcpDiscovery, VpcCreationRequest, VpcUpdateRequest};

/// Builds a nonempty IPv4 ingress rule so response tests can distinguish instance, initial VPC, and updated VPC policy.
fn service_vpc_nsg_rule(
    id: &str,
    source_prefix: &str,
) -> rpc::forge::NetworkSecurityGroupRuleAttributes {
    rpc::forge::NetworkSecurityGroupRuleAttributes {
        id: Some(id.to_string()),
        direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress.into(),
        ipv6: false,
        src_port_start: None,
        src_port_end: None,
        dst_port_start: None,
        dst_port_end: None,
        protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
        action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
        priority: 1_000,
        source_net: Some(
            rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                source_prefix.to_string(),
            ),
        ),
        destination_net: Some(
            rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                "0.0.0.0/0".to_string(),
            ),
        ),
    }
}

/// Replaces only persisted `service_interfaces` while checking `network_config_version` so missing-reference
/// tests retain host interfaces and reject writes after a concurrent version change.
async fn replace_service_vpc_endpoints(
    env: &api_fixtures::TestEnv,
    instance_id: InstanceId,
    endpoints: &[InstanceServiceInterfaceConfig],
) {
    // Replace only `service_interfaces` using the persisted `network_config_version`.
    let mut txn = env.db_txn().await;
    let stored = db::instance::find_by_id(txn.as_mut(), instance_id)
        .await
        .unwrap()
        .expect("fixture instance remains persisted");
    let mut network = stored.config.network;
    network.service_interfaces = endpoints.to_vec();
    db::instance::update_network_config(
        txn.as_mut(),
        instance_id,
        stored.network_config_version,
        &network,
        false,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Reload committed `service_interfaces` so an ineffective fixture write cannot satisfy a missing-reference test.
    let persisted = db::instance::find_by_id(&env.pool, instance_id)
        .await
        .unwrap()
        .expect("fixture instance remains persisted");
    assert_eq!(persisted.config.network.service_interfaces, endpoints);
}

mod authentication;

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
    assert_eq!(
        db::machine::try_update_network_config(txn.deref_mut(), &dpu_machine_id, version, &value)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
}

async fn record_dpu_network_status(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
    network_config_version: Option<String>,
) {
    env.api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
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
            lldp: None,
            lldp_snapshot_fingerprint: None,
        }))
        .await
        .unwrap();
}

/// Creates an FNN environment without default network segments.
///
/// Admin IPv6 projection tests control segment creation order because host allocation before or
/// after admin-VPC attachment is the behavior under test.
async fn create_admin_ipv6_test_env(pool: sqlx::PgPool) -> api_fixtures::TestEnv {
    let mut site_prefixes = api_fixtures::TEST_SITE_PREFIXES.to_vec();
    site_prefixes.push("2001:db8::/32".parse().unwrap());
    let mut overrides = TestEnvOverrides {
        site_prefixes: Some(site_prefixes),
        create_network_segments: Some(false),
        ..Default::default()
    }
    .with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        l3_only: false,
        vpc_vni: Some(10000),
        routing_profile: FnnRoutingProfileConfig::default(),
    });
    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;
    create_underlay_network_segment(&env.api).await;
    env
}

/// Creates a dual-stack admin segment with caller-controlled IPv6 reservation and VPC state.
///
/// The shared builder keeps normal projection, cross-segment reassignment, and pre-FNN `/127`
/// allocation on the same public segment-creation path.
async fn create_dual_stack_admin_segment(
    env: &api_fixtures::TestEnv,
    name: &str,
    ipv4_prefix: &str,
    ipv4_gateway: &str,
    ipv6_prefix: &str,
    ipv6_reserve_first: i32,
    vpc_id: Option<VpcId>,
) -> NetworkSegmentId {
    env.api
        .create_network_segment(tonic::Request::new(
            rpc::forge::NetworkSegmentCreationRequest {
                id: None,
                mtu: Some(1500),
                name: name.to_string(),
                prefixes: vec![
                    rpc::forge::NetworkPrefix {
                        id: None,
                        prefix: ipv4_prefix.to_string(),
                        gateway: Some(ipv4_gateway.to_string()),
                        reserve_first: 3,
                        free_ip_count: 0,
                        svi_ip: None,
                        free_ip_count_v2: None,
                        free_ip_count_saturated: false,
                        dhcpv6_link_address: None,
                    },
                    rpc::forge::NetworkPrefix {
                        id: None,
                        prefix: ipv6_prefix.to_string(),
                        gateway: None,
                        reserve_first: ipv6_reserve_first,
                        free_ip_count: 0,
                        svi_ip: None,
                        free_ip_count_v2: None,
                        free_ip_count_saturated: false,
                        dhcpv6_link_address: None,
                    },
                ],
                subdomain_id: Some(env.domain.into()),
                vpc_id,
                segment_type: rpc::forge::NetworkSegmentType::Admin as i32,
                infer_slaac_eui64_addresses: false,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .id
        .expect("created admin segment must have an id")
}

/// Reject either family move atomically, but allow it once no sibling is retained.
#[crate::sqlx_test]
async fn test_static_assignment_rejects_cross_segment_dual_stack(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_admin_ipv6_test_env(pool).await;
    let source_id = create_dual_stack_admin_segment(
        &env,
        "SOURCE",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:1::/64",
        3,
        None,
    )
    .await;
    let target_id = create_dual_stack_admin_segment(
        &env,
        "TARGET",
        "192.0.12.0/24",
        "192.0.12.1",
        "2001:db8:2::/64",
        3,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;

    struct Case {
        scenario: &'static str,
        mac: &'static str,
        ipv4: &'static str,
        ipv6: &'static str,
        requested: &'static str,
        sibling: &'static str,
    }
    for Case {
        scenario,
        mac,
        ipv4,
        ipv6,
        requested,
        sibling,
    } in [
        Case {
            scenario: "IPv4 move strands source IPv6",
            mac: "aa:bb:cc:dd:ee:30",
            ipv4: "192.0.2.42",
            ipv6: "2001:db8:1::42",
            requested: "192.0.12.42",
            sibling: "2001:db8:1::42",
        },
        Case {
            scenario: "IPv6 move strands source IPv4",
            mac: "aa:bb:cc:dd:ee:31",
            ipv4: "192.0.2.43",
            ipv6: "2001:db8:1::43",
            requested: "2001:db8:2::43",
            sibling: "192.0.2.43",
        },
    ] {
        let mut txn = env.db_txn().await;
        let interface = db::machine_interface::validate_existing_mac_and_create(
            &mut txn,
            mac.parse()?,
            &["192.0.2.1".parse()?],
            None,
            None,
        )
        .await?;
        db::machine_interface_address::delete(&mut txn, &interface.id).await?;
        for address in [ipv4, ipv6] {
            db::machine_interface_address::insert(
                &mut txn,
                interface.id,
                address.parse()?,
                model::allocation_type::AllocationType::Dhcp,
            )
            .await?;
        }
        let before = db::machine_interface::find_one(&mut *txn, interface.id).await?;
        assert_eq!(before.segment_id, source_id, "{scenario}");
        let before_allocations: serde_json::Value = sqlx::query_scalar(
            "SELECT jsonb_agg(to_jsonb(a) ORDER BY address) FROM machine_interface_addresses a WHERE interface_id = $1",
        )
        .bind(interface.id)
        .fetch_one(&mut *txn)
        .await?;
        txn.commit().await?;

        let error = env
            .api
            .assign_static_address(tonic::Request::new(
                rpc::forge::AssignStaticAddressRequest {
                    interface_id: Some(interface.id),
                    ip_address: requested.to_string(),
                },
            ))
            .await
            .expect_err("a cross-segment move must not retain an out-of-segment sibling");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");

        // Reload independently after the rejected RPC, including allocation types and timestamps.
        let mut txn = env.db_txn().await;
        let after = db::machine_interface::find_one(&mut *txn, interface.id).await?;
        assert_eq!(after.segment_id, before.segment_id, "{scenario}");
        assert_eq!(after.domain_id, before.domain_id, "{scenario}");
        assert_eq!(after.hostname, before.hostname, "{scenario}");
        let after_allocations: serde_json::Value = sqlx::query_scalar(
            "SELECT jsonb_agg(to_jsonb(a) ORDER BY address) FROM machine_interface_addresses a WHERE interface_id = $1",
        )
        .bind(interface.id)
        .fetch_one(&mut *txn)
        .await?;
        assert_eq!(after_allocations, before_allocations, "{scenario}");
        txn.rollback().await?;

        let same_segment_address = if requested.parse::<std::net::IpAddr>()?.is_ipv4() {
            ipv4
        } else {
            ipv6
        };
        env.api
            .assign_static_address(tonic::Request::new(
                rpc::forge::AssignStaticAddressRequest {
                    interface_id: Some(interface.id),
                    ip_address: same_segment_address.to_string(),
                },
            ))
            .await?;
        let mut txn = env.db_txn().await;
        let same_segment = db::machine_interface::find_one(&mut *txn, interface.id).await?;
        assert_eq!(same_segment.segment_id, source_id, "{scenario}");
        assert_eq!(same_segment.addresses.len(), 2, "{scenario}");
        assert!(
            same_segment.addresses.contains(&sibling.parse()?),
            "{scenario}"
        );
        db::machine_interface_address::delete_by_interface_family(
            &mut txn,
            interface.id,
            sibling.parse::<std::net::IpAddr>()?.address_family(),
            model::allocation_type::AllocationType::Dhcp,
        )
        .await?;
        txn.commit().await?;
        env.api
            .assign_static_address(tonic::Request::new(
                rpc::forge::AssignStaticAddressRequest {
                    interface_id: Some(interface.id),
                    ip_address: requested.to_string(),
                },
            ))
            .await?;
        let mut txn = env.db_txn().await;
        let moved = db::machine_interface::find_one(&mut *txn, interface.id).await?;
        assert_eq!(moved.segment_id, target_id, "{scenario}");
        assert_eq!(
            moved.addresses,
            vec![requested.parse::<std::net::IpAddr>()?],
            "{scenario}"
        );
        txn.rollback().await?;
    }
    Ok(())
}

/// A sibling committed while assignment waits must participate in the segment check.
#[crate::sqlx_test]
async fn test_static_assignment_rechecks_sibling_after_waiting(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_admin_ipv6_test_env(pool.clone()).await;
    let source_id = create_dual_stack_admin_segment(
        &env,
        "SOURCE",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:1::/64",
        3,
        None,
    )
    .await;
    create_dual_stack_admin_segment(
        &env,
        "TARGET",
        "192.0.12.0/24",
        "192.0.12.1",
        "2001:db8:2::/64",
        3,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let interface = db::machine_interface::validate_existing_mac_and_create(
        &mut txn,
        "aa:bb:cc:dd:ee:32".parse()?,
        &["192.0.2.1".parse()?],
        None,
        None,
    )
    .await?;
    db::machine_interface_address::delete(&mut txn, &interface.id).await?;
    db::machine_interface_address::insert(
        &mut txn,
        interface.id,
        "192.0.2.44".parse()?,
        model::allocation_type::AllocationType::Dhcp,
    )
    .await?;
    txn.commit().await?;

    let mut holder = env.db_txn().await;
    db::machine_interface::lock_for_address_assignment(&mut holder, interface.id).await?;
    db::machine_interface_address::insert(
        &mut holder,
        interface.id,
        "2001:db8:1::44".parse()?,
        model::allocation_type::AllocationType::Dhcp,
    )
    .await?;
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await?;
    let assignment = env.api.assign_static_address(tonic::Request::new(
        rpc::forge::AssignStaticAddressRequest {
            interface_id: Some(interface.id),
            ip_address: "192.0.12.44".to_string(),
        },
    ));
    let commit_sibling = async {
        db::test_support::postgres::wait_for_blocked_query(
            &pool,
            holder_pid,
            "SELECT segment_id FROM machine_interfaces",
        )
        .await;
        holder.commit().await
    };
    let (assignment, sibling_commit) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(assignment, commit_sibling)
        })
        .await?;
    sibling_commit?;
    assert_eq!(
        assignment
            .expect_err("the newly committed sibling must prevent the move")
            .code(),
        tonic::Code::FailedPrecondition,
    );
    let mut txn = env.db_txn().await;
    let after = db::machine_interface::find_one(&mut *txn, interface.id).await?;
    assert_eq!(after.segment_id, source_id);
    assert_eq!(after.addresses.len(), 2);
    assert!(after.addresses.contains(&"192.0.2.44".parse()?));
    assert!(after.addresses.contains(&"2001:db8:1::44".parse()?));
    txn.rollback().await?;
    Ok(())
}

/// Configures service slots, one optional BF4 deployment, FNN loopbacks, IPv6 deny prefixes, and site prefixes
/// so response tests can select deployment configuration and render IPv4 and IPv6 service interfaces.
async fn create_service_vpc_test_env(
    pool: sqlx::PgPool,
    slot_count: u32,
    use_vpc_vrf_loopback: bool,
    configured_bf4_deployment: Option<DpuDeploymentType>,
) -> api_fixtures::TestEnv {
    let mut config = api_fixtures::get_config();
    config.dpu_config.service_vpc_slot_count = slot_count;
    config.site_global_vpc_vni = None;
    config.deny_prefixes = vec!["2001:db8:ffff::/64".parse().unwrap()];
    // Supply only the selected BF4 entry so inventory tests cannot borrow another deployment's configuration.
    let bf4_deployment = |name: &str| DpfDeploymentConfig {
        bfb_url: None,
        bluefield_software: Some(DpfBlueFieldSoftwareConfig {
            os_iso: format!("http://example.com/{name}.iso"),
            pldm_fw_bundle: BTreeMap::from([(
                "test-psid".to_string(),
                format!("http://example.com/{name}.pldm"),
            )]),
        }),
        flavor_name: format!("test-{name}-flavor"),
        deployment_name: format!("test-{name}"),
        node_label_key: format!("test.nvidia.com/{name}"),
        ..Default::default()
    };
    config.dpf.deployment_scoped_service_interfaces =
        configured_bf4_deployment == Some(DpuDeploymentType::Bf4Astra);
    config.dpf.deployments.bf4_generic = (configured_bf4_deployment
        == Some(DpuDeploymentType::Bf4Generic))
    .then(|| bf4_deployment("bf4-generic"));
    config.dpf.deployments.bf4_astra = (configured_bf4_deployment
        == Some(DpuDeploymentType::Bf4Astra))
    .then(|| bf4_deployment("bf4-astra"));
    let mut site_prefixes = api_fixtures::TEST_SITE_PREFIXES.to_vec();
    site_prefixes.push("2001:db8::/32".parse().unwrap());
    let mut overrides = TestEnvOverrides::with_config(config).with_fnn_config(None);
    overrides.site_prefixes = Some(site_prefixes);
    let fnn = overrides.fnn_config.as_mut().unwrap();
    fnn.use_vpc_vrf_loopback = use_vpc_vrf_loopback;
    // Give the service VPC a wire-visible profile difference from the host VPC.
    fnn.routing_profiles
        .get_mut("EXTERNAL")
        .unwrap()
        .leak_tenant_host_routes_to_underlay = Some(true);
    api_fixtures::create_test_env_with_overrides(pool, overrides).await
}

/// Persists the DPU product name used by deployment classification so inventory tests can select or withhold that evidence.
async fn set_dpu_product_name(
    env: &api_fixtures::TestEnv,
    dpu_id: DpuMachineId,
    product_name: &str,
) {
    let mut txn = env.db_txn().await;
    let dpu = db::machine::find_one(txn.as_mut(), &dpu_id, Default::default())
        .await
        .unwrap()
        .expect("fixture DPU remains persisted");
    let mut hardware_info = dpu
        .status
        .hardware_info
        .expect("the fixture DPU includes hardware information");
    hardware_info
        .dmi_data
        .as_mut()
        .expect("the fixture DPU includes DMI information")
        .product_name = product_name.to_string();

    // Reopen topology ingestion before publishing the updated DMI product name.
    db::machine_topology::set_topology_update_needed(txn.as_mut(), &dpu.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(txn.as_mut(), &dpu.id, &hardware_info)
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

/// Marks a fixture host as DPF-ingested so inventory tests exercise the persisted provisioning classification.
async fn mark_host_dpf_ingested(env: &api_fixtures::TestEnv, host_id: StableHostMachineId) {
    // Persist `dpf.used_for_ingestion` before checking service-slot classification.
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &host_id)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Reload the host so an ineffective fixture write cannot satisfy an inventory test.
    let host = db::machine::find_one(&mut env.db_reader(), &host_id, Default::default())
        .await
        .unwrap()
        .expect("fixture host remains persisted");
    assert!(host.config.dpf.used_for_ingestion);
}

/// Persists a generated service segment and returns its endpoint record so response tests do not depend on the activation handler.
#[allow(clippy::too_many_arguments)]
async fn create_service_vpc_endpoint(
    env: &api_fixtures::TestEnv,
    vpc_id: VpcId,
    vpc_prefix: IpNetwork,
    link_prefix: IpNetwork,
    dpu_id: DpuMachineId,
    attachment_id: uuid::Uuid,
    interface_ordinal: u32,
    slot_index: u32,
    mac_address: MacAddress,
) -> InstanceServiceInterfaceConfig {
    let vpc_prefix_id = VpcPrefixId::new();

    // Create the selected VPC prefix through the public API before allocating its exact linknet.
    env.api
        .create_vpc_prefix(tonic::Request::new(rpc::forge::VpcPrefixCreationRequest {
            id: Some(vpc_prefix_id),
            prefix: String::new(),
            vpc_id: Some(vpc_id),
            config: Some(rpc::forge::VpcPrefixConfig {
                prefix: vpc_prefix.to_string(),
            }),
            metadata: Some(Metadata {
                name: format!("service VPC prefix {vpc_prefix}"),
                ..Default::default()
            }),
            site_prefix_id: None,
        }))
        .await
        .unwrap();

    // Use the production allocator to preserve Ready state, MTU, gateway, and reservation rules.
    let mut txn = env.db_txn().await;
    let allocator =
        PrefixAllocator::new(vpc_prefix_id, vpc_prefix, None, link_prefix.prefix()).unwrap();
    let (network_segment_id, _) = allocator
        .allocate_network_segment_for_prefix(txn.as_mut(), vpc_id, link_prefix)
        .await
        .unwrap();
    let segment = db::network_segment::find_by(
        txn.as_mut(),
        db::ObjectColumnFilter::One(db::network_segment::IdColumn, &network_segment_id),
        Default::default(),
    )
    .await
    .unwrap()
    .pop()
    .expect("generated service segment is persisted");
    let network_prefix_id = segment
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix == link_prefix)
        .map(|prefix| prefix.id)
        .expect("generated service segment contains its exact link prefix");
    txn.commit().await.unwrap();

    InstanceServiceInterfaceConfig {
        attachment_id,
        interface_ordinal,
        dpu_id,
        slot_index,
        vpc_id,
        vpc_prefix_id,
        network_segment_id,
        network_prefix_id,
        link_prefix,
        mac_address,
        internal_uuid: uuid::Uuid::new_v4(),
    }
}

/// Keeps the allocated host and committed IPv4/IPv6 endpoint records together for independent response tests.
struct ServiceVpcEndpointFixture {
    tenant: rpc::forge::Tenant,
    managed_host: api_fixtures::TestManagedHost,
    instance_id: InstanceId,
    service_vpc_id: VpcId,
    service_vni: u32,
    instance_network_security_group: rpc::forge::NetworkSecurityGroup,
    ipv4_endpoint: InstanceServiceInterfaceConfig,
    ipv6_endpoint: InstanceServiceInterfaceConfig,
    ipv4_service_id: carbide_uuid::extension_service::ExtensionServiceId,
    ipv6_service_id: carbide_uuid::extension_service::ExtensionServiceId,
}

/// Creates a two-DPU instance with one IPv4 and one IPv6 endpoint in a service VPC distinct from its host VPC.
/// The separate host/service routing profiles and nonempty instance NSG let tests detect host policy leaking into service configuration.
/// Callers must configure FNN and at least two service slots.
async fn create_service_vpc_endpoint_fixture(
    env: &api_fixtures::TestEnv,
) -> ServiceVpcEndpointFixture {
    // Create one tenant with distinct host and service VPCs.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "service-vpc-projection".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(Metadata {
                name: "service-vpc-projection".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();
    let host_segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "host primary VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;
    // Use a wire-visible EXTERNAL profile so service routing cannot be mistaken for host policy.
    let service_vpc = env
        .api
        .create_vpc(tonic::Request::new(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "selected service VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL".to_string())
                .rpc(),
        ))
        .await
        .unwrap()
        .into_inner();
    let service_vpc_id = service_vpc.id.expect("created service VPC has an ID");
    let service_vni = service_vpc
        .status
        .expect("created service VPC has status")
        .vni
        .expect("created service VPC has a VNI");

    // Give the host interface a distinct nonempty instance policy before allocation.
    let instance_network_security_group = env
        .api
        .create_network_security_group(tonic::Request::new(
            rpc::forge::CreateNetworkSecurityGroupRequest {
                id: None,
                tenant_organization_id: tenant.organization_id.clone(),
                metadata: Some(Metadata {
                    name: "instance policy".to_string(),
                    ..Default::default()
                }),
                network_security_group_attributes: Some(
                    rpc::forge::NetworkSecurityGroupAttributes {
                        rules: vec![service_vpc_nsg_rule("instance-rule", "10.0.0.0/8")],
                        stateful_egress: false,
                    },
                ),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .network_security_group
        .expect("created instance NSG is returned");

    // Allocate the host on another VPC, then commit one endpoint per DPU and address family.
    let managed_host = api_fixtures::create_managed_host_multi_dpu(env, 2).await;
    mark_host_dpf_ingested(env, managed_host.id).await;
    let instance_id = managed_host
        .instance_builer(env)
        .tenant_org(tenant.organization_id.clone())
        .config(rpc::forge::InstanceConfig {
            os: Some(api_fixtures::instance::default_os_config()),
            network: Some(api_fixtures::instance::single_interface_network_config(
                host_segment_id,
            )),
            network_security_group_id: Some(instance_network_security_group.id.clone()),
            ..Default::default()
        })
        .build()
        .await
        .id;
    let ipv4_attachment_id = uuid::Uuid::new_v4();
    let ipv6_attachment_id = uuid::Uuid::new_v4();
    let ipv4_endpoint = create_service_vpc_endpoint(
        env,
        service_vpc_id,
        "192.2.4.0/24".parse().unwrap(),
        "192.2.4.0/31".parse().unwrap(),
        managed_host.dpu_ids[0],
        ipv4_attachment_id,
        0,
        0,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x41]),
    )
    .await;
    let ipv6_endpoint = create_service_vpc_endpoint(
        env,
        service_vpc_id,
        "2001:db8:200::/64".parse().unwrap(),
        "2001:db8:200::/127".parse().unwrap(),
        managed_host.dpu_ids[1],
        ipv6_attachment_id,
        0,
        1,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x42]),
    )
    .await;
    let ipv4_service_id = api_fixtures::instance::persist_service_vpc_attachment(
        env,
        instance_id,
        tenant.organization_id.clone(),
        vec![ipv4_endpoint.clone()],
    )
    .await;
    let ipv6_service_id = api_fixtures::instance::persist_service_vpc_attachment(
        env,
        instance_id,
        tenant.organization_id.clone(),
        vec![ipv6_endpoint.clone()],
    )
    .await;

    ServiceVpcEndpointFixture {
        tenant,
        managed_host,
        instance_id,
        service_vpc_id,
        service_vni,
        instance_network_security_group,
        ipv4_endpoint,
        ipv6_endpoint,
        ipv4_service_id,
        ipv6_service_id,
    }
}

/// Changes the persisted host network mode so one test can prove the primary-Admin exception and fresh Tenant restoration.
async fn set_host_use_admin_network(
    env: &api_fixtures::TestEnv,
    host_id: StableHostMachineId,
    use_admin_network: bool,
) {
    // Change only `use_admin_network` through the current config version to preserve optimistic concurrency.
    let mut txn = env.db_txn().await;
    let host = db::machine::find_one(txn.as_mut(), &host_id, Default::default())
        .await
        .unwrap()
        .expect("fixture host remains persisted");
    let mut network_config = host.network_config.value;
    network_config.use_admin_network = Some(use_admin_network);
    assert_eq!(
        db::machine::try_update_network_config(
            txn.as_mut(),
            &host_id,
            host.network_config.version,
            &network_config,
        )
        .await
        .unwrap(),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_clear_use_admin_network_changed_requires_pending_version(pool: sqlx::PgPool) {
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

    let current_version =
        db::machine::find_one(&mut env.db_reader(), &dpu_machine_id, Default::default())
            .await
            .unwrap()
            .unwrap()
            .network_config
            .version;

    struct Case {
        scenario: &'static str,
        acknowledged_version: ConfigVersion,
        expected: ConditionalWrite<(), AdminNetworkChangeNotPending>,
        flag_after: bool,
    }
    // Each step uses the previous step's committed flag.
    for case in [
        Case {
            scenario: "stale acknowledgment keeps the newer flag",
            acknowledged_version: stale_version,
            expected: ConditionalWrite::NotApplied(AdminNetworkChangeNotPending),
            flag_after: true,
        },
        Case {
            scenario: "matching acknowledgment clears the flag",
            acknowledged_version: current_version,
            expected: ConditionalWrite::Applied(()),
            flag_after: false,
        },
        Case {
            scenario: "repeated acknowledgment has nothing to clear",
            acknowledged_version: current_version,
            expected: ConditionalWrite::NotApplied(AdminNetworkChangeNotPending),
            flag_after: false,
        },
    ] {
        let mut txn = env.db_txn().await;
        let result = db::machine::clear_use_admin_network_changed_if_version_matches(
            txn.deref_mut(),
            &dpu_machine_id,
            &case.acknowledged_version,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(result, case.expected, "{}", case.scenario);

        let dpu = db::machine::find_one(&mut env.db_reader(), &dpu_machine_id, Default::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            dpu.network_config.value.use_admin_network_changed,
            Some(case.flag_after),
            "{}",
            case.scenario
        );
        assert_eq!(
            dpu.network_config.version, current_version,
            "{}",
            case.scenario
        );
    }
}

/// Verifies successful responses for known DPUs carry inventory while unsupported or unresolved deployments never advertise invented slots.
/// This prevents DPU agents from creating listeners that persisted provisioning evidence cannot support.
#[crate::sqlx_test]
async fn service_vpc_inventory_is_present_without_an_instance(pool: sqlx::PgPool) {
    let env =
        create_service_vpc_test_env(pool, 2, false, Some(DpuDeploymentType::Bf4Generic)).await;

    // Create a non-DPF host and clear its persisted DPU product name.
    let non_dpf_host = create_managed_host(&env).await;
    set_dpu_product_name(&env, non_dpf_host.dpu().id, "").await;
    let non_dpf_response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(non_dpf_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    // A non-DPF host without product evidence still has a known empty inventory.
    assert!(
        non_dpf_response
            .service_vpc_slot_inventory
            .expect("known target always carries slot inventory")
            .slots
            .is_empty()
    );

    // Fetch before marking DPF ingestion to compare inventory without changing `managed_host_config_version`.
    let dpf_host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let before_ingestion = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpf_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    let unchanged_version = before_ingestion.managed_host_config_version;
    assert!(
        before_ingestion
            .service_vpc_slot_inventory
            .expect("non-DPF target carries known inventory")
            .slots
            .is_empty()
    );

    // Mark the host DPF-ingested and fetch again without assigning an instance or VPC.
    mark_host_dpf_ingested(&env, dpf_host.id).await;
    let dpf_response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpf_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(dpf_response.managed_host_config_version, unchanged_version);

    // Names and indices come from ServiceVpcSlots rather than a second naming formula.
    let inventory = dpf_response
        .service_vpc_slot_inventory
        .expect("DPF target carries configured inventory");
    assert_eq!(
        inventory.slots,
        vec![
            rpc::forge::ServiceVpcSlot {
                slot_index: 0,
                hbn_interface_name: "iface_svc_0".to_string(),
            },
            rpc::forge::ServiceVpcSlot {
                slot_index: 1,
                hbn_interface_name: "iface_svc_1".to_string(),
            },
        ]
    );
    assert!(dpf_response.service_interfaces.is_empty());

    // A secondary DPU without an instance remains on Admin while retaining fixed inventory.
    let secondary_response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpf_host.dpu_ids[1]),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(secondary_response.instance_id.is_none());
    assert!(secondary_response.use_admin_network);
    assert!(secondary_response.service_interfaces.is_empty());
    assert_eq!(
        secondary_response
            .service_vpc_slot_inventory
            .expect("secondary DPU retains configured inventory without an instance")
            .slots,
        inventory.slots
    );

    // BF4 without a CX9 declaration selects BF4 Generic and retains the configured inventory.
    set_dpu_product_name(&env, dpf_host.dpu().id, "BlueField-4 SmartNIC Main Card").await;
    let bf4_generic_response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpf_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        bf4_generic_response
            .service_vpc_slot_inventory
            .expect("BF4 Generic carries configured inventory")
            .slots,
        inventory.slots
    );

    // Removing the persisted product name makes deployment classification unknown rather than zero-capacity.
    set_dpu_product_name(&env, dpf_host.dpu().id, "").await;
    let unresolved = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpf_host.dpu().id),
        }))
        .await
        .expect_err("missing DPU product evidence must not become zero capacity");
    assert_eq!(unresolved.code(), tonic::Code::FailedPrecondition);
    assert!(unresolved.message().contains("product name is absent"));

    // BF4 Astra must not borrow the configured BF4 Generic deployment.
    let astra_host = super::instance::create_bf4_astra_host(&env, 0x61).await;
    let unconfigured_astra = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(astra_host.dpu().id),
        }))
        .await
        .expect_err("unconfigured BF4 Astra must not borrow BF4 Generic inventory");
    assert_eq!(unconfigured_astra.code(), tonic::Code::FailedPrecondition);
    assert!(
        unconfigured_astra
            .message()
            .contains("no DPUDeployment is configured for Bf4Astra")
    );
}

/// Verifies BF4 Generic cannot borrow Astra configuration while configured Astra returns its known empty inventory.
/// This prevents the configured service-VPC slot count from advertising listeners that provisioning never configured on that DPU.
#[crate::sqlx_test]
async fn service_vpc_inventory_rejects_unconfigured_bf4_deployment(pool: sqlx::PgPool) {
    // Retain global capacity with only the BF4 Astra deployment configured.
    let env = create_service_vpc_test_env(pool, 2, false, Some(DpuDeploymentType::Bf4Astra)).await;

    // Set the host's `dpf.used_for_ingestion` marker and the DPU's BF4 Generic product name.
    let managed_host = create_managed_host(&env).await;
    mark_host_dpf_ingested(&env, managed_host.id).await;
    set_dpu_product_name(
        &env,
        managed_host.dpu().id,
        "BlueField-4 SmartNIC Main Card",
    )
    .await;

    // The missing deployment entry must withhold invented listener inventory.
    let error = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .expect_err("unconfigured BF4 Generic must not inherit BF3 slots");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("no DPUDeployment is configured for Bf4Generic")
    );

    // The configured Astra class remains a known excluded platform with empty inventory.
    let astra_host = super::instance::create_bf4_astra_host(&env, 0x62).await;
    let astra_response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(astra_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        astra_response
            .service_vpc_slot_inventory
            .expect("configured Astra carries known inventory")
            .slots
            .is_empty()
    );
}

/// Verifies zero service capacity remains known and empty after an ingested Astra host's expected-machine declaration is erased.
/// This keeps service-slot classification from withholding host networking when no service slots are configured.
#[crate::sqlx_test]
async fn service_vpc_configured_zero_is_known_empty(pool: sqlx::PgPool) {
    // Configure zero slots and only Astra so erasing CX9 evidence would select an unconfigured Generic deployment.
    let env = create_service_vpc_test_env(pool, 0, false, Some(DpuDeploymentType::Bf4Astra)).await;
    let managed_host = super::instance::create_bf4_astra_host(&env, 0x63).await;

    // Fetch the ingested DPU before removing its expected-machine declaration.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        response
            .service_vpc_slot_inventory
            .expect("configured zero remains known inventory")
            .slots
            .is_empty()
    );

    // Erase declarations through the same API used by expected-machines erase.
    env.api
        .delete_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("expected-machine erase succeeds after ingestion");

    // Reload declarations to verify the erase committed before fetching network configuration.
    let erased = env
        .api
        .get_all_expected_machines(tonic::Request::new(()))
        .await
        .expect("expected-machine declarations remain readable")
        .into_inner();
    assert!(erased.expected_machines.is_empty());

    // Fetch again to prove declaration removal does not withhold host networking at zero capacity.
    let after_erase = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .expect("zero service capacity does not require an expected-machine declaration")
        .into_inner();
    assert!(
        after_erase
            .service_vpc_slot_inventory
            .expect("configured zero remains known inventory after erase")
            .slots
            .is_empty()
    );
}

#[crate::sqlx_test]
async fn test_managed_host_network_config_machine_identity(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let message = ManagedHostNetworkConfigRequest {
        dpu_machine_id: Some(mh.dpu().id),
    };
    let expected = env
        .api
        .get_managed_host_network_config(tonic::Request::new(message.clone()))
        .await
        .unwrap()
        .into_inner();

    for (scenario, principal, expected_code) in [
        (
            "matching DPU",
            Principal::SpiffeMachineIdentifier(mh.dpu().id.to_string()),
            tonic::Code::Ok,
        ),
        (
            "sibling DPU",
            Principal::SpiffeMachineIdentifier(mh.dpu_n(1).id.to_string()),
            tonic::Code::PermissionDenied,
        ),
        (
            "host credentials",
            Principal::SpiffeMachineIdentifier(mh.id.to_string()),
            tonic::Code::PermissionDenied,
        ),
        (
            "malformed machine identity",
            Principal::SpiffeMachineIdentifier("invalid-machine-id".to_string()),
            tonic::Code::PermissionDenied,
        ),
        (
            "administrative delegation",
            Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "nico-cli-client".to_string(),
                None,
            )),
            tonic::Code::Ok,
        ),
        (
            "service delegation",
            Principal::SpiffeServiceIdentifier("elektra-site-agent".to_string()),
            tonic::Code::Ok,
        ),
    ] {
        let mut request = tonic::Request::new(message.clone());
        let mut context = crate::auth::AuthContext::default();
        context.principals.push(principal);
        request.extensions_mut().insert(context);

        match env.api.get_managed_host_network_config(request).await {
            Ok(response) => {
                assert_eq!(expected_code, tonic::Code::Ok, "{scenario}");
                assert_eq!(response.into_inner(), expected, "{scenario}");
            }
            Err(status) => assert_eq!(status.code(), expected_code, "{scenario}: {status}"),
        }
    }
}

#[crate::sqlx_test]
// This test verifies parity between `addresses` and the compatibility fields,
// plus presence-bearing site policy carried in the same response.
#[allow(deprecated)]
async fn test_managed_host_network_config(pool: sqlx::PgPool) {
    // The default fixture omits `lo-ip-v6`, which must preserve the existing IPv4-only response.
    let mut config = api_fixtures::get_config();
    config.dhcpv6_server_preference = Some(0);
    let env =
        api_fixtures::create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config))
            .await;
    let host_config = env.managed_host_config();
    let mh = dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = mh.dpu().id;

    // Fetch a Machines network config
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        response.instance.is_none(),
        "a host without an instance must omit the nested instance"
    );
    assert!(
        response
            .managed_host_config
            .as_ref()
            .expect("managed host config")
            .loopback_ip_v6
            .is_none(),
        "sites without lo-ip-v6 must remain IPv4-only"
    );
    assert_eq!(response.dhcpv6_server_preference, Some(0));

    let admin_interface = response.admin_interface.expect("admin interface");
    assert!(admin_interface.ipv6_interface_config.is_none());
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

/// Verifies complete FNN dual-stack projection, non-FNN IPv4-only gating, and a degraded
/// V6-host/no-SVI response, because canonical, compatibility, and valid IPv4 must agree.
/// Enabling L3-only admin must change only attachment mode so allocated addresses stay stable.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_projects_dual_stack_admin_addresses(pool: sqlx::PgPool) {
    // Attach a normal dual-stack segment to the admin VPC before provisioning the host.
    let env = create_admin_ipv6_test_env(pool).await;
    let admin_segment_id = create_dual_stack_admin_segment(
        &env,
        "DUAL_STACK_ADMIN",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:9::/64",
        3,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();

    // Provisioning runs admin-address reconciliation, so both families must already be persisted
    // before the response is assembled.
    let host_config = env.managed_host_config();
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;

    // Read the primary interface through the database API to prove provisioning persisted exactly
    // one address per family rather than trusting its return value.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        admin_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|interface| interface.primary_interface)
    .expect("primary admin interface");
    txn.rollback().await.unwrap();
    assert_eq!(persisted_interface.addresses.len(), 2);
    let persisted_ipv4 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv4())
        .expect("persisted IPv4 address");
    let persisted_ipv6 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv6())
        .expect("persisted IPv6 address");

    // Fetch through the public RPC and verify IPv4 compatibility plus complete, matching IPv6
    // projections in deterministic V4-then-V6 order.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    let admin_interface = response.admin_interface.expect("admin interface");
    let ipv4 = persisted_ipv4.to_string();
    let ipv6 = persisted_ipv6.to_string();
    let ipv4_host_route = format!("{ipv4}/32");
    let ipv6_host_route = format!("{ipv6}/128");
    assert_eq!(admin_interface.gateway.as_deref(), Some("192.0.2.1/24"));
    assert_eq!(admin_interface.ip.as_deref(), Some(ipv4.as_str()));
    assert_eq!(
        admin_interface.interface_prefix.as_deref(),
        Some(ipv4_host_route.as_str())
    );
    assert_eq!(admin_interface.vpc_prefixes, vec![ipv4_host_route.clone()]);
    assert_eq!(admin_interface.prefix.as_deref(), Some("192.0.2.0/24"));
    assert_eq!(admin_interface.svi_ip.as_deref(), Some("192.0.2.2/24"));
    assert_eq!(
        admin_interface.ipv6_interface_config,
        Some(rpc::forge::FlatInterfaceIpv6Config {
            ip: ipv6.clone(),
            interface_prefix: ipv6_host_route.clone(),
            svi_ip: Some("2001:db8:9::2/64".to_string()),
        })
    );
    let expected_ipv4_address = rpc::forge::InterfaceAddressConfig {
        address_family: rpc::forge::AddressFamily::V4.into(),
        ip: ipv4,
        interface_prefix: ipv4_host_route,
        prefix: "192.0.2.0/24".to_string(),
        gateway: Some("192.0.2.1/24".to_string()),
        svi_ip: Some("192.0.2.2/24".to_string()),
        tenant_vrf_loopback_ip: None,
    };
    assert_eq!(
        admin_interface.addresses,
        vec![
            expected_ipv4_address.clone(),
            rpc::forge::InterfaceAddressConfig {
                address_family: rpc::forge::AddressFamily::V6.into(),
                ip: ipv6,
                interface_prefix: ipv6_host_route,
                prefix: "2001:db8:9::/64".to_string(),
                gateway: None,
                svi_ip: Some("2001:db8:9::2/64".to_string()),
                tenant_vrf_loopback_ip: None,
            },
        ]
    );

    // Enable routed admin in a second API using the same database and persisted host.
    let mut routed_config = env.api.runtime_config.as_ref().clone();
    routed_config
        .fnn
        .as_mut()
        .expect("FNN configuration")
        .admin_vpc
        .as_mut()
        .expect("admin FNN configuration")
        .l3_only = true;
    let routed_api = crate::test_support::builder::TestApiBuilder::new(
        env.pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(std::sync::Arc::new(routed_config))
    .build();

    // Fetch the same host through the public RPC with L3-only admin enabled.
    let routed_response = routed_api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .expect("routed admin configuration")
        .into_inner();
    // Compare every admin response field except the attachment choice.
    assert!(admin_interface.is_l2_segment);
    let mut expected_routed_interface = admin_interface.clone();
    expected_routed_interface.is_l2_segment = false;
    assert_eq!(
        routed_response.admin_interface,
        Some(expected_routed_interface)
    );

    // Reload the host interface after the routed configuration response.
    let retained_interface = db::machine_interface::find_one(&env.pool, persisted_interface.id)
        .await
        .expect("persisted admin interface");
    // The same interface must retain its allocated addresses.
    assert_eq!(retained_interface.addresses, persisted_interface.addresses);

    // The same allocated segment must remain IPv4-only when admin FNN is disabled, even though
    // the database contains a complete IPv6 prefix/address pair.
    let mut txn = env.db_txn().await;
    let mut snapshot = managed_host.snapshot(&mut txn).await;
    let booturl = None;
    let (non_fnn_admin_interface, _) = crate::ethernet_virtualization::admin_network(
        txn.as_mut(),
        &snapshot,
        &dpu_machine_id,
        crate::ethernet_virtualization::AdminNetworkOptions {
            fnn_enabled: false,
            l3_only: false,
            common_pools: &env.common_pools,
            booturl: &booturl,
            use_vpc_vrf_loopback: false,
            routing_profile: None,
        },
    )
    .await
    .unwrap();
    txn.rollback().await.unwrap();
    assert!(non_fnn_admin_interface.ipv6_interface_config.is_none());
    let [non_fnn_address] = non_fnn_admin_interface.addresses.as_slice() else {
        panic!("non-FNN admin must expose exactly one canonical address")
    };
    assert_eq!(
        non_fnn_address.address_family(),
        rpc::forge::AddressFamily::V4
    );
    assert_eq!(
        non_fnn_address.ip.as_str(),
        non_fnn_admin_interface.ip.as_deref().unwrap()
    );

    // A dual-stack segment still requires its host IPv4 allocation, even when
    // the IPv6 configuration could otherwise provide a complete response.
    let active_interface = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter_mut()
        .find(|interface| interface.id == persisted_interface.id)
        .unwrap();
    active_interface.addresses.retain(std::net::IpAddr::is_ipv6);
    assert_eq!(active_interface.addresses, vec![persisted_ipv6]);
    let mut txn = env.db_txn().await;
    let error = crate::ethernet_virtualization::admin_network(
        txn.as_mut(),
        &snapshot,
        &dpu_machine_id,
        crate::ethernet_virtualization::AdminNetworkOptions {
            fnn_enabled: true,
            l3_only: false,
            common_pools: &env.common_pools,
            booturl: &booturl,
            use_vpc_vrf_loopback: false,
            routing_profile: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("no IPv4 address found"));
    txn.rollback().await.unwrap();

    // Model a failed IPv6 SVI backfill while retaining the allocated host address. The incomplete
    // IPv6 role set must not prevent Core from returning the valid IPv4 config.
    let mut txn = env.db_txn().await;
    let cleared_svi = sqlx::query(
        "UPDATE network_prefixes SET svi_ip = NULL \
         WHERE segment_id = $1 AND family(prefix) = 6",
    )
    .bind(admin_segment_id)
    .execute(txn.as_mut())
    .await
    .unwrap();
    assert_eq!(cleared_svi.rows_affected(), 1);
    txn.commit().await.unwrap();

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .expect("missing IPv6 SVI must not break an IPv4-only admin response")
        .into_inner();
    let admin_interface = response.admin_interface.expect("admin interface");
    assert!(admin_interface.ipv6_interface_config.is_none());
    assert_eq!(admin_interface.addresses, vec![expected_ipv4_address]);
}

/// Gatewayless admin definitions must survive DPU discovery and produce stateful DHCPv6 config.
/// Unrelated relay metadata must not redirect the host allocation away from its admin segment.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_supports_ipv6_only_admin(pool: sqlx::PgPool) {
    let env = create_admin_ipv6_test_env(pool).await;
    let config: crate::cfg::file::InitialObjectsConfig = toml::from_str(
        r#"
            [networks.ipv6-admin]
            type = "admin"
            prefix = "2001:db8:9::/64"
            mtu = 1500
            reserve_first = 3

            [networks.unrelated-ipv6-underlay]
            type = "underlay"
            prefix = "2001:db8:a::/64"
            dhcpv6_link_address = "2001:db8:9::"
            mtu = 1500
            reserve_first = 3
        "#,
    )
    .unwrap();
    crate::db_init::create_initial_networks(&env.api, &env.pool, &config.networks.unwrap())
        .await
        .unwrap();
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();

    let mut txn = env.db_txn().await;
    let segment = db::network_segment::find_by_name(&mut txn, "ipv6-admin")
        .await
        .unwrap();
    let [prefix] = segment.prefixes.as_slice() else {
        panic!("IPv6-only admin segment must have exactly one prefix")
    };
    assert!(prefix.gateway.is_none());
    assert!(prefix.dhcpv6_link_address.is_none());
    assert_eq!(prefix.svi_ip, Some("2001:db8:9::2".parse().unwrap()));
    let unrelated = db::network_segment::find_by_name(&mut txn, "unrelated-ipv6-underlay")
        .await
        .unwrap();
    assert_eq!(
        unrelated.prefixes[0].dhcpv6_link_address,
        Some(prefix.prefix.network())
    );
    txn.rollback().await.unwrap();

    let mut host_config = env.managed_host_config();
    let dpu_oob_mac = host_config.get_and_assert_single_dpu().oob_mac_address;
    // The fixture's IPv4 admin relay is absent here. Declare the underlay DPU
    // OS identity before DHCP so site exploration does not probe it as a BMC.
    host_config
        .expected_machine_data
        .get_or_insert_with(Default::default)
        .interfaces
        .push(ExpectedInterface {
            mac_address: Some(dpu_oob_mac),
            role: ExpectedInterfaceRole::DpuOs,
            ..Default::default()
        });
    api_fixtures::site_explorer::register_expected_machine(&env, &host_config, None).await;
    let dpu_config = host_config.get_and_assert_single_dpu();
    env.api
        .discover_dhcp(
            DhcpDiscovery::builder(
                dpu_config.oob_mac_address,
                FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.ip(),
            )
            .tonic_request(),
        )
        .await
        .expect("DPU out-of-band DHCP must succeed on the IPv4 underlay");
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;
    let mut txn = env.db_txn().await;
    let dpu_interfaces =
        db::machine_interface::find_by_mac_address(txn.as_mut(), dpu_config.oob_mac_address)
            .await
            .unwrap();
    let [dpu_interface] = dpu_interfaces.as_slice() else {
        panic!("DPU discovery requires exactly one out-of-band interface")
    };
    txn.rollback().await.unwrap();

    // Site exploration creates the host link before the fixture submits DPU
    // discovery. Check the public RPC explicitly so that earlier setup cannot
    // hide a failure to rediscover an IPv6-only admin interface.
    assert_eq!(
        dpu::dpu_discover_machine(&env, dpu_config, dpu_interface.id).await,
        dpu_machine_id.into()
    );

    let mut txn = env.db_txn().await;
    let interfaces = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        segment.id,
    )
    .await
    .unwrap();
    txn.rollback().await.unwrap();
    let [interface] = interfaces.as_slice() else {
        panic!("discovery must persist exactly one host admin interface")
    };
    assert!(interface.primary_interface);
    assert_eq!(interface.attached_dpu_machine_id, Some(dpu_machine_id));
    let [std::net::IpAddr::V6(address)] = interface.addresses.as_slice() else {
        panic!("IPv6-only admin interface must have exactly one IPv6 address")
    };
    assert!(prefix.prefix.contains((*address).into()));
    let host_route = format!("{address}/128");

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.use_admin_network);
    assert!(response.enable_dhcp);
    assert!(response.is_primary_dpu);
    assert_eq!(response.host_interface_id, Some(interface.id.to_string()));
    let admin = response.admin_interface.as_ref().expect("admin interface");
    assert!(admin.ip.is_none());
    assert!(admin.gateway.is_none());
    assert!(admin.prefix.is_none());
    assert!(admin.interface_prefix.is_none());
    assert!(admin.svi_ip.is_none());
    assert!(admin.vpc_prefixes.is_empty());
    assert_eq!(
        admin.ipv6_interface_config,
        Some(rpc::forge::FlatInterfaceIpv6Config {
            ip: address.to_string(),
            interface_prefix: host_route.clone(),
            svi_ip: Some("2001:db8:9::2/64".to_string()),
        })
    );
    assert_eq!(
        admin.addresses,
        vec![rpc::forge::InterfaceAddressConfig {
            address_family: rpc::forge::AddressFamily::V6.into(),
            ip: address.to_string(),
            interface_prefix: host_route.clone(),
            prefix: "2001:db8:9::/64".to_string(),
            gateway: None,
            svi_ip: Some("2001:db8:9::2/64".to_string()),
            tenant_vrf_loopback_ip: None,
        }]
    );

    // The public response must produce a stateful DHCPv6 binding without requiring IPv4.
    let vlan = format!("vlan{}", admin.vlan_id);
    let dhcp =
        carbide_rpc_utils::dhcp::HostConfig::try_from(response, "pf0hpf_if", "pf0vf", "_if", true)
            .unwrap();
    assert_eq!(dhcp.host_interface_id, interface.id);
    assert_eq!(dhcp.host_ip_addresses.len(), 1);
    let dhcp_interface = &dhcp.host_ip_addresses[&vlan];
    assert!(dhcp_interface.address.is_none());
    assert!(dhcp_interface.gateway.is_none());
    assert!(dhcp_interface.prefix.is_none());
    assert_eq!(
        dhcp_interface.ipv6,
        Some(carbide_rpc_utils::dhcp::InterfaceInfoV6 {
            address: Some(*address),
            prefix: host_route,
        })
    );

    // Without FNN there is no IPv6 admin SVI path, even for this complete segment.
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    let error = crate::ethernet_virtualization::admin_network(
        txn.as_mut(),
        &snapshot,
        &dpu_machine_id,
        crate::ethernet_virtualization::AdminNetworkOptions {
            fnn_enabled: false,
            l3_only: false,
            common_pools: &env.common_pools,
            booturl: &None,
            use_vpc_vrf_loopback: false,
            routing_profile: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("has no IPv4 network prefix"));
    txn.rollback().await.unwrap();

    // A failed SVI backfill has no IPv4 fallback here; returning an empty admin config would
    // leave the host without either DHCPv6 routing or a usable IPv4 configuration.
    let cleared = sqlx::query("UPDATE network_prefixes SET svi_ip = NULL WHERE segment_id = $1")
        .bind(segment.id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(cleared.rows_affected(), 1);
    let error = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("no complete IPv6 configuration"));
}

/// Verifies Core never combines a retained IPv6 address with a different admin segment.
///
/// Static reassignment and production reconciliation operate per address family, so the response
/// boundary must suppress stale IPv6 rather than publishing a host from one segment with another
/// segment's prefix and SVI.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_ipv6_outside_reassigned_admin_segment(
    pool: sqlx::PgPool,
) {
    // Provision the host while the source is the only admin segment.
    let env = create_admin_ipv6_test_env(pool).await;
    let source_segment_id = create_dual_stack_admin_segment(
        &env,
        "DUAL_STACK_ADMIN_SOURCE",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:9::/64",
        3,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    let host_config = env.managed_host_config();
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;

    // Capture the primary interface, its source IPv6 address, and the shared admin VPC.
    let mut txn = env.db_txn().await;
    let source_interface = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        source_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|interface| interface.primary_interface)
    .expect("primary source admin interface");
    let primary_interface_id = source_interface.id;
    let source_ipv6 = source_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv6())
        .expect("source admin interface must have IPv6");
    let admin_vpc_id = db::vpc::find_by_segment(txn.as_mut(), source_segment_id)
        .await
        .unwrap()
        .expect("source admin segment must belong to the admin VPC")
        .id;
    txn.rollback().await.unwrap();

    // Add a second dual-stack admin segment to the same FNN VPC.
    let target_segment_id = create_dual_stack_admin_segment(
        &env,
        "DUAL_STACK_ADMIN_TARGET",
        "192.0.12.0/24",
        "192.0.12.1",
        "2001:db8:12::/64",
        3,
        Some(admin_vpc_id),
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();

    // Simulate legacy inconsistent storage. The public assignment API now rejects this move.
    let target_ipv4: std::net::IpAddr = "192.0.12.42".parse().unwrap();
    let mut txn = env.db_txn().await;
    db::machine_interface_address::assign_static(&mut txn, primary_interface_id, target_ipv4)
        .await
        .unwrap();
    db::machine_interface::update_segment_id(
        &mut txn,
        primary_interface_id,
        target_segment_id,
        Some(env.domain.into()),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Production reconciliation must not make the stale sibling-family allocation appear valid.
    let mut txn = env.db_txn().await;
    db::machine_interface::reconcile_admin_addresses_for_host(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Re-read persistence to establish the inconsistent but reachable projection input.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_one(txn.as_mut(), primary_interface_id)
        .await
        .unwrap();
    txn.rollback().await.unwrap();
    assert_eq!(persisted_interface.segment_id, target_segment_id);
    assert_eq!(
        persisted_interface
            .addresses
            .iter()
            .copied()
            .find(|address| address.is_ipv4()),
        Some(target_ipv4)
    );
    assert!(
        persisted_interface.addresses.contains(&source_ipv6),
        "reconciliation currently preserves the existing sibling-family address"
    );

    // The public payload must retain target IPv4 while omitting both IPv6 representations.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    let admin_interface = response.admin_interface.expect("admin interface");
    assert_eq!(admin_interface.ip.as_deref(), Some("192.0.12.42"));
    assert_eq!(
        admin_interface.interface_prefix.as_deref(),
        Some("192.0.12.42/32")
    );
    assert_eq!(admin_interface.prefix.as_deref(), Some("192.0.12.0/24"));
    assert_eq!(admin_interface.gateway.as_deref(), Some("192.0.12.1/24"));
    assert_eq!(admin_interface.svi_ip.as_deref(), Some("192.0.12.2/24"));
    assert!(admin_interface.ipv6_interface_config.is_none());
    let [canonical_ipv4] = admin_interface.addresses.as_slice() else {
        panic!("reassigned admin interface must expose only canonical IPv4")
    };
    assert_eq!(
        canonical_ipv4.address_family(),
        rpc::forge::AddressFamily::V4
    );
    assert_eq!(canonical_ipv4.ip, "192.0.12.42");
    assert_eq!(canonical_ipv4.interface_prefix, "192.0.12.42/32");
    assert_eq!(canonical_ipv4.prefix, "192.0.12.0/24");
    assert_eq!(canonical_ipv4.gateway.as_deref(), Some("192.0.12.1/24"));
    assert_eq!(canonical_ipv4.svi_ip.as_deref(), Some("192.0.12.2/24"));
}

/// Verifies Core omits admin IPv6 when the host owns the address later used for VRR.
///
/// A `/127` allocated before FNN attachment has only enough endpoints for the host and SVI, so
/// publishing its network endpoint as VRR would duplicate a live host address.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_admin_ipv6_when_host_owns_vrr_address(
    pool: sqlx::PgPool,
) {
    // Allocate the host before admin-FNN startup attaches the segment and backfills its SVI.
    let env = create_admin_ipv6_test_env(pool).await;
    let admin_segment_id = create_dual_stack_admin_segment(
        &env,
        "ADMIN_IPV6_127",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:9::/127",
        0,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let host_config = env.managed_host_config();
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;

    // Re-read persistence to prove the host took the network endpoint before an SVI existed.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        admin_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|interface| interface.primary_interface)
    .expect("primary admin interface");
    let persisted_ipv4 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv4())
        .expect("persisted IPv4 address");
    let persisted_ipv6 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(|address| address.is_ipv6())
        .expect("persisted IPv6 address");
    let persisted_admin_segment = db::network_segment::admin(txn.as_mut())
        .await
        .unwrap()
        .into_iter()
        .find(|segment| segment.id == admin_segment_id)
        .expect("persisted admin segment");
    let ipv6_prefix = persisted_admin_segment
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv6())
        .expect("persisted IPv6 prefix");
    assert_eq!(persisted_ipv6, ipv6_prefix.prefix.network());
    assert!(ipv6_prefix.svi_ip.is_none());
    txn.rollback().await.unwrap();

    // Production startup attaches the existing segment and assigns its only other endpoint to SVI.
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    let mut txn = env.db_txn().await;
    let persisted_admin_segment = db::network_segment::admin(txn.as_mut())
        .await
        .unwrap()
        .into_iter()
        .find(|segment| segment.id == admin_segment_id)
        .expect("persisted admin segment");
    let ipv6_prefix = persisted_admin_segment
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv6())
        .expect("persisted IPv6 prefix");
    assert_eq!(ipv6_prefix.svi_ip, Some("2001:db8:9::1".parse().unwrap()));
    txn.rollback().await.unwrap();

    // The public payload retains IPv4 but suppresses the conflicting sidecar and canonical V6.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    let admin_interface = response.admin_interface.expect("admin interface");
    let ipv4 = persisted_ipv4.to_string();
    let ipv4_host_route = format!("{persisted_ipv4}/32");
    assert_eq!(admin_interface.gateway.as_deref(), Some("192.0.2.1/24"));
    assert_eq!(admin_interface.ip.as_deref(), Some(ipv4.as_str()));
    assert_eq!(
        admin_interface.interface_prefix.as_deref(),
        Some(ipv4_host_route.as_str())
    );
    assert_eq!(admin_interface.vpc_prefixes, vec![ipv4_host_route.clone()]);
    assert_eq!(admin_interface.prefix.as_deref(), Some("192.0.2.0/24"));
    assert_eq!(admin_interface.svi_ip.as_deref(), Some("192.0.2.2/24"));
    assert!(admin_interface.ipv6_interface_config.is_none());
    assert_eq!(
        admin_interface.addresses,
        vec![rpc::forge::InterfaceAddressConfig {
            address_family: rpc::forge::AddressFamily::V4.into(),
            ip: ipv4,
            interface_prefix: ipv4_host_route,
            prefix: "192.0.2.0/24".to_string(),
            gateway: Some("192.0.2.1/24".to_string()),
            svi_ip: Some("192.0.2.2/24".to_string()),
            tenant_vrf_loopback_ip: None,
        }]
    );
}

/// Verifies collision handling omits both IPv6 projections without hiding persisted IPv4.
///
/// Each collision test proves its distinct persisted role conflict before this helper checks the
/// common public response contract.
#[allow(deprecated)]
async fn assert_admin_ipv6_omitted_while_ipv4_survives(
    env: &api_fixtures::TestEnv,
    dpu_machine_id: DpuMachineId,
    persisted_ipv4: std::net::IpAddr,
) {
    // Fetch through the public RPC after establishing the persisted collision.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    let admin_interface = response.admin_interface.expect("admin interface");
    let ipv4 = persisted_ipv4.to_string();

    // Both compatibility and canonical projections must omit IPv6 while retaining IPv4.
    assert_eq!(admin_interface.ip.as_deref(), Some(ipv4.as_str()));
    assert!(admin_interface.ipv6_interface_config.is_none());
    let [canonical_ipv4] = admin_interface.addresses.as_slice() else {
        panic!("conflicting admin IPv6 must leave exactly one canonical IPv4 address")
    };
    assert_eq!(
        canonical_ipv4.address_family(),
        rpc::forge::AddressFamily::V4
    );
    assert_eq!(canonical_ipv4.ip, ipv4);
}

/// Verifies Core omits admin IPv6 when the persisted SVI equals the VRR address.
///
/// Allocating an SVI before a host on an unreserved `/127` gives the SVI the network endpoint, so
/// projection must not program that same endpoint as VRR.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_admin_ipv6_when_svi_matches_vrr_address(
    pool: sqlx::PgPool,
) {
    // Attach the `/127` to FNN and allocate its SVI before provisioning the host.
    let env = create_admin_ipv6_test_env(pool).await;
    let admin_segment_id = create_dual_stack_admin_segment(
        &env,
        "ADMIN_IPV6_SVI_VRR_COLLISION",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:9::/127",
        0,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();

    // Provisioning must use the remaining endpoint while preserving valid IPv4.
    let host_config = env.managed_host_config();
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;

    // Re-read persistence to prove this case exercises SVI=VRR independently.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        admin_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|interface| interface.primary_interface)
    .expect("primary admin interface");
    let persisted_ipv4 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(std::net::IpAddr::is_ipv4)
        .expect("persisted IPv4 address");
    let persisted_ipv6 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(std::net::IpAddr::is_ipv6)
        .expect("persisted IPv6 address");
    let persisted_admin_segment = db::network_segment::admin(txn.as_mut())
        .await
        .unwrap()
        .into_iter()
        .find(|segment| segment.id == admin_segment_id)
        .expect("persisted admin segment");
    let ipv6_prefix = persisted_admin_segment
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv6())
        .expect("persisted IPv6 prefix");
    let persisted_svi = ipv6_prefix.svi_ip.expect("persisted IPv6 SVI");
    assert_eq!(persisted_svi, ipv6_prefix.prefix.network());
    assert_ne!(persisted_ipv6, persisted_svi);
    txn.rollback().await.unwrap();

    assert_admin_ipv6_omitted_while_ipv4_survives(&env, dpu_machine_id, persisted_ipv4).await;
}

/// Verifies Core omits admin IPv6 when public static assignment gives the host the SVI address.
///
/// SVI storage is separate from machine-address ownership, so the response boundary must reject
/// this reachable collision while continuing to return the interface's IPv4 configuration.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_admin_ipv6_when_host_matches_svi_address(
    pool: sqlx::PgPool,
) {
    // Provision a normal dual-stack FNN admin interface with an existing IPv6 SVI.
    let env = create_admin_ipv6_test_env(pool).await;
    let admin_segment_id = create_dual_stack_admin_segment(
        &env,
        "ADMIN_IPV6_HOST_SVI_COLLISION",
        "192.0.2.0/24",
        "192.0.2.1",
        "2001:db8:9::/64",
        3,
        None,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
        .await
        .unwrap();
    crate::db_init::update_network_segments_svi_ip(&env.pool)
        .await
        .unwrap();
    let host_config = env.managed_host_config();
    let managed_host =
        dpu::create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    let dpu_machine_id = managed_host.dpu().id;

    // Read the public-assignment target and interface identity from persistence.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_by_machine_and_segment(
        txn.as_mut(),
        managed_host.id.as_machine_id(),
        admin_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|interface| interface.primary_interface)
    .expect("primary admin interface");
    let primary_interface_id = persisted_interface.id;
    let persisted_admin_segment = db::network_segment::admin(txn.as_mut())
        .await
        .unwrap()
        .into_iter()
        .find(|segment| segment.id == admin_segment_id)
        .expect("persisted admin segment");
    let persisted_svi = persisted_admin_segment
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv6())
        .and_then(|prefix| prefix.svi_ip)
        .expect("persisted IPv6 SVI");
    txn.rollback().await.unwrap();

    // Public static assignment can select an SVI because it is not a machine-address owner.
    env.api
        .assign_static_address(tonic::Request::new(
            rpc::forge::AssignStaticAddressRequest {
                interface_id: Some(primary_interface_id),
                ip_address: persisted_svi.to_string(),
            },
        ))
        .await
        .unwrap();

    // Re-read persistence to prove this case exercises host=SVI independently.
    let mut txn = env.db_txn().await;
    let persisted_interface = db::machine_interface::find_one(txn.as_mut(), primary_interface_id)
        .await
        .unwrap();
    let persisted_ipv4 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(std::net::IpAddr::is_ipv4)
        .expect("persisted IPv4 address");
    let persisted_ipv6 = persisted_interface
        .addresses
        .iter()
        .copied()
        .find(std::net::IpAddr::is_ipv6)
        .expect("persisted IPv6 address");
    assert_eq!(persisted_interface.segment_id, admin_segment_id);
    assert_eq!(persisted_ipv6, persisted_svi);
    txn.rollback().await.unwrap();

    assert_admin_ipv6_omitted_while_ipv4_survives(&env, dpu_machine_id, persisted_ipv4).await;
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
            dpu_machine_id: Some(dpu_machine_id),
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
async fn test_record_dpu_network_status_requires_existing_dpu(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let dpu_machine_id: DpuMachineId =
        "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
            .parse()
            .unwrap();

    let error = env
        .api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
            dpu_agent_version: Some(dpu::TEST_DPU_AGENT_VERSION.to_string()),
            observed_at: Some(SystemTime::now().into()),
            network_config_version: Some(ConfigVersion::initial().version_string()),
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(
        error.message(),
        format!("machine not found: {dpu_machine_id}")
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
            dpu_machine_id: Some(dpu_machine_id),
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
async fn test_rejected_network_observation_does_not_acknowledge_admin_network_change(
    pool: sqlx::PgPool,
) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let dpu_machine_id = mh.dpu().id;
    record_dpu_network_status(&env, dpu_machine_id, None).await;
    set_use_admin_network_changed(&env, dpu_machine_id, true).await;
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT network_status_observation FROM machines WHERE id = $1")
            .bind(dpu_machine_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();

    // The configuration version matches, but this report is older than the
    // persisted observation. It must not acknowledge the network change.
    let error = env
        .api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
            network_config_version: Some(response.managed_host_config_version),
            observed_at: Some(SystemTime::UNIX_EPOCH.into()),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(error.message(), "internal database error");
    let source = std::error::Error::source(&error)
        .expect("database status source")
        .source()
        .expect("original database error")
        .downcast_ref::<db::DatabaseError>()
        .expect("typed database error");
    let db::DatabaseError::Sqlx(annotated) = source else {
        panic!("expected the original database error: {source:?}");
    };
    assert!(matches!(annotated.source, sqlx::Error::RowNotFound));
    assert!(
        annotated
            .to_string()
            .contains("update machine status observation"),
        "{annotated}"
    );
    assert_eq!(
        use_admin_network_changed(&env, dpu_machine_id).await,
        Some(true)
    );
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT network_status_observation FROM machines WHERE id = $1")
            .bind(dpu_machine_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(after, before);
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
    let network_config_version = "V2-T1".to_string();

    // Remove the state-handler queue table so the host wakeup enqueue fails
    // while the status report itself stays healthy.
    sqlx::query("DROP TABLE machine_state_controller_queued_objects")
        .execute(&pool)
        .await
        .unwrap();

    let metrics = MetricsCapture::start();
    // A never-before-observed network config version registers as a change,
    // which is what prompts the host state-handler wakeup.
    record_dpu_network_status(&env, dpu_machine_id, Some(network_config_version.clone())).await;

    assert_eq!(
        metrics.counter_delta(
            "carbide_state_handler_wakeup_failures_total",
            &[("trigger", "dpu_network_status")],
        ),
        1.0
    );

    // The first report committed despite the failed wakeup. Its versions
    // now match, so another report must not enqueue another host wakeup.
    record_dpu_network_status(&env, dpu_machine_id, Some(network_config_version)).await;

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
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        response.bgp_leaf_session_password,
        Some("test-bgp-password".to_string())
    );
}

/// Verifies legacy global route targets with omitted VNIs become explicit zero
/// values in both managed-host response fields before reaching the DPU.
#[crate::sqlx_test]
async fn test_managed_host_network_config_normalizes_legacy_global_route_targets(
    pool: sqlx::PgPool,
) {
    // Omit each local administrator while distinct ASNs identify both fields.
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    let fnn_config = overrides
        .fnn_config
        .as_mut()
        .expect("fixture FNN configuration");
    fnn_config.common_internal_route_target = Some(RouteTargetConfig {
        asn: 64_512,
        vni: None,
        vni_self: false,
    });
    fnn_config.additional_route_target_imports = vec![RouteTargetConfig {
        asn: 64_513,
        vni: None,
        vni_self: false,
    }];
    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = dpu::create_dpu_machine(&env, &host_config).await;

    // Fetch the response carrying both global route-target fields.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();

    // Both fields must contain concrete zero rather than omitted legacy values.
    assert_eq!(
        response.common_internal_route_target,
        Some(rpc::common::RouteTarget {
            asn: 64_512,
            vni: Some(0),
            vni_self: false,
        })
    );
    assert_eq!(
        response.additional_route_target_imports,
        vec![rpc::common::RouteTarget {
            asn: 64_513,
            vni: Some(0),
            vni_self: false,
        }]
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
        vni: Some(123),
        vni_self: false,
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
            dpu_machine_id: Some(mh.dpu().id),
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
    assert_eq!(vpc_routing_profile.route_target_imports[0].vni, Some(123));
    assert!(!vpc_routing_profile.route_target_imports[0].vni_self);
}

/// Verifies Core resolves one symbolic target independently for each interface
/// VPC and policy direction so the DPU receives only concrete values.
#[crate::sqlx_test]
async fn test_managed_host_network_config_includes_per_vpc_routing_profiles(pool: sqlx::PgPool) {
    // Reuse one symbolic target so only the associated VPC distinguishes
    // rendered values.
    let symbolic_target = RouteTargetConfig {
        asn: 429_000_000,
        vni: None,      // Keep one definition reusable across distinct VPCs.
        vni_self: true, // Each interface must substitute its own allocated VNI.
    };

    // Share one conditional rule across both profiles to verify resolution for each VPC.
    let conditional_export = crate::cfg::file::PrefixRouteTargetConfig {
        prefix: "0.0.0.0/0".parse().expect("default route"),
        match_type: carbide_network::virtualization::ExportPrefixMatchType::Exact,
        max_prefix_length: None,
        route_targets: vec![symbolic_target.clone()],
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
                        route_target_imports: Some(vec![symbolic_target.clone()]),
                        route_targets_on_exports_by_prefix: Some(vec![conditional_export.clone()]),
                        ..Default::default()
                    },
                ),
                (
                    "EXTERNAL".to_string(),
                    FnnRoutingProfileConfig {
                        internal: Some(false),
                        access_tier: Some(2),
                        leak_tenant_host_routes_to_underlay: Some(true),
                        route_targets_on_exports: Some(vec![symbolic_target.clone()]),
                        route_targets_on_exports_by_prefix: Some(vec![conditional_export]),
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
            dpu_machine_id: Some(mh.dpu().id),
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
            asn: symbolic_target.asn,
            vni: Some(internal_vni),
            vni_self: false,
        }]
    );

    // The identical conditional rule must carry each interface VPC's concrete target.
    for (vni, profile) in &profiles_by_vni {
        assert_eq!(
            profile.route_targets_on_exports_by_prefix,
            vec![rpc::forge::PrefixRouteTarget {
                prefix: "0.0.0.0/0".into(),
                match_type: rpc::forge::ExportPrefixMatchType::Exact as i32,
                max_prefix_length: None,
                route_targets: vec![rpc::common::RouteTarget {
                    asn: symbolic_target.asn,
                    vni: Some(*vni),
                    vni_self: false,
                }],
            }]
        );
    }

    let external_profile = profiles_by_vni.get(&external_vni).unwrap();
    assert!(external_profile.leak_tenant_host_routes_to_underlay);
    assert_eq!(
        external_profile.route_targets_on_exports,
        vec![rpc::common::RouteTarget {
            asn: symbolic_target.asn,
            vni: Some(external_vni),
            vni_self: false,
        }]
    );
}

/// Verifies each DPU receives its local IPv4 or IPv6 endpoint with the selected VPC's routing and peering configuration.
/// This keeps service networking independent of the host VPC while preserving public host interfaces and service-only FNN settings.
#[crate::sqlx_test]
async fn service_vpc_endpoints_use_local_selected_vpc_state(pool: sqlx::PgPool) {
    // Box this test's future to reduce the stack frames reserved by the SQLx runner.
    Box::pin(async move {
        let env = create_service_vpc_test_env(pool, 2, true, None).await;
        let ServiceVpcEndpointFixture {
            tenant,
            managed_host,
            service_vpc_id,
            service_vni,
            ipv4_endpoint,
            ipv6_endpoint,
            ipv4_service_id,
            ipv6_service_id,
            ..
        } = create_service_vpc_endpoint_fixture(&env).await;

        // Create a peer with a distinct advertised prefix to identify the selected service VPC.
        let peer_vpc = env
            .api
            .create_vpc(tonic::Request::new(
                VpcCreationRequest::builder(tenant.organization_id.as_str())
                    .metadata(Metadata {
                        name: "service peer VPC".to_string(),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("INTERNAL".to_string())
                    .rpc(),
            ))
            .await
            .unwrap()
            .into_inner();
        let peer_vpc_id = peer_vpc.id.expect("created peer VPC has an ID");
        let peer_vni = peer_vpc
            .status
            .expect("created peer VPC has status")
            .vni
            .expect("created peer VPC has a VNI");

        // Give the peer one advertised prefix and activate peering before rendering endpoints.
        env.api
            .create_vpc_prefix(tonic::Request::new(rpc::forge::VpcPrefixCreationRequest {
                id: None,
                prefix: String::new(),
                vpc_id: Some(peer_vpc_id),
                config: Some(rpc::forge::VpcPrefixConfig {
                    prefix: "192.3.4.0/24".to_string(),
                }),
                metadata: Some(Metadata {
                    name: "service peer prefix".to_string(),
                    ..Default::default()
                }),
                site_prefix_id: None,
            }))
            .await
            .unwrap();
        env.api
            .create_vpc_peering(tonic::Request::new(rpc::forge::VpcPeeringCreationRequest {
                vpc_id: Some(service_vpc_id),
                peer_vpc_id: Some(peer_vpc_id),
                id: None,
            }))
            .await
            .unwrap();

        // Read the secondary DPU's allocated ASN for the response-wide FNN assertions below.
        let secondary_dpu_asn = {
            let mut txn = env.db_txn().await;
            let asn = managed_host
                .dpu_n(1)
                .db_machine(&mut txn)
                .await
                .asn
                .expect("FNN fixture DPU has an ASN");
            txn.commit().await.unwrap();
            asn
        };
        // Fetch both DPU views; the secondary remains on Admin without a host interface.
        let initial_primary_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let secondary_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[1]),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!initial_primary_response.use_admin_network);
        assert!(secondary_response.use_admin_network);

        // A service-only Admin response still carries the response-wide FNN settings.
        assert_eq!(
            secondary_response.network_virtualization_type,
            Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
        );
        assert_eq!(secondary_response.asn, secondary_dpu_asn);
        assert!(secondary_response.site_fabric_null_routes.is_some());
        assert_eq!(
            secondary_response.deny_prefixes,
            vec!["2001:db8:ffff::/64".to_string()]
        );
        let [primary_service] = initial_primary_response.service_interfaces.as_slice() else {
            panic!("primary DPU must receive only its local endpoint")
        };
        let [secondary_service] = secondary_response.service_interfaces.as_slice() else {
            panic!("secondary DPU must receive only its local endpoint")
        };

        // Each response must preserve its local endpoint's service ID, attachment ID, ordinal, slot,
        // selected VPC, link prefix, MAC address, and internal UUID.
        assert_eq!(primary_service.service_id, ipv4_service_id.to_string());
        assert_eq!(
            primary_service.attachment_id,
            Some(ipv4_endpoint.attachment_id.into())
        );
        assert_eq!(primary_service.interface_ordinal, 0);
        assert_eq!(primary_service.slot_index, 0);
        assert_eq!(primary_service.vpc_id, Some(service_vpc_id));
        assert_eq!(
            primary_service.link_prefix,
            ipv4_endpoint.link_prefix.to_string()
        );
        assert_eq!(
            primary_service.mac_address,
            ipv4_endpoint.mac_address.to_string()
        );
        assert_eq!(
            primary_service.internal_uuid,
            Some(ipv4_endpoint.internal_uuid.into())
        );
        assert_eq!(secondary_service.service_id, ipv6_service_id.to_string());
        assert_eq!(
            secondary_service.attachment_id,
            Some(ipv6_endpoint.attachment_id.into())
        );
        assert_eq!(secondary_service.interface_ordinal, 0);
        assert_eq!(secondary_service.slot_index, 1);
        assert_eq!(secondary_service.vpc_id, Some(service_vpc_id));
        assert_eq!(
            secondary_service.link_prefix,
            ipv6_endpoint.link_prefix.to_string()
        );
        assert_eq!(
            secondary_service.mac_address,
            ipv6_endpoint.mac_address.to_string()
        );
        assert_eq!(
            secondary_service.internal_uuid,
            Some(ipv6_endpoint.internal_uuid.into())
        );

        // Both families carry the selected VPC's exact routing policy and distinct DPU loopbacks.
        let primary_service_loopback = primary_service
            .tenant_vrf_loopback_ip
            .as_deref()
            .expect("primary service interface carries its tenant VRF loopback");
        let secondary_service_loopback = secondary_service
            .tenant_vrf_loopback_ip
            .as_deref()
            .expect("secondary service interface carries its tenant VRF loopback");
        assert_ne!(primary_service_loopback, secondary_service_loopback);
        for service_interface in [primary_service, secondary_service] {
            assert_eq!(service_interface.vpc_vni, service_vni);
            assert_eq!(service_interface.mtu, Some(9000));
            assert!(
                service_interface
                    .vpc_routing_profile
                    .as_ref()
                    .is_some_and(|profile| profile.leak_tenant_host_routes_to_underlay)
            );
            assert!(service_interface.interface_routing_profile.is_none());
            let mut service_vpc_prefixes = service_interface.vpc_prefixes.clone();
            service_vpc_prefixes.sort_unstable();
            assert_eq!(
                service_vpc_prefixes,
                vec!["192.2.4.0/24".to_string(), "2001:db8:200::/64".to_string()]
            );
            assert_eq!(service_interface.vpc_peer_prefixes, vec!["192.3.4.0/24"]);
            assert_eq!(service_interface.vpc_peer_vnis, vec![peer_vni]);
        }

        // The service VPC must remain independent of the consumer's host-facing VPC.
        assert_ne!(
            initial_primary_response.tenant_interfaces[0].vpc_vni,
            primary_service.vpc_vni
        );
        assert_eq!(initial_primary_response.tenant_interfaces.len(), 1);
        assert!(secondary_response.tenant_interfaces.is_empty());

        // Service endpoints must not change the host-facing public interface cardinality.
        let public_instance = initial_primary_response
            .instance
            .as_ref()
            .expect("response retains the public instance");
        assert_eq!(
            public_instance
                .config
                .as_ref()
                .and_then(|config| config.network.as_ref())
                .expect("public instance config retains host networking")
                .interfaces
                .len(),
            1
        );
        assert_eq!(
            public_instance
                .status
                .as_ref()
                .and_then(|status| status.network.as_ref())
                .expect("public instance status retains host networking")
                .interfaces
                .len(),
            1
        );

        // Server-owned service-interface `internal_uuid` values must not leak through the public instance view.
        let public_instance = serde_json::to_string(public_instance).unwrap();
        assert!(!public_instance.contains(&ipv4_endpoint.internal_uuid.to_string()));
        assert!(!public_instance.contains(&ipv6_endpoint.internal_uuid.to_string()));
    })
    .await;
}

/// Verifies absent, populated, updated, and empty VPC NSGs reach service interfaces without changing the instance network version.
/// A conflicting instance NSG remains on the host interface and never supplies service policy.
#[crate::sqlx_test]
async fn service_vpc_endpoints_refresh_only_selected_vpc_nsg(pool: sqlx::PgPool) {
    // Box this test's future to reduce the stack frames reserved by the SQLx runner.
    Box::pin(async move {
        let env = create_service_vpc_test_env(pool, 2, true, None).await;
        let ServiceVpcEndpointFixture {
            tenant,
            managed_host,
            instance_id,
            service_vpc_id,
            instance_network_security_group,
            ..
        } = create_service_vpc_endpoint_fixture(&env).await;

        // Capture the stored instance network version before changing VPC policy.
        let persisted_network_version = db::instance::find_by_id(&env.pool, instance_id)
            .await
            .unwrap()
            .expect("fixture instance remains persisted")
            .network_config_version;

        // An absent service-VPC NSG must remain absent even when the instance has a nonempty NSG.
        let response_without_vpc_policy = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let [service_without_vpc_policy] =
            response_without_vpc_policy.service_interfaces.as_slice()
        else {
            panic!("primary DPU must retain its service endpoint without VPC policy")
        };
        assert!(service_without_vpc_policy.network_security_group.is_none());

        // Attach a VPC-only NSG after endpoint persistence to prove policy does not need an instance `network_config_version` increment.
        let network_security_group = env
            .api
            .create_network_security_group(tonic::Request::new(
                rpc::forge::CreateNetworkSecurityGroupRequest {
                    id: None,
                    tenant_organization_id: tenant.organization_id.clone(),
                    metadata: Some(Metadata {
                        name: "service VPC policy".to_string(),
                        ..Default::default()
                    }),
                    network_security_group_attributes: Some(
                        rpc::forge::NetworkSecurityGroupAttributes {
                            rules: vec![service_vpc_nsg_rule("service-rule-v1", "192.0.2.0/24")],
                            stateful_egress: true, // Provides a wire-visible VPC policy value.
                        },
                    ),
                },
            ))
            .await
            .unwrap()
            .into_inner()
            .network_security_group
            .expect("created NSG is returned");
        env.api
            .update_vpc(
                VpcUpdateRequest::builder()
                    .id(service_vpc_id)
                    .metadata(Metadata {
                        name: "selected service VPC".to_string(),
                        ..Default::default()
                    })
                    .network_security_group_id(network_security_group.id.clone())
                    .tonic_request(),
            )
            .await
            .unwrap();

        // Fetch both DPU views; the secondary remains on Admin without a host interface.
        let initial_primary_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let secondary_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[1]),
            }))
            .await
            .unwrap()
            .into_inner();
        // Both DPUs must use the VPC NSG rather than the instance NSG.
        for service_interface in [
            &initial_primary_response.service_interfaces[0],
            &secondary_response.service_interfaces[0],
        ] {
            assert_eq!(
                service_interface
                    .network_security_group
                    .as_ref()
                    .map(|group| group.id.as_str()),
                Some(network_security_group.id.as_str())
            );
            assert_eq!(
                service_interface
                    .network_security_group
                    .as_ref()
                    .map(|group| group.source),
                Some(i32::from(
                    rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc
                ))
            );
            assert!(
                service_interface
                    .network_security_group
                    .as_ref()
                    .is_some_and(|group| group.stateful_egress)
            );
        }

        // Host and service interfaces select their independently configured policy sources.
        let initial_service_policy = initial_primary_response.service_interfaces[0]
            .network_security_group
            .as_ref()
            .expect("service interface carries VPC policy");
        assert_eq!(
            initial_service_policy.version,
            network_security_group.version
        );
        assert_eq!(
            initial_service_policy.rules[0]
                .rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some("service-rule-v1")
        );
        let host_policy = initial_primary_response.tenant_interfaces[0]
            .network_security_group
            .as_ref()
            .expect("host interface carries instance policy");
        assert_eq!(host_policy.id, instance_network_security_group.id);
        assert_eq!(
            host_policy.source,
            i32::from(rpc::forge::NetworkSecurityGroupSource::NsgSourceInstance)
        );
        assert_eq!(
            host_policy.rules[0]
                .rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some("instance-rule")
        );

        // Update only the selected service VPC policy, leaving the instance network version untouched.
        let updated_network_security_group = env
            .api
            .update_network_security_group(tonic::Request::new(
                rpc::forge::UpdateNetworkSecurityGroupRequest {
                    id: network_security_group.id.clone(),
                    tenant_organization_id: tenant.organization_id.clone(),
                    metadata: network_security_group.metadata.clone(),
                    network_security_group_attributes: Some(
                        rpc::forge::NetworkSecurityGroupAttributes {
                            rules: vec![service_vpc_nsg_rule("service-rule-v2", "198.51.100.0/24")],
                            stateful_egress: true,
                        },
                    ),
                    if_version_match: Some(network_security_group.version.clone()),
                },
            ))
            .await
            .unwrap()
            .into_inner()
            .network_security_group
            .expect("updated service VPC NSG is returned");
        let primary_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let updated_service_policy = primary_response.service_interfaces[0]
            .network_security_group
            .as_ref()
            .expect("fresh response carries updated VPC policy");
        assert_eq!(
            updated_service_policy.version,
            updated_network_security_group.version
        );
        assert_eq!(
            updated_service_policy.rules[0]
                .rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some("service-rule-v2")
        );

        // Empty VPC rules must retain a present VPC NSG rather than falling back to instance policy.
        let empty_network_security_group = env
            .api
            .update_network_security_group(tonic::Request::new(
                rpc::forge::UpdateNetworkSecurityGroupRequest {
                    id: network_security_group.id.clone(),
                    tenant_organization_id: tenant.organization_id.clone(),
                    metadata: network_security_group.metadata.clone(),
                    network_security_group_attributes: Some(
                        rpc::forge::NetworkSecurityGroupAttributes {
                            rules: vec![],
                            stateful_egress: true,
                        },
                    ),
                    if_version_match: Some(updated_network_security_group.version.clone()),
                },
            ))
            .await
            .unwrap()
            .into_inner()
            .network_security_group
            .expect("updated empty VPC NSG is returned");
        let response_with_empty_vpc_policy = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let [service_with_empty_vpc_policy] =
            response_with_empty_vpc_policy.service_interfaces.as_slice()
        else {
            panic!("primary DPU must retain its service endpoint with empty VPC policy")
        };
        let empty_service_policy = service_with_empty_vpc_policy
            .network_security_group
            .as_ref()
            .expect("empty VPC NSG remains present");
        assert_eq!(empty_service_policy.id, network_security_group.id);
        assert_eq!(
            empty_service_policy.version,
            empty_network_security_group.version
        );
        assert_eq!(
            empty_service_policy.source,
            i32::from(rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc)
        );
        assert!(empty_service_policy.rules.is_empty());
        assert!(empty_service_policy.stateful_egress);

        // Fresh VPC policy appears without mutating the instance network version.
        assert_eq!(
            db::instance::find_by_id(&env.pool, instance_id)
                .await
                .unwrap()
                .expect("fixture instance remains persisted")
                .network_config_version,
            persisted_network_version
        );
    })
    .await;
}

/// Verifies stored Admin mode suppresses primary service interfaces while secondary services and fixed slot inventories remain available.
/// Restoring Tenant mode includes primary services with a newer managed_host_config_version so agents receive the changed configuration.
#[crate::sqlx_test]
async fn service_vpc_endpoints_follow_host_network_mode(pool: sqlx::PgPool) {
    // Box this test's future to reduce the stack frames reserved by the SQLx runner.
    Box::pin(async move {
        let env = create_service_vpc_test_env(pool, 2, true, None).await;
        let ServiceVpcEndpointFixture { managed_host, .. } =
            create_service_vpc_endpoint_fixture(&env).await;

        // Primary Admin omits routed service ports, while the secondary Admin response keeps its port.
        set_host_use_admin_network(&env, managed_host.id, true).await;
        let primary_admin = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        let secondary_admin = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[1]),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(primary_admin.use_admin_network);
        assert!(primary_admin.service_interfaces.is_empty());
        assert_eq!(
            primary_admin
                .service_vpc_slot_inventory
                .expect("Admin response retains fixed inventory")
                .slots
                .len(),
            2
        );
        assert_eq!(secondary_admin.service_interfaces.len(), 1);
        assert_eq!(
            secondary_admin
                .service_vpc_slot_inventory
                .expect("secondary Admin response retains fixed inventory")
                .slots
                .len(),
            2
        );

        // Returning to Tenant publishes the primary endpoint under a newer host network version.
        set_host_use_admin_network(&env, managed_host.id, false).await;
        let fresh_tenant = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(fresh_tenant.service_interfaces.len(), 1);
        assert_ne!(
            fresh_tenant.managed_host_config_version,
            primary_admin.managed_host_config_version
        );
    })
    .await;
}

/// Verifies missing VNI, segment, or VPC-prefix data and incompatible MTU or address reservations reject the network-config request.
/// Both link families must retain their HBN and service addresses before an agent receives networking.
#[crate::sqlx_test]
async fn service_vpc_endpoints_reject_invalid_retained_networks(pool: sqlx::PgPool) {
    // Box this test's future to reduce the stack frames reserved by the SQLx runner.
    Box::pin(async move {
        let env = create_service_vpc_test_env(pool, 2, true, None).await;
        let ServiceVpcEndpointFixture {
            managed_host,
            instance_id,
            service_vpc_id,
            service_vni,
            ipv4_endpoint,
            ipv6_endpoint,
            ..
        } = create_service_vpc_endpoint_fixture(&env).await;

        // A missing selected-VPC VNI withholds the successful payload instead of emitting zero.
        sqlx::query("UPDATE vpcs SET status = $1 WHERE id = $2")
            .bind(sqlx::types::Json(model::vpc::VpcStatus { vni: None }))
            .bind(service_vpc_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let missing_vni = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .expect_err("service VPC without a VNI must not produce networking");
        assert_eq!(missing_vni.code(), tonic::Code::FailedPrecondition);
        assert!(missing_vni.message().contains("has no allocated VNI"));

        // Restore the selected VPC's `status.vni` before the remaining validation checks.
        sqlx::query("UPDATE vpcs SET status = $1 WHERE id = $2")
            .bind(sqlx::types::Json(model::vpc::VpcStatus {
                vni: Some(
                    i32::try_from(service_vni)
                        .expect("fixture VNI fits the database representation"),
                ),
            }))
            .bind(service_vpc_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let restored_status: sqlx::types::Json<model::vpc::VpcStatus> =
            sqlx::query_scalar("SELECT status FROM vpcs WHERE id = $1")
                .bind(service_vpc_id)
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(
            restored_status.0.vni,
            Some(i32::try_from(service_vni).expect("fixture VNI fits the database representation"))
        );

        // A missing generated segment must withhold the response instead of emitting partial service-interface configuration.
        let mut endpoint_with_missing_segment = ipv4_endpoint.clone();
        endpoint_with_missing_segment.network_segment_id = NetworkSegmentId::new();
        replace_service_vpc_endpoints(
            &env,
            instance_id,
            &[endpoint_with_missing_segment, ipv6_endpoint.clone()],
        )
        .await;
        let missing_segment = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .expect_err(
                "service endpoint without its generated segment must not produce networking",
            );
        assert_eq!(missing_segment.code(), tonic::Code::FailedPrecondition);
        assert!(
            missing_segment
                .message()
                .contains("references missing network segment")
        );
        // Restore the generated segment reference before testing a missing selected-VPC prefix.
        replace_service_vpc_endpoints(
            &env,
            instance_id,
            &[ipv4_endpoint.clone(), ipv6_endpoint.clone()],
        )
        .await;

        // A missing selected-VPC prefix must withhold the response instead of emitting inconsistent policy.
        sqlx::query("UPDATE network_vpc_prefixes SET deleted = NOW() WHERE id = $1")
            .bind(ipv4_endpoint.vpc_prefix_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let missing_prefix = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            }))
            .await
            .expect_err(
                "service endpoint without its selected-VPC prefix must not produce networking",
            );
        assert_eq!(missing_prefix.code(), tonic::Code::FailedPrecondition);
        assert!(
            missing_prefix
                .message()
                .contains("references missing VPC prefix")
        );

        // Restore the endpoint's active VPC prefix before validating incompatible segments.
        sqlx::query("UPDATE network_vpc_prefixes SET deleted = NULL WHERE id = $1")
            .bind(ipv4_endpoint.vpc_prefix_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let mut txn = env.db_txn().await;
        assert!(
            db::vpc_prefix::find_by_vpc(txn.as_mut(), service_vpc_id)
                .await
                .unwrap()
                .iter()
                .any(|prefix| prefix.id == ipv4_endpoint.vpc_prefix_id)
        );
        txn.commit().await.unwrap();

        // Inject one schema-permitted incompatibility at a time to exercise retained-segment validation.
        let incompatible_segments = [
            // A retained lower MTU must not bypass the fixed service-interface MTU contract.
            (
                "incorrect MTU",
                &ipv4_endpoint,
                1500,
                Some(ipv4_endpoint.link_prefix.network()),
                0,
                "incompatible VPC, MTU, lifecycle, or readiness state",
            ),
            // IPv4 must reserve base+0 through its gateway rather than consuming the service address.
            (
                "incorrect IPv4 gateway",
                &ipv4_endpoint,
                9000,
                Some(ipv4_endpoint.link_prefix.broadcast()),
                0,
                "does not preserve HBN base+0",
            ),
            // IPv6 must reserve base+0 explicitly because it has no gateway.
            (
                "missing IPv6 reservation",
                &ipv6_endpoint,
                9000,
                None,
                0,
                "does not preserve HBN base+0",
            ),
        ];
        for (scenario, endpoint, mtu, gateway, num_reserved, diagnostic) in incompatible_segments {
            // Save `mtu`, `gateway`, and `num_reserved` so each case can restore the original segment and prefix.
            let original: (i32, Option<std::net::IpAddr>, i32) = sqlx::query_as(
                "SELECT ns.mtu, np.gateway, np.num_reserved
             FROM network_segments ns
             JOIN network_prefixes np ON np.segment_id = ns.id
             WHERE ns.id = $1 AND np.id = $2",
            )
            .bind(endpoint.network_segment_id)
            .bind(endpoint.network_prefix_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
            // Persist this case's incompatible MTU or address reservation before testing response validation.
            let mut txn = env.db_txn().await;
            sqlx::query("UPDATE network_segments SET mtu = $1 WHERE id = $2")
                .bind(mtu)
                .bind(endpoint.network_segment_id)
                .execute(txn.as_mut())
                .await
                .unwrap();
            sqlx::query(
                "UPDATE network_prefixes SET gateway = $1, num_reserved = $2 WHERE id = $3",
            )
            .bind(gateway)
            .bind(num_reserved)
            .bind(endpoint.network_prefix_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
            txn.commit().await.unwrap();

            // The receiving DPU must reject the incompatible retained segment.
            let error = env
                .api
                .get_managed_host_network_config(tonic::Request::new(
                    ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(endpoint.dpu_id),
                    },
                ))
                .await
                .expect_err(scenario);
            assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
            assert!(error.message().contains(diagnostic), "{scenario}: {error}");

            // Restore both records before the next validation case.
            let mut txn = env.db_txn().await;
            sqlx::query("UPDATE network_segments SET mtu = $1 WHERE id = $2")
                .bind(original.0)
                .bind(endpoint.network_segment_id)
                .execute(txn.as_mut())
                .await
                .unwrap();
            sqlx::query(
                "UPDATE network_prefixes SET gateway = $1, num_reserved = $2 WHERE id = $3",
            )
            .bind(original.1)
            .bind(original.2)
            .bind(endpoint.network_prefix_id)
            .execute(txn.as_mut())
            .await
            .unwrap();
            txn.commit().await.unwrap();
            let restored: (i32, Option<std::net::IpAddr>, i32) = sqlx::query_as(
                "SELECT ns.mtu, np.gateway, np.num_reserved
             FROM network_segments ns
             JOIN network_prefixes np ON np.segment_id = ns.id
             WHERE ns.id = $1 AND np.id = $2",
            )
            .bind(endpoint.network_segment_id)
            .bind(endpoint.network_prefix_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
            assert_eq!(restored, original, "{scenario}");
        }
    })
    .await;
}

/// Verifies deleted instances and terminating attachments are omitted from DPU service configuration while their endpoint and allocation records remain stored.
/// Configuration reads must leave those records available for later cleanup.
#[crate::sqlx_test]
async fn service_vpc_endpoint_filtering_preserves_allocations(pool: sqlx::PgPool) {
    // Box this test's future to reduce the stack frames reserved by the SQLx runner.
    Box::pin(async move {
        let env = create_service_vpc_test_env(pool, 2, true, None).await;
        let ServiceVpcEndpointFixture {
            managed_host,
            instance_id,
            service_vpc_id,
            ipv4_endpoint,
            ipv6_endpoint,
            ..
        } = create_service_vpc_endpoint_fixture(&env).await;

        // Fetch both active endpoints to allocate the loopbacks whose retention is checked below.
        for dpu_id in &managed_host.dpu_ids {
            let response = env
                .api
                .get_managed_host_network_config(tonic::Request::new(
                    ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(*dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(response.service_interfaces.len(), 1);
        }

        // Seed the secondary service address because both lifecycle checks suppress this endpoint.
        sqlx::query(
            "INSERT INTO instance_addresses
            (instance_id, address, prefix, segment_id, vpc_id)
         VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(instance_id)
        .bind(ipv6_endpoint.link_prefix.broadcast())
        .bind(ipv6_endpoint.link_prefix)
        .bind(ipv6_endpoint.network_segment_id)
        .bind(ipv6_endpoint.vpc_id)
        .execute(&env.pool)
        .await
        .unwrap();

        // Reload the committed service address allocation before either lifecycle filter runs.
        let service_allocations_before_filtering =
            db::instance_address::find_by_segment_id(&env.pool, &ipv6_endpoint.network_segment_id)
                .await
                .unwrap();

        // Deleted-instance filtering removes desired networking while retaining endpoint records for cleanup.
        sqlx::query("UPDATE instances SET deleted = NOW() WHERE id = $1")
            .bind(instance_id)
            .execute(&env.pool)
            .await
            .unwrap();
        let deleted_instance_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[1]),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(deleted_instance_response.service_interfaces.is_empty());
        let service_allocations_after_deletion =
            db::instance_address::find_by_segment_id(&env.pool, &ipv6_endpoint.network_segment_id)
                .await
                .unwrap();
        assert_eq!(
            db::instance::find_by_id(&env.pool, instance_id)
                .await
                .unwrap()
                .expect("deleted instance remains available for cleanup")
                .config
                .network
                .service_interfaces,
            vec![ipv4_endpoint.clone(), ipv6_endpoint.clone()]
        );

        // Restore the instance so the terminating-attachment path remains independently reachable.
        sqlx::query("UPDATE instances SET deleted = NULL WHERE id = $1")
            .bind(instance_id)
            .execute(&env.pool)
            .await
            .unwrap();

        // Mark the attachment terminating and prove its service interface disappears without deleting its service address allocation.
        let mut txn = env.db_txn().await;
        let stored = db::instance::find_by_id(txn.as_mut(), instance_id)
            .await
            .unwrap()
            .expect("fixture instance remains persisted");
        let current_services = stored.config.extension_services.clone();
        let mut terminating_services = current_services.clone();
        terminating_services
            .service_configs
            .iter_mut()
            .find(|config| config.id == Some(ipv6_endpoint.attachment_id))
            .expect("secondary attachment remains persisted")
            .removed = Some(chrono::Utc::now());
        assert_eq!(
            db::instance::update_extension_services_config(
                txn.as_mut(),
                instance_id,
                stored.extension_services_config_version,
                &current_services,
                &terminating_services,
                false,
            )
            .await
            .unwrap(),
            ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();
        let terminated_response = env
            .api
            .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[1]),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(terminated_response.service_interfaces.is_empty());
        let service_allocations_after_termination =
            db::instance_address::find_by_segment_id(&env.pool, &ipv6_endpoint.network_segment_id)
                .await
                .unwrap();
        assert_eq!(
            terminated_response
                .service_vpc_slot_inventory
                .as_ref()
                .expect("terminated secondary response retains fixed inventory")
                .slots
                .len(),
            2
        );
        let retained = db::instance::find_by_id(&env.pool, instance_id)
            .await
            .unwrap()
            .expect("terminating instance remains persisted");
        assert!(
            retained
                .config
                .extension_services
                .service_configs
                .iter()
                .find(|config| config.id == Some(ipv6_endpoint.attachment_id))
                .expect("secondary attachment remains persisted")
                .removed
                .is_some()
        );
        assert_eq!(
            retained.config.network.service_interfaces,
            vec![ipv4_endpoint.clone(), ipv6_endpoint.clone()]
        );

        // Compare the persisted service address allocation at each stage so filtering cannot silently release the address.
        for (scenario, allocations) in [
            // A real allocation must exist before the later observations can prove retention.
            ("before filtering", service_allocations_before_filtering),
            // Deleted instances retain service addresses until cleanup completes.
            ("deleted instance", service_allocations_after_deletion),
            // Attachment termination independently retains the same service address.
            (
                "terminating attachment",
                service_allocations_after_termination,
            ),
        ] {
            assert_eq!(
                allocations
                    .iter()
                    .map(|allocation| (
                        allocation.instance_id,
                        allocation.segment_id,
                        allocation.vpc_id,
                        allocation.address,
                        allocation.prefix,
                    ))
                    .collect::<Vec<_>>(),
                vec![(
                    instance_id,
                    ipv6_endpoint.network_segment_id,
                    ipv6_endpoint.vpc_id,
                    ipv6_endpoint.link_prefix.broadcast(),
                    ipv6_endpoint.link_prefix,
                )],
                "{scenario}"
            );
        }

        // Generated segments, network prefixes, VPC prefixes, and VPC/DPU loopback rows remain
        // reloadable after the deletion and termination rendering checks.
        let mut txn = env.db_txn().await;
        let retained_vpc_prefixes = db::vpc_prefix::find_by_vpc(txn.as_mut(), service_vpc_id)
            .await
            .unwrap();
        for endpoint in [&ipv4_endpoint, &ipv6_endpoint] {
            let segment = db::network_segment::find_by(
                txn.as_mut(),
                db::ObjectColumnFilter::One(
                    db::network_segment::IdColumn,
                    &endpoint.network_segment_id,
                ),
                Default::default(),
            )
            .await
            .unwrap()
            .pop()
            .expect("generated endpoint segment remains persisted");
            assert!(segment.prefixes.iter().any(|prefix| {
                prefix.id == endpoint.network_prefix_id && prefix.prefix == endpoint.link_prefix
            }));
            assert!(
                retained_vpc_prefixes
                    .iter()
                    .any(|prefix| prefix.id == endpoint.vpc_prefix_id)
            );
            assert!(
                db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &endpoint.vpc_id)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        txn.commit().await.unwrap();
    })
    .await;
}

/// Verifies a host interface and two service attachments selecting the same VPC reuse one per-VPC, per-DPU tenant VRF loopback.
/// This prevents host and service interfaces from allocating different tenant VRF loopback addresses or losing another attachment's endpoint during termination.
#[crate::sqlx_test]
async fn service_vpc_endpoint_reuses_host_vpc_loopback(pool: sqlx::PgPool) {
    let env = create_service_vpc_test_env(pool, 1, true, None).await;

    // Allocate the host on the same FNN VPC later selected by its service endpoint.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "service-vpc-loopback".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(Metadata {
                name: "service-vpc-loopback".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();
    let host_segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "shared host and service VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;
    let mut txn = env.db_txn().await;
    let shared_vpc_id = db::vpc::find_by_segment(txn.as_mut(), host_segment_id)
        .await
        .unwrap()
        .expect("host segment belongs to a VPC")
        .id;
    txn.commit().await.unwrap();
    let managed_host = create_managed_host(&env).await;
    mark_host_dpf_ingested(&env, managed_host.id).await;
    let (instance, _) = managed_host
        .instance_builer(&env)
        .tenant_org(tenant.organization_id.clone())
        .single_interface_network_config(host_segment_id)
        .build_and_return()
        .await;
    let first_attachment_id = uuid::Uuid::new_v4();
    let first_endpoint = create_service_vpc_endpoint(
        &env,
        shared_vpc_id,
        "192.1.4.0/25".parse().unwrap(),
        "192.1.4.0/31".parse().unwrap(),
        managed_host.dpu().id,
        first_attachment_id,
        0,
        0,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x51]),
    )
    .await;
    let second_attachment_id = uuid::Uuid::new_v4();
    // Reuse slot 0 with a different attachment and link prefix to exercise independent attachment termination.
    let second_endpoint = create_service_vpc_endpoint(
        &env,
        shared_vpc_id,
        "192.1.4.128/25".parse().unwrap(),
        "192.1.4.128/31".parse().unwrap(),
        managed_host.dpu().id,
        second_attachment_id,
        0,
        0,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x52]),
    )
    .await;
    let first_service_id = api_fixtures::instance::persist_service_vpc_attachment(
        &env,
        instance.id,
        tenant.organization_id.clone(),
        vec![first_endpoint.clone()],
    )
    .await;
    let second_service_id = api_fixtures::instance::persist_service_vpc_attachment(
        &env,
        instance.id,
        tenant.organization_id,
        vec![second_endpoint.clone()],
    )
    .await;

    // Fetch once so host rendering loads the durable loopback into the request cache for both service attachments to reuse.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    let host_loopback = response.tenant_interfaces[0]
        .addresses
        .iter()
        .find(|address| address.address_family() == rpc::forge::AddressFamily::V4)
        .and_then(|address| address.tenant_vrf_loopback_ip.as_ref())
        .expect("host IPv4 address carries its tenant VRF loopback");
    assert_eq!(response.service_interfaces.len(), 2);
    let first_service = response
        .service_interfaces
        .iter()
        .find(|interface| interface.attachment_id == Some(first_attachment_id.into()))
        .expect("first service attachment is rendered");
    let second_service = response
        .service_interfaces
        .iter()
        .find(|interface| interface.attachment_id == Some(second_attachment_id.into()))
        .expect("second service attachment is rendered");
    assert_eq!(first_service.service_id, first_service_id.to_string());
    assert_eq!(second_service.service_id, second_service_id.to_string());
    assert_eq!(first_service.slot_index, 0);
    assert_eq!(second_service.slot_index, 0);
    assert_eq!(
        first_service.link_prefix,
        first_endpoint.link_prefix.to_string()
    );
    assert_eq!(
        second_service.link_prefix,
        second_endpoint.link_prefix.to_string()
    );
    let first_service_loopback = first_service
        .tenant_vrf_loopback_ip
        .as_ref()
        .expect("first service interface carries its tenant VRF loopback");
    let second_service_loopback = second_service
        .tenant_vrf_loopback_ip
        .as_ref()
        .expect("second service interface carries its tenant VRF loopback");
    assert_eq!(first_service_loopback, host_loopback);
    assert_eq!(second_service_loopback, host_loopback);

    // The shared wire value must be the single durable allocation for this DPU and VPC.
    let mut txn = env.db_txn().await;
    let persisted =
        db::vpc_dpu_loopback::find(txn.as_mut(), &managed_host.dpu().id, &shared_vpc_id)
            .await
            .unwrap()
            .expect("shared tenant VRF loopback is persisted");
    assert_eq!(persisted.loopback_ip.to_string(), *host_loopback);
    let allocation_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM vpc_dpu_loopbacks WHERE dpu_id = $1 AND vpc_id = $2",
    )
    .bind(managed_host.dpu().id)
    .bind(shared_vpc_id)
    .fetch_one(txn.as_mut())
    .await
    .unwrap();
    assert_eq!(allocation_count, 1);
    txn.commit().await.unwrap();

    // Terminate only the first attachment while the second still uses the same slot and VPC.
    let mut txn = env.db_txn().await;
    let stored = db::instance::find_by_id(txn.as_mut(), instance.id)
        .await
        .unwrap()
        .expect("fixture instance remains persisted");
    let current_services = stored.config.extension_services.clone();
    let mut terminating_services = current_services.clone();
    terminating_services
        .service_configs
        .iter_mut()
        .find(|config| config.id == Some(first_attachment_id))
        .expect("first attachment remains persisted")
        .removed = Some(chrono::Utc::now());
    assert_eq!(
        db::instance::update_extension_services_config(
            txn.as_mut(),
            instance.id,
            stored.extension_services_config_version,
            &current_services,
            &terminating_services,
            false,
        )
        .await
        .unwrap(),
        ConditionalWrite::Applied(())
    );
    txn.commit().await.unwrap();

    // The surviving attachment must retain its endpoint and shared loopback.
    let after_termination = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .unwrap()
        .into_inner();
    let [surviving_service] = after_termination.service_interfaces.as_slice() else {
        panic!("terminating one attachment must preserve the other endpoint")
    };
    assert_eq!(surviving_service, second_service);

    // Reload both attachment records and the durable loopback after rendering the survivor.
    let retained = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("fixture instance remains persisted");
    assert_eq!(retained.config.extension_services, terminating_services);
    assert_eq!(
        retained.config.network.service_interfaces,
        vec![first_endpoint, second_endpoint]
    );
    let mut txn = env.db_txn().await;
    let retained_loopback =
        db::vpc_dpu_loopback::find(txn.as_mut(), &managed_host.dpu().id, &shared_vpc_id)
            .await
            .unwrap()
            .expect("surviving attachment retains the shared loopback");
    assert_eq!(retained_loopback.loopback_ip, persisted.loopback_ip);
    txn.commit().await.unwrap();
}

/// Verifies a committed service endpoint renders without a tenant VRF loopback when allocation is disabled.
/// This prevents service rendering from allocating an address despite the site's loopback setting.
#[crate::sqlx_test]
async fn service_only_vpc_without_loopbacks_does_not_allocate(pool: sqlx::PgPool) {
    let env = create_service_vpc_test_env(pool, 1, false, None).await;

    // Create a tenant and FNN VPC before assigning a host interface to the primary DPU.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "service-vpc-loopback-disabled".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(Metadata {
                name: "service-vpc-loopback-disabled".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();
    let host_segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "service VPC without loopbacks".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;
    let mut txn = env.db_txn().await;
    let vpc_id = db::vpc::find_by_segment(txn.as_mut(), host_segment_id)
        .await
        .unwrap()
        .expect("host segment belongs to a VPC")
        .id;
    txn.commit().await.unwrap();
    let managed_host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    mark_host_dpf_ingested(&env, managed_host.id).await;
    let (instance, _) = managed_host
        .instance_builer(&env)
        .tenant_org(tenant.organization_id.clone())
        .single_interface_network_config(host_segment_id)
        .build_and_return()
        .await;

    // Commit one service endpoint on the secondary DPU, which has no host interface.
    let endpoint = create_service_vpc_endpoint(
        &env,
        vpc_id,
        "192.7.4.0/24".parse().unwrap(),
        "192.7.4.0/31".parse().unwrap(),
        managed_host.dpu_ids[1],
        uuid::Uuid::new_v4(),
        0,
        0,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x71]),
    )
    .await;
    api_fixtures::instance::persist_service_vpc_attachment(
        &env,
        instance.id,
        tenant.organization_id,
        vec![endpoint.clone()],
    )
    .await;
    let mut txn = env.db_txn().await;
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &vpc_id)
            .await
            .unwrap()
            .is_none()
    );
    txn.commit().await.unwrap();

    // Successful rendering must retain the endpoint while omitting its tenant VRF loopback.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(endpoint.dpu_id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.tenant_interfaces.is_empty());
    let [service] = response.service_interfaces.as_slice() else {
        panic!("disabled loopbacks must not suppress the service endpoint")
    };
    assert_eq!(service.internal_uuid, Some(endpoint.internal_uuid.into()));
    assert!(service.tenant_vrf_loopback_ip.is_none());

    // Reload the durable allocation after rendering to detect an unwanted side effect.
    let mut txn = env.db_txn().await;
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &vpc_id)
            .await
            .unwrap()
            .is_none()
    );
    txn.commit().await.unwrap();
}

/// Verifies two local service VPCs retain distinct policy and loopbacks, and a later endpoint failure rolls back earlier allocations.
/// This prevents per-request VPC caches from crossing VPCs and failed responses from consuming durable loopback resources.
#[crate::sqlx_test]
async fn service_vpc_local_vpcs_preserve_policy_and_rollback(pool: sqlx::PgPool) {
    let env = create_service_vpc_test_env(pool, 2, true, None).await;

    // Allocate the host before committing any service endpoints on its secondary DPU.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "service-vpc-local-vpcs".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(Metadata {
                name: "service-vpc-local-vpcs".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();
    let host_segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "host VPC for local service VPCs".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;
    let managed_host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    mark_host_dpf_ingested(&env, managed_host.id).await;
    let (instance, _) = managed_host
        .instance_builer(&env)
        .tenant_org(tenant.organization_id.clone())
        .single_interface_network_config(host_segment_id)
        .build_and_return()
        .await;

    // Give each local service VPC distinct prefixes, routing policy, and a nonempty NSG.
    let selected_vpcs = [
        // The first endpoint selects INTERNAL policy, keeping its routing profile distinct from the EXTERNAL endpoint's.
        ("192.8.4.0/24", "192.8.4.0/31", "INTERNAL", false),
        // EXTERNAL policy makes an incorrectly reused VPC configuration observable.
        ("192.9.4.0/24", "192.9.4.0/31", "EXTERNAL", true),
    ];
    let mut endpoints = Vec::new();
    let mut expected_policies = Vec::new();
    for (slot_index, (prefix, link_prefix, profile, leak_host_routes)) in
        selected_vpcs.into_iter().enumerate()
    {
        let vpc = env
            .api
            .create_vpc(tonic::Request::new(
                VpcCreationRequest::builder(tenant.organization_id.as_str())
                    .metadata(Metadata {
                        name: format!("local service VPC {slot_index}"),
                        ..Default::default()
                    })
                    .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type(profile.to_string())
                    .rpc(),
            ))
            .await
            .unwrap()
            .into_inner();
        let vpc_id = vpc.id.expect("created service VPC has an ID");
        let vni = vpc
            .status
            .as_ref()
            .and_then(|status| status.vni)
            .expect("created service VPC has a VNI");
        let rule_id = format!("local-service-rule-{slot_index}");
        let policy = env
            .api
            .create_network_security_group(tonic::Request::new(
                rpc::forge::CreateNetworkSecurityGroupRequest {
                    id: None,
                    tenant_organization_id: tenant.organization_id.clone(),
                    metadata: Some(Metadata {
                        name: format!("local service policy {slot_index}"),
                        ..Default::default()
                    }),
                    network_security_group_attributes: Some(
                        rpc::forge::NetworkSecurityGroupAttributes {
                            rules: vec![service_vpc_nsg_rule(&rule_id, prefix)],
                            stateful_egress: true,
                        },
                    ),
                },
            ))
            .await
            .unwrap()
            .into_inner()
            .network_security_group
            .expect("created service VPC NSG is returned");
        env.api
            .update_vpc(
                VpcUpdateRequest::builder()
                    .id(vpc_id)
                    .metadata(vpc.metadata.expect("created service VPC has metadata"))
                    .network_security_group_id(policy.id.clone())
                    .tonic_request(),
            )
            .await
            .unwrap();
        let endpoint = create_service_vpc_endpoint(
            &env,
            vpc_id,
            prefix.parse().unwrap(),
            link_prefix.parse().unwrap(),
            managed_host.dpu_ids[1],
            uuid::Uuid::new_v4(),
            0,
            u32::try_from(slot_index).unwrap(),
            MacAddress::new([0x02, 0, 0, 0, 0, 0x81 + slot_index as u8]),
        )
        .await;
        api_fixtures::instance::persist_service_vpc_attachment(
            &env,
            instance.id,
            tenant.organization_id.clone(),
            vec![endpoint.clone()],
        )
        .await;
        endpoints.push(endpoint);
        expected_policies.push((vni, prefix, policy, rule_id, leak_host_routes));
    }
    // Persist both endpoints in creation order so the later prefix failure follows the first loopback allocation.
    replace_service_vpc_endpoints(&env, instance.id, &endpoints).await;

    // Hide only the later endpoint's VPC prefix while preserving its endpoint/network-prefix IDs.
    sqlx::query("UPDATE network_vpc_prefixes SET deleted = NOW() WHERE id = $1")
        .bind(endpoints[1].vpc_prefix_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let pool_name = env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name();
    let pool_before: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(entry) FROM resource_pool entry WHERE name = $1 ORDER BY id",
    )
    .bind(pool_name)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let allocations_before: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(allocation) FROM resource_pool_allocations allocation
         WHERE pool_name = $1 ORDER BY allocation_id",
    )
    .bind(pool_name)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let mut txn = env.db_txn().await;
    for endpoint in &endpoints {
        assert!(
            db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &endpoint.vpc_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    txn.commit().await.unwrap();

    // The second endpoint's diagnostic proves rendering progressed beyond the first allocation.
    let error = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu_ids[1]),
        }))
        .await
        .expect_err("later endpoint failure must withhold the entire response");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains(&format!(
        "service interface {} references missing VPC prefix {}",
        endpoints[1].internal_uuid, endpoints[1].vpc_prefix_id
    )));

    // Reload the loopback pool, allocation records, and VPC/DPU loopback rows to prove the failed response left no allocation changes.
    let pool_after: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(entry) FROM resource_pool entry WHERE name = $1 ORDER BY id",
    )
    .bind(pool_name)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let allocations_after: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(allocation) FROM resource_pool_allocations allocation
         WHERE pool_name = $1 ORDER BY allocation_id",
    )
    .bind(pool_name)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(pool_after, pool_before);
    assert_eq!(allocations_after, allocations_before);
    let mut txn = env.db_txn().await;
    for endpoint in &endpoints {
        assert!(
            db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &endpoint.vpc_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    txn.commit().await.unwrap();

    // Restore the later prefix before retrying both independently selected VPCs.
    sqlx::query("UPDATE network_vpc_prefixes SET deleted = NULL WHERE id = $1")
        .bind(endpoints[1].vpc_prefix_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu_ids[1]),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.service_interfaces.len(), 2);
    assert!(response.tenant_interfaces.is_empty());

    // Each endpoint must retain its own VNI, prefixes, NSG, routing profile, and durable loopback.
    let mut loopbacks = Vec::new();
    let mut txn = env.db_txn().await;
    for (endpoint, (vni, prefix, policy, rule_id, leak_host_routes)) in
        endpoints.iter().zip(&expected_policies)
    {
        let service = response
            .service_interfaces
            .iter()
            .find(|service| service.attachment_id == Some(endpoint.attachment_id.into()))
            .expect("each local attachment is rendered");
        assert_eq!(service.vpc_id, Some(endpoint.vpc_id));
        assert_eq!(service.vpc_vni, *vni);
        assert_eq!(service.vpc_prefixes, vec![prefix.to_string()]);
        let service_policy = service
            .network_security_group
            .as_ref()
            .expect("each selected VPC retains its NSG");
        assert_eq!(service_policy.id, policy.id);
        assert_eq!(service_policy.version, policy.version);
        assert_eq!(service_policy.rules.len(), 1);
        assert_eq!(
            service_policy.rules[0]
                .rule
                .as_ref()
                .and_then(|rule| rule.id.as_deref()),
            Some(rule_id.as_str())
        );
        assert_eq!(
            service
                .vpc_routing_profile
                .as_ref()
                .expect("FNN service retains its VPC routing profile")
                .leak_tenant_host_routes_to_underlay,
            *leak_host_routes
        );
        let durable_loopback =
            db::vpc_dpu_loopback::find(txn.as_mut(), &endpoint.dpu_id, &endpoint.vpc_id)
                .await
                .unwrap()
                .expect("each local VPC persists its own loopback");
        assert_eq!(
            service.tenant_vrf_loopback_ip.as_deref(),
            Some(durable_loopback.loopback_ip.to_string().as_str())
        );
        loopbacks.push(durable_loopback.loopback_ip);
    }
    assert_ne!(expected_policies[0].0, expected_policies[1].0);
    assert_ne!(loopbacks[0], loopbacks[1]);
    txn.commit().await.unwrap();
}

/// Verifies a service-only DPU returns resource exhaustion when its selected VPC cannot allocate a tenant VRF loopback.
/// This prevents a successful response from publishing a service interface without its required durable loopback allocation.
#[crate::sqlx_test]
async fn service_only_vpc_loopback_exhaustion_withholds_response(pool: sqlx::PgPool) {
    let env = create_service_vpc_test_env(pool, 1, true, None).await;

    // Create one FNN VPC shared by the host interface and a service endpoint on the secondary DPU.
    let tenant = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "service-vpc-loopback-exhaustion".to_string(),
            routing_profile_type: Some("INTERNAL".to_string()),
            metadata: Some(Metadata {
                name: "service-vpc-loopback-exhaustion".to_string(),
                ..Default::default()
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .tenant
        .unwrap();
    let host_segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(tenant.organization_id.as_str())
                .metadata(Metadata {
                    name: "exhausted service loopback VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL".to_string())
                .rpc(),
        )
        .await;
    let mut txn = env.db_txn().await;
    let shared_vpc_id = db::vpc::find_by_segment(txn.as_mut(), host_segment_id)
        .await
        .unwrap()
        .expect("host segment belongs to a VPC")
        .id;
    txn.commit().await.unwrap();
    let managed_host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    mark_host_dpf_ingested(&env, managed_host.id).await;
    let (instance, _) = managed_host
        .instance_builer(&env)
        .tenant_org(tenant.organization_id.clone())
        .single_interface_network_config(host_segment_id)
        .build_and_return()
        .await;
    let attachment_id = uuid::Uuid::new_v4();
    let endpoint = create_service_vpc_endpoint(
        &env,
        shared_vpc_id,
        "192.6.4.0/24".parse().unwrap(),
        "192.6.4.0/31".parse().unwrap(),
        managed_host.dpu_ids[1],
        attachment_id,
        0,
        0,
        MacAddress::new([0x02, 0, 0, 0, 0, 0x61]),
    )
    .await;
    api_fixtures::instance::persist_service_vpc_attachment(
        &env,
        instance.id,
        tenant.organization_id,
        vec![endpoint],
    )
    .await;

    // Occupy every configured loopback value so the service-only DPU reaches the allocator's exhaustion path.
    let exhausted_entries =
        sqlx::query("UPDATE resource_pool SET state = $1, allocated = NOW() WHERE name = $2")
            .bind(sqlx::types::Json(ResourcePoolEntryState::Allocated {
                owner: "service-loopback-exhaustion".to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }))
            .bind(env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name())
            .execute(&env.pool)
            .await
            .unwrap()
            .rows_affected();
    assert!(exhausted_entries > 0);

    // The secondary DPU must receive an error and no durable VPC loopback row.
    let error = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu_ids[1]),
        }))
        .await
        .expect_err("exhausted tenant VRF loopback pool must withhold service networking");
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);
    let mut txn = env.db_txn().await;
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &managed_host.dpu_ids[1], &shared_vpc_id)
            .await
            .unwrap()
            .is_none()
    );
    txn.commit().await.unwrap();
}

/// Verifies tenant-interface rendering rejects a retained negative VPC VNI so
/// managed-host configuration cannot publish its wrapped unsigned value.
#[crate::sqlx_test]
async fn test_managed_host_network_config_rejects_negative_tenant_vni(pool: sqlx::PgPool) {
    // Create one valid tenant interface before changing its persisted VPC status.
    let env = api_fixtures::create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Inject retained state allowed by JSONB but not produced by current APIs.
    let mut txn = env.db_txn().await;
    let vpc = db::vpc::find_by_segment(txn.as_mut(), segment_id)
        .await
        .unwrap()
        .expect("fixture segment belongs to a VPC");
    sqlx::query("UPDATE vpcs SET status = $1 WHERE id = $2")
        .bind(sqlx::types::Json(model::vpc::VpcStatus {
            vni: Some(-1), // Drives the defensive signed-to-unsigned conversion.
        }))
        .bind(vpc.id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Request the DPU configuration that renders the retained tenant VPC.
    let error = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu().id),
        }))
        .await
        .expect_err("negative retained VNI must be rejected");

    // Fail explicitly instead of returning the wrapped `u32`.
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.message(),
        format!("internal error: VPC {} has a negative VNI", vpc.id)
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
            dpu_machine_id: Some(dpu_machine_id),
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
            dpu_machine_id: Some(dpu_machine_id),
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

/// Verifies the admin VPC resolves symbolic targets once for both routing-profile
/// response fields while the default loopback setting remains disabled.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_admin_fnn_vrf_loopback_by_default(
    pool: sqlx::PgPool,
) {
    // Configure one symbolic admin target and a fixed VNI for deterministic resolution.
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: true,
        l3_only: false,
        vpc_vni: Some(10000),
        routing_profile: FnnRoutingProfileConfig {
            leak_default_route_from_underlay: Some(true),
            route_target_imports: Some(vec![RouteTargetConfig {
                asn: 429_000_000,
                vni: None,      // Keep the configured target symbolic.
                vni_self: true, // Resolve from the allocated admin VPC VNI.
            }]),
            ..Default::default()
        },
    });

    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;

    // Attach the FNN admin VPC because test env setup does not run production setup hooks.
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
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
            dpu_machine_id: Some(dpu_machine_id),
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

    let routing_profile = admin_interface
        .vpc_routing_profile
        .as_ref()
        .expect("admin interface should include per-VPC routing profile");
    assert!(admin_interface.interface_routing_profile.is_none());
    assert!(routing_profile.leak_default_route_from_underlay);
    assert_eq!(routing_profile.route_target_imports.len(), 1);
    assert_eq!(routing_profile.route_target_imports[0].asn, 429_000_000);
    assert_eq!(routing_profile.route_target_imports[0].vni, Some(10000));
    assert!(!routing_profile.route_target_imports[0].vni_self);
    assert_eq!(response.routing_profile.as_ref(), Some(routing_profile));

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

/// Verifies disabled admin FNN policy is omitted because no admin VPC exists
/// to resolve its symbolic route targets. L3-only opt-in must also remain inactive.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_managed_host_network_config_omits_disabled_admin_fnn_routing_profile(
    pool: sqlx::PgPool,
) {
    // Configure a symbolic admin target while leaving admin FNN disabled.
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().admin_vpc = Some(AdminFnnConfig {
        enabled: false, // Prevents creation of the required admin VPC.
        l3_only: true,  // Must remain inactive while admin FNN is disabled.
        vpc_vni: None,
        routing_profile: FnnRoutingProfileConfig {
            leak_default_route_from_underlay: Some(true),
            route_target_imports: Some(vec![RouteTargetConfig {
                asn: 429_000_000,
                vni: None,
                vni_self: true, // Must remain unused while admin FNN is disabled.
            }]),
            ..Default::default()
        },
    });
    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;
    let managed_host = create_managed_host(&env).await;

    // Fetch configuration for a DPU remaining on the non-FNN admin network.
    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(managed_host.dpu_ids[0]),
        }))
        .await
        .unwrap()
        .into_inner();

    // Neither field may expose policy from the disabled admin configuration.
    let admin_interface = response.admin_interface.expect("admin interface");
    assert_eq!(admin_interface.vpc_vni, 0);
    assert!(admin_interface.vpc_routing_profile.is_none());
    assert!(response.routing_profile.is_none());

    // L3-only opt-in cannot change the attachment while admin FNN is disabled.
    assert!(admin_interface.is_l2_segment);
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
            dpu_machine_id: Some(dpu_machine_id),
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
        l3_only: false,
        vpc_vni: Some(10000),
        routing_profile: FnnRoutingProfileConfig::default(),
    });
    let env = api_fixtures::create_test_env_with_overrides(pool, overrides).await;
    crate::db_init::create_admin_vpc(&env.api, Some(10000))
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
        .find_instance_by_machine_id(tonic::Request::new(mh.id.to_machine_id()))
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
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "test1".to_string(),
            service_type: DpuExtensionServiceType::KubernetesPod as i32,
            tenant_organization_id: "best_org".to_string(),
            description: None,
            data: create_extension_service_data("test"),
            credential: None,
            observability: None,
            deployment_type: 0,
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
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "test2".to_string(),
            service_type: DpuExtensionServiceType::KubernetesPod as i32,
            tenant_organization_id: "best_org".to_string(),
            description: None,
            data: create_extension_service_data("test2"),
            credential: None,
            observability: None,
            deployment_type: 0,
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
                service_vpc_ids: vec![],
            },
            InstanceDpuExtensionServiceConfig {
                service_id: extension_service2.service_id.clone(),
                version: service2_version.clone(),
                service_vpc_ids: vec![],
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
            dpu_machine_id: Some(dpu_1_id),
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
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .unwrap()
        .into_inner();

    let admin_if = response.admin_interface.as_ref().unwrap();

    // dpu-health is not updated here
    let err = env
        .api
        .record_dpu_network_status(tonic::Request::new(DpuNetworkStatus {
            dpu_machine_id: Some(dpu_machine_id),
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
            lldp: None,
            lldp_snapshot_fingerprint: None,
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
async fn rejected_quarantine_write_keeps_health_report(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let host_id = mh.host().id;
    env.api
        .set_managed_host_quarantine_state(tonic::Request::new(
            rpc::forge::SetManagedHostQuarantineStateRequest {
                machine_id: Some(host_id.into()),
                quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                    mode: rpc::forge::ManagedHostQuarantineMode::BlockAllTraffic.into(),
                    reason: Some("retained quarantine".to_string()),
                }),
            },
        ))
        .await
        .unwrap();

    for (scenario, clear) in [("set quarantine", false), ("clear quarantine", true)] {
        let mut writer = env.db_txn().await;
        let writer_pid: i32 =
            sqlx::query_scalar("SELECT pg_backend_pid() FROM machines WHERE id = $1 FOR UPDATE")
                .bind(host_id)
                .fetch_one(&mut *writer)
                .await
                .unwrap();
        let before = mh.host().db_machine(&mut writer).await;

        let request = async {
            if clear {
                env.api
                    .clear_managed_host_quarantine_state(tonic::Request::new(
                        rpc::forge::ClearManagedHostQuarantineStateRequest {
                            machine_id: Some(host_id.into()),
                        },
                    ))
                    .await
                    .map(|_| ())
            } else {
                env.api
                    .set_managed_host_quarantine_state(tonic::Request::new(
                        rpc::forge::SetManagedHostQuarantineStateRequest {
                            machine_id: Some(host_id.into()),
                            quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                                mode: rpc::forge::ManagedHostQuarantineMode::BlockAllTraffic.into(),
                                reason: Some("rejected replacement".to_string()),
                            }),
                        },
                    ))
                    .await
                    .map(|_| ())
            }
        };
        let competing_write = async {
            // The request has read its network version and is now waiting to
            // update it. Commit a different version before releasing the row.
            common::postgres::wait_for_blocked_query(
                &env.pool,
                writer_pid,
                "UPDATE machines SET network_config_version",
            )
            .await;
            assert_eq!(
                db::machine::try_update_network_config(
                    &mut writer,
                    &host_id,
                    before.network_config.version,
                    &before.network_config.value,
                )
                .await
                .unwrap(),
                db::ConditionalWrite::Applied(())
            );
            let version = db::machine::get_network_config(&mut *writer, &host_id)
                .await
                .unwrap()
                .version;
            writer.commit().await.unwrap();
            version
        };
        let (result, winning_version) =
            tokio::time::timeout(std::time::Duration::from_secs(15), async {
                tokio::join!(request, competing_write)
            })
            .await
            .expect(scenario);
        let status = result.expect_err("a rejected quarantine write must fail the API request");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition, "{scenario}");

        let mut txn = env.db_txn().await;
        let after = mh.host().db_machine(&mut txn).await;
        assert_eq!(
            after.network_config.value, before.network_config.value,
            "{scenario}"
        );
        assert_eq!(after.network_config.version, winning_version, "{scenario}");
        assert_eq!(after.health_reports, before.health_reports, "{scenario}");
        txn.commit().await.unwrap();
    }
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
