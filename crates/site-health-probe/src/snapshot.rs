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

//! Immutable family snapshots. A reader builds one snapshot per count family
//! from one sweep and hands it to a [`SnapshotSink`] only when every read the
//! family needs succeeded. A family whose reads failed keeps its last
//! snapshot, so one stuck read never freezes or zeroes the other families.

use std::any::Any;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use carbide_site_health_probe_api::{Family, Unit};

use crate::gate::CallError;

/// The state readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Reader {
    Devices,
    RestInventory,
}

impl Reader {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reader::Devices => "devices",
            Reader::RestInventory => "rest_inventory",
        }
    }
}

/// When and by which sweep a snapshot was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SweepMeta {
    /// Start of this probe process, in Unix milliseconds. Tells two
    /// processes' sweeps apart.
    pub process: u64,
    /// Sweep number of this reader within the process, from 1.
    pub seq: u64,
    pub reader: Reader,
    pub start: SystemTime,
    /// When the last read of the sweep returned.
    pub end: SystemTime,
}

impl SweepMeta {
    /// An opaque ID, unique per reader sweep across process restarts.
    pub(crate) fn snapshot_id(&self) -> String {
        format!("{}-{}-{}", self.process, self.reader.as_str(), self.seq)
    }
}

/// Unix milliseconds of `at`, for process IDs.
pub(crate) fn unix_millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// One family's data from one sweep.
#[derive(Debug)]
pub(crate) struct FamilySnapshot<T> {
    pub meta: SweepMeta,
    pub data: T,
}

/// A snapshot of any family, as the sink stores it. Consumers downcast to the
/// family's data type.
pub(crate) trait AnySnapshot: Send + Sync {
    fn meta(&self) -> &SweepMeta;
    fn as_any(&self) -> &dyn Any;
}

impl<T: Send + Sync + 'static> AnySnapshot for FamilySnapshot<T> {
    fn meta(&self) -> &SweepMeta {
        &self.meta
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The units of a unit family, sorted by `(source, type, id)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UnitsData {
    pub units: Vec<Unit>,
    /// Units whose state was not in the pinned state table, published as
    /// `other`.
    pub unknown_states: u64,
}

impl UnitsData {
    pub(crate) fn new(mut units: Vec<Unit>, unknown_states: u64) -> Self {
        units.sort_by(|a, b| (a.source, a.unit_type, &a.id).cmp(&(b.source, b.unit_type, &b.id)));
        Self {
            units,
            unknown_states,
        }
    }
}

/// Why a family kept its last snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FailureKind {
    Timeout,
    Pushback,
    Rpc,
    /// Every read answered, but some unit could not be pinned down: an ID
    /// missing from its by-ID read yet still in the census, or a row that
    /// joins to no unit of another family.
    Unresolved,
    Decode,
}

impl FailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FailureKind::Timeout => "timeout",
            FailureKind::Pushback => "pushback",
            FailureKind::Rpc => "rpc",
            FailureKind::Unresolved => "unresolved",
            FailureKind::Decode => "decode",
        }
    }
}

impl From<&CallError> for FailureKind {
    fn from(err: &CallError) -> Self {
        match err {
            CallError::Timeout => FailureKind::Timeout,
            CallError::Pushback { .. } => FailureKind::Pushback,
            CallError::Status(_) | CallError::Transport(_) => FailureKind::Rpc,
            CallError::Decode(_) => FailureKind::Decode,
        }
    }
}

/// The NICo machines of the newest complete device sweep, for readers that
/// compare another API against them.
pub(crate) trait CensusView: Send + Sync {
    /// Host and DPU IDs, or `None` before the first complete sweep of both.
    fn machine_ids(&self) -> Option<BTreeSet<String>>;
}

/// Where readers deliver their sweeps.
pub(crate) trait SnapshotSink: Send + Sync {
    /// Replaces `family`'s snapshot with a complete new one.
    fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>);
    /// Records that `family` could not be read this sweep and keeps its last
    /// snapshot.
    fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str);
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::Mutex;

    use super::*;

    /// Records everything readers deliver.
    #[derive(Default)]
    pub(crate) struct FakeSink {
        pub published: Mutex<Vec<(Family, Arc<dyn AnySnapshot>)>>,
        pub failures: Mutex<Vec<(Reader, Family, FailureKind, String)>>,
    }

    impl FakeSink {
        /// The newest published snapshot of `family`, downcast to `T`.
        pub(crate) fn latest<T: 'static + Clone>(&self, family: Family) -> Option<(SweepMeta, T)> {
            self.published
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(f, _)| *f == family)
                .map(|(_, snap)| {
                    let typed = snap
                        .as_any()
                        .downcast_ref::<FamilySnapshot<T>>()
                        .expect("family data type");
                    (typed.meta.clone(), typed.data.clone())
                })
        }

        /// How many snapshots of `family` were published.
        pub(crate) fn count(&self, family: Family) -> usize {
            self.published
                .lock()
                .unwrap()
                .iter()
                .filter(|(f, _)| *f == family)
                .count()
        }

        /// The failure kinds recorded for `family`, oldest first.
        pub(crate) fn failure_kinds(&self, family: Family) -> Vec<FailureKind> {
            self.failures
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, f, _, _)| *f == family)
                .map(|(_, _, kind, _)| *kind)
                .collect()
        }
    }

    impl SnapshotSink for FakeSink {
        fn publish(&self, family: Family, snap: Arc<dyn AnySnapshot>) {
            self.published.lock().unwrap().push((family, snap));
        }

        fn failed(&self, reader: Reader, family: Family, kind: FailureKind, error: &str) {
            self.failures
                .lock()
                .unwrap()
                .push((reader, family, kind, error.to_string()));
        }
    }
}
