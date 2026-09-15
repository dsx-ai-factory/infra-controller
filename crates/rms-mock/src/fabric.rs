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
//! The fabric has no simulated behaviour of its own: switches are either
//! enabled in it or not, and their fabric manager is reported healthy. What
//! this module exists for is to remember what a caller set, so that reading
//! the fabric back agrees with what was written to it.
//!
//! `enabled` doubles as the primary marker: the fabric manager runs on the
//! one enabled switch of a rack, and a reader that finds several enabled
//! switches treats the rack as having multiple primaries and waits. So once a
//! rack's fabric manager has been configured, only its elected primary reads
//! back as enabled unless a caller has set a switch explicitly.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::resolve::NodeRef;

/// The health string a caller maps to a healthy fabric manager.
///
/// The vocabulary is exactly `"ok"` and `"not ok"`; anything else, including
/// an empty or unparseable payload, is read as unknown. It is spelled once
/// here rather than at each call site for that reason.
pub(crate) const FABRIC_MANAGER_OK: &str = "ok";

/// The `addition-info` value a caller reads as "the fabric manager control
/// plane is configured on this switch". It is reported for the primary only,
/// since that is the switch running the fabric manager.
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

/// A switch that can run a rack's fabric manager.
///
/// Only a switch the inventory has is a candidate: a node the request names
/// but no simulated device answers for cannot be reached, so electing it would
/// hand the caller a primary it cannot use.
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

/// The candidate at the lowest position in the rack, which is how the proto
/// has RMS choose when no primary is requested. A candidate without a
/// reported position sorts last, and node id breaks ties, so repeated
/// election over the same switches picks the same one.
fn lowest<'a>(candidates: impl IntoIterator<Item = Candidate<'a>>) -> Option<&'a str> {
    candidates
        .into_iter()
        .min_by_key(|c| (c.slot.unwrap_or(u32::MAX), c.node_id))
        .map(|c| c.node_id)
}

/// Which switches participate in the scale-up fabric, and which one per rack
/// runs the fabric manager.
pub(crate) struct FabricState {
    /// Explicit per-switch settings written through the fabric-state RPC,
    /// keyed by node id.
    enabled: Mutex<HashMap<String, bool>>,
    /// Elected primary switch per rack, keyed by rack id.
    primaries: Mutex<HashMap<String, String>>,
}

impl FabricState {
    pub(crate) fn new() -> Self {
        Self {
            enabled: Mutex::new(HashMap::new()),
            primaries: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn set_enabled(&self, node_id: &str, enabled: bool) {
        crate::lock(&self.enabled).insert(node_id.to_owned(), enabled);
    }

    /// Record which of a rack's switches runs the fabric manager.
    ///
    /// The requested switch wins when it is one of the candidates; otherwise
    /// the candidate at the lowest rack position is chosen, as in [`lowest`].
    /// Returns the elected node id, or `None` when there is nothing to elect.
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

    /// Make sure every rack with a candidate in `nodes` has a primary before
    /// it is read.
    ///
    /// Election state lives in this process. A caller that configured the
    /// fabric against an earlier process, or that skips configuration because
    /// an unknown job is reported complete, still expects one primary per rack
    /// when it reads the fabric back; a rack with none is elected here with
    /// the same rule as `elect_primary`. Racks that already have a primary are
    /// left alone. `nodes` yields `(rack_id, candidate)` pairs.
    pub(crate) fn ensure_primaries<'a>(
        &self,
        nodes: impl IntoIterator<Item = (&'a str, Candidate<'a>)>,
    ) {
        let mut by_rack: HashMap<&str, Vec<Candidate<'a>>> = HashMap::new();
        for (rack_id, candidate) in nodes {
            by_rack.entry(rack_id).or_default().push(candidate);
        }
        let mut primaries = crate::lock(&self.primaries);
        for (rack_id, candidates) in by_rack {
            if primaries.contains_key(rack_id) {
                continue;
            }
            if let Some(primary) = lowest(candidates) {
                primaries.insert(rack_id.to_owned(), primary.to_owned());
            }
        }
    }

    /// Whether a switch is its rack's elected primary.
    pub(crate) fn is_primary(&self, rack_id: &str, node_id: &str) -> bool {
        crate::lock(&self.primaries)
            .get(rack_id)
            .map(String::as_str)
            == Some(node_id)
    }

    /// Whether a switch reads back as enabled in the fabric.
    ///
    /// An explicit setting always wins. Otherwise a switch is enabled exactly
    /// when it is its rack's elected primary. The status RPCs elect a primary
    /// before reading, so a rack that has never been configured still reads
    /// back one enabled switch, provided the mock has a device for one.
    pub(crate) fn is_enabled(&self, rack_id: &str, node_id: &str) -> bool {
        if let Some(explicit) = crate::lock(&self.enabled).get(node_id) {
            return *explicit;
        }
        self.is_primary(rack_id, node_id)
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
    fn only_the_elected_primary_reads_back_enabled() {
        let fabric = FabricState::new();
        let switches = [c("sw-1", None), c("sw-2", None)];

        // A rack without a primary has no enabled switch.
        assert!(!fabric.is_enabled("rack-a", "sw-1"));

        fabric.elect_primary("rack-a", &switches, None);
        assert!(fabric.is_enabled("rack-a", "sw-1"));
        assert!(!fabric.is_enabled("rack-a", "sw-2"));

        // An explicit setting overrides the election either way.
        fabric.set_enabled("sw-1", false);
        assert!(!fabric.is_enabled("rack-a", "sw-1"));
        fabric.set_enabled("sw-2", true);
        assert!(fabric.is_enabled("rack-a", "sw-2"));
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
        assert!(!fabric.is_enabled("rack-a", "sw-1"));
        assert!(fabric.is_primary("rack-b", "sw-9"));
        assert!(!fabric.is_enabled("rack-b", "sw-3"));
        assert!(!fabric.is_primary("rack-c", "sw-1"));
    }
}
