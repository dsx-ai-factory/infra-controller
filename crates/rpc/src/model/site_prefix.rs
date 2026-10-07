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

use ipnetwork::IpNetwork;
use model::metadata::Metadata;
use model::site_prefix::{
    NewTenantManagedSitePrefix, PrefixMatch, RetireTenantManagedSitePrefix, SitePrefix,
    SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope, SitePrefixSearchFilter,
    UpdateSitePrefixMetadata,
};
use model::tenant::TenantOrganizationId;

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl From<SitePrefixAuthority> for rpc::SitePrefixAuthority {
    fn from(value: SitePrefixAuthority) -> Self {
        match value {
            SitePrefixAuthority::OperatorManaged => Self::OperatorManaged,
            SitePrefixAuthority::TenantManaged => Self::TenantManaged,
        }
    }
}

impl TryFrom<rpc::SitePrefixAuthority> for SitePrefixAuthority {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixAuthority) -> Result<Self, Self::Error> {
        match value {
            rpc::SitePrefixAuthority::OperatorManaged => Ok(Self::OperatorManaged),
            rpc::SitePrefixAuthority::TenantManaged => Ok(Self::TenantManaged),
            rpc::SitePrefixAuthority::Unspecified => Err(RpcDataConversionError::InvalidValue(
                "SitePrefixAuthority".to_string(),
                value.as_str_name().to_string(),
            )),
        }
    }
}

impl From<SitePrefixRoutingScope> for rpc::SitePrefixRoutingScope {
    fn from(value: SitePrefixRoutingScope) -> Self {
        match value {
            SitePrefixRoutingScope::DatacenterOnly => Self::DatacenterOnly,
        }
    }
}

impl TryFrom<rpc::SitePrefixRoutingScope> for SitePrefixRoutingScope {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixRoutingScope) -> Result<Self, Self::Error> {
        match value {
            rpc::SitePrefixRoutingScope::DatacenterOnly => Ok(Self::DatacenterOnly),
            rpc::SitePrefixRoutingScope::Unspecified => Err(RpcDataConversionError::InvalidValue(
                "SitePrefixRoutingScope".to_string(),
                value.as_str_name().to_string(),
            )),
        }
    }
}

impl From<SitePrefixLifecycleState> for rpc::SitePrefixLifecycleState {
    fn from(value: SitePrefixLifecycleState) -> Self {
        match value {
            SitePrefixLifecycleState::Provisioning => Self::Provisioning,
            SitePrefixLifecycleState::Ready => Self::Ready,
            SitePrefixLifecycleState::Deleting => Self::Deleting,
            SitePrefixLifecycleState::Error => Self::Error,
        }
    }
}

impl TryFrom<rpc::SitePrefixLifecycleState> for SitePrefixLifecycleState {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixLifecycleState) -> Result<Self, RpcDataConversionError> {
        match value {
            rpc::SitePrefixLifecycleState::Provisioning => Ok(Self::Provisioning),
            rpc::SitePrefixLifecycleState::Ready => Ok(Self::Ready),
            rpc::SitePrefixLifecycleState::Deleting => Ok(Self::Deleting),
            rpc::SitePrefixLifecycleState::Error => Ok(Self::Error),
            rpc::SitePrefixLifecycleState::Unspecified => {
                Err(RpcDataConversionError::InvalidValue(
                    "SitePrefixLifecycleState".to_string(),
                    value.as_str_name().to_string(),
                ))
            }
        }
    }
}

impl From<SitePrefix> for rpc::SitePrefix {
    fn from(value: SitePrefix) -> Self {
        let SitePrefix {
            id,
            config,
            metadata,
            status,
            version,
            created_at,
            updated_at,
        } = value;

        Self {
            id: Some(id),
            config: Some(rpc::SitePrefixConfig {
                prefix: config.prefix.to_string(),
                tenant_organization_id: config
                    .tenant_organization_id
                    .map(|tenant_organization_id| tenant_organization_id.to_string()),
                routing_scope: rpc::SitePrefixRoutingScope::from(config.routing_scope) as i32,
            }),
            status: Some(rpc::SitePrefixStatus {
                authority: rpc::SitePrefixAuthority::from(status.authority) as i32,
                lifecycle_state: rpc::SitePrefixLifecycleState::from(status.lifecycle_state) as i32,
                quota: None,
            }),
            metadata: Some(metadata.into()),
            version: version.version_string(),
            created_at: Some(created_at.into()),
            updated_at: Some(updated_at.into()),
        }
    }
}

