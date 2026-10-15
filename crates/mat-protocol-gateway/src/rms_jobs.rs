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

//! Gateway job ids and what they stand for on the machine-a-tron instances.
//!
//! Every job id a caller receives from the gateway is minted here and maps to jobs on one or more
//! instances. Instance job ids are never handed out: two instances number their jobs
//! independently, so the same string can name two different jobs, and a caller must be able to
//! poll any id it was given without knowing which instance runs it.
//!
//! A batch the proxy split across instances gets one gateway parent id that maps to the
//! per-instance parents. Its state is the worst, least advanced of theirs: a failed part fails
//! the batch, otherwise a queued part keeps it queued and a running part keeps it running, and it
//! is complete only once every part is.
//!
//! The table lives in process memory. An id neither issued nor polled for [`JOB_RETENTION`] is
//! forgotten, and a gateway restart forgets every id at once; the proxy answers such ids the way
//! the RMS mock answers ids it never issued. Ids carry a per-process token so an id from a
//! previous process is never mistaken for one issued by the current one.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use librms::protos::rack_manager as rms;

use crate::ownership::SourceId;

/// How long an id is kept after it was last issued or polled.
const JOB_RETENTION: Duration = Duration::from_secs(60 * 60);

/// A job as one machine-a-tron instance names it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BackendJob {
    pub(crate) source: SourceId,
    pub(crate) job_id: String,
}

/// What a gateway job id stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobRecord {
    /// One job on one instance.
    Single(BackendJob),
    /// The parent of a batch split across instances: the per-instance parent of each part.
    Aggregate(Vec<BackendJob>),
}

struct Entry {
    record: JobRecord,
    /// Last time the id was issued or polled; eviction counts from here.
    touched: Instant,
}

struct Table {
    next: u64,
    entries: HashMap<String, Entry>,
    /// Reverse index so that a backend job seen twice keeps its gateway id.
    singles: HashMap<BackendJob, String>,
    /// The aggregate each per-instance parent was folded into.
    aggregate_of: HashMap<BackendJob, String>,
    last_sweep: Instant,
}

/// The in-memory map from gateway job ids to instance jobs.
pub(crate) struct JobMap {
    prefix: String,
    retention: Duration,
    table: Mutex<Table>,
}

impl Default for JobMap {
    fn default() -> Self {
        Self::new()
    }
}

impl JobMap {
    /// An empty map whose ids are `gw-<process token>-<n>`.
    pub(crate) fn new() -> Self {
        let token = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or_default();
        Self::with_prefix_and_retention(format!("gw-{token:x}"), JOB_RETENTION)
    }

