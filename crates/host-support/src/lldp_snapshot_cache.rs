// SPDX-FileCopyrightText: Copyright (c) 2025-2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Decides whether a machine's freshly collected LLDP neighbors are worth
//! reporting, by holding the last snapshot nico-api acknowledged and
//! classifying each new collection against it. Sending the resulting report is
//! the caller's business: scout has its own RPC, the DPU agent attaches it to
//! its status push.

use ::rpc::forge::{InterfaceLldp, LldpReport, LldpReportResult};

use crate::lldp_collector::{LldpCollectorResult, LldpNeighbor};

/// Classifies each LLDP collection against the last snapshot nico-api
/// acknowledged. Hold one instance across the poll loop so the cache persists
/// between iterations.
#[derive(Debug, Default)]
pub struct LldpSnapshotCache {
    last_reported: Option<Vec<InterfaceLldp>>,
}

impl LldpSnapshotCache {
    /// Create an empty cache, holding no acknowledged snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify one collection attempt against the cached snapshot, producing
    /// the report to send.
    ///
    /// Nothing is committed to the cache until [`Self::confirm_reported`], so a
    /// report that never reached nico-api classifies as
    /// [`LldpReportResult::Updated`] again rather than being suppressed as
    /// [`LldpReportResult::Unchanged`].
    ///
    /// [`LldpReportResult::Unchanged`] is only ever produced from a successful
    /// collection, which is why a collection that keeps failing is reported as a
    /// failure each time rather than folded into it.
    pub fn classify(&self, collected: LldpCollectorResult<Vec<LldpNeighbor>>) -> LldpReport {
        let Ok(neighbors) = collected else {
            return report(LldpReportResult::CollectionFailed, vec![]);
        };

        let mut interfaces: Vec<InterfaceLldp> = neighbors
            .into_iter()
            .map(|neighbor| InterfaceLldp {
                mac_address: neighbor.local_mac,
                lldp: Some(neighbor.switch),
            })
            .collect();

        // `lldpcli` imposes no order on its output, and one local interface can
        // see several neighbors, each contributing an entry with the same MAC.
        // Sort by the full link identity before the cache comparison so that a
        // merely reordered snapshot does not read as a changed one.
        interfaces.sort_by(|left, right| link_identity(left).cmp(&link_identity(right)));

        if self.last_reported.as_deref() == Some(interfaces.as_slice()) {
            return report(LldpReportResult::Unchanged, vec![]);
        }

        report(LldpReportResult::Updated, interfaces)
    }

    /// Advance the cache after the transport confirmed `report` landed.
    pub fn confirm_reported(&mut self, report: LldpReport) {
        match result_of(&report) {
            LldpReportResult::Updated => self.last_reported = Some(report.interfaces),
            // Forget the snapshot, so the next successful collection re-asserts the full
            // topology
            LldpReportResult::CollectionFailed => self.last_reported = None,
            // Nothing new reached nico-api, so it still holds what it had.
            LldpReportResult::Unchanged | LldpReportResult::Unspecified => {}
        }
    }
}

fn report(result: LldpReportResult, interfaces: Vec<InterfaceLldp>) -> LldpReport {
    LldpReport {
        result: result.into(),
        interfaces,
    }
}

/// The generated message keeps the result as an `i32` and this codegen does not
/// emit prost's enum accessor, so read it back in one place. An unrecognized
/// value reads as [`LldpReportResult::Unspecified`], which no caller acts on.
pub fn result_of(report: &LldpReport) -> LldpReportResult {
    LldpReportResult::try_from(report.result).unwrap_or(LldpReportResult::Unspecified)
}

/// A local interface can see several neighbors, so
/// the identity has to name both endpoints of the link.
fn link_identity(interface: &InterfaceLldp) -> (&str, &str, &str, &str, &str, &str) {
    let Some(switch) = interface.lldp.as_ref() else {
        return (&interface.mac_address, "", "", "", "", "");
    };
    (
        &interface.mac_address,
        &switch.local_port,
        &switch.id_type,
        &switch.id_value,
        &switch.remote_port_type,
        &switch.remote_port_value,
    )
}

#[cfg(test)]
mod tests {
    use ::rpc::machine_discovery::LldpSwitchData;

    use super::*;
    use crate::lldp_collector::LldpCollectorError;

    fn switch(local_port: &str, remote_port: &str) -> LldpSwitchData {
        LldpSwitchData {
            local_port: local_port.to_string(),
            remote_port_type: "ifname".to_string(),
            remote_port_value: remote_port.to_string(),
            ..Default::default()
        }
    }

    fn neighbor(mac: &str, local_port: &str, remote_port: &str) -> LldpNeighbor {
        LldpNeighbor {
            local_mac: mac.to_string(),
            switch: switch(local_port, remote_port),
        }
    }

    fn iface(mac: &str, local_port: &str, remote_port: &str) -> InterfaceLldp {
        InterfaceLldp {
            mac_address: mac.to_string(),
            lldp: Some(switch(local_port, remote_port)),
        }
    }

    /// One neighbor on one port, the simplest snapshot the collector can yield.
    fn one_neighbor() -> Vec<LldpNeighbor> {
        vec![neighbor("aa:bb:cc:dd:ee:ff", "p0", "p0")]
    }

