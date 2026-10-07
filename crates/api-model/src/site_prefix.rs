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

use carbide_uuid::site_prefix::SitePrefixId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::ConfigValidationError;
use crate::metadata::Metadata;
use crate::tenant::TenantOrganizationId;

/// Identifies which lifecycle authority owns a site prefix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[sqlx(type_name = "site_prefix_authority")]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // `Managed` is part of both canonical authority names.
pub enum SitePrefixAuthority {
    #[serde(alias = "configured")]
    OperatorManaged,
    TenantManaged,
}

/// Describes where a site prefix may be routed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[sqlx(type_name = "site_prefix_routing_scope")]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SitePrefixRoutingScope {
    DatacenterOnly,
}

/// Tenant-facing lifecycle state for a site prefix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, sqlx::Type)]
#[sqlx(type_name = "site_prefix_lifecycle_state")]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SitePrefixLifecycleState {
    Provisioning,
    Ready,
    Deleting,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitePrefix {
    pub id: SitePrefixId,
    pub config: SitePrefixConfig,
    pub metadata: Metadata,
    pub status: SitePrefixStatus,
    pub version: ConfigVersion,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitePrefixConfig {
    pub prefix: IpNetwork,
    pub tenant_organization_id: Option<TenantOrganizationId>,
    pub routing_scope: SitePrefixRoutingScope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitePrefixStatus {
    pub authority: SitePrefixAuthority,
    pub lifecycle_state: SitePrefixLifecycleState,
}

/// A site prefix before it is persisted.
#[derive(Clone, Debug)]
pub struct NewSitePrefix {
    pub id: SitePrefixId,
    pub config: SitePrefixConfig,
    pub metadata: Metadata,
    pub status: SitePrefixStatus,
}

impl NewSitePrefix {
    /// Builds an operator-managed row from a configured site fabric prefix.
    pub fn configured(prefix: IpNetwork) -> Self {
        let prefix = IpNetwork::new(prefix.network(), prefix.prefix())
            .expect("an existing IP network has a valid prefix length");
        Self {
            id: SitePrefixId::new(),
            config: SitePrefixConfig {
                prefix,
                tenant_organization_id: None,
                routing_scope: SitePrefixRoutingScope::DatacenterOnly,
            },
            metadata: Metadata {
                name: prefix.to_string(),
                ..Metadata::default()
            },
            status: SitePrefixStatus {
                authority: SitePrefixAuthority::OperatorManaged,
                lifecycle_state: SitePrefixLifecycleState::Ready,
            },
        }
    }

    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let prefix = self.config.prefix;
        if prefix.ip() != prefix.network() {
            return Err(ConfigValidationError::invalid_value(format!(
                "site prefix {prefix} is not in canonical CIDR form"
            )));
        }

        match (
            self.status.authority,
            self.config.tenant_organization_id.is_some(),
        ) {
            (SitePrefixAuthority::OperatorManaged, false)
            | (SitePrefixAuthority::TenantManaged, true) => {}
            (SitePrefixAuthority::OperatorManaged, true) => {
                return Err(ConfigValidationError::invalid_value(
                    "an operator-managed site prefix cannot have a tenant owner",
                ));
            }
            (SitePrefixAuthority::TenantManaged, false) => {
                return Err(ConfigValidationError::invalid_value(
                    "a tenant-managed site prefix requires a tenant owner",
                ));
            }
        }

        if self.status.authority == SitePrefixAuthority::OperatorManaged
            && !matches!(
                self.status.lifecycle_state,
                SitePrefixLifecycleState::Ready | SitePrefixLifecycleState::Deleting
            )
        {
            return Err(ConfigValidationError::invalid_value(
                "an operator-managed site prefix must be ready or deleting",
            ));
        }

        self.metadata.validate(false)
    }
}

/// `NewTenantManagedSitePrefix` is the caller-controlled part of a tenant
/// SitePrefix create. Core derives the authority, routing scope, and initial
/// lifecycle state when the row is persisted.
#[derive(Clone, Debug)]
pub struct NewTenantManagedSitePrefix {
    pub id: SitePrefixId,
    pub tenant_organization_id: TenantOrganizationId,
    pub prefix: IpNetwork,
    pub metadata: Metadata,
}

impl NewTenantManagedSitePrefix {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        // Keep the persisted subset of this policy in sync with the
        // `site_prefixes_tenant_admission_check` database constraint.
        let prefix = self.prefix;
        if prefix.ip() != prefix.network() {
            return Err(ConfigValidationError::invalid_value(format!(
                "site prefix {prefix} is not in canonical CIDR form"
            )));
        }

        let IpNetwork::V4(prefix) = prefix else {
            return Err(ConfigValidationError::invalid_value(
                "tenant-managed site prefixes must use IPv4",
            ));
        };

        if !(8..=31).contains(&prefix.prefix()) {
            return Err(ConfigValidationError::invalid_value(
                "tenant-managed site prefix lengths must be between /8 and /31",
            ));
        }

        let private_roots = [
            "10.0.0.0/8".parse::<IpNetwork>().unwrap(),
            "172.16.0.0/12".parse::<IpNetwork>().unwrap(),
            "192.168.0.0/16".parse::<IpNetwork>().unwrap(),
        ];
        if !private_roots.iter().any(|private_root| {
            private_root.prefix() <= prefix.prefix() && private_root.contains(prefix.ip().into())
        }) {
            return Err(ConfigValidationError::invalid_value(format!(
                "tenant-managed site prefix {} must be contained in RFC1918 address space",
                self.prefix
            )));
        }

        self.metadata.validate(true)
    }

    /// Builds the complete row while keeping server-owned fields out of the
    /// mutation request.
    pub fn into_new_site_prefix(self) -> NewSitePrefix {
        NewSitePrefix {
            id: self.id,
            config: SitePrefixConfig {
                prefix: self.prefix,
                tenant_organization_id: Some(self.tenant_organization_id),
                routing_scope: SitePrefixRoutingScope::DatacenterOnly,
            },
            metadata: self.metadata,
            status: SitePrefixStatus {
                authority: SitePrefixAuthority::TenantManaged,
                lifecycle_state: SitePrefixLifecycleState::Provisioning,
            },
        }
    }
}

