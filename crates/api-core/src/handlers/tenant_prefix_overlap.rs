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

//! Tenant prefix overlap policy shared by `VpcPrefix` and `NetworkSegment`
//! handlers.

use carbide_network::virtualization::VpcVirtualizationType;
use ipnetwork::IpNetwork;
use model::site_prefix::{
    SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixRoutingScope,
};
use model::vpc::Vpc;

use crate::CarbideError;
use crate::cfg::file::VpcIsolationBehaviorType;

const INELIGIBLE_OVERLAP: &str =
    "the requested prefix overlaps address space that is not eligible for reuse";

/// `overlap_error` returns the common client error for an overlap that cannot
/// be reused.
///
/// It does not identify the conflicting resource because that resource may
/// belong to another tenant.
pub(super) fn overlap_error() -> CarbideError {
    CarbideError::InvalidArgument(INELIGIBLE_OVERLAP.to_string())
}

/// `VpcPrefixParticipant` groups the `VpcPrefix`, VPC, and `SitePrefix` facts
/// needed for one exact CIDR check.
pub(super) struct VpcPrefixParticipant<'a> {
    /// Exact `VpcPrefix` CIDR under consideration.
    pub(super) prefix: IpNetwork,
    /// Whether the `VpcPrefix` has started deletion.
    pub(super) is_deleted: bool,
    /// VPC that owns the `VpcPrefix`.
    pub(super) vpc: &'a Vpc,
    /// `SitePrefix` referenced by the `VpcPrefix`.
    pub(super) site_prefix: &'a SitePrefix,
}

/// `contains_prefix` returns whether `parent` contains `child` in the same
/// address family.
pub(super) fn contains_prefix(parent: IpNetwork, child: IpNetwork) -> bool {
    match (parent, child) {
        (IpNetwork::V4(parent), IpNetwork::V4(child)) => child.is_subnet_of(parent),
        (IpNetwork::V6(parent), IpNetwork::V6(child)) => child.is_subnet_of(parent),
        _ => false,
    }
}

/// `site_prefix_is_eligible` checks the `SitePrefix` requirements for one
/// `VpcPrefix`.
///
/// `allow_deleting` is true only for an existing `VpcPrefix`. Its CIDR remains
/// reserved while the `SitePrefix` is `Deleting`, but a new `VpcPrefix`
/// requires a `Ready` `SitePrefix`.
fn site_prefix_is_eligible(
    site_prefix: &SitePrefix,
    vpc: &Vpc,
    prefix: IpNetwork,
    allow_deleting: bool,
) -> bool {
    site_prefix.status.authority == SitePrefixAuthority::TenantManaged
        && site_prefix
            .config
            .tenant_organization_id
            .as_ref()
            .map(|id| id.as_str())
            == Some(vpc.config.tenant_organization_id.as_str())
        && site_prefix.config.routing_scope == SitePrefixRoutingScope::DatacenterOnly
        && contains_prefix(site_prefix.config.prefix, prefix)
        && match site_prefix.status.lifecycle_state {
            SitePrefixLifecycleState::Ready => true,
            SitePrefixLifecycleState::Deleting => allow_deleting,
            SitePrefixLifecycleState::Provisioning | SitePrefixLifecycleState::Error => false,
        }
}

