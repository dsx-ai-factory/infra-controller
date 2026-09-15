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

use super::*;

/// Builds a routing profile with multiple entries in each set-like
/// collection used by the fingerprint tests.
fn comparison_routing_profile() -> rpc::RoutingProfile {
    rpc::RoutingProfile {
        route_target_imports: vec![
            ::rpc::common::RouteTarget {
                asn: 65_001,
                vni: 101,
            },
            ::rpc::common::RouteTarget {
                asn: 65_002,
                vni: 102,
            },
        ],
        route_targets_on_exports: vec![
            ::rpc::common::RouteTarget {
                asn: 65_003,
                vni: 103,
            },
            ::rpc::common::RouteTarget {
                asn: 65_004,
                vni: 104,
            },
        ],
        leak_default_route_from_underlay: false,
        leak_tenant_host_routes_to_underlay: false,
        tenant_leak_communities_accepted: false,
        accepted_leaks_from_underlay: vec![
            rpc::PrefixFilterPolicyEntry {
                prefix: "10.0.0.0/8".to_string(),
            },
            rpc::PrefixFilterPolicyEntry {
                prefix: "172.16.0.0/12".to_string(),
            },
        ],
        allowed_anycast_prefixes: vec![
            rpc::PrefixFilterPolicyEntry {
                prefix: "192.0.2.0/24".to_string(),
            },
            rpc::PrefixFilterPolicyEntry {
                prefix: "198.51.100.0/24".to_string(),
            },
        ],
    }
}

/// Builds a resolved rule with nested prefix sets and caller-selected ID,
/// priority, and action.
fn comparison_security_group_rule(
    id: &str,
    priority: u32,
    action: rpc::NetworkSecurityGroupRuleAction,
) -> rpc::ResolvedNetworkSecurityGroupRule {
    rpc::ResolvedNetworkSecurityGroupRule {
        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
            id: Some(id.to_string()),
            direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress.into(),
            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny.into(),
            action: action.into(),
            priority,
            source_net: Some(
                rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            destination_net: Some(
                rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                    "0.0.0.0/0".to_string(),
                ),
            ),
            ..Default::default()
        }),
        src_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
        dst_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
    }
}

/// Builds an interface that exercises every nested collection normalized
/// by the fingerprint.
///
/// The deprecated scalar fields remain populated because Core dual-writes them for older agents.
#[allow(deprecated)]
fn comparison_interface(id: &str, vlan_id: u32, vni: u32) -> rpc::FlatInterfaceConfig {
    rpc::FlatInterfaceConfig {
        function_type: rpc::InterfaceFunctionType::Physical.into(),
        vlan_id,
        vni,
        gateway: Some("10.0.0.1/24".to_string()),
        ip: Some("10.0.0.2".to_string()),
        interface_prefix: Some("10.0.0.0/31".to_string()),
        virtual_function_id: None,
        vpc_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
        prefix: Some("10.0.0.0/24".to_string()),
        fqdn: format!("{id}.example.test"),
        booturl: Some("http://boot.example.test/ipxe".to_string()),
        vpc_vni: vni + 1_000,
        svi_ip: Some("10.0.0.1".to_string()),
        tenant_vrf_loopback_ip: Some("10.0.0.3".to_string()),
        is_l2_segment: false,
        vpc_peer_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
        vpc_peer_vnis: vec![2_001, 2_002],
        mtu: Some(9_000),
        ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::/127".to_string(),
            svi_ip: Some("2001:db8::".to_string()),
        }),
        vpc_routing_profile: Some(comparison_routing_profile()),
        interface_routing_profile: Some(rpc::FlatInterfaceRoutingProfile {
            allowed_anycast_prefixes: vec![
                rpc::PrefixFilterPolicyEntry {
                    prefix: "203.0.113.0/25".to_string(),
                },
                rpc::PrefixFilterPolicyEntry {
                    prefix: "203.0.113.128/25".to_string(),
                },
            ],
        }),
        addresses: vec![],
        network_security_group: Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
            id: format!("nsg-{id}"),
            version: "nsg-v1".to_string(),
            source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
            rules: vec![
                comparison_security_group_rule(
                    "first-rule",
                    100,
                    rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit,
                ),
                comparison_security_group_rule(
                    "second-rule",
                    200,
                    rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny,
                ),
            ],
            stateful_egress: false,
        }),
        internal_uuid: Some(::rpc::common::Uuid {
            value: id.to_string(),
        }),
    }
}

