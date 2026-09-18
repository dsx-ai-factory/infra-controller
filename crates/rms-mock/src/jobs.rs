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
/// when a caller keeps issuing batches for unmatched nodes.
const MAX_FAILED_JOBS: usize = 4096;

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

struct Job {
    node_id: String,
    rack_id: String,
    /// Why the job is to end in failure rather than completion, when it is.
    failure: Option<String>,
    observations: u32,
    parent: Option<String>,
    /// `Some` for a parent job: its children in request order.
    children: Option<Vec<String>>,
    /// Reported with the poll that completes the job.
    effect: Option<Effect>,
}

struct Ledger {
    jobs: HashMap<String, Job>,
    /// Node-level jobs that have reported failed, oldest first.
    failed: VecDeque<String>,
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
    pub(crate) parent: String,
    /// `(node_id, job_id)` in request order.
    pub(crate) children: Vec<(&'a str, String)>,
}

/// What a poll of a job returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobStatus {
    pub(crate) job_id: String,
    pub(crate) state: JobState,
    /// Empty for a parent job.
    pub(crate) node_id: String,
    /// Empty for a parent whose children span more than one rack.
    pub(crate) rack_id: String,
    pub(crate) parent_job_id: Option<String>,
    /// Why the job failed; empty unless `state` is [`JobState::Failed`].
    pub(crate) error_message: String,
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

    /// Start a job that will complete unless the fault table selects it, and
    /// return its id.
    pub(crate) fn start(&self, node_id: &str, rack_id: &str) -> String {
        let mut ledger = crate::lock(&self.ledger);
        self.insert(&mut ledger, node_id, rack_id, None, None, None)
    }

    /// Start a job that will fail with `error`, and return its id.
    pub(crate) fn start_failing(&self, node_id: &str, rack_id: &str, error: String) -> String {
        let mut ledger = crate::lock(&self.ledger);
        self.insert(&mut ledger, node_id, rack_id, Some(error), None, None)
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
        let parent_rack_id = match racks.len() {
            1 => refs[0].rack_id,
            _ => "",
        };

        let mut ledger = crate::lock(&self.ledger);
        let parent = self.allocate_id(&mut ledger);
        let children: Vec<(&str, String)> = refs
            .iter()
            .map(|r| {
                let failure = (!r.matched()).then(|| UNMATCHED_NODE.to_owned());
                let id = self.insert(
                    &mut ledger,
                    r.node_id,
                    r.rack_id,
                    failure,
                    Some(&parent),
                    effect,
                );
                (r.node_id, id)
            })
            .collect();
        ledger.jobs.insert(
            parent.clone(),
            Job {
                node_id: String::new(),
                rack_id: parent_rack_id.to_owned(),
                failure: None,
                observations: 0,
                parent: None,
                children: Some(children.iter().map(|(_, id)| id.clone()).collect()),
                effect: None,
            },
        );

        BatchJobIds { parent, children }
    }

