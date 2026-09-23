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

//! Asynchronous job tracking.
//!
//! A job advances each time it is polled rather than with time. A batch is
//! one parent job with one child per node: polling the parent advances every
//! child and reports what they add up to.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::FaultConfig;
use crate::envelope::{BatchOutcome, NodeResult, UNMATCHED_NODE};
use crate::resolve::NodeRef;
use crate::rms;

/// Prefix of generated job ids.
const JOB_ID_PREFIX: &str = "rms-mock";

/// The poll on which a job reports its terminal state; earlier polls see it
/// running.
const TERMINAL_AFTER_OBSERVATIONS: u32 = 2;

/// Failed node-level jobs kept before the oldest is forgotten; bounds memory
/// when a caller keeps issuing batches for unmatched nodes. RMS instead keeps
/// completed and failed jobs for 24 hours by default and caps its job tracker
/// at 10,000 records. A failed job forgotten here reads as completed.
const MAX_FAILED_JOBS: usize = 4096;

/// The id a job is polled by. The ids this crate issues are
/// `rms-mock-<run>-<n>`, but a caller may poll any spelling, and one the
/// store has no record of reads as a completed job. Never blank.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct JobId(String);

/// A blank string, which names no job.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BlankJobId;

impl JobId {
    /// The canonical spelling of the `n`th id of run `run`.
    fn new(run: u64, n: u64) -> Self {
        Self(format!("{JOB_ID_PREFIX}-{run}-{n}"))
    }

    /// The `(run, n)` of an id spelled as this crate issues them; `None` for
    /// any other string, `-01` for `-1` included.
    fn parts(&self) -> Option<(u64, u64)> {
        let (run, n) = self
            .0
            .strip_prefix(JOB_ID_PREFIX)?
            .strip_prefix('-')?
            .split_once('-')?;
        let (run, n): (u64, u64) = (run.parse().ok()?, n.parse().ok()?);
        (Self::new(run, n) == *self).then_some((run, n))
    }
}

impl FromStr for JobId {
    type Err = BlankJobId;

    fn from_str(s: &str) -> Result<Self, BlankJobId> {
        if s.trim().is_empty() {
            return Err(BlankJobId);
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<JobId> for String {
    fn from(id: JobId) -> Self {
        id.0
    }
}

/// Where a job has got to; rendered per RPC as an enum or a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JobState {
    Running,
    Completed,
    Failed,
}

impl JobState {
    /// The `JobExecutionState` value; never `Unspecified`.
    pub(crate) fn as_execution_state(self) -> i32 {
        let state = match self {
            Self::Running => rms::JobExecutionState::Running,
            Self::Completed => rms::JobExecutionState::Completed,
            Self::Failed => rms::JobExecutionState::Failed,
        };
        state as i32
    }

    /// The lowercase spelling for the RPCs that report state as a string.
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        match self {
            Self::Running => false,
            Self::Completed | Self::Failed => true,
        }
    }

    /// What a parent reports, given its children: failed once any child has
    /// failed, completed once every child has, running otherwise.
    fn of_children(children: &[JobStatus]) -> Self {
        if children.iter().any(|c| c.state == Self::Failed) {
            Self::Failed
        } else if children.iter().all(|c| c.state == Self::Completed) {
            Self::Completed
        } else {
            Self::Running
        }
    }
}

/// What completing a node-level job does to the mock's state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    /// The switch loses its fabric primary role, as a factory reset would.
    ResetFabricRole,
}

/// A job the ledger tracks.
enum Job {
    Node(NodeJob),
    Parent(ParentJob),
}

/// A job for one node, or for a rack with no node to tie it to.
struct NodeJob {
    /// `None` for a rack-level job.
    node_id: Option<String>,
    rack_id: String,
    /// Why the job is to end in failure rather than completion, when it is.
    failure: Option<String>,
    observations: u32,
    parent: Option<JobId>,
    /// Reported with the poll that completes the job.
    effect: Option<Effect>,
}

