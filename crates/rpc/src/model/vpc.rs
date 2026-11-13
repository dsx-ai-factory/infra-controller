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

use carbide_network::virtualization::DEFAULT_NETWORK_VIRTUALIZATION_TYPE;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::network_security_group::NetworkSecurityGroupIdParseError;
use config_version::ConfigVersion;
use model::metadata::{LabelFilter, Metadata};
use model::vpc::{
    ChangeVpcRoutingProfile, NewVpc, PowerResourceGroupUpdate, PrefixFilterPolicyEntry,
    PrefixRouteTargetConfig, RouteTargetConfig, UpdateVpc, UpdateVpcVirtualization, Vpc,
    VpcPeering, VpcRoutingProfileOverrides, VpcSearchFilter, VpcStatus,
};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<rpc::forge::VpcSearchFilter> for VpcSearchFilter {
    fn from(filter: rpc::forge::VpcSearchFilter) -> Self {
        VpcSearchFilter {
            name: filter.name,
            tenant_org_id: filter.tenant_org_id,
            network_virtualization_type: None,
            label: filter.label.map(LabelFilter::from),
        }
    }
}

impl From<Vpc> for rpc::forge::Vpc {
    fn from(src: Vpc) -> Self {
        let desired_vni = src.config.vni.map(|v| v as u32);
        let virt_type =
            rpc::forge::VpcVirtualizationType::from(src.config.network_virtualization_type) as i32;
        let nsg_id = src
            .config
            .network_security_group_id
            .map(|nsg_id| nsg_id.to_string());
        let routing_profile_overrides = src.config.routing_profile_overrides.map(Into::into);
        let metadata = Some(rpc::Metadata {
            name: src.metadata.name,
            description: src.metadata.description,
            labels: src
                .metadata
                .labels
                .iter()
                .map(|(key, value)| rpc::forge::Label {
                    key: key.clone(),
                    value: value.clone().none_if_empty(),
                })
                .collect(),
        });

        rpc::forge::Vpc {
            id: Some(src.id),
            version: src.version.version_string(),
            created: Some(src.created.into()),
            updated: Some(src.updated.into()),
            deleted: src.deleted.map(|t| t.into()),
            metadata,

            config: Some(rpc::forge::VpcConfig {
                tenant_organization_id: src.config.tenant_organization_id,
                tenant_keyset_id: src.config.tenant_keyset_id,
                network_virtualization_type: Some(virt_type),
                network_security_group_id: nsg_id,
                dhcp_option_set_id: src.config.dhcp_option_set_id.map(|id| id.to_string()),
                default_nvlink_logical_partition_id: src.config.default_nvlink_logical_partition_id,
                vni: desired_vni,
                routing_profile_type: src.config.routing_profile_type,
                routing_profile_overrides,
                power_resource_group: src.config.power_resource_group.clone(),
                slaac_enabled: Some(src.config.slaac_enabled),
            }),
            status: Some(rpc::forge::VpcStatus::from(src.status)),
        }
    }
}

impl From<VpcStatus> for rpc::forge::VpcStatus {
    fn from(src: VpcStatus) -> Self {
        rpc::forge::VpcStatus {
            // This is the pattern we have elsewhere because a VNI should never be negative.
            vni: src.vni.map(|x| x as u32),
            // The API handler resolves this from the current runtime config.
            effective_routing_profile: None,
        }
    }
}

impl TryFrom<rpc::common::RouteTarget> for RouteTargetConfig {
    type Error = RpcDataConversionError;

    fn try_from(target: rpc::common::RouteTarget) -> Result<Self, Self::Error> {
        let target = RouteTargetConfig {
            asn: target.asn,
            vni: target.vni,
            vni_self: target.vni_self,
        };
        target
            .validate()
            .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?;
        Ok(target)
    }
}

impl From<RouteTargetConfig> for rpc::common::RouteTarget {
    fn from(target: RouteTargetConfig) -> Self {
        Self {
            asn: target.asn,
            vni: target.vni,
            vni_self: target.vni_self,
        }
    }
}

impl TryFrom<rpc::forge::PrefixRouteTarget> for PrefixRouteTargetConfig {
    type Error = RpcDataConversionError;

