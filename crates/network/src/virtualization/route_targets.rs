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

use crate::ip::address_family::IdentifyAddressFamily;
use crate::ip::prefix::IpNet;

// Share the per-profile expansion bound between Core admission and agent validation.
const PREFIX_ROUTE_TARGET_ACTIONS_MAX: usize = 256;

/// Selects the contained prefixes that receive additional export route targets.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportPrefixMatchType {
    /// Matches the configured prefix and all its more-specific prefixes (the default).
    #[default]
    Ge,
    /// Matches contained prefixes through `max_prefix_length`, defaulting to the family width.
    Le,
    /// Matches only the configured prefix.
    Exact,
}

/// Adds route targets to matching type-5 exports without originating or admitting routes.
///
/// The route-target type differs between Core's symbolic configuration and the
/// agent's concrete configuration; both use the same prefix matching contract.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrefixRouteTargetConfig<T> {
    /// IPv4 or IPv6 CIDR prefix; RPC conversion and rendering clear host bits.
    pub prefix: IpNet,
    /// Contained-prefix matching mode; omission defaults to `ge`.
    #[serde(default)]
    pub match_type: ExportPrefixMatchType,
    /// Inclusive maximum prefix length, accepted only with `le`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prefix_length: Option<u32>,
    /// Required, nonempty targets added cumulatively with every matching rule.
    pub route_targets: Vec<T>,
}

/// Rejects conditional export rules that cannot express a valid NVUE policy.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrefixRouteTargetConfigError {
    /// Rejects rules that would have no effect.
    #[error("route_targets must contain at least one route target")]
    EmptyRouteTargets,
    /// Rejects a maximum that the selected match mode would ignore.
    #[error("max_prefix_length is only accepted with match_type = le")]
    UnexpectedMaximum,
    /// Rejects a maximum outside the prefix's address family and containment range.
    #[error("max_prefix_length {maximum} must be between {prefix_length} and {address_width}")]
    InvalidMaximum {
        /// Supplied inclusive maximum.
        maximum: u32,
        /// Configured prefix length.
        prefix_length: u32,
        /// Address-family width.
        address_width: u32,
    },
    /// Rejects policies above the shared expanded-action limit.
    #[error(
        "route_targets_on_exports_by_prefix exceeds {PREFIX_ROUTE_TARGET_ACTIONS_MAX} route-target actions"
    )]
    TooManyRules,
}

impl<T> PrefixRouteTargetConfig<T> {
    /// Validates matching and nonempty targets, returning the inclusive maximum length.
    ///
    /// Callers must validate each target's encoding and resolve symbolic values
    /// before sending the policy to a DPU.
    pub fn validate(&self) -> Result<u32, PrefixRouteTargetConfigError> {
        if self.route_targets.is_empty() {
            return Err(PrefixRouteTargetConfigError::EmptyRouteTargets);
        }
        // Reject ignored maxima so a typo cannot silently widen an export policy.
        if self.match_type != ExportPrefixMatchType::Le && self.max_prefix_length.is_some() {
            return Err(PrefixRouteTargetConfigError::UnexpectedMaximum);
        }
        let prefix_length = u32::from(self.prefix.prefix_len());
        let address_width = u32::from(self.prefix.address_family().interface_prefix_len());
        let maximum = match self.match_type {
            ExportPrefixMatchType::Exact => prefix_length,
            ExportPrefixMatchType::Ge => address_width,
            ExportPrefixMatchType::Le => self.max_prefix_length.unwrap_or(address_width),
        };
        if !(prefix_length..=address_width).contains(&maximum) {
            return Err(PrefixRouteTargetConfigError::InvalidMaximum {
                maximum,
                prefix_length,
                address_width,
            });
        }
        Ok(maximum)
    }

    /// Validates every rule and limits the profile to 256 actions across both address families.
    ///
    /// Each target gets its own continuing action, so multiple targets need no
    /// NVUE scalar containing a vendor-specific route-target list.
    /// The limit leaves sequence 65535 available for each family's terminal permit.
    pub fn validate_entries(entries: &[Self]) -> Result<(), PrefixRouteTargetConfigError> {
        let mut action_count = 0_usize;
        for entry in entries {
            entry.validate()?;
            // Count every prefix/target pair because each produces a separate NVUE action.
            action_count = action_count.saturating_add(entry.route_targets.len());
            if action_count > PREFIX_ROUTE_TARGET_ACTIONS_MAX {
                return Err(PrefixRouteTargetConfigError::TooManyRules);
            }
        }
        Ok(())
    }
}