impl rpc::SitePrefix {
    /// Adds the tenant aggregate quota computed by Core's read path.
    pub fn with_quota(mut self, used: u32, limit: u32) -> Self {
        if let Some(status) = &mut self.status {
            status.quota = Some(rpc::SitePrefixQuotaUsage { used, limit });
        }
        self
    }
}

impl TryFrom<rpc::SitePrefixCreationRequest> for NewTenantManagedSitePrefix {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixCreationRequest) -> Result<Self, Self::Error> {
        let id = value
            .id
            .ok_or(RpcDataConversionError::MissingArgument("id"))?;
        let tenant_organization_id =
            TenantOrganizationId::try_from(value.tenant_organization_id.clone()).map_err(|_| {
                RpcDataConversionError::InvalidTenantOrg(value.tenant_organization_id.clone())
            })?;
        let prefix = IpNetwork::try_from(value.prefix.as_str())?;
        let metadata = value
            .metadata
            .map(TryInto::try_into)
            .transpose()?
            .unwrap_or_else(Metadata::new_with_default_name);

        let site_prefix = Self {
            id,
            tenant_organization_id,
            prefix,
            metadata,
        };
        site_prefix.validate().map_err(|error| {
            RpcDataConversionError::InvalidArgument(format!(
                "tenant-managed SitePrefix is not valid: {error}"
            ))
        })?;
        Ok(site_prefix)
    }
}

impl TryFrom<rpc::SitePrefixUpdateRequest> for UpdateSitePrefixMetadata {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixUpdateRequest) -> Result<Self, Self::Error> {
        let id = value
            .id
            .ok_or(RpcDataConversionError::MissingArgument("id"))?;
        let tenant_organization_id =
            TenantOrganizationId::try_from(value.tenant_organization_id.clone()).map_err(|_| {
                RpcDataConversionError::InvalidTenantOrg(value.tenant_organization_id.clone())
            })?;
        let metadata: Metadata = value
            .metadata
            .ok_or(RpcDataConversionError::MissingArgument("metadata"))?
            .try_into()?;
        metadata.validate(true).map_err(|error| {
            RpcDataConversionError::InvalidArgument(format!(
                "SitePrefix metadata is not valid: {error}"
            ))
        })?;
        let if_version_match = value
            .if_version_match
            .map(|version| {
                version
                    .parse()
                    .map_err(|_| RpcDataConversionError::InvalidConfigVersion(version.to_string()))
            })
            .transpose()?;

        Ok(Self {
            id,
            tenant_organization_id,
            metadata,
            if_version_match,
        })
    }
}

impl TryFrom<rpc::SitePrefixDeletionRequest> for RetireTenantManagedSitePrefix {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixDeletionRequest) -> Result<Self, Self::Error> {
        let id = value
            .id
            .ok_or(RpcDataConversionError::MissingArgument("id"))?;
        let tenant_organization_id =
            TenantOrganizationId::try_from(value.tenant_organization_id.clone()).map_err(|_| {
                RpcDataConversionError::InvalidTenantOrg(value.tenant_organization_id)
            })?;

        Ok(Self {
            id,
            tenant_organization_id,
        })
    }
}

impl TryFrom<rpc::SitePrefixSearchFilter> for SitePrefixSearchFilter {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::SitePrefixSearchFilter) -> Result<Self, Self::Error> {
        let tenant_organization_id = value
            .tenant_organization_id
            .map(|tenant_organization_id| {
                TenantOrganizationId::try_from(tenant_organization_id.clone())
                    .map_err(|_| RpcDataConversionError::InvalidTenantOrg(tenant_organization_id))
            })
            .transpose()?;

        let authority = value
            .authority
            .map(parse_site_prefix_authority)
            .transpose()?;
        let routing_scope = value
            .routing_scope
            .map(parse_site_prefix_routing_scope)
            .transpose()?;
        let lifecycle_state = value
            .lifecycle_state
            .map(parse_site_prefix_lifecycle_state)
            .transpose()?;