/// A batch's job, which reports what its children add up to.
struct ParentJob {
    /// `None` when the children span more than one rack.
    rack_id: Option<String>,
    /// `(node_id, job_id)` in request order.
    children: Vec<(String, JobId)>,
}

/// A node-level job as it is asked for.
enum NewJob<'a> {
    /// A job of its own for one node; the fault table decides its outcome.
    Standalone { node_id: &'a str, rack_id: &'a str },
    /// A job of its own for a rack, tied to no node, that fails with `error`.
    RackFailure { rack_id: &'a str, error: String },
    /// One child of the batch `parent`. The child of a node no device
    /// matched fails with [`UNMATCHED_NODE`]; any other carries `effect`,
    /// and the fault table decides its outcome.
    Child {
        node: &'a NodeRef<'a>,
        parent: &'a JobId,
        effect: Option<Effect>,
    },
}

struct Ledger {
    jobs: HashMap<JobId, Job>,
    /// Node-level jobs that have reported failed, oldest first.
    failed: VecDeque<JobId>,
    next_id: u64,
}

/// Every job handed out and still to be reported on.
///
/// A job seen complete is forgotten, a parent is forgotten once it has
/// reported a terminal state or has no child left, and a forgotten job reads
/// as complete. A failed node-level job keeps reading failed until
/// `max_failed` newer jobs have failed.
pub(crate) struct JobStore {
    ledger: Mutex<Ledger>,
    /// Unix nanoseconds at which the store was created. Part of every id, so a
    /// restart never re-issues an id a client still polls.
    run: u64,
    faults: FaultConfig,
    max_failed: usize,
}

/// The ids a batch was issued.
pub(crate) struct BatchJobIds<'a> {
    pub(crate) parent: JobId,
    /// `(node_id, job_id)` in request order.
    pub(crate) children: Vec<(&'a str, JobId)>,
}

/// What a poll of a job returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobStatus {
    pub(crate) job_id: JobId,
    pub(crate) state: JobState,
    /// `None` for a parent, and for a job tied to no node.
    pub(crate) node_id: Option<String>,
    /// `None` for a parent whose children span more than one rack, and for
    /// a job the store has no record of.
    pub(crate) rack_id: Option<String>,
    pub(crate) parent_job_id: Option<JobId>,
    /// Why the job failed; `None` unless `state` is [`JobState::Failed`].
    pub(crate) error_message: Option<String>,
    /// A parent's children as this poll left them; empty otherwise.
    pub(crate) children: Vec<JobStatus>,
    /// What this poll's completion does to the mock; `None` unless `state`
    /// is [`JobState::Completed`] and the job was started with an effect.
    pub(crate) effect: Option<Effect>,
}