/// Builds one populated response shared by the rendered, non-HBN, and
/// ordering comparison cases.
fn comparison_network_config() -> ManagedHostNetworkConfigResponse {
    ManagedHostNetworkConfigResponse {
        service_interfaces: vec![],
        service_vpc_slot_inventory: None,
        asn: 65_000,
        dhcp_servers: vec!["10.10.0.1".to_string(), "10.10.0.2".to_string()],
        vni_device: "vxlan48".to_string(),
        managed_host_config: Some(rpc::ManagedHostNetworkConfig {
            loopback_ip: "10.20.0.1".to_string(),
            quarantine_state: None,
            loopback_ip_v6: Some("2001:db8:ffff::1".to_string()),
        }),
        managed_host_config_version: "managed-v1".to_string(),
        use_admin_network: false,
        admin_interface: Some(comparison_interface("admin", 10, 100)),
        tenant_interfaces: vec![
            comparison_interface("first-interface", 20, 200),
            comparison_interface("second-interface", 30, 300),
        ],
        instance_network_config_version: "instance-v1".to_string(),
        network_virtualization_type: Some(rpc::VpcVirtualizationType::Fnn.into()),
        vpc_vni: Some(1_000),
        route_servers: vec!["10.30.0.1".to_string(), "10.30.0.2".to_string()],
        remote_id: "dpu-remote-id".to_string(),
        deprecated_deny_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
        deny_prefixes: vec!["100.64.0.0/10".to_string(), "169.254.0.0/16".to_string()],
        site_fabric_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
        vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
        stateful_acls_enabled: true,
        ntp_servers: vec!["10.40.0.1".to_string(), "10.40.0.2".to_string()],
        enable_dhcp: true,
        host_interface_id: Some("60cef902-9779-4666-8362-c9bb4b37185f".to_string()),
        is_primary_dpu: true,
        internet_l3_vni: Some(4_000),
        datacenter_asn: 65_100,
        common_internal_route_target: Some(::rpc::common::RouteTarget {
            asn: 65_200,
            vni: 5_000,
        }),
        additional_route_target_imports: vec![
            ::rpc::common::RouteTarget {
                asn: 65_201,
                vni: 5_001,
            },
            ::rpc::common::RouteTarget {
                asn: 65_202,
                vni: 5_002,
            },
        ],
        network_security_policy_overrides: vec![
            comparison_security_group_rule(
                "first-override",
                10,
                rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit,
            ),
            comparison_security_group_rule(
                "second-override",
                20,
                rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny,
            ),
        ],
        routing_profile: Some(comparison_routing_profile()),
        anycast_site_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
        tenant_host_asn: Some(65_300),
        site_global_vpc_vni: Some(6_000),
        bgp_leaf_session_password: Some("leaf-password".to_string()),
        ..Default::default()
    }
}

/// Selects the unchanged baseline or one response mutation used to verify the
/// HBN skip decision. The address-list case intentionally remains a match
/// because HBN does not consume the list and the fingerprint excludes it.
#[derive(Clone, Copy, Debug)]
enum RenderedInputChange {
    Unchanged,
    ManagedVersion,
    InstanceVersion,
    ManagedHostLoopback,
    DenyPrefix,
    SiteFabricPrefix,
    AdminInterfaceVni,
    TenantInterfaceVni,
    TenantInterfaceIp,
    TenantInterfaceAddresses,
    TenantVpcPrefix,
    TenantPeerPrefix,
    TenantPeerVni,
    VpcRoutingProfile,
    InterfaceRoutingProfile,
    NetworkSecurityGroup,
    NetworkSecurityPolicyOverride,
    NetworkVirtualizationType,
    PrimaryDpu,
}