        let prefix_match = match (value.prefix_match, value.prefix_match_type) {
            (None, None) => None,
            (Some(prefix), Some(prefix_match_type)) => {
                let prefix = IpNetwork::try_from(prefix.as_str())?;
                let prefix_match_type =
                    rpc::PrefixMatchType::try_from(prefix_match_type).map_err(|_| {
                        RpcDataConversionError::InvalidValue(
                            "PrefixMatchType".to_string(),
                            prefix_match_type.to_string(),
                        )
                    })?;
                Some(match prefix_match_type {
                    rpc::PrefixMatchType::PrefixExact => PrefixMatch::Exact(prefix),
                    rpc::PrefixMatchType::PrefixContains => PrefixMatch::Contains(prefix),
                    rpc::PrefixMatchType::PrefixContainedBy => PrefixMatch::ContainedBy(prefix),
                })
            }
            (Some(_), None) => {
                return Err(RpcDataConversionError::MissingArgument("prefix_match_type"));
            }
            (None, Some(_)) => {
                return Err(RpcDataConversionError::InvalidArgument(
                    "prefix_match_type requires prefix_match".to_string(),
                ));
            }
        };

        Ok(Self {
            tenant_organization_id,
            authority,
            routing_scope,
            lifecycle_state,
            prefix_match,
        })
    }
}

fn parse_site_prefix_authority(value: i32) -> Result<SitePrefixAuthority, RpcDataConversionError> {
    let value = rpc::SitePrefixAuthority::try_from(value).map_err(|_| {
        RpcDataConversionError::InvalidValue("SitePrefixAuthority".to_string(), value.to_string())
    })?;
    value.try_into()
}

fn parse_site_prefix_routing_scope(
    value: i32,
) -> Result<SitePrefixRoutingScope, RpcDataConversionError> {
    let value = rpc::SitePrefixRoutingScope::try_from(value).map_err(|_| {
        RpcDataConversionError::InvalidValue(
            "SitePrefixRoutingScope".to_string(),
            value.to_string(),
        )
    })?;
    value.try_into()
}