    fn with_prefix_and_retention(prefix: String, retention: Duration) -> Self {
        Self {
            prefix,
            retention,
            table: Mutex::new(Table {
                next: 1,
                entries: HashMap::new(),
                singles: HashMap::new(),
                aggregate_of: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// The gateway id for a job on one instance, minting one on first sight.
    ///
    /// An empty backend id stays empty: it means the instance issued no job, and a gateway id
    /// standing for "no job" would send the caller polling for nothing.
    pub(crate) fn intern(&self, source: &SourceId, job_id: &str) -> String {
        self.intern_at(source, job_id, Instant::now())
    }

    fn intern_at(&self, source: &SourceId, job_id: &str, now: Instant) -> String {
        if job_id.is_empty() {
            return String::new();
        }
        let job = BackendJob {
            source: source.clone(),
            job_id: job_id.to_owned(),
        };
        let mut table = self.lock();
        self.sweep_if_due(&mut table, now);
        if let Some(existing) = table.singles.get(&job) {
            let existing = existing.clone();
            if let Some(entry) = table.entries.get_mut(&existing) {
                entry.touched = now;
            }
            return existing;
        }
        let id = self.mint(&mut table);
        table.singles.insert(job.clone(), id.clone());
        table.entries.insert(
            id.clone(),
            Entry {
                record: JobRecord::Single(job),
                touched: now,
            },
        );
        id
    }

    /// The gateway id for the parents a batch produced: none, one, or one per instance.
    pub(crate) fn batch_id(&self, parts: Vec<BackendJob>) -> String {
        self.batch_id_at(parts, Instant::now())
    }

    fn batch_id_at(&self, parts: Vec<BackendJob>, now: Instant) -> String {
        match parts.len() {
            0 => String::new(),
            1 => self.intern_at(&parts[0].source, &parts[0].job_id, now),
            _ => {
                let mut table = self.lock();
                self.sweep_if_due(&mut table, now);
                let id = self.mint(&mut table);
                for part in &parts {
                    table.aggregate_of.insert(part.clone(), id.clone());
                }
                table.entries.insert(
                    id.clone(),
                    Entry {
                        record: JobRecord::Aggregate(parts),
                        touched: now,
                    },
                );
                id
            }
        }
    }

    /// What `gateway_job_id` stands for, or `None` for an id this process does not know. A
    /// successful lookup counts as use and extends the id's retention.
    pub(crate) fn resolve(&self, gateway_job_id: &str) -> Option<JobRecord> {
        self.resolve_at(gateway_job_id, Instant::now())
    }

    fn resolve_at(&self, gateway_job_id: &str, now: Instant) -> Option<JobRecord> {
        let mut table = self.lock();
        self.sweep_if_due(&mut table, now);
        let entry = table.entries.get_mut(gateway_job_id)?;
        entry.touched = now;
        Some(entry.record.clone())
    }

    /// The gateway id of the aggregate `part` is a per-instance parent of, if any.
    pub(crate) fn aggregate_of(&self, part: &BackendJob) -> Option<String> {
        self.lock().aggregate_of.get(part).cloned()
    }

    fn mint(&self, table: &mut Table) -> String {
        let id = format!("{}-{}", self.prefix, table.next);
        table.next += 1;
        id
    }

    /// Sweeps at most once per tenth of the retention, and at least once a minute for long
    /// retentions, so the cost is amortised over the calls that grow the table.
    fn sweep_if_due(&self, table: &mut Table, now: Instant) {
        let interval = (self.retention / 10).min(Duration::from_secs(60));
        if now.saturating_duration_since(table.last_sweep) >= interval {
            self.evict(table, now);
        }
    }

    fn evict(&self, table: &mut Table, now: Instant) -> usize {
        table.last_sweep = now;
        let expired: Vec<String> = table
            .entries
            .iter()
            .filter(|(_, entry)| now.saturating_duration_since(entry.touched) >= self.retention)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            match table.entries.remove(id).map(|entry| entry.record) {
                Some(JobRecord::Single(job)) => {
                    table.singles.remove(&job);
                }
                Some(JobRecord::Aggregate(parts)) => {
                    for part in parts {
                        if table.aggregate_of.get(&part) == Some(id) {
                            table.aggregate_of.remove(&part);
                        }
                    }
                }
                None => {}
            }
        }
        if !expired.is_empty() {
            tracing::debug!(
                forgotten_job_count = expired.len(),
                remaining_job_count = table.entries.len(),
                "Forgot RMS job ids not used within the retention"
            );
        }
        expired.len()
    }

    /// A poisoned table is still a usable table; panicking here would take the RMS surface down
    /// for every instance.
    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Where a job has got to, independent of how the RPC spells it.
///
/// `GetJobStatus` reports a `JobExecutionState` value, the firmware job status a
/// `FirmwareJobState` value and the certificate and switch image job statuses a lowercase name,
/// so one phase covers every encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobPhase {
    Queued,
    Running,
    Completed,
    Failed,
    /// Unset or unrecognised: the proto3 default, or a spelling outside the vocabulary.
    Unknown,
}

impl JobPhase {
    /// From a `JobExecutionState` value.
    pub(crate) fn from_execution_state(state: i32) -> Self {
        match state {
            1 => Self::Queued,
            2 => Self::Running,
            3 => Self::Completed,
            4 => Self::Failed,
            _ => Self::Unknown,
        }
    }

    /// From a `FirmwareJobState` value.
    pub(crate) fn from_firmware_job_state(state: i32) -> Self {
        match rms::FirmwareJobState::try_from(state) {
            Ok(rms::FirmwareJobState::Queued) => Self::Queued,
            Ok(rms::FirmwareJobState::Running) => Self::Running,
            Ok(rms::FirmwareJobState::Completed) => Self::Completed,
            Ok(rms::FirmwareJobState::Failed) => Self::Failed,
            Ok(rms::FirmwareJobState::Unspecified) | Err(_) => Self::Unknown,
        }
    }

    /// From the string state of the certificate and switch image job status RPCs.
    pub(crate) fn from_wire_str(state: &str) -> Self {
        match state.trim().to_ascii_lowercase().as_str() {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    /// The `JobExecutionState` value; `Unknown` is the proto3 default.
    pub(crate) fn execution_state(self) -> i32 {
        match self {
            Self::Queued => 1,
            Self::Running => 2,
            Self::Completed => 3,
            Self::Failed => 4,
            Self::Unknown => 0,
        }
    }

    /// The spelling of the string-typed job status RPC; `Unknown` is empty.
    pub(crate) fn wire_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unknown => "",
        }
    }

    /// Lower is worse or less advanced. Unknown sits between queued and running: it is not
    /// finished, and the caller must keep polling.
    fn rank(self) -> u8 {
        match self {
            Self::Failed => 0,
            Self::Queued => 1,
            Self::Unknown => 2,
            Self::Running => 3,
            Self::Completed => 4,
        }
    }

    /// Index of the part that decides an aggregate: a failed part, else the least advanced one;
    /// the first such part when several tie. `None` for an empty list.
    pub(crate) fn deciding_index(phases: &[JobPhase]) -> Option<usize> {
        phases
            .iter()
            .enumerate()
            .min_by_key(|(_, phase)| phase.rank())
            .map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str) -> SourceId {
        SourceId(name.to_owned())
    }

    fn job(source_name: &str, id: &str) -> BackendJob {
        BackendJob {
            source: source(source_name),
            job_id: id.to_owned(),
        }
    }

    fn map(retention: Duration) -> JobMap {
        JobMap::with_prefix_and_retention("t".to_owned(), retention)
    }

    #[test]
    fn interning_is_idempotent_per_instance_and_distinct_across_instances() {
        let jobs = map(JOB_RETENTION);

        let first = jobs.intern(&source("a"), "rms-mock-1");
        let again = jobs.intern(&source("a"), "rms-mock-1");
        let other_instance = jobs.intern(&source("b"), "rms-mock-1");

        assert_eq!(first, "t-1");
        assert_eq!(again, first, "the same backend job keeps its gateway id");
        assert_eq!(
            other_instance, "t-2",
            "equal ids on different instances are different jobs"
        );
        assert_eq!(
            jobs.resolve("t-2"),
            Some(JobRecord::Single(job("b", "rms-mock-1")))
        );
        assert_eq!(jobs.intern(&source("a"), ""), "", "no job stays no job");
        assert_eq!(jobs.batch_id(Vec::new()), "");
        assert_eq!(
            jobs.resolve("rms-mock-1"),
            None,
            "backend ids are never gateway ids"
        );
    }

    #[test]
    fn a_batch_over_several_instances_gets_an_aggregate_id_and_its_parts_know_it() {
        let jobs = map(JOB_RETENTION);

        let single = jobs.batch_id(vec![job("a", "rms-mock-7")]);
        let split = jobs.batch_id(vec![job("a", "rms-mock-8"), job("b", "rms-mock-3")]);

        assert_eq!(
            jobs.resolve(&single),
            Some(JobRecord::Single(job("a", "rms-mock-7")))
        );
        assert_eq!(
            jobs.resolve(&split),
            Some(JobRecord::Aggregate(vec![
                job("a", "rms-mock-8"),
                job("b", "rms-mock-3")
            ]))
        );
        assert_eq!(
            jobs.aggregate_of(&job("b", "rms-mock-3")),
            Some(split),
            "a per-instance parent reports the aggregate as its parent"
        );
        assert_eq!(jobs.aggregate_of(&job("a", "rms-mock-7")), None);
    }

    #[test]
    fn fresh_maps_carry_a_process_token_in_their_prefix() {
        let id = JobMap::new().intern(&source("a"), "rms-mock-1");
        assert!(id.starts_with("gw-") && id.len() > "gw--1".len(), "{id}");
    }

    #[test]
    fn ids_not_used_within_the_retention_are_forgotten() {
        let retention = Duration::from_secs(600);
        let jobs = map(retention);
        let start = Instant::now();
        let a = source("a");
        let single = jobs.intern_at(&a, "rms-mock-1", start);
        let split = jobs.batch_id_at(vec![job("a", "rms-mock-2"), job("b", "rms-mock-2")], start);

        // Polling keeps the aggregate alive; the single, never polled, is forgotten.
        assert!(jobs.resolve_at(&split, start + retention / 2).is_some());
        assert_eq!(jobs.resolve_at(&single, start + retention), None);
        assert!(jobs.resolve_at(&split, start + retention).is_some());
        assert_eq!(
            jobs.aggregate_of(&job("a", "rms-mock-2")).as_deref(),
            Some(split.as_str())
        );

        // A forgotten backend job seen again gets a fresh id.
        assert_ne!(jobs.intern_at(&a, "rms-mock-1", start + retention), single);

        // Long after its last use the aggregate goes too, with its parts' membership.
        assert_eq!(jobs.resolve_at(&split, start + retention * 3), None);
        assert_eq!(jobs.aggregate_of(&job("a", "rms-mock-2")), None);
    }

    #[test]
    fn the_deciding_part_is_a_failure_or_the_least_advanced() {
        use JobPhase::*;
        assert_eq!(JobPhase::deciding_index(&[]), None);
        assert_eq!(JobPhase::deciding_index(&[Completed, Completed]), Some(0));
        assert_eq!(JobPhase::deciding_index(&[Completed, Running]), Some(1));
        assert_eq!(
            JobPhase::deciding_index(&[Running, Queued, Completed]),
            Some(1)
        );
        assert_eq!(JobPhase::deciding_index(&[Running, Unknown]), Some(1));
        assert_eq!(
            JobPhase::deciding_index(&[Queued, Failed, Running]),
            Some(1)
        );
    }

    #[test]
    fn phases_round_trip_through_both_wire_encodings() {
        assert_eq!(JobPhase::from_execution_state(42), JobPhase::Unknown);
        assert_eq!(JobPhase::from_wire_str(" Running "), JobPhase::Running);
        assert_eq!(JobPhase::from_wire_str("installing"), JobPhase::Unknown);
        for phase in [
            JobPhase::Queued,
            JobPhase::Running,
            JobPhase::Completed,
            JobPhase::Failed,
            JobPhase::Unknown,
        ] {
            assert_eq!(
                JobPhase::from_execution_state(phase.execution_state()),
                phase
            );
            assert_eq!(JobPhase::from_wire_str(phase.wire_str()), phase);
        }
    }
}
