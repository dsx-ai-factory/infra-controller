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
    RouteTargetConfig, UpdateVpc, UpdateVpcVirtualization, Vpc, VpcPeering,
    VpcRoutingProfileOverrides, VpcSearchFilter, VpcStatus,
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

impl TryFrom<rpc::forge::VpcRoutingProfileOverrides> for VpcRoutingProfileOverrides {
    type Error = RpcDataConversionError;

    fn try_from(profile: rpc::forge::VpcRoutingProfileOverrides) -> Result<Self, Self::Error> {
        Ok(Self {
            route_target_imports: profile.route_target_imports.map(|targets| {
                targets
                    .values
                    .into_iter()
                    .map(|target| RouteTargetConfig {
                        asn: target.asn,
                        vni: target.vni,
                    })
                    .collect()
            }),
            route_targets_on_exports: profile.route_targets_on_exports.map(|targets| {
                targets
                    .values
                    .into_iter()
                    .map(|target| RouteTargetConfig {
                        asn: target.asn,
                        vni: target.vni,
                    })
                    .collect()
            }),
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
            route_target_imports: profile.route_target_imports.map(|targets| {
                rpc::common::RouteTargets {
                    values: targets
                        .into_iter()
                        .map(|target| rpc::common::RouteTarget {
                            asn: target.asn,
                            vni: target.vni,
                        })
                        .collect(),
                }
            }),
            route_targets_on_exports: profile.route_targets_on_exports.map(|targets| {
                rpc::common::RouteTargets {
                    values: targets
                        .into_iter()
                        .map(|target| rpc::common::RouteTarget {
                            asn: target.asn,
                            vni: target.vni,
                        })
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
        } = db_vpc_peering;

        let id = Some(id);
        let vpc_id = Some(vpc_id);
        let peer_vpc_id = Some(peer_vpc_id);

        Self {
            id,
            vpc_id,
            peer_vpc_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::value_scenarios;
    use carbide_uuid::vpc::VpcId;
    use model::vpc::VpcConfig;

    use super::*;

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