fn parse_site_prefix_lifecycle_state(
    value: i32,
) -> Result<SitePrefixLifecycleState, RpcDataConversionError> {
    let value = rpc::SitePrefixLifecycleState::try_from(value).map_err(|_| {
        RpcDataConversionError::InvalidValue(
            "SitePrefixLifecycleState".to_string(),
            value.to_string(),
        )
    })?;
    value.try_into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use carbide_uuid::site_prefix::SitePrefixId;
    use chrono::{DateTime, Utc};
    use config_version::ConfigVersion;
    use model::metadata::Metadata;
    use model::site_prefix::{SitePrefixConfig, SitePrefixStatus};
    use prost::Message;

    use super::*;

    #[derive(Debug, Default, Eq, PartialEq)]
    struct SearchFilterView {
        tenant_organization_id: Option<String>,
        authority: Option<SitePrefixAuthority>,
        routing_scope: Option<SitePrefixRoutingScope>,
        lifecycle_state: Option<SitePrefixLifecycleState>,
        prefix_match: Option<(&'static str, String)>,
    }

    impl From<SitePrefixSearchFilter> for SearchFilterView {
        fn from(value: SitePrefixSearchFilter) -> Self {
            let prefix_match = value.prefix_match.map(|prefix_match| match prefix_match {
                PrefixMatch::Exact(prefix) => ("exact", prefix.to_string()),
                PrefixMatch::Contains(prefix) => ("contains", prefix.to_string()),
                PrefixMatch::ContainedBy(prefix) => ("contained_by", prefix.to_string()),
            });

            Self {
                tenant_organization_id: value
                    .tenant_organization_id
                    .map(|tenant_organization_id| tenant_organization_id.to_string()),
                authority: value.authority,
                routing_scope: value.routing_scope,
                lifecycle_state: value.lifecycle_state,
                prefix_match,
            }
        }
    }

    fn rpc_metadata(name: &str) -> rpc::Metadata {
        rpc::Metadata {
            name: name.to_string(),
            description: "test metadata".to_string(),
            labels: vec![rpc::Label {
                key: "env".to_string(),
                value: Some("test".to_string()),
            }],
        }
    }

    #[test]
    fn site_prefix_authority_uses_stable_numbers_and_canonical_names() {
        check_values(
            [
                Check {
                    scenario: "unspecified remains value 0",
                    input: rpc::SitePrefixAuthority::Unspecified,
                    expect: (
                        0,
                        "SITE_PREFIX_AUTHORITY_UNSPECIFIED",
                        Some(rpc::SitePrefixAuthority::Unspecified),
                    ),
                },
                Check {
                    scenario: "operator-managed remains value 1",
                    input: rpc::SitePrefixAuthority::OperatorManaged,
                    expect: (
                        1,
                        "SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED",
                        Some(rpc::SitePrefixAuthority::OperatorManaged),
                    ),
                },
                Check {
                    scenario: "tenant-managed remains value 2",
                    input: rpc::SitePrefixAuthority::TenantManaged,
                    expect: (
                        2,
                        "SITE_PREFIX_AUTHORITY_TENANT_MANAGED",
                        Some(rpc::SitePrefixAuthority::TenantManaged),
                    ),
                },
            ],
            |authority| {
                let name = authority.as_str_name();
                (
                    authority as i32,
                    name,
                    rpc::SitePrefixAuthority::from_str_name(name),
                )
            },
        );
    }

    #[test]
    fn site_prefix_authority_descriptor_preserves_configured_alias() {
        let descriptor_set =
            prost_types::FileDescriptorSet::decode(crate::REFLECTION_API_SERVICE_DESCRIPTOR)
                .unwrap();
        let forge = descriptor_set
            .file
            .iter()
            .find(|file| file.package.as_deref() == Some("forge"))
            .unwrap();
        let authority = forge
            .enum_type
            .iter()
            .find(|enumeration| enumeration.name.as_deref() == Some("SitePrefixAuthority"))
            .unwrap();

        assert_eq!(
            authority
                .options
                .as_ref()
                .and_then(|options| options.allow_alias),
            Some(true)
        );

        let values = authority
            .value
            .iter()
            .map(|value| {
                (
                    value.name.as_deref().unwrap(),
                    value.number.unwrap(),
                    value
                        .options
                        .as_ref()
                        .and_then(|options| options.deprecated)
                        .unwrap_or(false),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                ("SITE_PREFIX_AUTHORITY_UNSPECIFIED", 0, false),
                ("SITE_PREFIX_AUTHORITY_OPERATOR_MANAGED", 1, false),
                ("SITE_PREFIX_AUTHORITY_CONFIGURED", 1, true),
                ("SITE_PREFIX_AUTHORITY_TENANT_MANAGED", 2, false),
            ]
        );
    }

    // The deprecated `Configured` spelling and canonical `OperatorManaged`
    // spelling both use value 1, so existing protobuf bytes retain their
    // meaning after the rename.
    #[test]
    fn operator_managed_authority_preserves_configured_wire_value() {
        let status = rpc::SitePrefixStatus {
            authority: rpc::SitePrefixAuthority::OperatorManaged as i32,
            ..Default::default()
        };

        let bytes = status.encode_to_vec();
        assert_eq!(bytes, [0x08, 0x01]);

        let decoded = rpc::SitePrefixStatus::decode(bytes.as_slice()).unwrap();
        assert_eq!(
            rpc::SitePrefixAuthority::try_from(decoded.authority).unwrap(),
            rpc::SitePrefixAuthority::OperatorManaged
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    struct CreationView {
        id: SitePrefixId,
        tenant_organization_id: String,
        prefix: String,
        metadata_name: String,
    }

    impl From<NewTenantManagedSitePrefix> for CreationView {
        fn from(value: NewTenantManagedSitePrefix) -> Self {
            Self {
                id: value.id,
                tenant_organization_id: value.tenant_organization_id.to_string(),
                prefix: value.prefix.to_string(),
                metadata_name: value.metadata.name,
            }
        }
    }

    #[test]
    fn site_prefix_creation_conversion_is_strict() {
        let id = SitePrefixId::new();
        check_cases(
            [
                Case {
                    scenario: "valid request",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Yields(CreationView {
                        id,
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        metadata_name: "tenant prefix".to_string(),
                    }),
                },
                Case {
                    scenario: "metadata defaults on create",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "172.16.0.0/16".to_string(),
                        metadata: None,
                    },
                    expect: Yields(CreationView {
                        id,
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "172.16.0.0/16".to_string(),
                        metadata_name: "default_name".to_string(),
                    }),
                },
                Case {
                    scenario: "missing ID",
                    input: rpc::SitePrefixCreationRequest {
                        id: None,
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid tenant",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant a".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "non-canonical prefix",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "10.0.0.1/24".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "non-private prefix",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "203.0.113.0/24".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "IPv6 prefix",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "fd00::/48".to_string(),
                        metadata: Some(rpc_metadata("tenant prefix")),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid metadata",
                    input: rpc::SitePrefixCreationRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        prefix: "10.0.0.0/24".to_string(),
                        metadata: Some(rpc_metadata("x")),
                    },
                    expect: Fails,
                },
            ],
            |request| {
                NewTenantManagedSitePrefix::try_from(request)
                    .map(CreationView::from)
                    .map_err(drop)
            },
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    struct UpdateView {
        id: SitePrefixId,
        tenant_organization_id: String,
        metadata_name: String,
        if_version_match: Option<String>,
    }

    impl From<UpdateSitePrefixMetadata> for UpdateView {
        fn from(value: UpdateSitePrefixMetadata) -> Self {
            Self {
                id: value.id,
                tenant_organization_id: value.tenant_organization_id.to_string(),
                metadata_name: value.metadata.name,
                if_version_match: value
                    .if_version_match
                    .map(|version| version.version_string()),
            }
        }
    }

    #[test]
    fn site_prefix_update_conversion_requires_valid_metadata() {
        let id = SitePrefixId::new();
        let version = ConfigVersion::initial().version_string();
        check_cases(
            [
                Case {
                    scenario: "valid versioned update",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: Some(rpc_metadata("updated prefix")),
                        if_version_match: Some(version.clone()),
                    },
                    expect: Yields(UpdateView {
                        id,
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata_name: "updated prefix".to_string(),
                        if_version_match: Some(version),
                    }),
                },
                Case {
                    scenario: "valid unversioned update",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: Some(rpc_metadata("updated prefix")),
                        if_version_match: None,
                    },
                    expect: Yields(UpdateView {
                        id,
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata_name: "updated prefix".to_string(),
                        if_version_match: None,
                    }),
                },
                Case {
                    scenario: "missing ID",
                    input: rpc::SitePrefixUpdateRequest {
                        id: None,
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: Some(rpc_metadata("updated prefix")),
                        if_version_match: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid tenant",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant a".to_string(),
                        metadata: Some(rpc_metadata("updated prefix")),
                        if_version_match: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "missing metadata",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: None,
                        if_version_match: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid metadata",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: Some(rpc_metadata("x")),
                        if_version_match: None,
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid version",
                    input: rpc::SitePrefixUpdateRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                        metadata: Some(rpc_metadata("updated prefix")),
                        if_version_match: Some("not-a-version".to_string()),
                    },
                    expect: Fails,
                },
            ],
            |request| {
                UpdateSitePrefixMetadata::try_from(request)
                    .map(UpdateView::from)
                    .map_err(drop)
            },
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    struct DeletionView {
        id: SitePrefixId,
        tenant_organization_id: String,
    }

    impl From<RetireTenantManagedSitePrefix> for DeletionView {
        fn from(value: RetireTenantManagedSitePrefix) -> Self {
            Self {
                id: value.id,
                tenant_organization_id: value.tenant_organization_id.to_string(),
            }
        }
    }

    #[test]
    fn site_prefix_deletion_conversion_requires_identity_and_owner() {
        let id = SitePrefixId::new();
        check_cases(
            [
                Case {
                    scenario: "valid retirement request",
                    input: rpc::SitePrefixDeletionRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant-a".to_string(),
                    },
                    expect: Yields(DeletionView {
                        id,
                        tenant_organization_id: "tenant-a".to_string(),
                    }),
                },
                Case {
                    scenario: "missing ID",
                    input: rpc::SitePrefixDeletionRequest {
                        id: None,
                        tenant_organization_id: "tenant-a".to_string(),
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid tenant",
                    input: rpc::SitePrefixDeletionRequest {
                        id: Some(id),
                        tenant_organization_id: "tenant a".to_string(),
                    },
                    expect: Fails,
                },
            ],
            |request| {
                RetireTenantManagedSitePrefix::try_from(request)
                    .map(DeletionView::from)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn site_prefix_search_filter_conversion_is_strict() {
        check_cases(
            [
                Case {
                    scenario: "empty filter returns the complete inventory",
                    input: rpc::SitePrefixSearchFilter::default(),
                    expect: Yields(SearchFilterView::default()),
                },
                Case {
                    scenario: "all scalar filters and exact prefix match",
                    input: rpc::SitePrefixSearchFilter {
                        tenant_organization_id: Some("tenant-a".to_string()),
                        authority: Some(rpc::SitePrefixAuthority::TenantManaged as i32),
                        routing_scope: Some(rpc::SitePrefixRoutingScope::DatacenterOnly as i32),
                        lifecycle_state: Some(rpc::SitePrefixLifecycleState::Provisioning as i32),
                        prefix_match: Some("10.0.0.0/8".to_string()),
                        prefix_match_type: Some(rpc::PrefixMatchType::PrefixExact as i32),
                    },
                    expect: Yields(SearchFilterView {
                        tenant_organization_id: Some("tenant-a".to_string()),
                        authority: Some(SitePrefixAuthority::TenantManaged),
                        routing_scope: Some(SitePrefixRoutingScope::DatacenterOnly),
                        lifecycle_state: Some(SitePrefixLifecycleState::Provisioning),
                        prefix_match: Some(("exact", "10.0.0.0/8".to_string())),
                    }),
                },
                Case {
                    scenario: "contains prefix match",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match: Some("10.0.0.0/24".to_string()),
                        prefix_match_type: Some(rpc::PrefixMatchType::PrefixContains as i32),
                        ..Default::default()
                    },
                    expect: Yields(SearchFilterView {
                        prefix_match: Some(("contains", "10.0.0.0/24".to_string())),
                        ..Default::default()
                    }),
                },
                Case {
                    scenario: "contained-by prefix match",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match: Some("10.0.0.0/8".to_string()),
                        prefix_match_type: Some(rpc::PrefixMatchType::PrefixContainedBy as i32),
                        ..Default::default()
                    },
                    expect: Yields(SearchFilterView {
                        prefix_match: Some(("contained_by", "10.0.0.0/8".to_string())),
                        ..Default::default()
                    }),
                },
                Case {
                    scenario: "invalid tenant organization ID",
                    input: rpc::SitePrefixSearchFilter {
                        tenant_organization_id: Some("tenant a".to_string()),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "unspecified authority",
                    input: rpc::SitePrefixSearchFilter {
                        authority: Some(rpc::SitePrefixAuthority::Unspecified as i32),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "unknown authority",
                    input: rpc::SitePrefixSearchFilter {
                        authority: Some(999),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "unspecified routing scope",
                    input: rpc::SitePrefixSearchFilter {
                        routing_scope: Some(rpc::SitePrefixRoutingScope::Unspecified as i32),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "unspecified lifecycle state",
                    input: rpc::SitePrefixSearchFilter {
                        lifecycle_state: Some(rpc::SitePrefixLifecycleState::Unspecified as i32),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "prefix without match type",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match: Some("10.0.0.0/8".to_string()),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "match type without prefix",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match_type: Some(rpc::PrefixMatchType::PrefixExact as i32),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "invalid prefix",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match: Some("not-a-prefix".to_string()),
                        prefix_match_type: Some(rpc::PrefixMatchType::PrefixExact as i32),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "unknown prefix match type",
                    input: rpc::SitePrefixSearchFilter {
                        prefix_match: Some("10.0.0.0/8".to_string()),
                        prefix_match_type: Some(999),
                        ..Default::default()
                    },
                    expect: Fails,
                },
            ],
            |filter| {
                SitePrefixSearchFilter::try_from(filter)
                    .map(SearchFilterView::from)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn site_prefix_model_to_rpc_conversion_covers_inventory_variants() {
        struct ConversionCase {
            scenario: &'static str,
            authority: SitePrefixAuthority,
            tenant_organization_id: Option<&'static str>,
            lifecycle_state: SitePrefixLifecycleState,
        }

        let cases = [
            ConversionCase {
                scenario: "operator-managed ready",
                authority: SitePrefixAuthority::OperatorManaged,
                tenant_organization_id: None,
                lifecycle_state: SitePrefixLifecycleState::Ready,
            },
            ConversionCase {
                scenario: "operator-managed deleting",
                authority: SitePrefixAuthority::OperatorManaged,
                tenant_organization_id: None,
                lifecycle_state: SitePrefixLifecycleState::Deleting,
            },
            ConversionCase {
                scenario: "tenant provisioning",
                authority: SitePrefixAuthority::TenantManaged,
                tenant_organization_id: Some("tenant-a"),
                lifecycle_state: SitePrefixLifecycleState::Provisioning,
            },
            ConversionCase {
                scenario: "tenant ready",
                authority: SitePrefixAuthority::TenantManaged,
                tenant_organization_id: Some("tenant-a"),
                lifecycle_state: SitePrefixLifecycleState::Ready,
            },
            ConversionCase {
                scenario: "tenant deleting",
                authority: SitePrefixAuthority::TenantManaged,
                tenant_organization_id: Some("tenant-a"),
                lifecycle_state: SitePrefixLifecycleState::Deleting,
            },
            ConversionCase {
                scenario: "tenant error",
                authority: SitePrefixAuthority::TenantManaged,
                tenant_organization_id: Some("tenant-a"),
                lifecycle_state: SitePrefixLifecycleState::Error,
            },
        ];

        for case in cases {
            let id = SitePrefixId::new();
            let created_at: DateTime<Utc> = "2026-07-23T12:00:00Z".parse().unwrap();
            let updated_at: DateTime<Utc> = "2026-07-23T12:01:00Z".parse().unwrap();
            let version = ConfigVersion::initial();
            let model = SitePrefix {
                id,
                config: SitePrefixConfig {
                    prefix: "10.0.0.0/8".parse().unwrap(),
                    tenant_organization_id: case
                        .tenant_organization_id
                        .map(|tenant_organization_id| tenant_organization_id.parse().unwrap()),
                    routing_scope: SitePrefixRoutingScope::DatacenterOnly,
                },
                metadata: Metadata {
                    name: "site-prefix".to_string(),
                    description: "conversion test".to_string(),
                    labels: HashMap::from([("env".to_string(), "test".to_string())]),
                },
                status: SitePrefixStatus {
                    authority: case.authority,
                    lifecycle_state: case.lifecycle_state,
                },
                version,
                created_at,
                updated_at,
            };

            let converted = rpc::SitePrefix::from(model);
            assert_eq!(converted.id, Some(id), "{}", case.scenario);
            let tenant_quota = (case.authority == SitePrefixAuthority::TenantManaged).then(|| {
                converted
                    .clone()
                    .with_quota(3, 8)
                    .status
                    .unwrap()
                    .quota
                    .unwrap()
            });
            assert_eq!(
                converted.version,
                version.version_string(),
                "{}",
                case.scenario
            );
            assert_eq!(
                DateTime::<Utc>::try_from(converted.created_at.unwrap()).unwrap(),
                created_at,
                "{}",
                case.scenario
            );
            assert_eq!(
                DateTime::<Utc>::try_from(converted.updated_at.unwrap()).unwrap(),
                updated_at,
                "{}",
                case.scenario
            );

            let config = converted.config.expect("config should be populated");
            assert_eq!(config.prefix, "10.0.0.0/8", "{}", case.scenario);
            assert_eq!(
                config.tenant_organization_id.as_deref(),
                case.tenant_organization_id,
                "{}",
                case.scenario
            );
            assert_eq!(
                rpc::SitePrefixRoutingScope::try_from(config.routing_scope).unwrap(),
                rpc::SitePrefixRoutingScope::DatacenterOnly,
                "{}",
                case.scenario
            );

            let status = converted.status.expect("status should be populated");
            assert!(
                status.quota.is_none(),
                "base conversion should not invent tenant quota"
            );
            assert_eq!(
                SitePrefixAuthority::try_from(
                    rpc::SitePrefixAuthority::try_from(status.authority).unwrap()
                )
                .unwrap(),
                case.authority,
                "{}",
                case.scenario
            );
            assert_eq!(
                SitePrefixLifecycleState::try_from(
                    rpc::SitePrefixLifecycleState::try_from(status.lifecycle_state).unwrap()
                )
                .unwrap(),
                case.lifecycle_state,
                "{}",
                case.scenario
            );

            if let Some(quota) = tenant_quota {
                assert_eq!(quota.used, 3, "{}", case.scenario);
                assert_eq!(quota.limit, 8, "{}", case.scenario);
            }

            let metadata = converted.metadata.expect("metadata should be populated");
            assert_eq!(metadata.name, "site-prefix", "{}", case.scenario);
            assert_eq!(metadata.description, "conversion test", "{}", case.scenario);
            assert_eq!(metadata.labels.len(), 1, "{}", case.scenario);
        }
    }
}
