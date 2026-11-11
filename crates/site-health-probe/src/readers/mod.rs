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

//! State readers. Each is a framework [`Probe`](crate::framework::Probe), so
//! it inherits scheduling, skipped ticks, the run timeout, panic isolation
//! and the latency histogram, and publishes family snapshots through a
//! [`SnapshotSink`](crate::snapshot::SnapshotSink).

pub(crate) mod devices;
pub(crate) mod rest_instances;
pub(crate) mod rest_inventory;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use carbide_site_health_probe_api::Family;
use eyre::eyre;

use crate::gate::CallError;
use crate::snapshot::{FailureKind, FamilySnapshot, Reader, SnapshotSink, SweepMeta};

/// Why a family could not be built this sweep. `error` is logged and
/// served by `/v1/config`, so it holds the operation and a bounded category
/// only, never upstream text.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub kind: FailureKind,
    pub error: String,
}

impl Failure {
    /// `what` is the probe's own name for the failed operation.
    pub(crate) fn call(what: &str, err: &CallError) -> Self {
        Self {
            kind: err.into(),
            error: format!("{what}: {}", err.category()),
        }
    }

    pub(crate) fn unresolved(error: String) -> Self {
        Self {
            kind: FailureKind::Unresolved,
            error,
        }
    }

    /// The same failure, as seen by a family that depends on the failed read.
    pub(crate) fn needed_by(&self, family: Family) -> Self {
        Self {
            kind: self.kind,
            error: format!("{family:?} needs a failed read: {}", self.error),
        }
    }
}

/// Publishes `family` from the sweep `meta`, or records why it keeps its
/// last snapshot. Returns whether it was published.
pub(crate) fn deliver<T: Send + Sync + 'static>(
    sink: &dyn SnapshotSink,
    meta: &SweepMeta,
    family: Family,
    built: Result<T, Failure>,
) -> bool {
    match built {
        Ok(data) => {
            sink.publish(
                family,
                Arc::new(FamilySnapshot {
                    meta: meta.clone(),
                    data,
                }),
            );
            true
        }
        Err(failure) => {
            tracing::warn!(
                reader = meta.reader.as_str(),
                family = ?family,
                kind = failure.kind.as_str(),
                error = %failure.error,
                "family keeps its last snapshot"
            );
            sink.failed(meta.reader, family, failure.kind, &failure.error);
            false
        }
    }
}

/// One reader's current sweep: which of its families it delivered. When the
/// scheduler drops a sweep at its timeout, the families it never delivered
/// get a timeout failure, so their last attempt does not still read as the
/// previous success.
pub(crate) struct Attempt {
    reader: Reader,
    families: &'static [Family],
    state: Mutex<AttemptState>,
}

#[derive(Default)]
struct AttemptState {
    delivered: BTreeSet<Family>,
    /// Set once the sweep timed out: its timeouts are recorded, and a late
    /// delivery of that sweep is dropped.
    closed: bool,
}

impl Attempt {
    pub(crate) fn new(reader: Reader, families: &'static [Family]) -> Self {
        Self {
            reader,
            families,
            state: Mutex::new(AttemptState::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, AttemptState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a sweep. The scheduler never starts one while the previous
    /// run is still outstanding.
    pub(crate) fn begin(&self) {
        *self.state() = AttemptState::default();
    }

    /// [`deliver`]s `family` and notes it as delivered once the sink took
    /// it. A sweep that already timed out delivers nothing: its families
    /// are accounted for as timed out.
    pub(crate) fn deliver<T: Send + Sync + 'static>(
        &self,
        sink: &dyn SnapshotSink,
        meta: &SweepMeta,
        family: Family,
        built: Result<T, Failure>,
    ) -> bool {
        // Held across the sink call, so a timeout never interleaves with a
        // delivery.
        let mut state = self.state();
        if state.closed {
            tracing::warn!(
                reader = self.reader.as_str(),
                family = ?family,
                "late delivery of a timed-out sweep dropped"
            );
            return false;
        }
        let published = deliver(sink, meta, family, built);
        state.delivered.insert(family);
        published
    }

    /// Records a timeout for every family this sweep did not deliver, once
    /// per sweep, from whichever timeout path ends it first.
    pub(crate) fn timed_out(&self, sink: &dyn SnapshotSink) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.closed = true;
        for family in self
            .families
            .iter()
            .filter(|f| !state.delivered.contains(f))
        {
            tracing::warn!(
                reader = self.reader.as_str(),
                family = ?family,
                "sweep ran past its timeout, family keeps its last snapshot"
            );
            sink.failed(
                self.reader,
                *family,
                FailureKind::Timeout,
                "the sweep ran past its timeout",
            );
        }
    }
}

/// The families a sweep could not publish, and whether every lost read timed
/// out. It becomes the sweep's run error: nothing failed is success, a set
/// that is only timeouts is a timeout, and any other failure is a generic
/// incomplete sweep. A sweep held up only by call timeouts is a slow nico-api,
/// which the scheduler counts apart from a down one; a non-timeout failure in
/// the same sweep keeps the louder failure outcome.
#[derive(Default)]
pub(crate) struct Incomplete {
    failed: Vec<Family>,
    timeouts: usize,
}

impl Incomplete {
    /// Records a family that kept its last snapshot this sweep. `kind` is its
    /// failure kind, or `None` when a late delivery was dropped after the
    /// sweep had already timed out.
    pub(crate) fn record(&mut self, family: Family, kind: Option<FailureKind>) {
        self.failed.push(family);
        if kind == Some(FailureKind::Timeout) {
            self.timeouts += 1;
        }
    }