/// `pair_is_eligible` returns whether two `VpcPrefix` records may reuse one
/// exact CIDR.
///
/// The pair eligibility checks ownership, FNN isolation, distinct VNIs,
/// `SitePrefix` state, and each VPC's resolved routing profile. Callers still
/// need `db::tenant_prefix_overlap::lock_checks` and must reject every
/// ineligible overlap.
pub(super) fn pair_is_eligible(
    runtime_config: &crate::cfg::file::CarbideConfig,
    candidate: VpcPrefixParticipant<'_>,
    existing: VpcPrefixParticipant<'_>,
) -> bool {
    if !runtime_config.tenant_prefix_overlap_enabled
        || !matches!(
            runtime_config.vpc_isolation_behavior,
            VpcIsolationBehaviorType::MutualIsolation
        )
        || runtime_config.site_global_vpc_vni.is_some()
        || existing.is_deleted
        || candidate.prefix != existing.prefix
        || candidate.vpc.id == existing.vpc.id
        || candidate.vpc.config.tenant_organization_id == existing.vpc.config.tenant_organization_id
        || candidate.vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || existing.vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
        || !site_prefix_is_eligible(
            candidate.site_prefix,
            candidate.vpc,
            candidate.prefix,
            false,
        )
        || !site_prefix_is_eligible(existing.site_prefix, existing.vpc, existing.prefix, true)
        || !matches!(
            (candidate.vpc.status.vni, existing.vpc.status.vni),
            (Some(candidate_vni), Some(existing_vni)) if candidate_vni != existing_vni
        )
    {
        return false;
    }

    let Some(fnn) = runtime_config.fnn.as_ref() else {
        return false;
    };
    if fnn.common_internal_route_target.is_some() || !fnn.additional_route_target_imports.is_empty()
    {
        return false;
    }
    let Ok(candidate_profile) = fnn.resolve_vpc_routing_profile(&candidate.vpc.config) else {
        return false;
    };
    let Ok(existing_profile) = fnn.resolve_vpc_routing_profile(&existing.vpc.config) else {
        return false;
    };

    candidate_profile.is_eligible_for_tenant_prefix_overlap()
        && existing_profile.is_eligible_for_tenant_prefix_overlap()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::site_prefix::SitePrefixId;
    use carbide_uuid::vpc::VpcId;
    use chrono::Utc;
    use config_version::ConfigVersion;
    use model::metadata::Metadata;
    use model::site_prefix::{SitePrefixConfig, SitePrefixStatus};
    use model::vpc::{VpcConfig, VpcStatus};

    use super::*;
    use crate::cfg::file::{FnnConfig, FnnRoutingProfileConfig};

    /// Test-specific enum that selects one eligibility rule to vary.
    #[derive(Clone, Copy, Debug)]
    enum Variation {
        Eligible,
        RetainedRootDeleting,
        SiteGateDisabled,
        OpenIsolation,
        SiteGlobalVpcVni,
        CommonInternalRouteTarget,
        AdditionalRouteTargetImport,
        NestedPrefix,
        SameVpc,
        SameTenant,
        ExistingNotFnn,
        CandidateOperatorRoot,
        ExistingWrongTenantRoot,
        ExistingRootProvisioning,
        CandidateUnsafeProfile,
        ExistingUnsafeProfile,
        CandidateVniMissing,
        ExistingVniMissing,
        SameVni,
        ExistingPrefixDeleting,
    }

    /// Test-specific function that enables every site and profile condition required for reuse.
    fn eligible_config() -> crate::cfg::file::CarbideConfig {
        let mut config = crate::test_support::default_config::get();
        config.tenant_prefix_overlap_enabled = true;
        config.vpc_isolation_behavior = VpcIsolationBehaviorType::MutualIsolation;
        config.fnn = Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([
                (
                    "ELIGIBLE".to_string(),
                    FnnRoutingProfileConfig {
                        tenant_prefix_overlap_eligible: true,
                        internal: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "UNSAFE".to_string(),
                    FnnRoutingProfileConfig {
                        internal: Some(true),
                        ..Default::default()
                    },
                ),
            ]),
            use_vpc_vrf_loopback: false,
        });
        config
    }

    /// Test-specific function that builds an FNN VPC with the requested tenant and VNI.
    fn vpc(tenant: &str, vni: Option<i32>) -> Vpc {
        Vpc {
            id: VpcId::new(),
            version: ConfigVersion::initial(),
            config: VpcConfig {
                tenant_organization_id: tenant.to_string(),
                tenant_keyset_id: None,
                network_virtualization_type: VpcVirtualizationType::Fnn,
                network_security_group_id: None,
                default_nvlink_logical_partition_id: None,
                vni: None,
                routing_profile_type: Some("ELIGIBLE".to_string()),
                routing_profile_overrides: None,
                power_resource_group: None,
                slaac_enabled: false,
            },
            status: VpcStatus { vni },
            metadata: Metadata::default(),
            created: Utc::now(),
            updated: Utc::now(),
            deleted: None,
        }
    }

    /// Test-specific function that builds a ready tenant-managed SitePrefix for one tenant.
    fn site_prefix(tenant: &str, prefix: IpNetwork) -> SitePrefix {
        SitePrefix {
            id: SitePrefixId::new(),
            config: SitePrefixConfig {
                prefix,
                tenant_organization_id: Some(tenant.parse().unwrap()),
                routing_scope: SitePrefixRoutingScope::DatacenterOnly,
            },
            metadata: Metadata::default(),
            status: SitePrefixStatus {
                authority: SitePrefixAuthority::TenantManaged,
                lifecycle_state: SitePrefixLifecycleState::Ready,
            },
            version: ConfigVersion::initial(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Test-specific function that checks each pair eligibility rule from an eligible pair.
    #[test]
    fn exact_pair_eligibility_is_fail_closed() {
        let exact: IpNetwork = "10.0.1.0/24".parse().unwrap();

        check_values(
            [
                Check {
                    scenario: "eligible pair",
                    input: Variation::Eligible,
                    expect: true,
                },
                Check {
                    scenario: "existing SitePrefix is deleting",
                    input: Variation::RetainedRootDeleting,
                    expect: true,
                },
                Check {
                    scenario: "site gate is disabled",
                    input: Variation::SiteGateDisabled,
                    expect: false,
                },
                Check {
                    scenario: "site isolation is open",
                    input: Variation::OpenIsolation,
                    expect: false,
                },
                Check {
                    scenario: "site uses a global VPC VNI",
                    input: Variation::SiteGlobalVpcVni,
                    expect: false,
                },
                Check {
                    scenario: "site uses a common internal route target",
                    input: Variation::CommonInternalRouteTarget,
                    expect: false,
                },
                Check {
                    scenario: "site adds route target imports",
                    input: Variation::AdditionalRouteTargetImport,
                    expect: false,
                },
                Check {
                    scenario: "candidate CIDR is nested",
                    input: Variation::NestedPrefix,
                    expect: false,
                },
                Check {
                    scenario: "prefixes belong to the same VPC",
                    input: Variation::SameVpc,
                    expect: false,
                },
                Check {
                    scenario: "prefixes belong to the same tenant",
                    input: Variation::SameTenant,
                    expect: false,
                },
                Check {
                    scenario: "existing VPC does not use FNN",
                    input: Variation::ExistingNotFnn,
                    expect: false,
                },
                Check {
                    scenario: "candidate SitePrefix is operator managed",
                    input: Variation::CandidateOperatorRoot,
                    expect: false,
                },
                Check {
                    scenario: "existing SitePrefix belongs to another tenant",
                    input: Variation::ExistingWrongTenantRoot,
                    expect: false,
                },
                Check {
                    scenario: "existing SitePrefix is provisioning",
                    input: Variation::ExistingRootProvisioning,
                    expect: false,
                },
                Check {
                    scenario: "candidate profile is unsafe",
                    input: Variation::CandidateUnsafeProfile,
                    expect: false,
                },
                Check {
                    scenario: "existing profile is unsafe",
                    input: Variation::ExistingUnsafeProfile,
                    expect: false,
                },
                Check {
                    scenario: "candidate VNI is missing",
                    input: Variation::CandidateVniMissing,
                    expect: false,
                },
                Check {
                    scenario: "existing VNI is missing",
                    input: Variation::ExistingVniMissing,
                    expect: false,
                },
                Check {
                    scenario: "VPCs use the same VNI",
                    input: Variation::SameVni,
                    expect: false,
                },
                Check {
                    scenario: "existing VpcPrefix is deleting",
                    input: Variation::ExistingPrefixDeleting,
                    expect: false,
                },
            ],
            |variation| {
                let mut config = eligible_config();
                let mut candidate_prefix = exact;
                let mut candidate_vpc = vpc("tenant-a", Some(100));
                let mut existing_vpc = vpc("tenant-b", Some(200));
                let mut candidate_site_prefix =
                    site_prefix("tenant-a", "10.0.0.0/16".parse().unwrap());
                let mut existing_site_prefix =
                    site_prefix("tenant-b", "10.0.0.0/16".parse().unwrap());
                let mut existing_deleted = false;

                match variation {
                    Variation::Eligible => {}
                    Variation::RetainedRootDeleting => {
                        existing_site_prefix.status.lifecycle_state =
                            SitePrefixLifecycleState::Deleting;
                    }
                    Variation::SiteGateDisabled => config.tenant_prefix_overlap_enabled = false,
                    Variation::OpenIsolation => {
                        config.vpc_isolation_behavior = VpcIsolationBehaviorType::Open;
                    }
                    Variation::SiteGlobalVpcVni => config.site_global_vpc_vni = Some(5_000),
                    Variation::CommonInternalRouteTarget => {
                        config.fnn.as_mut().unwrap().common_internal_route_target =
                            Some(crate::cfg::file::RouteTargetConfig { asn: 1, vni: 2 });
                    }
                    Variation::AdditionalRouteTargetImport => {
                        config.fnn.as_mut().unwrap().additional_route_target_imports =
                            vec![crate::cfg::file::RouteTargetConfig { asn: 3, vni: 4 }];
                    }
                    Variation::NestedPrefix => {
                        candidate_prefix = "10.0.1.0/25".parse().unwrap();
                    }
                    Variation::SameVpc => existing_vpc.id = candidate_vpc.id,
                    Variation::SameTenant => {
                        existing_vpc.config.tenant_organization_id = "tenant-a".to_string();
                        existing_site_prefix.config.tenant_organization_id =
                            Some("tenant-a".parse().unwrap());
                    }
                    Variation::ExistingNotFnn => {
                        existing_vpc.config.network_virtualization_type =
                            VpcVirtualizationType::Flat;
                    }
                    Variation::CandidateOperatorRoot => {
                        candidate_site_prefix.status.authority =
                            SitePrefixAuthority::OperatorManaged;
                    }
                    Variation::ExistingWrongTenantRoot => {
                        existing_site_prefix.config.tenant_organization_id =
                            Some("tenant-c".parse().unwrap());
                    }
                    Variation::ExistingRootProvisioning => {
                        existing_site_prefix.status.lifecycle_state =
                            SitePrefixLifecycleState::Provisioning;
                    }
                    Variation::CandidateUnsafeProfile => {
                        candidate_vpc.config.routing_profile_type = Some("UNSAFE".to_string());
                    }
                    Variation::ExistingUnsafeProfile => {
                        existing_vpc.config.routing_profile_type = Some("UNSAFE".to_string());
                    }
                    Variation::CandidateVniMissing => candidate_vpc.status.vni = None,
                    Variation::ExistingVniMissing => existing_vpc.status.vni = None,
                    Variation::SameVni => existing_vpc.status.vni = candidate_vpc.status.vni,
                    Variation::ExistingPrefixDeleting => existing_deleted = true,
                }

                pair_is_eligible(
                    &config,
                    VpcPrefixParticipant {
                        prefix: candidate_prefix,
                        is_deleted: false,
                        vpc: &candidate_vpc,
                        site_prefix: &candidate_site_prefix,
                    },
                    VpcPrefixParticipant {
                        prefix: exact,
                        is_deleted: existing_deleted,
                        vpc: &existing_vpc,
                        site_prefix: &existing_site_prefix,
                    },
                )
            },
        );
    }
}