impl JobStore {
    pub(crate) fn new(faults: FaultConfig) -> Self {
        let run = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|since| u64::try_from(since.as_nanos()).ok())
            .unwrap_or_default();
        Self::with_run(faults, run, MAX_FAILED_JOBS)
    }

    fn with_run(faults: FaultConfig, run: u64, max_failed: usize) -> Self {
        Self {
            ledger: Mutex::new(Ledger {
                jobs: HashMap::new(),
                failed: VecDeque::new(),
                next_id: 1,
            }),
            run,
            faults,
            max_failed,
        }
    }

    /// Start a job for one node, which completes unless the fault table
    /// selects it, and return its id.
    pub(crate) fn start(&self, node_id: &str, rack_id: &str) -> JobId {
        let mut ledger = crate::lock(&self.ledger);
        self.insert(&mut ledger, NewJob::Standalone { node_id, rack_id })
    }

    /// Start a job for a rack, tied to no node, that fails with `error`, and
    /// return its id.
    pub(crate) fn start_failing(&self, rack_id: &str, error: String) -> JobId {
        let mut ledger = crate::lock(&self.ledger);
        self.insert(&mut ledger, NewJob::RackFailure { rack_id, error })
    }

    /// Start a parent with one child per node, in request order. The child
    /// of a node no device matched fails with [`UNMATCHED_NODE`]; every other
    /// child carries `effect`.
    pub(crate) fn start_batch<'a>(
        &self,
        refs: impl IntoIterator<Item = &'a NodeRef<'a>>,
        effect: Option<Effect>,
    ) -> BatchJobIds<'a> {
        let refs: Vec<&NodeRef<'a>> = refs.into_iter().collect();
        let racks: HashSet<&str> = refs.iter().map(|r| r.rack_id).collect();
        let rack_id = match racks.len() {
            1 => Some(refs[0].rack_id.to_owned()),
            _ => None,
        };

        let mut ledger = crate::lock(&self.ledger);
        let parent = self.allocate_id(&mut ledger);
        let children: Vec<(&str, JobId)> = refs
            .iter()
            .map(|node| {
                let id = self.insert(
                    &mut ledger,
                    NewJob::Child {
                        node,
                        parent: &parent,
                        effect,
                    },
                );
                (node.node_id, id)
            })
            .collect();
        ledger.jobs.insert(
            parent.clone(),
            Job::Parent(ParentJob {
                rack_id,
                children: children
                    .iter()
                    .map(|(node_id, id)| ((*node_id).to_owned(), id.clone()))
                    .collect(),
            }),
        );

        BatchJobIds { parent, children }
    }

    /// Whether this process issued `job_id`: it carries this run and a
    /// sequence number handed out so far.
    pub(crate) fn issued(&self, job_id: &JobId) -> bool {
        job_id.parts().is_some_and(|(run, n)| {
            run == self.run && (1..crate::lock(&self.ledger).next_id).contains(&n)
        })
    }

    /// Poll a job, advancing it. Polling a parent advances each of its
    /// children once and reports their aggregate. A job this process never
    /// issued, or has forgotten, is reported complete.
    pub(crate) fn observe(&self, job_id: &JobId) -> JobStatus {
        let mut ledger = crate::lock(&self.ledger);
        let (rack_id, child_refs) = match ledger.jobs.get(job_id) {
            None => {
                tracing::warn!(%job_id, "Reporting an unknown job as complete");
                return Self::forgotten(job_id, None);
            }
            Some(Job::Node(_)) => {
                let status = self
                    .observe_node(&mut ledger, job_id)
                    .expect("looked up under the same lock");
                if status.state == JobState::Completed {
                    Self::forget_node(&mut ledger, job_id);
                }
                return status;
            }
            Some(Job::Parent(parent)) => (parent.rack_id.clone(), parent.children.clone()),
        };

        let children: Vec<JobStatus> = child_refs
            .iter()
            .map(|(_, id)| {
                let status = self
                    .observe_node(&mut ledger, id)
                    .unwrap_or_else(|| Self::forgotten(id, Some(job_id)));
                if status.state == JobState::Completed {
                    ledger.jobs.remove(id);
                }
                status
            })
            .collect();
        let state = JobState::of_children(&children);
        let error_message = match state {
            JobState::Failed => {
                let results: Vec<NodeResult<'_>> = child_refs
                    .iter()
                    .zip(&children)
                    .map(|((node_id, _), c)| {
                        (
                            node_id.as_str(),
                            c.error_message.clone().map_or(Ok(()), Err),
                        )
                    })
                    .collect();
                Some(BatchOutcome::of(&results).message)
            }
            JobState::Running | JobState::Completed => None,
        };
        if state.is_terminal() {
            ledger.jobs.remove(job_id);
        }

        JobStatus {
            job_id: job_id.clone(),
            state,
            node_id: None,
            rack_id,
            parent_job_id: None,
            error_message,
            children,
            effect: None,
        }
    }

    /// Insert a node-level job. An earlier job for the same node is left to
    /// report its own outcome.
    fn insert(&self, ledger: &mut Ledger, job: NewJob<'_>) -> JobId {
        let job = match job {
            NewJob::Standalone { node_id, rack_id } => NodeJob {
                node_id: Some(node_id.to_owned()),
                rack_id: rack_id.to_owned(),
                failure: self.fault(node_id),
                observations: 0,
                parent: None,
                effect: None,
            },
            NewJob::RackFailure { rack_id, error } => NodeJob {
                node_id: None,
                rack_id: rack_id.to_owned(),
                failure: Some(error),
                observations: 0,
                parent: None,
                effect: None,
            },
            NewJob::Child {
                node,
                parent,
                effect,
            } => NodeJob {
                node_id: Some(node.node_id.to_owned()),
                rack_id: node.rack_id.to_owned(),
                failure: if node.matched() {
                    self.fault(node.node_id)
                } else {
                    Some(UNMATCHED_NODE.to_owned())
                },
                observations: 0,
                parent: Some(parent.clone()),
                effect,
            },
        };
        let id = self.allocate_id(ledger);
        ledger.jobs.insert(id.clone(), Job::Node(job));
        id
    }

    fn allocate_id(&self, ledger: &mut Ledger) -> JobId {
        let id = JobId::new(self.run, ledger.next_id);
        ledger.next_id += 1;
        id
    }

    /// Whether the fault table fails a job for this node, and why.
    fn fault(&self, node_id: &str) -> Option<String> {
        self.faults
            .fail_jobs_for_node_ids
            .iter()
            .any(|n| n == node_id)
            .then(|| {
                tracing::warn!(node_id, "Failing this node's job as configured");
                format!("simulated failure for node {node_id}")
            })
    }

    /// Advance a node-level job by one poll; `None` when the ledger has no
    /// such job. The poll that first fails a job records it, forgetting the
    /// oldest failed job once more than `max_failed` are kept.
    fn observe_node(&self, ledger: &mut Ledger, job_id: &JobId) -> Option<JobStatus> {
        let job = match ledger.jobs.get_mut(job_id)? {
            Job::Node(job) => job,
            Job::Parent(_) => return None,
        };
        job.observations += 1;
        let (state, error_message) = if job.observations < TERMINAL_AFTER_OBSERVATIONS {
            (JobState::Running, None)
        } else {
            match &job.failure {
                Some(reason) => (JobState::Failed, Some(reason.clone())),
                None => (JobState::Completed, None),
            }
        };
        let status = JobStatus {
            job_id: job_id.clone(),
            state,
            node_id: job.node_id.clone(),
            rack_id: Some(job.rack_id.clone()),
            parent_job_id: job.parent.clone(),
            error_message,
            children: Vec::new(),
            effect: match state {
                JobState::Completed => job.effect,
                JobState::Running | JobState::Failed => None,
            },
        };
        if state == JobState::Failed && job.observations == TERMINAL_AFTER_OBSERVATIONS {
            ledger.failed.push_back(job_id.clone());
            if ledger.failed.len() > self.max_failed
                && let Some(oldest) = ledger.failed.pop_front()
            {
                Self::forget_node(ledger, &oldest);
            }
        }
        Some(status)
    }

    /// Drop a node-level job, and its parent once no child of it is left.
    fn forget_node(ledger: &mut Ledger, job_id: &JobId) {
        let parent = match ledger.jobs.remove(job_id) {
            Some(Job::Node(job)) => job.parent,
            Some(Job::Parent(_)) | None => return,
        };
        if let Some(parent) = parent
            && let Some(Job::Parent(batch)) = ledger.jobs.get(&parent)
            && batch
                .children
                .iter()
                .all(|(_, id)| !ledger.jobs.contains_key(id))
        {
            ledger.jobs.remove(&parent);
        }
    }

    /// A job the ledger does not have reads as complete, listing one
    /// completed child when it is polled as a parent.
    fn forgotten(job_id: &JobId, parent: Option<&JobId>) -> JobStatus {
        let completed = |job_id: &JobId, parent: Option<&JobId>| JobStatus {
            job_id: job_id.clone(),
            state: JobState::Completed,
            node_id: None,
            rack_id: None,
            parent_job_id: parent.cloned(),
            error_message: None,
            children: Vec::new(),
            effect: None,
        };
        match parent {
            Some(_) => completed(job_id, parent),
            None => JobStatus {
                children: vec![completed(&JobId(format!("{job_id}-child")), Some(job_id))],
                ..completed(job_id, None)
            },
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        crate::lock(&self.ledger).jobs.len()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::{Effect, JobId, JobState, JobStatus, JobStore, MAX_FAILED_JOBS};
    use crate::config::FaultConfig;
    use crate::inventory::SimNode;
    use crate::resolve::NodeRef;

    /// The run every test store is built with, so ids can be named exactly.
    const RUN: u64 = 7;

    fn store(faults: FaultConfig) -> JobStore {
        JobStore::with_run(faults, RUN, MAX_FAILED_JOBS)
    }

    fn fail_nodes(ids: &[&str]) -> FaultConfig {
        FaultConfig {
            fail_jobs_for_node_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A node the request named that the inventory has.
    fn node<'a>(rack_id: &'a str, node_id: &'a str, device: &'a SimNode) -> NodeRef<'a> {
        NodeRef {
            node_id,
            rack_id,
            node: Some(device),
        }
    }

    /// A node the request named that the inventory does not have.
    fn stranger<'a>(rack_id: &'a str, node_id: &'a str) -> NodeRef<'a> {
        NodeRef {
            node_id,
            rack_id,
            node: None,
        }
    }

    fn id(s: &str) -> JobId {
        s.parse().unwrap()
    }

    fn states(children: &[JobStatus]) -> Vec<JobState> {
        children.iter().map(|c| c.state).collect()
    }

    #[test]
    fn a_job_id_is_any_string_but_a_blank_one() {
        check_values(
            [
                Check {
                    scenario: "an id this crate issues",
                    input: "rms-mock-7-1",
                    expect: Some("rms-mock-7-1".to_owned()),
                },
                Check {
                    scenario: "a spelling it never issues is kept verbatim",
                    input: " from-a-past-life ",
                    expect: Some(" from-a-past-life ".to_owned()),
                },
                Check {
                    scenario: "empty",
                    input: "",
                    expect: None,
                },
                Check {
                    scenario: "whitespace",
                    input: "  ",
                    expect: None,
                },
            ],
            |s| s.parse::<JobId>().ok().map(String::from),
        );
    }

    #[test]
    fn ids_are_sequential_and_carry_the_run_with_the_parent_first() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());

        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];

        let batch = jobs.start_batch(&nodes, None);
        assert_eq!(batch.parent, id("rms-mock-7-1"));
        assert_eq!(
            batch.children,
            vec![("n1", id("rms-mock-7-2")), ("n2", id("rms-mock-7-3"))]
        );
        assert_eq!(jobs.start("n3", "rack-a"), id("rms-mock-7-4"));
        assert!(jobs.issued(&id("rms-mock-7-4")));
        assert!(!jobs.issued(&id("rms-mock-7-5")), "not handed out yet");
        assert!(!jobs.issued(&id("rms-mock-7-4-child")));
        assert!(
            !jobs.issued(&id("rms-mock-7-04")),
            "only the canonical spelling"
        );

        // Another run issues the same n under a different run, so an id from
        // before a restart is unknown to the process after it rather than
        // another node's job.
        let later = JobStore::with_run(FaultConfig::default(), RUN + 1, MAX_FAILED_JOBS);
        let fresh = later.start("n9", "rack-a");
        assert_eq!(fresh, id("rms-mock-8-1"));
        assert!(!later.issued(&id("rms-mock-7-1")));
        assert!(later.issued(&fresh));
        assert_eq!(later.observe(&id("rms-mock-7-1")).node_id, None);
        assert_eq!(later.observe(&fresh).node_id.as_deref(), Some("n9"));

        let clock = JobStore::new(FaultConfig::default()).start("n1", "rack-a");
        let (run, n) = clock.parts().unwrap_or_else(|| panic!("{clock}"));
        assert_eq!(n, 1);
        assert!(run > 1_700_000_000_000_000_000, "{clock}");
    }

    #[test]
    fn a_parent_reports_what_its_children_add_up_to() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let child = |i: usize| &batch.children[i].1;

        // Polling a child advances that child alone.
        let first = jobs.observe(child(0));
        assert_eq!(first.state, JobState::Running);
        assert_eq!(first.parent_job_id, Some(id("rms-mock-7-1")));
        assert_eq!(
            (first.node_id.as_deref(), first.rack_id.as_deref()),
            (Some("n1"), Some("rack-a"))
        );

        // Polling the parent advances every child once: n1 completes on its
        // second poll while n2 is only now running.
        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Running);
        assert_eq!(
            (parent.node_id.as_deref(), parent.rack_id.as_deref()),
            (None, Some("rack-a"))
        );
        assert_eq!(parent.parent_job_id, None);
        assert_eq!(
            states(&parent.children),
            [JobState::Completed, JobState::Running]
        );
        assert_eq!(&parent.children[1].job_id, child(1));

        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Completed);
        assert_eq!(parent.error_message, None);

        // An empty batch has nothing to do and is done.
        let empty = jobs.start_batch(&[] as &[NodeRef<'_>], None);
        assert_eq!(jobs.observe(&empty.parent).state, JobState::Completed);
    }

    #[test]
    fn a_parent_over_two_racks_has_no_rack() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());

        let mixed_nodes = [node("rack-a", "n1", &device), node("rack-b", "n2", &device)];

        let mixed = jobs.start_batch(&mixed_nodes, None);
        let parent = jobs.observe(&mixed.parent);
        assert_eq!(parent.rack_id, None);
        assert_eq!(parent.children[0].rack_id.as_deref(), Some("rack-a"));
        assert_eq!(parent.children[1].rack_id.as_deref(), Some("rack-b"));
    }

    #[test]
    fn a_selected_job_runs_and_then_fails_and_so_does_its_parent() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n2"]));
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let failing = &batch.children[1].1;

        assert_eq!(jobs.observe(failing).state, JobState::Running);
        let failed = jobs.observe(failing);
        assert_eq!(failed.state, JobState::Failed);
        assert!(
            failed
                .error_message
                .as_deref()
                .is_some_and(|m| m.contains("n2")),
            "{:?}",
            failed.error_message
        );

        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Failed);
        assert!(
            parent
                .error_message
                .as_deref()
                .is_some_and(|m| m.contains("n2")),
            "{:?}",
            parent.error_message
        );
        assert_eq!(
            states(&parent.children),
            [JobState::Running, JobState::Failed]
        );

        // The healthy sibling and a standalone job for another node complete.
        assert_eq!(
            jobs.observe(&batch.children[0].1).state,
            JobState::Completed
        );
        let fine = jobs.start("n3", "rack-a");
        jobs.observe(&fine);
        assert_eq!(jobs.observe(&fine).state, JobState::Completed);
        let doomed = jobs.start("n2", "rack-a");
        jobs.observe(&doomed);
        assert_eq!(jobs.observe(&doomed).state, JobState::Failed);
    }

    #[test]
    fn the_child_of_an_unmatched_node_fails_and_names_the_node() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device), stranger("rack-a", "ghost")];
        let batch = jobs.start_batch(&nodes, None);

        jobs.observe(&batch.parent);
        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Failed);
        assert_eq!(
            states(&parent.children),
            [JobState::Completed, JobState::Failed]
        );
        assert_eq!(parent.children[1].node_id.as_deref(), Some("ghost"));
        assert_eq!(
            parent.children[1].error_message.as_deref(),
            Some(super::UNMATCHED_NODE)
        );
        assert!(
            parent
                .error_message
                .as_deref()
                .is_some_and(|m| m.contains("ghost")),
            "{:?}",
            parent.error_message
        );
    }

    #[test]
    fn an_effect_is_reported_with_the_poll_that_completes_the_job() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n2"]));
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetFabricRole));

        let running = jobs.observe(&batch.parent);
        assert_eq!(
            running
                .children
                .iter()
                .map(|c| c.effect)
                .collect::<Vec<_>>(),
            [None, None]
        );

        // Once: the completed child is forgotten with its effect delivered;
        // the failed child never has one.
        let terminal = jobs.observe(&batch.parent);
        assert_eq!(
            terminal
                .children
                .iter()
                .map(|c| (c.state, c.effect))
                .collect::<Vec<_>>(),
            [
                (JobState::Completed, Some(Effect::ResetFabricRole)),
                (JobState::Failed, None)
            ]
        );
        assert_eq!(terminal.effect, None, "a parent has no effect of its own");
        assert_eq!(jobs.observe(&batch.children[0].1).effect, None);
    }

    #[test]
    fn the_ledger_forgets_a_job_once_its_outcome_is_reported() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n4"]));

        // Children polled directly: the parent goes with its last child.
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let child = |i: usize| &batch.children[i].1;
        jobs.observe(child(0));
        assert_eq!(jobs.observe(child(0)).state, JobState::Completed);
        assert_eq!(jobs.tracked(), 2, "the parent waits for its other child");
        jobs.observe(child(1));
        assert_eq!(jobs.observe(child(1)).state, JobState::Completed);
        assert_eq!(jobs.tracked(), 0);

        // A parent polled to failure has reported its outcome and goes; the
        // failed child keeps reading failed for a caller that polls it.
        let nodes = [node("rack-a", "n3", &device), node("rack-a", "n4", &device)];
        let batch = jobs.start_batch(&nodes, None);
        jobs.observe(&batch.parent);
        assert_eq!(jobs.observe(&batch.parent).state, JobState::Failed);
        assert_eq!(jobs.tracked(), 1);
        assert_eq!(jobs.observe(&batch.children[1].1).state, JobState::Failed);
        assert_eq!(jobs.tracked(), 1);

        // A job never issued here reads complete with one completed child
        // and is not added.
        let unknown = jobs.observe(&id("rms-mock-0-99"));
        assert_eq!(unknown.state, JobState::Completed);
        assert_eq!(unknown.children.len(), 1);
        assert_eq!(unknown.children[0].state, JobState::Completed);
        assert_eq!(unknown.children[0].parent_job_id, Some(id("rms-mock-0-99")));
        assert_ne!(unknown.children[0].job_id, unknown.job_id);
        assert_eq!(jobs.tracked(), 1);
    }

    #[test]
    fn starting_a_job_for_a_node_leaves_its_earlier_job_alone() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n1"]));
        let nodes = [node("rack-a", "n1", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let doomed = batch.children[0].1.clone();
        assert_eq!(jobs.observe(&doomed).state, JobState::Running);

        // A job for the same node from another RPC neither evicts the one in
        // flight nor changes what it and its parent report.
        let again = jobs.start("n1", "rack-a");
        assert_ne!(again, doomed);
        let failed = jobs.observe(&doomed);
        assert_eq!(
            (failed.state, failed.node_id.as_deref()),
            (JobState::Failed, Some("n1"))
        );
        assert_eq!(jobs.observe(&batch.parent).state, JobState::Failed);
        jobs.observe(&again);
        assert_eq!(jobs.observe(&again).state, JobState::Failed);
    }

    #[test]
    fn the_ledger_keeps_the_most_recent_failed_jobs_only() {
        let jobs = JobStore::with_run(fail_nodes(&["n1"]), RUN, 2);
        let fail = || {
            let id = jobs.start("n1", "rack-a");
            jobs.observe(&id);
            assert_eq!(jobs.observe(&id).state, JobState::Failed);
            id
        };
        let oldest = fail();
        let middle = fail();
        assert_eq!(jobs.tracked(), 2, "at the cap nothing is forgotten");

        // One more failure forgets the oldest, which then reads complete like
        // any job the ledger has no record of; the rest keep reading failed,
        // and polling them again does not count as a new failure.
        let newest = fail();
        assert_eq!(jobs.tracked(), 2);
        assert_eq!(jobs.observe(&oldest).state, JobState::Completed);
        assert_eq!(jobs.observe(&middle).state, JobState::Failed);
        assert_eq!(jobs.observe(&newest).state, JobState::Failed);
        assert_eq!(jobs.tracked(), 2);
    }
}