    /// Validates one wire rule before its symbolic targets can be persisted.
    fn try_from(entry: rpc::forge::PrefixRouteTarget) -> Result<Self, Self::Error> {
        // Reject unknown enum values before they can acquire the default GE meaning.
        let match_type = rpc::forge::ExportPrefixMatchType::try_from(entry.match_type)
            .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?;
        let entry = Self {
            prefix: entry
                .prefix
                .parse::<carbide_network::ip::prefix::IpNet>()
                .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?
                .trunc(),
            match_type: match_type.into(),
            max_prefix_length: entry.max_prefix_length,
            route_targets: entry
                .route_targets
                .into_iter()
                .map(RouteTargetConfig::try_from)
                .collect::<Result<_, _>>()?,
        };
        entry
            .validate()
            .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?;
        Ok(entry)
    }
}

impl From<PrefixRouteTargetConfig> for rpc::forge::PrefixRouteTarget {
    /// Canonicalizes the prefix while preserving symbolic targets in configuration responses.
    fn from(entry: PrefixRouteTargetConfig) -> Self {
        Self {
            prefix: entry.prefix.trunc().to_string(),
            match_type: rpc::forge::ExportPrefixMatchType::from(entry.match_type) as i32,
            max_prefix_length: entry.max_prefix_length,
            route_targets: entry.route_targets.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<rpc::forge::VpcRoutingProfileOverrides> for VpcRoutingProfileOverrides {
    type Error = RpcDataConversionError;

    fn try_from(profile: rpc::forge::VpcRoutingProfileOverrides) -> Result<Self, Self::Error> {
        let route_targets_on_exports_by_prefix = profile
            .route_targets_on_exports_by_prefix
            .map(|entries| {
                entries
                    .values
                    .into_iter()
                    .map(PrefixRouteTargetConfig::try_from)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        PrefixRouteTargetConfig::validate_entries(
            route_targets_on_exports_by_prefix
                .as_deref()
                .unwrap_or_default(),
        )
        .map_err(|error| RpcDataConversionError::InvalidArgument(error.to_string()))?;
        Ok(Self {
            route_targets_on_exports_by_prefix,
            route_target_imports: profile
                .route_target_imports
                .map(|targets| {
                    targets
                        .values
                        .into_iter()
                        .map(RouteTargetConfig::try_from)
                        .collect()
                })
                .transpose()?,
            route_targets_on_exports: profile
                .route_targets_on_exports
                .map(|targets| {
                    targets
                        .values
                        .into_iter()
                        .map(RouteTargetConfig::try_from)
                        .collect()
                })
                .transpose()?,
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            accepted_leaks_from_underlay: profile
                .accepted_leaks_from_underlay
                .map(|entries| {
                    entries
                        .values
                        .into_iter()
                        .map(|entry| {
                            Ok(PrefixFilterPolicyEntry {
                                prefix: entry.prefix.parse()?,
                            })
                        })
                        .collect::<Result<Vec<_>, RpcDataConversionError>>()
                })
                .transpose()?,
            allowed_anycast_prefixes: profile
                .allowed_anycast_prefixes
                .map(|entries| {
                    entries
                        .values
                        .into_iter()
                        .map(|entry| {
                            Ok(PrefixFilterPolicyEntry {
                                prefix: entry.prefix.parse()?,
                            })
                        })
                        .collect::<Result<Vec<_>, RpcDataConversionError>>()
                })
                .transpose()?,
        })
    }
}

impl From<VpcRoutingProfileOverrides> for rpc::forge::VpcRoutingProfileOverrides {
    fn from(profile: VpcRoutingProfileOverrides) -> Self {
        Self {
            route_targets_on_exports_by_prefix: profile.route_targets_on_exports_by_prefix.map(
                |entries| rpc::forge::PrefixRouteTargets {
                    values: entries.into_iter().map(Into::into).collect(),
                },
            ),
            route_target_imports: profile.route_target_imports.map(|targets| {
                rpc::common::RouteTargets {
                    values: targets
                        .into_iter()
                        .map(rpc::common::RouteTarget::from)
                        .collect(),
                }
            }),
            route_targets_on_exports: profile.route_targets_on_exports.map(|targets| {
                rpc::common::RouteTargets {
                    values: targets
                        .into_iter()
                        .map(rpc::common::RouteTarget::from)
                        .collect(),
                }
            }),
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            accepted_leaks_from_underlay: profile.accepted_leaks_from_underlay.map(|entries| {
                rpc::forge::PrefixFilterPolicyEntries {
                    values: entries
                        .into_iter()
                        .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                            prefix: entry.prefix.to_string(),
                        })
                        .collect(),
                }
            }),
            allowed_anycast_prefixes: profile.allowed_anycast_prefixes.map(|entries| {
                rpc::forge::PrefixFilterPolicyEntries {
                    values: entries
                        .into_iter()
                        .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                            prefix: entry.prefix.to_string(),
                        })
                        .collect(),
                }
            }),
        }
    }
}

impl TryFrom<rpc::forge::VpcCreationRequest> for NewVpc {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::VpcCreationRequest) -> Result<Self, Self::Error> {
        let virt_type = match value.network_virtualization_type {
            None => DEFAULT_NETWORK_VIRTUALIZATION_TYPE,
            Some(v) => rpc::network::vpc_virtualization_type_try_from_rpc(v)?,
        };
        let id = value.id.unwrap_or_else(|| uuid::Uuid::new_v4().into());

