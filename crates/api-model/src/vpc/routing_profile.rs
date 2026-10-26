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
use serde::{Deserialize, Serialize};

/// Defines a BGP route target used in FNN VRF import and export policies.
///
/// `vni` and `vni_self` are mutually exclusive. A two-byte ASN permits a
/// four-byte local administrator. A four-byte ASN permits only a two-byte local
/// administrator, so `asn` and the resolved local administrator cannot both
/// exceed `u16::MAX`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteTargetConfig {
    /// Autonomous System Number component of the route target.
    #[serde(default)]
    pub asn: u32,

    /// Literal local-administrator component. `None` uses zero when `vni_self` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vni: Option<u32>,

    /// Resolves the local-administrator component from the current VPC's L3 VNI.
    ///
    /// Omission defaults to `false`. `true` fails when no current VPC VNI is available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vni_self: bool,
}

/// Describes an invalid or unresolvable BGP route-target definition.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteTargetConfigError {
    /// Rejects definitions containing both literal and VPC-derived values.
    #[error("route target cannot specify both vni and vni_self")]
    ConflictingVni,

    /// Rejects symbolic resolution when no current VPC VNI is available.
    #[error("route target with vni_self requires a VPC VNI")]
    MissingVpcVni,

    /// Rejects values that cannot fit either standard ASN-specific encoding.
    #[error(
        "route target ASN {asn} and local administrator {local_administrator} cannot both exceed 65535"
    )]
    Unencodable {
        /// Autonomous System Number component.
        asn: u32,
        /// Route-target local-administrator component.
        local_administrator: u32,
    },
}

impl RouteTargetConfig {
    /// Validates literal and symbolic route-target fields before persistence or resolution.
    pub fn validate(&self) -> Result<(), RouteTargetConfigError> {
        // Each route target must have exactly one local-administrator source.
        if self.vni_self && self.vni.is_some() {
            return Err(RouteTargetConfigError::ConflictingVni);
        }

        let local_administrator = self.vni.unwrap_or_default();
        // A four-byte ASN leaves only two bytes for the local administrator.
        if !self.vni_self && self.asn > u16::MAX as u32 && local_administrator > u16::MAX as u32 {
            return Err(RouteTargetConfigError::Unencodable {
                asn: self.asn,
                local_administrator,
            });
        }

        Ok(())
    }

    /// Resolves legacy or VPC-derived values into the concrete representation consumed downstream.
    pub fn resolve(
        &self,
        vpc_vni: Option<u32>,
    ) -> Result<RouteTargetConfig, RouteTargetConfigError> {
        self.validate()?;

        // Symbolic targets require their owning VPC; legacy absence remains literal zero.
        let vni = if self.vni_self {
            vpc_vni.ok_or(RouteTargetConfigError::MissingVpcVni)?
        } else {
            self.vni.unwrap_or_default()
        };
        let resolved = RouteTargetConfig {
            asn: self.asn,
            vni: Some(vni),
            vni_self: false,
        };
        resolved.validate()?;
        Ok(resolved)
    }
}

/// An entry used by a DPU prefix-list policy.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrefixFilterPolicyEntry {
    /// Prefix matched by the policy.
    pub prefix: IpNetwork,
}

