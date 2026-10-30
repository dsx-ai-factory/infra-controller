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

/// The run result of a sweep: a failure naming the families it could not
/// publish, so the reader's `up` gauge drops.
pub(crate) fn incomplete(failed: &[Family]) -> eyre::Result<()> {
    if failed.is_empty() {
        Ok(())
    } else {
        Err(eyre!("incomplete sweep: {failed:?}"))
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
}