        let metadata = match value.metadata {
            Some(metadata) => metadata.try_into()?,
            None => Metadata::new_with_default_name(),
        };

        metadata.validate(true).map_err(|e| {
            RpcDataConversionError::InvalidArgument(format!("VPC metadata is not valid: {e}"))
        })?;

        Ok(NewVpc {
            id,
            tenant_organization_id: value.tenant_organization_id,
            vni: value.vni.map(|v| v.try_into()).transpose().map_err(
                |e: std::num::TryFromIntError| {
                    RpcDataConversionError::InvalidValue(
                        format!(
                            "`{}` cannot be converted to VNI",
                            value.vni.unwrap_or_default()
                        ),
                        e.to_string(),
                    )
                },
            )?,
            network_security_group_id: value
                .network_security_group_id
                .map(|nsg_id| nsg_id.parse())
                .transpose()
                .map_err(|e: NetworkSecurityGroupIdParseError| {
                    RpcDataConversionError::InvalidNetworkSecurityGroupId(e.value())
                })?,
            dhcp_option_set_id: value
                .dhcp_option_set_id
                .filter(|id| !id.is_empty())
                .map(|id| {
                    id.parse()
                        .map_err(|_| RpcDataConversionError::InvalidUuid("DhcpOptionSetId", id))
                })
                .transpose()?,
            routing_profile_type: None,
            routing_profile_overrides: value
                .routing_profile_overrides
                .map(TryInto::try_into)
                .transpose()?,
            power_resource_group: value
                .power_resource_group
                .filter(|resource_group| !resource_group.is_empty()),
            slaac_enabled: value.slaac_enabled.unwrap_or(false),
            network_virtualization_type: virt_type,
            metadata,
        })
    }
}

impl TryFrom<rpc::forge::VpcUpdateRequest> for UpdateVpc {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::VpcUpdateRequest) -> Result<Self, Self::Error> {
        let if_version_match: Option<ConfigVersion> =
            match &value.if_version_match {
                Some(version) => Some(version.parse::<ConfigVersion>().map_err(|_| {
                    RpcDataConversionError::InvalidConfigVersion(version.to_string())
                })?),
                None => None,
            };

        let metadata = match value.metadata {
            Some(metadata) => metadata.try_into()?,
            None => Metadata::new_with_default_name(),
        };

        metadata.validate(true).map_err(|e| {
            RpcDataConversionError::InvalidArgument(format!("VPC metadata is not valid: {e}"))
        })?;

        let power_resource_group = value.power_resource_group.map(|resource_group| {
            if resource_group.is_empty() {
                PowerResourceGroupUpdate::Clear
            } else {
                PowerResourceGroupUpdate::Set(resource_group)
            }
        });

        Ok(UpdateVpc {
            id: value
                .id
                .ok_or(RpcDataConversionError::MissingArgument("id"))?,
            network_security_group_id: value
                .network_security_group_id
                .map(|nsg_id| nsg_id.parse())
                .transpose()
                .map_err(|e: NetworkSecurityGroupIdParseError| {
                    RpcDataConversionError::InvalidNetworkSecurityGroupId(e.value())
                })?,
            dhcp_option_set_id: value
                .dhcp_option_set_id
                .filter(|id| !id.is_empty())
                .map(|id| {
                    id.parse()
                        .map_err(|_| RpcDataConversionError::InvalidUuid("DhcpOptionSetId", id))
                })
                .transpose()?,
            routing_profile_overrides: value
                .routing_profile_overrides
                .map(TryInto::try_into)
                .transpose()?,
            power_resource_group,
            if_version_match,
            metadata,
        })
    }
}