/// Routing-profile values set directly on a VPC.
///
/// Each present value overrides the corresponding property from the VPC's
/// named routing profile. `tenant_prefix_overlap_eligible`, `internal`, and
/// `access_tier` are intentionally absent because VPCs cannot override the base
/// profile's overlap admission, allocation, or access controls.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VpcRoutingProfileOverrides {
    pub route_target_imports: Option<Vec<RouteTargetConfig>>,
    pub route_targets_on_exports: Option<Vec<RouteTargetConfig>>,
    pub leak_default_route_from_underlay: Option<bool>,
    pub leak_tenant_host_routes_to_underlay: Option<bool>,
    pub tenant_leak_communities_accepted: Option<bool>,
    pub accepted_leaks_from_underlay: Option<Vec<PrefixFilterPolicyEntry>>,
    pub allowed_anycast_prefixes: Option<Vec<PrefixFilterPolicyEntry>>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    /// Verifies predecessor VPC override JSON preserves nonzero route targets
    /// so existing JSONB rows remain readable after `vni_self` is introduced.
    #[test]
    fn vpc_overrides_decode_predecessor_route_target_json() {
        // Keep the fixture in the predecessor form where `vni` was numeric and
        // `vni_self` did not exist.
        let stored = r#"{"route_target_imports":[{"asn":64512,"vni":9}]}"#;

        // Decode through the exact type stored in the VPC override column.
        let decoded: VpcRoutingProfileOverrides =
            serde_json::from_str(stored).expect("predecessor VPC overrides must decode");

        // Preserve the numeric target while defaulting only the symbolic marker.
        assert_eq!(
            decoded,
            VpcRoutingProfileOverrides {
                route_target_imports: Some(vec![RouteTargetConfig {
                    asn: 64_512,
                    vni: Some(9),    // Retains the predecessor's local administrator.
                    vni_self: false, // Defaults the absent predecessor field.
                }]),
                ..Default::default()
            }
        );
    }

    /// Verifies route-target resolution produces one unambiguous, encodable
    /// representation for downstream renderers.
    #[test]
    fn route_target_resolution_enforces_field_and_encoding_contracts() {
        check_cases(
            [
                // An explicit zero is a valid route-target local administrator.
                Case {
                    scenario: "literal zero",
                    input: (
                        RouteTargetConfig {
                            asn: 65_000,
                            vni: Some(0), // Explicit presence preserves literal zero.
                            vni_self: false,
                        },
                        None,
                    ),
                    expect: Yields(RouteTargetConfig {
                        asn: 65_000,
                        vni: Some(0),
                        vni_self: false,
                    }),
                },
                // An omitted legacy field retains proto3's historical literal-zero meaning.
                Case {
                    scenario: "legacy omitted VNI",
                    input: (
                        RouteTargetConfig {
                            asn: 65_000,
                            vni: None, // Omission exercises legacy-zero compatibility.
                            vni_self: false,
                        },
                        None,
                    ),
                    expect: Yields(RouteTargetConfig {
                        asn: 65_000,
                        vni: Some(0),
                        vni_self: false,
                    }),
                },
                // A symbolic value resolves from the supplied VPC and clears the marker.
                Case {
                    scenario: "VPC-derived local administrator",
                    input: (
                        RouteTargetConfig {
                            asn: 429_000_000,
                            vni: None,
                            vni_self: true, // Resolve from the supplied VPC VNI.
                        },
                        Some(65_321),
                    ),
                    expect: Yields(RouteTargetConfig {
                        asn: 429_000_000,
                        vni: Some(65_321),
                        vni_self: false,
                    }),
                },
                // Symbolic resolution cannot guess a value outside a VPC context.
                Case {
                    scenario: "symbolic value without a VPC",
                    input: (
                        RouteTargetConfig {
                            asn: 429_000_000,
                            vni: None,
                            vni_self: true,
                        },
                        None,
                    ),
                    expect: FailsWith(RouteTargetConfigError::MissingVpcVni),
                },
                // Literal and derived values are competing sources and must not coexist.
                Case {
                    scenario: "literal and symbolic values conflict",
                    input: (
                        RouteTargetConfig {
                            asn: 65_000,
                            vni: Some(0), // Presence conflicts even when the value is zero.
                            vni_self: true, // Selects the competing VPC-derived source.
                        },
                        Some(2),
                    ),
                    expect: FailsWith(RouteTargetConfigError::ConflictingVni),
                },
                // Four-byte ASNs retain the largest two-byte local administrator.
                Case {
                    scenario: "four-byte ASN with two-byte local administrator",
                    input: (
                        RouteTargetConfig {
                            asn: 429_000_000,
                            vni: Some(65_535),
                            vni_self: false,
                        },
                        None,
                    ),
                    expect: Yields(RouteTargetConfig {
                        asn: 429_000_000,
                        vni: Some(65_535),
                        vni_self: false,
                    }),
                },
                // Resolution must recheck the two-byte local-administrator limit.
                Case {
                    scenario: "four-byte ASN with oversized resolved local administrator",
                    input: (
                        RouteTargetConfig {
                            asn: 429_000_000, // Four-byte ASN restricts the resolved value.
                            vni: None,
                            vni_self: true, // Forces validation after VPC substitution.
                        },
                        Some(65_536),
                    ),
                    expect: FailsWith(RouteTargetConfigError::Unencodable {
                        asn: 429_000_000,
                        local_administrator: 65_536,
                    }),
                },
                // Two components wider than 16 bits fit neither ASN-specific encoding.
                Case {
                    scenario: "four-byte ASN with oversized local administrator",
                    input: (
                        RouteTargetConfig {
                            asn: 429_000_000,
                            vni: Some(65_536),
                            vni_self: false,
                        },
                        None,
                    ),
                    expect: FailsWith(RouteTargetConfigError::Unencodable {
                        asn: 429_000_000,
                        local_administrator: 65_536,
                    }),
                },
                // A two-byte ASN permits the full four-byte local-administrator range.
                Case {
                    scenario: "two-byte ASN with four-byte local administrator",
                    input: (
                        RouteTargetConfig {
                            asn: 65_535,
                            vni: Some(u32::MAX),
                            vni_self: false,
                        },
                        None,
                    ),
                    expect: Yields(RouteTargetConfig {
                        asn: 65_535,
                        vni: Some(u32::MAX),
                        vni_self: false,
                    }),
                },
            ],
            |(target, vpc_vni)| target.resolve(vpc_vni),
        );
    }
}