impl RenderedInputChange {
    /// Applies the selected case to the shared response.
    #[allow(deprecated)]
    fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
        match self {
            Self::Unchanged => {}
            Self::ManagedVersion => config.managed_host_config_version.push_str("-changed"),
            Self::InstanceVersion => config.instance_network_config_version.push_str("-changed"),
            Self::ManagedHostLoopback => {
                config
                    .managed_host_config
                    .as_mut()
                    .expect("comparison fixture has managed-host config")
                    .loopback_ip = "10.20.0.2".to_string();
            }
            Self::DenyPrefix => config.deny_prefixes.push("192.0.2.0/24".to_string()),
            Self::SiteFabricPrefix => {
                config
                    .site_fabric_prefixes
                    .push("192.168.0.0/16".to_string());
            }
            Self::AdminInterfaceVni => {
                config
                    .admin_interface
                    .as_mut()
                    .expect("comparison fixture has an admin interface")
                    .vni += 1;
            }
            Self::TenantInterfaceVni => config.tenant_interfaces[1].vni += 1,
            Self::TenantInterfaceIp => {
                config.tenant_interfaces[1].ip = Some("10.0.0.99".to_string());
            }
            Self::TenantInterfaceAddresses => {
                config.tenant_interfaces[1].addresses = vec![
                    rpc::InterfaceAddressConfig {
                        address_family: rpc::AddressFamily::V4.into(),
                        ip: "10.0.0.2".to_string(),
                        interface_prefix: "10.0.0.0/31".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        gateway: Some("10.0.0.1/24".to_string()),
                        svi_ip: Some("10.0.0.1".to_string()),
                        tenant_vrf_loopback_ip: Some("10.0.0.3".to_string()),
                    },
                    rpc::InterfaceAddressConfig {
                        address_family: rpc::AddressFamily::V6.into(),
                        ip: "2001:db8::1".to_string(),
                        interface_prefix: "2001:db8::/127".to_string(),
                        prefix: "2001:db8::/64".to_string(),
                        gateway: None,
                        svi_ip: Some("2001:db8::".to_string()),
                        tenant_vrf_loopback_ip: None,
                    },
                ];
            }
            Self::TenantVpcPrefix => {
                config.tenant_interfaces[1]
                    .vpc_prefixes
                    .push("192.168.0.0/16".to_string());
            }
            Self::TenantPeerPrefix => {
                config.tenant_interfaces[1]
                    .vpc_peer_prefixes
                    .push("203.0.113.0/24".to_string());
            }
            Self::TenantPeerVni => config.tenant_interfaces[1].vpc_peer_vnis.push(2_003),
            Self::VpcRoutingProfile => {
                config.tenant_interfaces[1]
                    .vpc_routing_profile
                    .as_mut()
                    .expect("comparison fixture has a VPC routing profile")
                    .leak_default_route_from_underlay = true;
            }
            Self::InterfaceRoutingProfile => {
                config.tenant_interfaces[1]
                    .interface_routing_profile
                    .as_mut()
                    .expect("comparison fixture has an interface routing profile")
                    .allowed_anycast_prefixes
                    .push(rpc::PrefixFilterPolicyEntry {
                        prefix: "100.64.0.0/10".to_string(),
                    });
            }
            Self::NetworkSecurityGroup => {
                config.tenant_interfaces[1]
                    .network_security_group
                    .as_mut()
                    .expect("comparison fixture has a network security group")
                    .stateful_egress = true;
            }
            Self::NetworkSecurityPolicyOverride => {
                config.network_security_policy_overrides[1]
                    .src_prefixes
                    .push("100.64.0.0/10".to_string());
            }
            Self::NetworkVirtualizationType => {
                config.network_virtualization_type =
                    Some(rpc::VpcVirtualizationType::EthernetVirtualizer.into());
            }
            Self::PrimaryDpu => config.is_primary_dpu = false,
        }
    }
}

/// A new agent process must render once before it can use the HBN skip
/// cache; no response may match an uninitialized fingerprint.
#[test]
fn current_network_version_never_matches_before_first_update() {
    let config = comparison_network_config();

    assert!(!CurrentNetworkVersion::default().matches_versions_from(&config, None));
}

