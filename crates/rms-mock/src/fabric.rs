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

//! Scale-up fabric state.
//!
//! Remembers which switch of each rack was elected to run the fabric manager;
//! only that switch reads back as enabled.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::resolve::NodeRef;

/// The `status` value a caller maps to a healthy fabric manager.
pub(crate) const FABRIC_MANAGER_OK: &str = "ok";

/// The `addition-info` value that marks the primary's control plane as
/// configured.
const CONTROL_PLANE_STATE_CONFIGURED: &str = "CONTROL_PLANE_STATE_CONFIGURED";

/// The JSON body reported for a healthy fabric manager service on a switch.
pub(crate) fn status_json(primary: bool) -> String {
    if primary {
        format!(
            r#"{{"status":"{FABRIC_MANAGER_OK}","addition-info":"{CONTROL_PLANE_STATE_CONFIGURED}"}}"#
        )
    } else {
        format!(r#"{{"status":"{FABRIC_MANAGER_OK}"}}"#)
    }
}

/// A switch the inventory has, and so one that can run a rack's fabric
/// manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Candidate<'a> {
    pub(crate) node_id: &'a str,
    /// Position in the rack, as the simulated chassis reports it.
    pub(crate) slot: Option<u32>,
}

impl<'a> Candidate<'a> {
    /// The candidate a requested node stands for, if it matched a device.
    pub(crate) fn of(node: &NodeRef<'a>) -> Option<Self> {
        node.node.map(|device| Self {
            node_id: node.node_id,
            slot: device.slot_number,
        })
    }
}

/// The candidate at the lowest rack position; unplaced candidates sort last
/// and node id breaks ties.
fn lowest<'a>(candidates: impl IntoIterator<Item = Candidate<'a>>) -> Option<&'a str> {
    candidates
        .into_iter()
        .min_by_key(|c| (c.slot.unwrap_or(u32::MAX), c.node_id))
        .map(|c| c.node_id)
}

/// Per-rack scale-up fabric state.
pub(crate) struct FabricState {
    /// Elected primary switch per rack, keyed by rack id.
    primaries: Mutex<HashMap<String, String>>,
}

impl FabricState {
    pub(crate) fn new() -> Self {
        Self {
            primaries: Mutex::new(HashMap::new()),
        }
    }

    /// Elect and record a rack's primary: the requested switch when it is a
    /// candidate, otherwise the [`lowest`]. Returns `None` when there is
    /// nothing to elect.
    pub(crate) fn elect_primary<'a>(
        &self,
        rack_id: &str,
        candidates: &[Candidate<'a>],
        requested: Option<&'a str>,
    ) -> Option<&'a str> {
        let primary = requested
            .filter(|id| candidates.iter().any(|c| c.node_id == *id))
            .or_else(|| lowest(candidates.iter().copied()))?;
        crate::lock(&self.primaries).insert(rack_id.to_owned(), primary.to_owned());
        Some(primary)
    }

    /// Elect a primary for every rack in `nodes` that has none yet; racks
    /// with one are left alone. `nodes` yields `(rack_id, candidate)` pairs.
    pub(crate) fn ensure_primaries<'a>(
        &self,
        nodes: impl IntoIterator<Item = (&'a str, Candidate<'a>)>,
    ) {
        let mut primaries = crate::lock(&self.primaries);
        let mut by_rack: HashMap<&str, Vec<Candidate<'a>>> = HashMap::new();
        for (rack_id, candidate) in nodes {
            if !primaries.contains_key(rack_id) {
                by_rack.entry(rack_id).or_default().push(candidate);
            }
        }
        for (rack_id, candidates) in by_rack {
            if let Some(primary) = lowest(candidates) {
                primaries.insert(rack_id.to_owned(), primary.to_owned());
            }
        }
    }

    /// Whether a switch is its rack's elected primary, which is when it reads
    /// back as enabled.
    pub(crate) fn is_primary(&self, rack_id: &str, node_id: &str) -> bool {
        crate::lock(&self.primaries)
            .get(rack_id)
            .map(String::as_str)
            == Some(node_id)
    }

    /// Forget a switch's primary role, as a factory reset would; the rack
    /// re-elects the next time its fabric is configured or read.
    pub(crate) fn reset(&self, rack_id: &str, node_id: &str) {
        let mut primaries = crate::lock(&self.primaries);
        if primaries.get(rack_id).map(String::as_str) == Some(node_id) {
            primaries.remove(rack_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::{Candidate, FabricState};

    fn c(node_id: &str, slot: Option<u32>) -> Candidate<'_> {
        Candidate { node_id, slot }
    }

    #[test]
    fn election_prefers_the_requested_switch_then_the_lowest_position() {
        let placed = [c("sw-3", Some(1)), c("sw-1", Some(2)), c("sw-2", None)];
        let unplaced = [c("sw-3", None), c("sw-1", None), c("sw-2", None)];
        check_values(
            [
                Check {
                    scenario: "a requested candidate wins",
                    input: (&placed[..], Some("sw-2")),
                    expect: Some("sw-2"),
                },
                Check {
                    scenario: "a requested non-candidate falls back to the lowest position",
                    input: (&placed[..], Some("not-in-rack")),
                    expect: Some("sw-3"),
                },
                Check {
                    scenario: "no request elects the lowest position",
                    input: (&placed[..], None),
                    expect: Some("sw-3"),
                },
                Check {
                    scenario: "without positions the lowest node id is elected",
                    input: (&unplaced[..], None),
                    expect: Some("sw-1"),
                },
                Check {
                    scenario: "nothing to elect",
                    input: (&[][..], None),
                    expect: None,
                },
            ],
            |(candidates, requested)| {
                FabricState::new().elect_primary("rack-a", candidates, requested)
            },
        );
    }

    #[test]
    fn reading_a_rack_without_a_primary_elects_one_and_keeps_existing_ones() {
        let fabric = FabricState::new();
        fabric.elect_primary("rack-a", &[c("sw-1", None), c("sw-2", None)], Some("sw-2"));

        fabric.ensure_primaries([
            ("rack-a", c("sw-1", None)),
            ("rack-a", c("sw-2", None)),
            ("rack-b", c("sw-9", Some(1))),
            ("rack-b", c("sw-3", Some(2))),
        ]);

        // rack-a keeps its requested primary; rack-b gets the lowest position,
        // which is not its lowest node id.
        assert!(fabric.is_primary("rack-a", "sw-2"));
        assert!(!fabric.is_primary("rack-a", "sw-1"));
        assert!(fabric.is_primary("rack-b", "sw-9"));
        assert!(!fabric.is_primary("rack-b", "sw-3"));
        assert!(!fabric.is_primary("rack-c", "sw-1"));
    }

    #[test]
    fn a_reset_switch_loses_its_primary_role_and_the_rack_re_elects() {
        let fabric = FabricState::new();
        fabric.elect_primary("rack-a", &[c("sw-1", None), c("sw-2", None)], Some("sw-2"));

        // Resetting a non-primary changes nothing.
        fabric.reset("rack-a", "sw-1");
        assert!(fabric.is_primary("rack-a", "sw-2"));

        // Resetting the primary leaves the rack to be re-elected on its next
        // read, with the usual rule.
        fabric.reset("rack-a", "sw-2");
        assert!(!fabric.is_primary("rack-a", "sw-2"));
        fabric.ensure_primaries([("rack-a", c("sw-2", None)), ("rack-a", c("sw-1", None))]);
        assert!(fabric.is_primary("rack-a", "sw-1"));
    }
}