    /// The run error. A timeout family kind comes only from
    /// `CallError::Timeout`, so the typed timeout carries no upstream text.
    pub(crate) fn finish(self) -> eyre::Result<()> {
        if self.failed.is_empty() {
            Ok(())
        } else if self.timeouts == self.failed.len() {
            Err(eyre::Report::new(CallError::Timeout).wrap_err("reader sweep timed out"))
        } else {
            Err(eyre!("incomplete sweep: {:?}", self.failed))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::snapshot::testing::FakeSink;

    #[test]
    fn attempt_is_closed_after_a_timeout() {
        const FAMILIES: [Family; 2] = [Family::RestTotals, Family::RestSiteMatch];
        let attempt = Attempt::new(Reader::RestInventory, &FAMILIES);
        let sink = FakeSink::default();
        let meta = SweepMeta {
            process: 1,
            seq: 1,
            reader: Reader::RestInventory,
            start: SystemTime::now(),
            end: SystemTime::now(),
        };
        attempt.begin();
        assert!(attempt.deliver(&sink, &meta, Family::RestTotals, Ok(())));
        attempt.timed_out(&sink);
        // The second timeout path of the same sweep adds nothing.
        attempt.timed_out(&sink);
        assert_eq!(
            sink.failure_kinds(Family::RestSiteMatch),
            vec![FailureKind::Timeout]
        );
        assert!(
            sink.failure_kinds(Family::RestTotals).is_empty(),
            "delivered in time"
        );
        // A late delivery of the timed-out sweep is dropped.
        assert!(!attempt.deliver(&sink, &meta, Family::RestSiteMatch, Ok(())));
        assert_eq!(sink.count(Family::RestSiteMatch), 0);
        // The next sweep starts open.
        attempt.begin();
        assert!(attempt.deliver(&sink, &meta, Family::RestSiteMatch, Ok(())));
    }

    #[test]
    fn only_all_timeouts_report_a_typed_timeout() {
        let typed_timeout = |err: &eyre::Report| {
            err.chain()
                .any(|cause| matches!(cause.downcast_ref::<CallError>(), Some(CallError::Timeout)))
        };
        // A sweep reads as a timeout only when every lost family timed out: a
        // mixed failure, or an unknown kind, keeps the generic failure, so no
        // typed timeout reaches the scheduler.
        let cases: [([Option<FailureKind>; 2], bool); 3] = [
            (
                [Some(FailureKind::Timeout), Some(FailureKind::Timeout)],
                true,
            ),
            ([Some(FailureKind::Timeout), Some(FailureKind::Rpc)], false),
            ([Some(FailureKind::Timeout), None], false),
        ];
        for (kinds, want) in cases {
            let mut incomplete = Incomplete::default();
            incomplete.record(Family::Host, kinds[0]);
            incomplete.record(Family::Dpu, kinds[1]);
            let err = incomplete.finish().expect_err("incomplete sweep");
            assert_eq!(typed_timeout(&err), want, "{kinds:?}");
        }
    }
}