/// The supplemental config file's path never changes between iterations, so
/// only its content hash can invalidate the cache: an in-place edit or a
/// removal must force a re-render even when the response is unchanged.
#[test]
fn current_network_version_tracks_supplemental_config_hash() {
    const PATCH: Option<&str> = Some(r#"{"vrf": {"storage": {}}}"#);
    const EDITED_PATCH: Option<&str> = Some(r#"{"vrf": null}"#);

    let config = comparison_network_config();
    // (scenario, content at last reconciliation, content now, still matches?)
    for (scenario, recorded, current, expected) in [
        ("unchanged content matches", PATCH, PATCH, true),
        ("edited content invalidates", PATCH, EDITED_PATCH, false),
        ("removed config invalidates", PATCH, None, false),
        ("still-absent config matches", None, None, true),
        ("newly added config invalidates", None, PATCH, false),
    ] {
        let mut current_version = CurrentNetworkVersion::default();
        current_version.update_from(&config, recorded);
        assert_eq!(
            current_version.matches_versions_from(&config, current),
            expected,
            "{scenario}"
        );
    }
}

/// Every input consumed by HBN must invalidate the cache, and either
/// explicit version must do the same.
#[test]
fn current_network_version_detects_rendered_input_changes() {
    use carbide_test_support::value_scenarios;

    value_scenarios!(run = |change| {
        let mut config = comparison_network_config();
        let mut current = CurrentNetworkVersion::default();
        current.update_from(&config, None);
        change.apply(&mut config);
        current.matches_versions_from(&config, None)
    };
        "unchanged configuration" {
            RenderedInputChange::Unchanged => true,
        }
        "version changes" {
            RenderedInputChange::ManagedVersion => false,
            RenderedInputChange::InstanceVersion => false,
        }
        "top-level rendering inputs" {
            RenderedInputChange::ManagedHostLoopback => false,
            RenderedInputChange::DenyPrefix => false,
            RenderedInputChange::SiteFabricPrefix => false,
            RenderedInputChange::NetworkVirtualizationType => false,
            RenderedInputChange::PrimaryDpu => false,
        }
        "interface rendering inputs" {
            RenderedInputChange::AdminInterfaceVni => false,
            RenderedInputChange::TenantInterfaceVni => false,
            RenderedInputChange::TenantInterfaceIp => false,
            RenderedInputChange::TenantVpcPrefix => false,
            RenderedInputChange::TenantPeerPrefix => false,
            RenderedInputChange::TenantPeerVni => false,
            RenderedInputChange::VpcRoutingProfile => false,
            RenderedInputChange::InterfaceRoutingProfile => false,
            RenderedInputChange::NetworkSecurityGroup => false,
            RenderedInputChange::NetworkSecurityPolicyOverride => false,
        }
        "family-neutral address list is not an HBN rendering input" {
            RenderedInputChange::TenantInterfaceAddresses => true,
        }
    );
}

/// `SetLikeInputReordering` lists the response collections this test
/// reverses to prove member order does not trigger another HBN apply.
#[derive(Clone, Copy, Debug)]
enum SetLikeInputReordering {
    DhcpServers,
    RouteServers,
    SiteFabricPrefixes,
    AdminInterfaceVpcPrefixes,
    TenantVpcPrefixes,
    TenantPeerPrefixes,
    TenantPeerVnis,
    AdditionalRouteTargets,
    TopLevelRoutingProfileRouteTargets,
    VpcRouteTargets,
    InterfaceAnycastPrefixes,
    NetworkSecurityGroupRules,
    NetworkSecurityGroupRulePrefixes,
    NetworkSecurityPolicyOverrides,
}

impl SetLikeInputReordering {
    /// Reverses the selected collection without changing its members.
    fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
        match self {
            Self::DhcpServers => config.dhcp_servers.reverse(),
            Self::RouteServers => config.route_servers.reverse(),
            Self::SiteFabricPrefixes => config.site_fabric_prefixes.reverse(),
            Self::AdminInterfaceVpcPrefixes => config
                .admin_interface
                .as_mut()
                .expect("comparison fixture has an admin interface")
                .vpc_prefixes
                .reverse(),
            Self::TenantVpcPrefixes => config.tenant_interfaces[0].vpc_prefixes.reverse(),
            Self::TenantPeerPrefixes => {
                config.tenant_interfaces[0].vpc_peer_prefixes.reverse();
            }
            Self::TenantPeerVnis => config.tenant_interfaces[0].vpc_peer_vnis.reverse(),
            Self::AdditionalRouteTargets => {
                config.additional_route_target_imports.reverse();
            }
            Self::TopLevelRoutingProfileRouteTargets => config
                .routing_profile
                .as_mut()
                .expect("comparison fixture has a top-level routing profile")
                .route_target_imports
                .reverse(),
            Self::VpcRouteTargets => {
                config.tenant_interfaces[0]
                    .vpc_routing_profile
                    .as_mut()
                    .expect("comparison fixture has a VPC routing profile")
                    .route_target_imports
                    .reverse();
            }
            Self::InterfaceAnycastPrefixes => {
                config.tenant_interfaces[0]
                    .interface_routing_profile
                    .as_mut()
                    .expect("comparison fixture has an interface routing profile")
                    .allowed_anycast_prefixes
                    .reverse();
            }
            Self::NetworkSecurityGroupRules => {
                config.tenant_interfaces[0]
                    .network_security_group
                    .as_mut()
                    .expect("comparison fixture has a network security group")
                    .rules
                    .reverse();
            }
            Self::NetworkSecurityGroupRulePrefixes => {
                config.tenant_interfaces[0]
                    .network_security_group
                    .as_mut()
                    .expect("comparison fixture has a network security group")
                    .rules[0]
                    .src_prefixes
                    .reverse();
            }
            Self::NetworkSecurityPolicyOverrides => {
                config.network_security_policy_overrides.reverse();
            }
        }
    }
}

/// Reordering set-like inputs must not cause an unnecessary HBN update.
#[test]
fn current_network_version_ignores_set_like_input_order() {
    use carbide_test_support::value_scenarios;

    value_scenarios!(run = |reordering| {
        let mut config = comparison_network_config();
        let mut current = CurrentNetworkVersion::default();
        current.update_from(&config, None);
        reordering.apply(&mut config);
        current.matches_versions_from(&config, None)
    };
        "top-level sets" {
            SetLikeInputReordering::DhcpServers => true,
            SetLikeInputReordering::RouteServers => true,
            SetLikeInputReordering::SiteFabricPrefixes => true,
            SetLikeInputReordering::AdditionalRouteTargets => true,
            SetLikeInputReordering::TopLevelRoutingProfileRouteTargets => true,
            SetLikeInputReordering::NetworkSecurityPolicyOverrides => true,
        }
        "interface sets" {
            SetLikeInputReordering::AdminInterfaceVpcPrefixes => true,
            SetLikeInputReordering::TenantVpcPrefixes => true,
            SetLikeInputReordering::TenantPeerPrefixes => true,
            SetLikeInputReordering::TenantPeerVnis => true,
            SetLikeInputReordering::VpcRouteTargets => true,
            SetLikeInputReordering::InterfaceAnycastPrefixes => true,
            SetLikeInputReordering::NetworkSecurityGroupRules => true,
            SetLikeInputReordering::NetworkSecurityGroupRulePrefixes => true,
        }
    );
}

/// Selects one collection whose relative order changes HBN behavior.
#[derive(Clone, Copy, Debug)]
enum OrderSensitiveInputReordering {
    TenantInterfaces,
    EqualPriorityNetworkSecurityGroupRules,
    EqualPriorityNetworkSecurityPolicyOverrides,
}

impl OrderSensitiveInputReordering {
    /// Makes the security-rule cases depend on stable equal-priority
    /// ordering before their entries are reversed.
    fn prepare(self, config: &mut ManagedHostNetworkConfigResponse) {
        match self {
            Self::TenantInterfaces => {}
            Self::EqualPriorityNetworkSecurityGroupRules => {
                let rules = &mut config.tenant_interfaces[0]
                    .network_security_group
                    .as_mut()
                    .expect("comparison fixture has a network security group")
                    .rules;
                let priority = rules[0]
                    .rule
                    .as_ref()
                    .expect("comparison fixture has rule attributes")
                    .priority;
                rules[1]
                    .rule
                    .as_mut()
                    .expect("comparison fixture has rule attributes")
                    .priority = priority;
            }
            Self::EqualPriorityNetworkSecurityPolicyOverrides => {
                let priority = config.network_security_policy_overrides[0]
                    .rule
                    .as_ref()
                    .expect("comparison fixture has rule attributes")
                    .priority;
                config.network_security_policy_overrides[1]
                    .rule
                    .as_mut()
                    .expect("comparison fixture has rule attributes")
                    .priority = priority;
            }
        }
    }

    /// Reverses the selected order-sensitive collection.
    fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
        match self {
            Self::TenantInterfaces => config.tenant_interfaces.reverse(),
            Self::EqualPriorityNetworkSecurityGroupRules => config.tenant_interfaces[0]
                .network_security_group
                .as_mut()
                .expect("comparison fixture has a network security group")
                .rules
                .reverse(),
            Self::EqualPriorityNetworkSecurityPolicyOverrides => {
                config.network_security_policy_overrides.reverse();
            }
        }
    }
}