/// `RouteTargetContributions` groups the profile and site targets added to a VPC.
/// Core checks its configured targets before admission; the agent composes its
/// resolved targets for FNN. Both use the same sources without sharing wire types.
/// Profile overrides must already be applied; an empty slice means no addition
/// from that source. A caller supplies the sources available at its own layer.
pub struct RouteTargetContributions<'a, T> {
    /// Targets the VPC imports in addition to its own target and authorized peers.
    pub profile_imports: &'a [T],
    /// Targets added to routes the VPC exports.
    pub profile_exports: &'a [T],
    /// Site-wide target added to every VPC's exported routes, when configured.
    pub common_export: Option<&'a T>,
    /// Site-wide targets imported by every VPC.
    pub site_imports: &'a [T],
}

/// `RouteTargets` contains the ordered FNN import and export targets for one VPC.
#[derive(Debug, PartialEq, Eq)]
pub struct RouteTargets<T> {
    /// Explicit import targets, before NVUE's automatic import entry.
    pub imports: Vec<T>,
    /// Explicit export targets, beginning with the VPC's own target.
    pub exports: Vec<T>,
    /// Whether NVUE also derives import targets automatically; FNN enables this.
    pub import_automatically: bool,
}

impl<T> RouteTargetContributions<'_, T> {
    /// Returns whether a profile or site adds targets beyond the VPC and its peers.
    /// Even a configured target equal to the VPC's own target counts: Core's
    /// admission for reused tenant prefixes requires these sources to be empty.
    pub fn has_additional_targets(&self) -> bool {
        !self.profile_imports.is_empty()
            || !self.profile_exports.is_empty()
            || self.common_export.is_some()
            || !self.site_imports.is_empty()
    }
}