/// Metadata-only mutation for a tenant-managed SitePrefix.
#[derive(Clone, Debug)]
pub struct UpdateSitePrefixMetadata {
    pub id: SitePrefixId,
    pub tenant_organization_id: TenantOrganizationId,
    pub metadata: Metadata,
    pub if_version_match: Option<ConfigVersion>,
}

/// Retirement intent for a tenant-managed SitePrefix.
#[derive(Clone, Debug)]
pub struct RetireTenantManagedSitePrefix {
    pub id: SitePrefixId,
    pub tenant_organization_id: TenantOrganizationId,
}

#[derive(Clone, Debug)]
pub enum PrefixMatch {
    Exact(IpNetwork),
    Contains(IpNetwork),
    ContainedBy(IpNetwork),
}

#[derive(Clone, Debug, Default)]
pub struct SitePrefixSearchFilter {
    pub tenant_organization_id: Option<TenantOrganizationId>,
    pub authority: Option<SitePrefixAuthority>,
    pub routing_scope: Option<SitePrefixRoutingScope>,
    pub lifecycle_state: Option<SitePrefixLifecycleState>,
    pub prefix_match: Option<PrefixMatch>,
}

impl<'r> FromRow<'r, PgRow> for SitePrefix {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("labels")?;

        Ok(Self {
            id: row.try_get("id")?,
            config: SitePrefixConfig {
                prefix: row.try_get("prefix")?,
                tenant_organization_id: row.try_get("tenant_organization_id")?,
                routing_scope: row.try_get("routing_scope")?,
            },
            metadata: Metadata {
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                labels: labels.0,
            },
            status: SitePrefixStatus {
                authority: row.try_get("authority")?,
                lifecycle_state: row.try_get("lifecycle_state")?,
            },
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;

    fn site_prefix(
        prefix: &str,
        authority: SitePrefixAuthority,
        tenant_organization_id: Option<&str>,
        lifecycle_state: SitePrefixLifecycleState,
    ) -> NewSitePrefix {
        NewSitePrefix {
            id: SitePrefixId::new(),
            config: SitePrefixConfig {
                prefix: prefix.parse().unwrap(),
                tenant_organization_id: tenant_organization_id.map(|id| id.parse().unwrap()),
                routing_scope: SitePrefixRoutingScope::DatacenterOnly,
            },
            metadata: Metadata::default(),
            status: SitePrefixStatus {
                authority,
                lifecycle_state,
            },
        }
    }

    fn tenant_managed_input(prefix: &str) -> NewTenantManagedSitePrefix {
        NewTenantManagedSitePrefix {
            id: SitePrefixId::new(),
            tenant_organization_id: "tenant-a".parse().unwrap(),
            prefix: prefix.parse().unwrap(),
            metadata: Metadata {
                name: "tenant prefix".to_string(),
                ..Metadata::default()
            },
        }
    }

    #[test]
    fn validate_site_prefix_invariants() {
        check_cases(
            [
                Case {
                    scenario: "operator-managed prefix",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::OperatorManaged,
                        None,
                        SitePrefixLifecycleState::Ready,
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "operator-managed prefix being deleted",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::OperatorManaged,
                        None,
                        SitePrefixLifecycleState::Deleting,
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "tenant-managed prefix",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::TenantManaged,
                        Some("tenant-a"),
                        SitePrefixLifecycleState::Provisioning,
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "noncanonical prefix",
                    input: site_prefix(
                        "10.0.0.1/24",
                        SitePrefixAuthority::OperatorManaged,
                        None,
                        SitePrefixLifecycleState::Ready,
                    ),
                    expect: Fails,
                },
                Case {
                    scenario: "operator-managed prefix with tenant owner",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::OperatorManaged,
                        Some("tenant-a"),
                        SitePrefixLifecycleState::Ready,
                    ),
                    expect: Fails,
                },
                Case {
                    scenario: "tenant-managed prefix without owner",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::TenantManaged,
                        None,
                        SitePrefixLifecycleState::Provisioning,
                    ),
                    expect: Fails,
                },
                Case {
                    scenario: "operator-managed prefix in provisioning",
                    input: site_prefix(
                        "10.0.0.0/8",
                        SitePrefixAuthority::OperatorManaged,
                        None,
                        SitePrefixLifecycleState::Provisioning,
                    ),
                    expect: Fails,
                },
            ],
            |site_prefix| site_prefix.validate().map_err(drop),
        );
    }