/// Interface order and equal-priority rule order must remain part of the
/// fingerprint because the renderer preserves both.
#[test]
fn current_network_version_preserves_semantically_significant_order() {
    use carbide_test_support::value_scenarios;

    value_scenarios!(run = |reordering| {
        let mut config = comparison_network_config();
        reordering.prepare(&mut config);
        let mut current = CurrentNetworkVersion::default();
        current.update_from(&config, None);
        reordering.apply(&mut config);
        current.matches_versions_from(&config, None)
    };
        "first-match interface order" {
            OrderSensitiveInputReordering::TenantInterfaces => false,
        }
        "equal-priority security rule order" {
            OrderSensitiveInputReordering::EqualPriorityNetworkSecurityGroupRules => false,
            OrderSensitiveInputReordering::EqualPriorityNetworkSecurityPolicyOverrides => false,
        }
    );
}

/// `NonHbnInputChange` lists the response fields this test changes to
/// prove they stay out of the HBN fingerprint.
#[derive(Clone, Copy, Debug)]
enum NonHbnInputChange {
    NetworkPingerType,
    MinimumFunctioningLinks,
    InstanceId,
    InstancePayload,
    DeprecatedDhcpFlag,
    DeprecatedDenyPrefixes,
    NtpServers,
    HostInterfaceId,
    ExtensionServices,
    AstraConfig,
    AdminNetworkTransition,
}