    // The first snapshot is a fresh one; once nico-api has acknowledged it, an
    // identical follow-up carries no interfaces, because nico-api already holds
    // that topology.
    #[test]
    fn updated_then_unchanged() {
        let mut cache = LldpSnapshotCache::new();

        let report = cache.classify(Ok(one_neighbor()));
        assert_eq!(result_of(&report), LldpReportResult::Updated);
        assert_eq!(
            report.interfaces,
            vec![iface("aa:bb:cc:dd:ee:ff", "p0", "p0")]
        );

        cache.confirm_reported(report);

        let report = cache.classify(Ok(one_neighbor()));
        assert_eq!(result_of(&report), LldpReportResult::Unchanged);
        assert!(
            report.interfaces.is_empty(),
            "an unchanged report carries no interfaces"
        );
    }

    // Collector order carries no meaning, so a snapshot whose interfaces merely
    // arrive in a different order is the same snapshot. The first pair shares a
    // MAC *and* a `local_port` -- the shape `lldp_collector` produces for
    // several neighbors on one port -- so only the remote port orders them;
    // neither endpoint alone would.
    #[test]
    fn reordered_snapshot_is_unchanged() {
        let mut cache = LldpSnapshotCache::new();

        let canonical = vec![
            neighbor("aa:bb:cc:dd:ee:aa", "swp1", "port-00"),
            neighbor("aa:bb:cc:dd:ee:aa", "swp1", "port-01"),
            neighbor("aa:bb:cc:dd:ee:ff", "swp3", "swp3"),
        ];
        let reversed: Vec<_> = canonical.iter().rev().cloned().collect();

        let report = cache.classify(Ok(reversed));
        assert_eq!(result_of(&report), LldpReportResult::Updated);
        assert_eq!(
            report.interfaces,
            vec![
                iface("aa:bb:cc:dd:ee:aa", "swp1", "port-00"),
                iface("aa:bb:cc:dd:ee:aa", "swp1", "port-01"),
                iface("aa:bb:cc:dd:ee:ff", "swp3", "swp3"),
            ],
            "the report should be in canonical link-identity order"
        );
        cache.confirm_reported(report);

        assert_eq!(
            result_of(&cache.classify(Ok(canonical))),
            LldpReportResult::Unchanged,
            "a reordered snapshot is the same snapshot"
        );
    }

    // Until the transport confirms the report landed, the same snapshot stays a
    // fresh one, so a send that failed is retried rather than suppressed.
    #[test]
    fn unreported_snapshot_stays_updated() {
        let cache = LldpSnapshotCache::new();

        assert_eq!(
            result_of(&cache.classify(Ok(one_neighbor()))),
            LldpReportResult::Updated
        );
        assert_eq!(
            result_of(&cache.classify(Ok(one_neighbor()))),
            LldpReportResult::Updated,
            "an unconfirmed snapshot must not be suppressed as unchanged"
        );
    }

    // A failed collection reports that it failed and carries no interfaces, and
    // forgets the cached snapshot rather than assuming nico-api held on to it.
    // A failure that persists keeps reporting a failure, and recovery re-asserts
    // the topology even when it turns out to be the same one.
    #[test]
    fn collection_error_clears_cache() {
        let mut cache = LldpSnapshotCache::new();
        let report = cache.classify(Ok(one_neighbor()));
        cache.confirm_reported(report);

        let report = cache.classify(Err(LldpCollectorError::Lldp("lldpcli is down".into())));
        assert_eq!(result_of(&report), LldpReportResult::CollectionFailed);
        assert!(
            report.interfaces.is_empty(),
            "a failed collection must not claim the machine has no neighbors"
        );
        cache.confirm_reported(report);

        let report = cache.classify(Err(LldpCollectorError::Lldp("lldpcli is down".into())));
        assert_eq!(
            result_of(&report),
            LldpReportResult::CollectionFailed,
            "a failure that repeats stays a failure; unchanged would claim the \
             neighbors were confirmed current"
        );
        cache.confirm_reported(report);

        let report = cache.classify(Ok(one_neighbor()));
        assert_eq!(
            result_of(&report),
            LldpReportResult::Updated,
            "recovery must re-assert the topology, since nico-api may have \
             discarded it during the outage"
        );
        assert_eq!(
            report.interfaces,
            vec![iface("aa:bb:cc:dd:ee:ff", "p0", "p0")]
        );
        cache.confirm_reported(report);

        assert_eq!(
            result_of(&cache.classify(Ok(one_neighbor()))),
            LldpReportResult::Unchanged,
            "the cache is live again once the re-asserted snapshot lands"
        );
    }

    // A successful collection that finds nothing is a real observation, not a
    // failure: the machine sees no neighbors and nico-api is told so.
    #[test]
    fn empty_snapshot_is_an_ordinary_update() {
        let mut cache = LldpSnapshotCache::new();

        let report = cache.classify(Ok(vec![]));
        assert_eq!(result_of(&report), LldpReportResult::Updated);
        assert!(report.interfaces.is_empty());
        cache.confirm_reported(report);

        assert_eq!(
            result_of(&cache.classify(Ok(vec![]))),
            LldpReportResult::Unchanged,
            "an empty snapshot is not repeated once acknowledged"
        );

        let report = cache.classify(Ok(one_neighbor()));
        cache.confirm_reported(report);

        let report = cache.classify(Ok(vec![]));
        assert_eq!(result_of(&report), LldpReportResult::Updated);
        assert!(report.interfaces.is_empty());

        cache.confirm_reported(report);
        assert_eq!(
            result_of(&cache.classify(Ok(vec![]))),
            LldpReportResult::Unchanged,
            "an empty snapshot is cached like any other"
        );
    }
}