impl TryFrom<rpc::forge::VpcUpdateVirtualizationRequest> for UpdateVpcVirtualization {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::VpcUpdateVirtualizationRequest) -> Result<Self, Self::Error> {
        let if_version_match: Option<ConfigVersion> =
            match &value.if_version_match {
                Some(version) => Some(version.parse::<ConfigVersion>().map_err(|_| {
                    RpcDataConversionError::InvalidConfigVersion(version.to_string())
                })?),
                None => None,
            };

        let network_virtualization_type = match value.network_virtualization_type {
            Some(v) => rpc::network::vpc_virtualization_type_try_from_rpc(v)?,
            None => {
                return Err(RpcDataConversionError::MissingArgument(
                    "network_virtualization_type",
                ));
            }
        };

        Ok(UpdateVpcVirtualization {
            id: value
                .id
                .ok_or(RpcDataConversionError::MissingArgument("id"))?,
            if_version_match,
            network_virtualization_type,
        })
    }
}

impl TryFrom<rpc::forge::VpcChangeRoutingProfileRequest> for ChangeVpcRoutingProfile {
    type Error = RpcDataConversionError;

    fn try_from(request: rpc::forge::VpcChangeRoutingProfileRequest) -> Result<Self, Self::Error> {
        let id = request
            .id
            .ok_or(RpcDataConversionError::MissingArgument("id"))?;
        let version = request
            .if_version_match
            .ok_or(RpcDataConversionError::MissingArgument("if_version_match"))?;
        let if_version_match = version
            .parse()
            .map_err(|_| RpcDataConversionError::InvalidConfigVersion(version))?;
        if request.routing_profile_type.is_empty() {
            return Err(RpcDataConversionError::InvalidArgument(
                "routing_profile_type must not be empty".to_string(),
            ));
        }
        let vni = request
            .vni
            .map(|vni| {
                i32::try_from(vni)
                    .ok()
                    .filter(|vni| (1..=0x00ff_ffff).contains(vni))
                    .ok_or_else(|| {
                        RpcDataConversionError::InvalidArgument(format!(
                            "requested VNI `{vni}` must be between 1 and 16777215"
                        ))
                    })
            })
            .transpose()?;
        Ok(Self {
            id,
            if_version_match,
            routing_profile_type: request.routing_profile_type,
            vni,
        })
    }
}

impl From<Vpc> for rpc::forge::VpcDeletionResult {
    fn from(_src: Vpc) -> Self {
        rpc::forge::VpcDeletionResult {}
    }
}