    /// Whether this process issued `job_id`: it carries this run and a
    /// sequence number handed out so far.
    pub(crate) fn issued(&self, job_id: &str) -> bool {
        let Some((run, n)) = job_id
            .strip_prefix(JOB_ID_PREFIX)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|rest| rest.split_once('-'))
        else {
            return false;
        };
        let (Ok(run), Ok(n)) = (run.parse::<u64>(), n.parse::<u64>()) else {
            return false;
        };
        // Only the canonical spelling: `-01` parses like `-1` but is not a key.
        format!("{JOB_ID_PREFIX}-{run}-{n}") == job_id
            && run == self.run
            && (1..crate::lock(&self.ledger).next_id).contains(&n)
    }

    /// Poll a job, advancing it. Polling a parent advances each of its
    /// children once and reports their aggregate. A job this process never
    /// issued, or has forgotten, is reported complete.
    pub(crate) fn observe(&self, job_id: &str) -> JobStatus {
        let mut ledger = crate::lock(&self.ledger);
        let Some(children) = ledger.jobs.get(job_id).map(|job| job.children.clone()) else {
            tracing::warn!(job_id, "Reporting an unknown job as complete");
            return Self::forgotten(job_id, None);
        };
        let Some(child_ids) = children else {
            let status = self
                .observe_node(&mut ledger, job_id)
                .expect("looked up under the same lock");
            if status.state == JobState::Completed {
                Self::forget_node(&mut ledger, job_id);
            }
            return status;
        };

        let children: Vec<JobStatus> = child_ids
            .iter()
            .map(|id| {
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
                let results: Vec<NodeResult<'_>> = children
                    .iter()
                    .map(|c| {
                        let outcome = match c.state {
                            JobState::Failed => Err(c.error_message.clone()),
                            JobState::Running | JobState::Completed => Ok(()),
                        };
                        (c.node_id.as_str(), outcome)
                    })
                    .collect();
                BatchOutcome::of(&results).message
            }
            JobState::Running | JobState::Completed => String::new(),
        };
        let rack_id = ledger.jobs[job_id].rack_id.clone();
        if state.is_terminal() {
            ledger.jobs.remove(job_id);
        }

        JobStatus {
            job_id: job_id.to_owned(),
            state,
            node_id: String::new(),
            rack_id,
            parent_job_id: None,
            error_message,
            children,
            effect: None,
        }
    }

    /// Insert a node-level job. An earlier job for the same node is left to
    /// report its own outcome.
    fn insert(
        &self,
        ledger: &mut Ledger,
        node_id: &str,
        rack_id: &str,
        failure: Option<String>,
        parent: Option<&str>,
        effect: Option<Effect>,
    ) -> String {
        let id = self.allocate_id(ledger);
        let failure = failure.or_else(|| self.fault(node_id));
        ledger.jobs.insert(
            id.clone(),
            Job {
                node_id: node_id.to_owned(),
                rack_id: rack_id.to_owned(),
                failure,
                observations: 0,
                parent: parent.map(str::to_owned),
                children: None,
                effect,
            },
        );
        id
    }

    fn allocate_id(&self, ledger: &mut Ledger) -> String {
        let id = format!("{JOB_ID_PREFIX}-{}-{}", self.run, ledger.next_id);
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
    fn observe_node(&self, ledger: &mut Ledger, job_id: &str) -> Option<JobStatus> {
        let job = ledger.jobs.get_mut(job_id)?;
        job.observations += 1;
        let state = if job.observations < TERMINAL_AFTER_OBSERVATIONS {
            JobState::Running
        } else if job.failure.is_some() {
            JobState::Failed
        } else {
            JobState::Completed
        };
        let status = JobStatus {
            job_id: job_id.to_owned(),
            state,
            node_id: job.node_id.clone(),
            rack_id: job.rack_id.clone(),
            parent_job_id: job.parent.clone(),
            error_message: match state {
                JobState::Failed => job.failure.clone().unwrap_or_default(),
                JobState::Running | JobState::Completed => String::new(),
            },
            children: Vec::new(),
            effect: match state {
                JobState::Completed => job.effect,
                JobState::Running | JobState::Failed => None,
            },
        };
        if state == JobState::Failed && job.observations == TERMINAL_AFTER_OBSERVATIONS {
            ledger.failed.push_back(job_id.to_owned());
            if ledger.failed.len() > self.max_failed
                && let Some(oldest) = ledger.failed.pop_front()
            {
                Self::forget_node(ledger, &oldest);
            }
        }
        Some(status)
    }

    /// Drop a node-level job, and its parent once no child of it is left.
    fn forget_node(ledger: &mut Ledger, job_id: &str) {
        let Some(job) = ledger.jobs.remove(job_id) else {
            return;
        };
        if let Some(parent) = job.parent
            && ledger
                .jobs
                .get(&parent)
                .and_then(|p| p.children.as_ref())
                .is_some_and(|ids| ids.iter().all(|id| !ledger.jobs.contains_key(id)))
        {
            ledger.jobs.remove(&parent);
        }
    }

    /// A job the ledger does not have reads as complete, listing one
    /// completed child when it is polled as a parent.
    fn forgotten(job_id: &str, parent: Option<&str>) -> JobStatus {
        let completed = |job_id: &str, parent: Option<&str>| JobStatus {
            job_id: job_id.to_owned(),
            state: JobState::Completed,
            node_id: String::new(),
            rack_id: String::new(),
            parent_job_id: parent.map(str::to_owned),
            error_message: String::new(),
            children: Vec::new(),
            effect: None,
        };
        match parent {
            Some(_) => completed(job_id, parent),
            None => JobStatus {
                children: vec![completed(&format!("{job_id}-child"), Some(job_id))],
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
    use super::{Effect, JobState, JobStatus, JobStore, MAX_FAILED_JOBS};
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

    fn states(children: &[JobStatus]) -> Vec<JobState> {
        children.iter().map(|c| c.state).collect()
    }

    #[test]
    fn ids_are_sequential_and_carry_the_run_with_the_parent_first() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());

        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];

        let batch = jobs.start_batch(&nodes, None);
        assert_eq!(batch.parent, "rms-mock-7-1");
        assert_eq!(
            batch.children,
            vec![
                ("n1", "rms-mock-7-2".to_string()),
                ("n2", "rms-mock-7-3".to_string())
            ]
        );
        assert_eq!(jobs.start("n3", "rack-a"), "rms-mock-7-4");
        assert!(jobs.issued("rms-mock-7-4"));
        assert!(!jobs.issued("rms-mock-7-5"), "not handed out yet");
        assert!(!jobs.issued("rms-mock-7-4-child"));
        assert!(!jobs.issued("rms-mock-7-04"), "only the canonical spelling");

        // Another run issues the same n under a different run, so an id from
        // before a restart is unknown to the process after it rather than
        // another node's job.
        let later = JobStore::with_run(FaultConfig::default(), RUN + 1, MAX_FAILED_JOBS);
        let fresh = later.start("n9", "rack-a");
        assert_eq!(fresh, "rms-mock-8-1");
        assert!(!later.issued("rms-mock-7-1"));
        assert!(later.issued(&fresh));
        assert_eq!(later.observe("rms-mock-7-1").node_id, "");
        assert_eq!(later.observe(&fresh).node_id, "n9");

        let clock = JobStore::new(FaultConfig::default()).start("n1", "rack-a");
        let run: u64 = clock
            .strip_prefix("rms-mock-")
            .and_then(|rest| rest.strip_suffix("-1"))
            .and_then(|run| run.parse().ok())
            .unwrap_or_else(|| panic!("{clock}"));
        assert!(run > 1_700_000_000_000_000_000, "{clock}");
    }

    #[test]
    fn a_parent_reports_what_its_children_add_up_to() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let child = |i: usize| batch.children[i].1.as_str();

        // Polling a child advances that child alone.
        let first = jobs.observe(child(0));
        assert_eq!(first.state, JobState::Running);
        assert_eq!(first.parent_job_id.as_deref(), Some("rms-mock-7-1"));
        assert_eq!(
            (first.node_id.as_str(), first.rack_id.as_str()),
            ("n1", "rack-a")
        );

        // Polling the parent advances every child once: n1 completes on its
        // second poll while n2 is only now running.
        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Running);
        assert_eq!(
            (parent.node_id.as_str(), parent.rack_id.as_str()),
            ("", "rack-a")
        );
        assert_eq!(parent.parent_job_id, None);
        assert_eq!(
            states(&parent.children),
            [JobState::Completed, JobState::Running]
        );
        assert_eq!(parent.children[1].job_id, child(1));

        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Completed);
        assert!(parent.error_message.is_empty());

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
        assert_eq!(parent.rack_id, "");
        assert_eq!(parent.children[0].rack_id, "rack-a");
        assert_eq!(parent.children[1].rack_id, "rack-b");
    }

    #[test]
    fn a_selected_job_runs_and_then_fails_and_so_does_its_parent() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n2"]));
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        let failing = batch.children[1].1.as_str();

        assert_eq!(jobs.observe(failing).state, JobState::Running);
        let failed = jobs.observe(failing);
        assert_eq!(failed.state, JobState::Failed);
        assert!(
            failed.error_message.contains("n2"),
            "{:?}",
            failed.error_message
        );

        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Failed);
        assert!(
            parent.error_message.contains("n2"),
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
        assert_eq!(parent.children[1].node_id, "ghost");
        assert_eq!(parent.children[1].error_message, super::UNMATCHED_NODE);
        assert!(
            parent.error_message.contains("ghost"),
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
        let child = |i: usize| batch.children[i].1.as_str();
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
        let unknown = jobs.observe("rms-mock-0-99");
        assert_eq!(unknown.state, JobState::Completed);
        assert_eq!(unknown.children.len(), 1);
        assert_eq!(unknown.children[0].state, JobState::Completed);
        assert_eq!(
            unknown.children[0].parent_job_id.as_deref(),
            Some("rms-mock-0-99")
        );
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
            (failed.state, failed.node_id.as_str()),
            (JobState::Failed, "n1")
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
