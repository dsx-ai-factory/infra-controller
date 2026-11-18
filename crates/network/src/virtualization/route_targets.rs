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