impl From<VpcPeering> for rpc::forge::VpcPeering {
    fn from(db_vpc_peering: VpcPeering) -> Self {
        let VpcPeering {
            id,
            vpc_id,
            peer_vpc_id,
            deletion_version,
        } = db_vpc_peering;

        let id = Some(id);
        let vpc_id = Some(vpc_id);
        let peer_vpc_id = Some(peer_vpc_id);

        Self {
            id,
            vpc_id,
            peer_vpc_id,
            state: if deletion_version.is_some() {
                rpc::forge::VpcPeeringState::Deleting
            } else {
                rpc::forge::VpcPeeringState::Ready
            } as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::Outcome::FailsWith;
    use carbide_test_support::{scenarios, value_scenarios};
    use carbide_uuid::vpc::VpcId;
    use model::vpc::VpcConfig;
    use prost::Message;

    use super::*;

    /// Verifies current and predecessor route-target schemas preserve nonzero
    /// and literal-zero values so mixed-version deployments remain compatible.
    #[test]
    fn route_target_wire_remains_compatible_with_predecessor_schema() {
        /// Models the predecessor agent's scalar field 2 to exercise its actual
        /// protobuf decoding behavior.
        #[derive(Clone, PartialEq, prost::Message)]
        struct LegacyRouteTarget {
            #[prost(uint32, tag = "1")]
            asn: u32,
            #[prost(uint32, tag = "2")]
            vni: u32,
        }

        let cases = [
            // A nonzero optional value must retain the predecessor's scalar value.
            (
                "nonzero local administrator",
                rpc::common::RouteTarget {
                    asn: 64_512,
                    vni: Some(73),
                    vni_self: false,
                },
                73,
            ),
            // An explicitly present zero must retain field 2 on the wire.
            (
                "explicit zero local administrator",
                rpc::common::RouteTarget {
                    asn: 64_513,
                    vni: Some(0), // Qualifies explicit-zero presence on the current side.
                    vni_self: false,
                },
                0,
            ),
        ];

        for (scenario, target, expected_vni) in cases {
            // Encode with current Core and decode with the predecessor agent type.
            let encoded = target.encode_to_vec();
            if expected_vni == 0 {
                assert!(
                    encoded.ends_with(&[0x10, 0x00]),
                    "{scenario}: optional field 2 was not encoded"
                );
            }
            let legacy =
                LegacyRouteTarget::decode(encoded.as_slice()).expect("legacy agent must decode");

            // Preserve both components for predecessor agents.
            assert_eq!(legacy.asn, target.asn, "{scenario}");
            assert_eq!(legacy.vni, expected_vni, "{scenario}");
        }

        // Encode predecessor literal zero, which proto3 omits from the wire.
        let predecessor_zero = LegacyRouteTarget {
            asn: 64_514,
            vni: 0,
        }
        .encode_to_vec();

        // Decode the predecessor bytes with the current optional-field schema.
        let current = rpc::common::RouteTarget::decode(predecessor_zero.as_slice())
            .expect("current Core must decode predecessor route targets");

        // Current protobuf decoding must observe the predecessor's omitted field.
        assert_eq!(current.vni, None);

        // Convert and resolve through the same Core model path used by VPC requests.
        let resolved = RouteTargetConfig::try_from(current)
            .expect("predecessor literal zero must remain valid")
            .resolve(None)
            .expect("legacy omission must resolve without a VPC VNI");

        // Predecessor scalar omission must retain literal-zero meaning.
        assert_eq!(
            resolved,
            RouteTargetConfig {
                asn: 64_514,
                vni: Some(0),
                vni_self: false,
            }
        );
    }

    /// Verifies protobuf defaults, normalized prefixes, and nested validation preserve the export contract.
    #[test]
    fn conditional_export_rpc_rejects_invalid_entries_and_preserves_presence() {
        use carbide_test_support::Outcome::Fails;

        // Omit match_type to exercise protobuf's GE default with a symbolic target.
        let rule = rpc::forge::PrefixRouteTarget {
            prefix: "192.0.2.7/24".into(), // Host bits must not survive conversion.
            route_targets: vec![rpc::common::RouteTarget {
                asn: 64512,
                vni: None,
                vni_self: true,
            }],
            ..Default::default()
        };
        let overrides = rpc::forge::VpcRoutingProfileOverrides {
            route_targets_on_exports_by_prefix: Some(rpc::forge::PrefixRouteTargets {
                values: vec![rule.clone()],
            }),
            ..Default::default()
        };
        // Stored definitions retain symbols while canonicalizing the matched network.
        let decoded = VpcRoutingProfileOverrides::try_from(overrides).expect("valid symbolic rule");
        let encoded = rpc::forge::VpcRoutingProfileOverrides::from(decoded);
        let retained = &encoded
            .route_targets_on_exports_by_prefix
            .expect("present rules")
            .values[0];
        assert_eq!(retained.prefix, "192.0.2.0/24");
        assert_eq!(
            retained.match_type,
            rpc::forge::ExportPrefixMatchType::Ge as i32
        );
        assert_eq!(retained.route_targets, rule.route_targets);

        // Supply an explicit zero maximum on a valid LE default-route rule.
        let mut zero_maximum = rule.clone();
        zero_maximum.prefix = "0.0.0.0/0".into();
        zero_maximum.match_type = rpc::forge::ExportPrefixMatchType::Le as i32;
        zero_maximum.max_prefix_length = Some(0);
        let preserved = rpc::forge::PrefixRouteTarget::from(
            PrefixRouteTargetConfig::try_from(zero_maximum).expect("valid zero maximum"),
        );
        // Preserve zero rather than omitting the bound and selecting every contained IPv4 prefix.
        assert_eq!(preserved.max_prefix_length, Some(0));

        // Keep invalid wire values separate from valid JSON enum names tested by the shared type.
        let mut unknown_match = rule.clone();
        unknown_match.match_type = 99; // Prost retains unknown enum integers.
        let mut empty_targets = rule.clone();
        empty_targets.route_targets.clear(); // Proto3 omission and empty arrays have the same value.
        let mut conflicting_target = rule;
        conflicting_target.route_targets[0].vni = Some(0); // Explicit zero still conflicts with vni_self.
        carbide_test_support::scenarios!(run = |entry| {
            PrefixRouteTargetConfig::try_from(entry).map(drop).map_err(drop)
        };
            "invalid conditional wire rules" {
                // Unknown wire integers must fail instead of using GE.
                unknown_match => Fails,
                // Empty repeated fields must fail even when protobuf omitted the field.
                empty_targets => Fails,
                // Nested values must use the same RT validation as top-level targets.
                conflicting_target => Fails,
            }
        );

        // Round-trip omitted and explicitly empty override lists without collapsing inheritance.
        carbide_test_support::value_scenarios!(run = |conditional: Option<rpc::forge::PrefixRouteTargets>| {
            let overrides = rpc::forge::VpcRoutingProfileOverrides {
                route_targets_on_exports_by_prefix: conditional.clone(),
                ..Default::default()
            };
            let stored = VpcRoutingProfileOverrides::try_from(overrides).expect("valid override");
            rpc::forge::VpcRoutingProfileOverrides::from(stored).route_targets_on_exports_by_prefix
                == conditional
        };
            "presence-aware conditional export lists" {
                // Omission must retain inheritance rather than select an empty policy.
                None => true,
                // A present empty list must remain an explicit inherited-policy clearing.
                Some(rpc::forge::PrefixRouteTargets { values: vec![] }) => true,
            }
        );
    }

    /// Verifies both RPC override directions reject a distinct invalid
    /// route-target contract before malformed policy reaches persistence.
    /// Conditional exports also reject target lists that exceed the configured-pair limit.
    #[test]
    fn vpc_routing_profile_overrides_reject_invalid_route_targets() {
        scenarios!(run = |profile| {
            VpcRoutingProfileOverrides::try_from(profile)
                .map(drop)
                .map_err(|error| match error {
                    RpcDataConversionError::InvalidArgument(message) => message,
                    error => panic!("unexpected route-target conversion error: {error}"),
                })
        };
            "invalid route-target override lists" {
                // Imports reject competing literal and VPC-derived sources.
                rpc::forge::VpcRoutingProfileOverrides {
                    route_target_imports: Some(rpc::common::RouteTargets {
                        values: vec![rpc::common::RouteTarget {
                            asn: 64_512,
                            vni: Some(0), // Presence conflicts even at zero.
                            vni_self: true,
                        }],
                    }),
                    ..Default::default()
                } => FailsWith(
                    "route target cannot specify both vni and vni_self".to_string()
                ),

                // Exports reject components that fit neither standard encoding.
                rpc::forge::VpcRoutingProfileOverrides {
                    route_targets_on_exports: Some(rpc::common::RouteTargets {
                        values: vec![rpc::common::RouteTarget {
                            asn: 429_000_000,
                            vni: Some(65_536),
                            vni_self: false,
                        }],
                    }),
                    ..Default::default()
                } => FailsWith(
                    "route target ASN 429000000 and local administrator 65536 cannot both exceed 65535"
                        .to_string()
                ),

                // One prefix cannot bypass the pair limit with many individually valid targets.
                rpc::forge::VpcRoutingProfileOverrides {
                    route_targets_on_exports_by_prefix: Some(rpc::forge::PrefixRouteTargets {
                        values: vec![rpc::forge::PrefixRouteTarget {
                            prefix: "0.0.0.0/0".to_string(),
                            route_targets: (0..=256) // One more target than the shared pair limit.
                                .map(|vni| rpc::common::RouteTarget {
                                    asn: 64_512,
                                    vni: Some(vni),
                                    vni_self: false,
                                })
                                .collect(),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                } => FailsWith(
                    "route_targets_on_exports_by_prefix exceeds 256 prefix/route-target pairs".to_string()
                ),
            }
        );
    }

    #[test]
    fn vpc_routing_change_requires_original_version_and_named_destination() {
        let vpc_id = VpcId::new();
        let request = rpc::forge::VpcChangeRoutingProfileRequest {
            id: Some(vpc_id),
            if_version_match: Some("V1-T0".to_string()),
            routing_profile_type: "PARTNER".to_string(),
            vni: None,
        };
        value_scenarios!(
            run = |input| ChangeVpcRoutingProfile::try_from(input)
                .map_err(|error| tonic::Status::from(error).code());
            "valid configured name" {
                request.clone() => Ok(ChangeVpcRoutingProfile {
                    id: vpc_id,
                    if_version_match: "V1-T0".parse().unwrap(),
                    routing_profile_type: "PARTNER".to_string(),
                    vni: None,
                }),
            }
            "missing ID" {
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: None, ..request.clone()
                } => Err(tonic::Code::InvalidArgument),
            }
            "missing version" {
                rpc::forge::VpcChangeRoutingProfileRequest {
                    if_version_match: None, ..request.clone()
                } => Err(tonic::Code::InvalidArgument),
            }
            "malformed version" {
                rpc::forge::VpcChangeRoutingProfileRequest {
                    if_version_match: Some("bad".to_string()), ..request.clone()
                } => Err(tonic::Code::InvalidArgument),
            }
            "missing destination" {
                rpc::forge::VpcChangeRoutingProfileRequest {
                    routing_profile_type: String::new(), ..request
                } => Err(tonic::Code::InvalidArgument),
            }
        );
    }

    #[test]
    fn vpc_routing_change_validates_requested_vni() {
        value_scenarios!(
            run = |vni| ChangeVpcRoutingProfile::try_from(
                rpc::forge::VpcChangeRoutingProfileRequest {
                    id: Some(VpcId::new()),
                    if_version_match: Some("V1-T0".to_string()),
                    routing_profile_type: "EXTERNAL".to_string(),
                    vni: Some(vni),
                }
            )
            .map(|change| change.vni)
            .map_err(|error| tonic::Status::from(error).code());
            "valid exact VNI boundaries" {
                1 => Ok(Some(1)),
                16_777_215 => Ok(Some(16_777_215)),
            }
            "invalid exact VNI" {
                0 => Err(tonic::Code::InvalidArgument),
                16_777_216 => Err(tonic::Code::InvalidArgument),
                u32::MAX => Err(tonic::Code::InvalidArgument),
            }
        );
    }

    fn sample_vpc() -> Vpc {
        Vpc {
            id: VpcId::from(uuid::Uuid::new_v4()),
            version: ConfigVersion::initial(),
            config: VpcConfig {
                tenant_organization_id: "tenant-1".to_string(),
                tenant_keyset_id: Some("keyset-1".to_string()),
                network_virtualization_type: VpcVirtualizationType::Fnn,
                network_security_group_id: None,
                dhcp_option_set_id: None,
                default_nvlink_logical_partition_id: None,
                vni: Some(42),
                routing_profile_type: Some("EXTERNAL".to_string()),
                routing_profile_overrides: None,
                power_resource_group: Some("tenant-1".to_string()),
                slaac_enabled: true,
            },
            status: VpcStatus { vni: Some(100) },
            metadata: Metadata::new_with_default_name(),
            created: chrono::Utc::now(),
            updated: chrono::Utc::now(),
            deleted: None,
        }
    }

    #[test]
    fn vpc_to_rpc_populates_structured_fields() {
        let vpc = sample_vpc();
        let rpc_vpc = rpc::forge::Vpc::from(vpc);

        let config = rpc_vpc.config.as_ref().expect("config must be set");
        assert_eq!(config.tenant_organization_id, "tenant-1");
        assert_eq!(config.tenant_keyset_id.as_deref(), Some("keyset-1"));
        assert_eq!(config.vni, Some(42));
        assert_eq!(config.routing_profile_type.as_deref(), Some("EXTERNAL"));
        assert_eq!(config.power_resource_group.as_deref(), Some("tenant-1"));
        assert_eq!(config.slaac_enabled, Some(true));
        assert_eq!(
            config.network_virtualization_type,
            Some(rpc::forge::VpcVirtualizationType::Fnn as i32)
        );

        let status = rpc_vpc.status.as_ref().expect("status must be set");
        assert_eq!(status.vni, Some(100));
    }

    #[test]
    fn vpc_to_rpc_reports_slaac_presence() {
        value_scenarios!(
            run = |slaac_enabled| {
                let mut vpc = sample_vpc();
                vpc.config.slaac_enabled = slaac_enabled;
                rpc::forge::Vpc::from(vpc)
                    .config
                    .expect("config must be set")
                    .slaac_enabled
            };
            "enabled" {
                true => Some(true),
            }
            "disabled" {
                false => Some(false),
            }
        );
    }

    fn vpc_update_request(power_resource_group: Option<&str>) -> rpc::forge::VpcUpdateRequest {
        rpc::forge::VpcUpdateRequest {
            id: Some(VpcId::from(uuid::Uuid::new_v4())),
            if_version_match: None,
            metadata: None,
            network_security_group_id: None,
            dhcp_option_set_id: None,
            default_nvlink_logical_partition_id: None,
            routing_profile_overrides: None,
            power_resource_group: power_resource_group.map(str::to_string),
        }
    }

    #[test]
    fn vpc_update_power_resource_group_semantics() {
        let set = UpdateVpc::try_from(vpc_update_request(Some("power-group")))
            .expect("non-empty resource group should be accepted");
        assert_eq!(
            set.power_resource_group,
            Some(PowerResourceGroupUpdate::Set("power-group".to_string()))
        );

        let clear = UpdateVpc::try_from(vpc_update_request(Some("")))
            .expect("empty resource group should clear the association");
        assert_eq!(
            clear.power_resource_group,
            Some(PowerResourceGroupUpdate::Clear)
        );

        let omitted = UpdateVpc::try_from(vpc_update_request(None))
            .expect("omitted operation should be accepted");
        assert_eq!(omitted.power_resource_group, None);
    }

    #[test]
    fn vpc_creation_power_resource_group_semantics() {
        value_scenarios!(
            run = |power_resource_group| {
                NewVpc::try_from(rpc::forge::VpcCreationRequest {
                    tenant_organization_id: "tenant-1".to_string(),
                    power_resource_group,
                    ..Default::default()
                })
                .expect("creation request should be valid")
                .power_resource_group
            };
            "non-empty resource group is preserved" {
                Some("power-group".to_string()) => Some("power-group".to_string()),
            }
            "empty resource group is treated as unset" {
                Some(String::new()) => None,
            }
            "omitted resource group remains unset" {
                None => None,
            }
        );
    }

    #[test]
    fn vpc_creation_slaac_presence_semantics() {
        value_scenarios!(
            run = |slaac_enabled| {
                NewVpc::try_from(rpc::forge::VpcCreationRequest {
                    tenant_organization_id: "tenant-1".to_string(),
                    slaac_enabled,
                    ..Default::default()
                })
                .expect("creation request should be valid")
                .slaac_enabled
            };
            "enabled explicitly" {
                Some(true) => true,
            }
            "disabled explicitly" {
                Some(false) => false,
            }
            "omission defaults to disabled" {
                None => false,
            }
        );
    }

    // `VpcSearchFilter::from` is a total conversion, so we project its output to
    // the fields the originals asserted: name, tenant_org_id, and the label as its
    // (key, value) pair (None when no label is present).
    #[test]
    fn vpc_search_filter_from_rpc() {
        type Projected = (
            Option<String>,
            Option<String>,
            Option<(String, Option<String>)>,
        );

        value_scenarios!(
            run = |rpc_filter| {
                let filter = VpcSearchFilter::from(rpc_filter);
                let projected: Projected = (
                    filter.name,
                    filter.tenant_org_id,
                    filter.label.map(|l| (l.key, l.value)),
                );
                projected
            };
            "all fields populated" {
                rpc::forge::VpcSearchFilter {
                    name: Some("my-vpc".to_string()),
                    tenant_org_id: Some("org-123".to_string()),
                    label: Some(rpc::forge::Label {
                        key: "env".to_string(),
                        value: Some("prod".to_string()),
                    }),
                } => (
                    Some("my-vpc".to_string()),
                    Some("org-123".to_string()),
                    Some(("env".to_string(), Some("prod".to_string()))),
                ),
            }

            "no fields" {
                rpc::forge::VpcSearchFilter {
                    name: None,
                    tenant_org_id: None,
                    label: None,
                } => (None, None, None),
            }

            "label key only" {
                rpc::forge::VpcSearchFilter {
                    name: None,
                    tenant_org_id: None,
                    label: Some(rpc::forge::Label {
                        key: "team".to_string(),
                        value: None,
                    }),
                } => (None, None, Some(("team".to_string(), None))),
            }
        );
    }
}