impl NonHbnInputChange {
    /// Applies the selected non-HBN change to the shared response.
    fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
        match self {
            Self::NetworkPingerType => {
                config.dpu_network_pinger_type = Some("icmp".to_string());
            }
            Self::MinimumFunctioningLinks => config.min_dpu_functioning_links = Some(1),
            Self::InstanceId => config.instance_id = Some(Default::default()),
            Self::InstancePayload => config.instance = Some(Default::default()),
            Self::DeprecatedDhcpFlag => config.enable_dhcp = false,
            Self::DeprecatedDenyPrefixes => config
                .deprecated_deny_prefixes
                .push("192.0.2.0/24".to_string()),
            Self::NtpServers => config.ntp_servers.push("10.40.0.3".to_string()),
            Self::HostInterfaceId => {
                config.host_interface_id = Some("7340b4f5-1721-4dd3-8fc3-9f91a4c29f61".to_string());
            }
            Self::ExtensionServices => {
                config
                    .dpu_extension_services
                    .push(rpc::ManagedHostDpuExtensionServiceConfig {
                        service_id: "extension-service".to_string(),
                        ..Default::default()
                    });
            }
            Self::AstraConfig => config.astra_config = Some(rpc::AstraConfig::default()),
            Self::AdminNetworkTransition => config.use_admin_network_changed = Some(true),
        }
    }
}