impl<T: Clone> RouteTargetContributions<'_, T> {
    /// Composes targets after callers resolve symbolic values and authorize peers.
    /// Source ordering and duplicates are preserved: exports are own, common,
    /// profile; imports are own, peers, profile, site, then automatic imports.
    /// Reordering the output would rewrite the agent's saved NVUE configuration.
    pub fn compose(&self, own: T, peer_imports: &[T]) -> RouteTargets<T> {
        RouteTargets {
            imports: std::iter::once(own.clone())
                .chain(peer_imports.iter().cloned())
                .chain(self.profile_imports.iter().cloned())
                .chain(self.site_imports.iter().cloned())
                .collect(),
            exports: std::iter::once(own)
                .chain(self.common_export.cloned())
                .chain(self.profile_exports.iter().cloned())
                .collect(),
            import_automatically: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// Verifies omitted matching uses GE and malformed rules fail before they can widen exports.
    #[test]
    fn conditional_export_rules_validate_matching_and_required_targets() {
        use carbide_test_support::Outcome::*;
        use carbide_test_support::scenarios;

        scenarios!(run = |input: &str| {
            // Decode the operator-facing representation before validating its semantic bounds.
            let entry: PrefixRouteTargetConfig<u32> =
                serde_json::from_str(input).map_err(|error| error.to_string())?;
            entry.validate().map_err(|error| error.to_string())
        };
            "matching defaults and bounds" {
                // Omitted matching includes all contained IPv4 prefixes.
                r#"{"prefix":"0.0.0.0/0","route_targets":[1]}"# => Yields(32),
                // Family width must follow IPv6 rather than the IPv4 default.
                r#"{"prefix":"::/0","route_targets":[1]}"# => Yields(128),
                // Exact default-route selection must not include more-specific routes.
                r#"{"prefix":"0.0.0.0/0","match_type":"exact","route_targets":[1]}"# => Yields(0),
                // A bounded LE rule must retain the operator's upper limit.
                r#"{"prefix":"10.0.0.0/16","match_type":"le","max_prefix_length":24,"route_targets":[1]}"# => Yields(24),
                // Omitted LE maximum deliberately has the same range as GE.
                r#"{"prefix":"2001:db8::/64","match_type":"le","route_targets":[1]}"# => Yields(128),
                // A maximum equal to the prefix length selects only that prefix.
                r#"{"prefix":"10.0.0.0/16","match_type":"le","max_prefix_length":16,"route_targets":[1]}"# => Yields(16),
                // An explicit zero remains a valid maximum for an LE default-route rule.
                r#"{"prefix":"0.0.0.0/0","match_type":"le","max_prefix_length":0,"route_targets":[1]}"# => Yields(0),
            }
            "invalid rules" {
                // A missing target list is malformed rather than an implicit no-op.
                r#"{"prefix":"10.0.0.0/16"}"# => Fails,
                // An explicit empty list must not activate a rule with no effect.
                r#"{"prefix":"10.0.0.0/16","route_targets":[]}"# => Fails,
                // A missing prefix cannot become an implicit default-route match.
                r#"{"route_targets":[1]}"# => Fails,
                // Invalid CIDRs must never reach template substitution.
                r#"{"prefix":"10.0.0.0/33","route_targets":[1]}"# => Fails,
                // Unknown match names must not fall back to the broad GE default.
                r#"{"prefix":"10.0.0.0/16","match_type":"unknown","route_targets":[1]}"# => Fails,
                // GE does not consume a maximum, so accepting one would conceal a typo.
                r#"{"prefix":"10.0.0.0/16","max_prefix_length":24,"route_targets":[1]}"# => Fails,
                // Containment excludes maxima shorter than the configured prefix.
                r#"{"prefix":"10.0.0.0/16","match_type":"le","max_prefix_length":15,"route_targets":[1]}"# => Fails,
                // IPv4 maxima cannot escape the 32-bit address family.
                r#"{"prefix":"10.0.0.0/16","match_type":"le","max_prefix_length":33,"route_targets":[1]}"# => Fails,
                // IPv6 maxima must still have an upper bound.
                r#"{"prefix":"2001:db8::/64","match_type":"le","max_prefix_length":129,"route_targets":[1]}"# => Fails,
            }
        );
    }

    /// Accepts 256 prefix/target pairs and rejects larger profiles so neither
    /// address-family splitting nor multiple targets on one prefix bypasses the expansion limit.
    #[test]
    fn conditional_export_action_limit_counts_both_families_and_every_target() {
        // Use valid default-prefix matches so only expanded action counts vary.
        let entry = |prefix: &str, target_count| PrefixRouteTargetConfig {
            prefix: prefix.parse().expect("valid default prefix"),
            match_type: ExportPrefixMatchType::Ge,
            max_prefix_length: None,
            route_targets: vec![0_u32; target_count],
        };

        carbide_test_support::value_scenarios!(run = |entries: Vec<PrefixRouteTargetConfig<u32>>| {
            // Apply the shared policy check used by Core and the agent.
            PrefixRouteTargetConfig::validate_entries(&entries)
        };
            "expanded action capacity" {
                // Both families share one profile capacity, including its exact upper bound.
                vec![entry("0.0.0.0/0", 128), entry("::/0", 128)] => Ok(()),
                // Individually small family maps must not bypass the combined limit.
                vec![entry("0.0.0.0/0", 128), entry("::/0", 129)] =>
                    Err(PrefixRouteTargetConfigError::TooManyRules),
            }
        );
    }

    /// Verifies composition preserves source order and repeated configured targets.
    #[test]
    fn composition_keeps_every_source_in_render_order() {
        let contributions = RouteTargetContributions {
            profile_imports: &[31, 30, 30],
            profile_exports: &[40],
            common_export: Some(&50),
            site_imports: &[60],
        };

        assert_eq!(
            contributions.compose(10, &[21, 20]),
            RouteTargets {
                imports: vec![10, 21, 20, 31, 30, 30, 60],
                exports: vec![10, 50, 40],
                import_automatically: true,
            }
        );
    }

    /// A target's source matters to admission even when its value already
    /// appears in the generated policy. Flattening the lists would lose that.
    #[test]
    fn configured_own_target_is_still_an_addition() {
        value_scenarios!(run = |profile_imports: &[u32]| {
            let contributions = RouteTargetContributions {
                profile_imports,
                profile_exports: &[],
                common_export: None,
                site_imports: &[],
            };
            contributions.has_additional_targets()
        };
            "only the generated target" {
                &[][..] => false,
            }
            "profile repeats the generated target" {
                &[10][..] => true,
            }
        );
    }
}