    #[test]
    fn site_prefix_authority_serde_accepts_legacy_input_and_emits_current_names() {
        check_cases(
            [
                Case {
                    scenario: "operator-managed name",
                    input: "operator_managed",
                    expect: Yields(SitePrefixAuthority::OperatorManaged),
                },
                Case {
                    scenario: "legacy configured name",
                    input: "configured",
                    expect: Yields(SitePrefixAuthority::OperatorManaged),
                },
                Case {
                    scenario: "tenant-managed name",
                    input: "tenant_managed",
                    expect: Yields(SitePrefixAuthority::TenantManaged),
                },
            ],
            |input| serde_json::from_value(serde_json::json!(input)).map_err(drop),
        );

        check_values(
            [
                Check {
                    scenario: "operator-managed authority",
                    input: SitePrefixAuthority::OperatorManaged,
                    expect: serde_json::json!("operator_managed"),
                },
                Check {
                    scenario: "tenant-managed authority",
                    input: SitePrefixAuthority::TenantManaged,
                    expect: serde_json::json!("tenant_managed"),
                },
            ],
            |authority| serde_json::to_value(authority).expect("authority should serialize"),
        );
    }

    #[test]
    fn validate_tenant_managed_prefix_policy() {
        check_cases(
            [
                Case {
                    scenario: "RFC1918 /8 boundary",
                    input: tenant_managed_input("10.0.0.0/8"),
                    expect: Yields(()),
                },
                Case {
                    scenario: "RFC1918 /31 boundary",
                    input: tenant_managed_input("10.0.0.0/31"),
                    expect: Yields(()),
                },
                Case {
                    scenario: "172.16 private root",
                    input: tenant_managed_input("172.16.0.0/12"),
                    expect: Yields(()),
                },
                Case {
                    scenario: "192.168 private subnet",
                    input: tenant_managed_input("192.168.10.0/24"),
                    expect: Yields(()),
                },
                Case {
                    scenario: "prefix wider than /8",
                    input: tenant_managed_input("10.0.0.0/7"),
                    expect: Fails,
                },
                Case {
                    scenario: "single address",
                    input: tenant_managed_input("10.0.0.1/32"),
                    expect: Fails,
                },
                Case {
                    scenario: "IPv6",
                    input: tenant_managed_input("fd00::/8"),
                    expect: Fails,
                },
                Case {
                    scenario: "shared address space",
                    input: tenant_managed_input("100.64.0.0/10"),
                    expect: Fails,
                },
                Case {
                    scenario: "outside the private part of 172/8",
                    input: tenant_managed_input("172.0.0.0/12"),
                    expect: Fails,
                },
                Case {
                    scenario: "outside 192.168/16",
                    input: tenant_managed_input("192.169.0.0/16"),
                    expect: Fails,
                },
                Case {
                    scenario: "noncanonical prefix",
                    input: tenant_managed_input("10.0.0.1/24"),
                    expect: Fails,
                },
            ],
            |site_prefix| site_prefix.validate().map_err(drop),
        );
    }

    #[test]
    fn tenant_managed_create_derives_server_owned_fields() {
        let request = tenant_managed_input("10.0.0.0/24");
        let id = request.id;
        let tenant_organization_id = request.tenant_organization_id.clone();
        let site_prefix = request.into_new_site_prefix();

        assert_eq!(site_prefix.id, id);
        assert_eq!(
            site_prefix.config.tenant_organization_id,
            Some(tenant_organization_id)
        );
        assert_eq!(
            site_prefix.config.routing_scope,
            SitePrefixRoutingScope::DatacenterOnly
        );
        assert_eq!(
            site_prefix.status,
            SitePrefixStatus {
                authority: SitePrefixAuthority::TenantManaged,
                lifecycle_state: SitePrefixLifecycleState::Provisioning,
            }
        );
    }

    #[test]
    fn lifecycle_state_serializes_for_state_history() {
        assert_eq!(
            serde_json::to_value(SitePrefixLifecycleState::Provisioning).unwrap(),
            serde_json::json!({"state": "provisioning"})
        );
    }
}