/// Inputs used outside HBN, read only at startup, or deprecated must not
/// retrigger HBN.
#[test]
fn current_network_version_ignores_non_hbn_inputs() {
    use carbide_test_support::value_scenarios;

    value_scenarios!(run = |change| {
        let mut config = comparison_network_config();
        let mut current = CurrentNetworkVersion::default();
        current.update_from(&config, None);
        change.apply(&mut config);
        current.matches_versions_from(&config, None)
    };
        "inputs applied outside HBN rendering" {
            NonHbnInputChange::MinimumFunctioningLinks => true,
            NonHbnInputChange::ExtensionServices => true,
            NonHbnInputChange::AstraConfig => true,
            NonHbnInputChange::AdminNetworkTransition => true,
        }
        "startup, status, DHCP-only, and deprecated inputs" {
            NonHbnInputChange::NetworkPingerType => true,
            NonHbnInputChange::InstanceId => true,
            NonHbnInputChange::InstancePayload => true,
            NonHbnInputChange::HostInterfaceId => true,
            NonHbnInputChange::NtpServers => true,
            NonHbnInputChange::DeprecatedDenyPrefixes => true,
            NonHbnInputChange::DeprecatedDhcpFlag => true,
        }
    );
}

enum ManagedHostConfigInput {
    Missing,
    WithoutIpv6Loopback,
    WithIpv6Loopback(&'static str),
}

struct Ipv6UnicastHealthRow {
    virtualization_type: VpcVirtualizationType,
    managed_host_config: ManagedHostConfigInput,
}

#[test]
fn ipv6_unicast_health_requires_fnn_with_an_ipv6_loopback() {
    use carbide_test_support::value_scenarios;

    value_scenarios!(run = |row: Ipv6UnicastHealthRow| {
        let managed_host_config = match row.managed_host_config {
            ManagedHostConfigInput::Missing => None,
            ManagedHostConfigInput::WithoutIpv6Loopback => {
                Some(rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: None,
                    quarantine_state: None,
                })
            }
            ManagedHostConfigInput::WithIpv6Loopback(loopback_ip_v6) => {
                Some(rpc::ManagedHostNetworkConfig {
                    loopback_ip: "10.0.0.1".to_string(),
                    loopback_ip_v6: Some(loopback_ip_v6.to_string()),
                    quarantine_state: None,
                })
            }
        };
        let conf = ManagedHostNetworkConfigResponse {
            managed_host_config,
            ..Default::default()
        };
        ipv6_unicast_health_enabled(&conf, row.virtualization_type)
    };
        "FNN with a reserved IPv6 loopback" {
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::Fnn,
                managed_host_config: ManagedHostConfigInput::WithIpv6Loopback("2001:db8::1"),
            } => true,
        }

        "inactive IPv6 underlay" {
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::Fnn,
                managed_host_config: ManagedHostConfigInput::Missing,
            } => false,
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::Fnn,
                managed_host_config: ManagedHostConfigInput::WithoutIpv6Loopback,
            } => false,
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
                managed_host_config: ManagedHostConfigInput::WithIpv6Loopback("2001:db8::1"),
            } => false,
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::EthernetVirtualizerWithNvue,
                managed_host_config: ManagedHostConfigInput::WithIpv6Loopback("2001:db8::1"),
            } => false,
            Ipv6UnicastHealthRow {
                virtualization_type: VpcVirtualizationType::Flat,
                managed_host_config: ManagedHostConfigInput::WithIpv6Loopback("2001:db8::1"),
            } => false,
        }
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_get_fabric_interfaces_data() {
    let fabric_interfaces_data = get_fabric_interfaces_data().await.unwrap();
    dbg!(fabric_interfaces_data.as_slice());
    // Under virtualization we probably can't make any assertions about
    // whether this list contains any interfaces, but uncommenting this
    // should pass on any Linux host with real hardware or a virtualized PCI
    // network interface.
    // assert!(fabric_interfaces_data.len() > 0);
}
