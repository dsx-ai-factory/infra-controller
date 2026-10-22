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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{FaultConfig, RmsMockConfig};
use crate::envelope::{BatchOutcome, NodeResult, UNMATCHED_NODE};
use crate::resolve::NodeRef;
use crate::rms;

/// Prefix of generated job ids.
const JOB_ID_PREFIX: &str = "rms-mock";

/// The poll on which a job reports its terminal state; earlier polls see it
/// running.
const TERMINAL_AFTER_OBSERVATIONS: u32 = 2;

/// The id a job is polled by. The ids this crate issues are
/// `rms-mock-<run>-<n>`, but a caller may poll any spelling, and one the
/// store has no record of is unknown. Never blank.
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
    #[cfg(test)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    /// Restore NVOS factory credentials and clear the switch fabric role.
    ResetSwitch { bmc_mac: mac_address::MacAddress },
    /// The switch's NVOS user `username` takes `password`, as a completed
    /// password rotation leaves it. The switch is the device whose BMC has
    /// `bmc_mac`, the one identifier every simulated device carries.
    SetSwitchPassword {
        bmc_mac: mac_address::MacAddress,
        username: Arc<str>,
        password: Arc<str>,
    },
}

/// A job the ledger tracks.
enum Job {
    Node(NodeJob),
    Parent(ParentJob),
    Terminal {
        status: JobStatus,
        completed_at: Instant,
    },
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
    /// and the fault table decides its outcome, unless the effect could not
    /// be built, which fails the child with the reason.
    Child {
        node: &'a NodeRef<'a>,
        parent: &'a JobId,
        effect: Result<Option<Effect>, String>,
    },
}

struct Ledger {
    jobs: HashMap<JobId, Job>,
    /// Terminal records in completion order.
    terminal: VecDeque<JobId>,
    next_id: u64,
    retention_limit_warned: bool,
}

/// Every job handed out and still to be reported on.
///
/// Terminal records retain their identities and outcomes without replaying
/// effects. Records exceeding `max_terminal` are evicted in completion order
/// after `min_retention` has elapsed, with a hard ceiling of four times the
/// target even for younger records.
pub(crate) struct JobStore {
    ledger: Mutex<Ledger>,
    /// Unix nanoseconds at which the store was created. Part of every id, so a
    /// restart never re-issues an id a client still polls.
    run: u64,
    faults: FaultConfig,
    max_terminal: usize,
    min_retention: Duration,
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
    pub(crate) fn new(config: &RmsMockConfig) -> Self {
        let run = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|since| u64::try_from(since.as_nanos()).ok())
            .unwrap_or_default();
        Self::with_run(config, run)
    }

    fn with_run(config: &RmsMockConfig, run: u64) -> Self {
        Self {
            ledger: Mutex::new(Ledger {
                jobs: HashMap::new(),
                terminal: VecDeque::new(),
                next_id: 1,
                retention_limit_warned: false,
            }),
            run,
            faults: config.faults.clone(),
            max_terminal: config.max_terminal_jobs,
            min_retention: Duration::from_secs(config.min_terminal_retention_secs),
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
        self.start_batch_with(refs, |_| Ok(effect.clone()))
    }

    /// [`Self::start_batch`], with each child's effect chosen from its node
    /// by `effect_for`, for an effect that names the device it acts on. A
    /// node whose effect cannot be built, `Err` with the reason, gets a child
    /// that fails with it.
    pub(crate) fn start_batch_with<'a>(
        &self,
        refs: impl IntoIterator<Item = &'a NodeRef<'a>>,
        effect_for: impl Fn(&NodeRef<'a>) -> Result<Option<Effect>, String>,
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
                        effect: effect_for(node),
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
    #[cfg(test)]
    fn issued(&self, job_id: &JobId) -> bool {
        job_id.parts().is_some_and(|(run, n)| {
            run == self.run && (1..crate::lock(&self.ledger).next_id).contains(&n)
        })
    }

    /// Advance a job and apply each completion effect under the ledger lock,
    /// before any caller can observe completion. Failed effects fail the job.
    pub(crate) fn observe_with(
        &self,
        job_id: &JobId,
        apply: impl Fn(&str, &str, &Effect) -> Result<(), String>,
    ) -> Result<JobStatus, tonic::Status> {
        self.observe_locked(&mut crate::lock(&self.ledger), job_id, &apply)
    }

    fn observe_locked(
        &self,
        ledger: &mut Ledger,
        job_id: &JobId,
        apply: &impl Fn(&str, &str, &Effect) -> Result<(), String>,
    ) -> Result<JobStatus, tonic::Status> {
        let (rack_id, child_refs) = match ledger.jobs.get(job_id) {
            None => {
                return Err(tonic::Status::not_found(crate::envelope::job_not_found(
                    job_id,
                )));
            }
            Some(Job::Terminal { status, .. }) => return Ok(status.clone()),
            Some(Job::Node(_)) => return Ok(self.observe_node(ledger, job_id, apply)),
            Some(Job::Parent(parent)) => (parent.rack_id.clone(), parent.children.clone()),
        };
        let mut children = Vec::with_capacity(child_refs.len());
        for (node_id, child_id) in &child_refs {
            match self.observe_locked(ledger, child_id, apply) {
                Ok(child) => children.push(child),
                Err(error) => {
                    children.push(JobStatus {
                        job_id: child_id.clone(),
                        state: JobState::Failed,
                        node_id: Some(node_id.clone()),
                        rack_id: rack_id.clone(),
                        parent_job_id: Some(job_id.clone()),
                        error_message: Some(format!(
                            "record evicted for child {child_id}: {}",
                            error.message()
                        )),
                        children: Vec::new(),
                        effect: None,
                    });
                }
            }
        }
        let state = JobState::of_children(&children);
        let error_message = if state == JobState::Failed {
            let results: Vec<NodeResult<'_>> = child_refs
                .iter()
                .zip(&children)
                .map(|((node_id, _), child)| {
                    (
                        node_id.as_str(),
                        child.error_message.clone().map_or(Ok(()), Err),
                    )
                })
                .collect();
            Some(BatchOutcome::of(&results).message)
        } else {
            None
        };
        let status = JobStatus {
            job_id: job_id.clone(),
            state,
            node_id: None,
            rack_id,
            parent_job_id: None,
            error_message,
            children,
            effect: None,
        };
        if status
            .children
            .iter()
            .all(|child| child.state.is_terminal())
        {
            self.retain_terminal(ledger, &status);
        }
        Ok(status)
    }

    #[cfg(test)]
    fn observe(&self, job_id: &JobId) -> JobStatus {
        self.observe_with(job_id, |_, _, _| Ok(())).unwrap()
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
            } => {
                let (effect, unbuildable) = match effect {
                    Ok(effect) => (effect, None),
                    Err(reason) => (None, Some(reason)),
                };
                NodeJob {
                    node_id: Some(node.node_id.to_owned()),
                    rack_id: node.rack_id.to_owned(),
                    failure: if node.matched() {
                        unbuildable.or_else(|| self.fault(node.node_id))
                    } else {
                        Some(UNMATCHED_NODE.to_owned())
                    },
                    observations: 0,
                    parent: Some(parent.clone()),
                    effect,
                }
            }
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

    /// Advance a node once. Only a successful effect may become Completed.
    fn observe_node(
        &self,
        ledger: &mut Ledger,
        job_id: &JobId,
        apply: &impl Fn(&str, &str, &Effect) -> Result<(), String>,
    ) -> JobStatus {
        let Some(Job::Node(job)) = ledger.jobs.get_mut(job_id) else {
            unreachable!("observe_locked selected a node under the same ledger lock");
        };
        job.observations += 1;
        let mut state = JobState::Running;
        let mut effect = None;
        if job.observations >= TERMINAL_AFTER_OBSERVATIONS {
            if job.failure.is_none() {
                effect = job.effect.take();
                if let Some(effect) = &effect {
                    // A panicking callback must leave a failed record after the
                    // ledger's poisoned mutex is recovered, never false completion.
                    job.failure = Some("completion effect did not finish".to_string());
                    job.failure = apply(
                        &job.rack_id,
                        job.node_id.as_deref().expect("effects name a node"),
                        effect,
                    )
                    .err();
                }
            }
            state = if job.failure.is_some() {
                JobState::Failed
            } else {
                JobState::Completed
            };
        }
        let status = JobStatus {
            job_id: job_id.clone(),
            state,
            node_id: job.node_id.clone(),
            rack_id: Some(job.rack_id.clone()),
            parent_job_id: job.parent.clone(),
            error_message: if state == JobState::Failed {
                job.failure.clone()
            } else {
                None
            },
            children: Vec::new(),
            effect: if state == JobState::Completed {
                effect
            } else {
                None
            },
        };
        if state.is_terminal() {
            self.retain_terminal(ledger, &status);
        }
        status
    }

    fn retain_terminal(&self, ledger: &mut Ledger, status: &JobStatus) {
        let mut retained = status.clone();
        retained.effect = None;
        for child in &mut retained.children {
            child.effect = None;
        }
        ledger.jobs.insert(
            status.job_id.clone(),
            Job::Terminal {
                status: retained,
                completed_at: Instant::now(),
            },
        );
        ledger.terminal.push_back(status.job_id.clone());
        let hard_limit = self.max_terminal.saturating_mul(4);
        while ledger.terminal.len() > self.max_terminal {
            let Some(oldest) = ledger.terminal.front() else {
                break;
            };
            let Some(Job::Terminal { completed_at, .. }) = ledger.jobs.get(oldest) else {
                unreachable!("the terminal queue contains only retained terminal jobs");
            };
            if completed_at.elapsed() < self.min_retention {
                if ledger.terminal.len() <= hard_limit {
                    break;
                }
                if !ledger.retention_limit_warned {
                    tracing::warn!(
                        max_terminal_jobs = self.max_terminal,
                        hard_limit,
                        "Terminal job hard ceiling reached; evicting records before their retention age"
                    );
                    ledger.retention_limit_warned = true;
                }
            }
            let oldest = ledger.terminal.pop_front().unwrap();
            ledger.jobs.remove(&oldest);
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

    use super::{Effect, JobId, JobState, JobStatus, JobStore};
    use crate::config::{FaultConfig, RmsMockConfig};
    use crate::inventory::SimNode;
    use crate::resolve::NodeRef;

    /// The run every test store is built with, so ids can be named exactly.
    const RUN: u64 = 7;

    fn store(faults: FaultConfig) -> JobStore {
        JobStore::with_run(
            &RmsMockConfig {
                faults,
                ..RmsMockConfig::default()
            },
            RUN,
        )
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

    fn mac() -> mac_address::MacAddress {
        mac_address::MacAddress::new([0; 6])
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
        let later = JobStore::with_run(&RmsMockConfig::default(), RUN + 1);
        let fresh = later.start("n9", "rack-a");
        assert_eq!(fresh, id("rms-mock-8-1"));
        assert!(!later.issued(&id("rms-mock-7-1")));
        assert!(later.issued(&fresh));
        assert_eq!(
            later
                .observe_with(&id("rms-mock-7-1"), |_, _, _| Ok(()))
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
        assert_eq!(later.observe(&fresh).node_id.as_deref(), Some("n9"));

        let clock = JobStore::new(&RmsMockConfig::default()).start("n1", "rack-a");
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
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));

        let running = jobs.observe(&batch.parent);
        assert_eq!(
            running
                .children
                .iter()
                .map(|c| c.effect.clone())
                .collect::<Vec<_>>(),
            [None, None]
        );

        // Once: the completed child is retained with its effect consumed;
        // the failed child never has one.
        let terminal = jobs.observe(&batch.parent);
        assert_eq!(
            terminal
                .children
                .iter()
                .map(|c| (c.state, c.effect.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    JobState::Completed,
                    Some(Effect::ResetSwitch { bmc_mac: mac() })
                ),
                (JobState::Failed, None)
            ]
        );
        assert_eq!(terminal.effect, None, "a parent has no effect of its own");
        assert_eq!(jobs.observe(&batch.children[0].1).effect, None);
    }

    /// A child whose effect cannot be built fails with the reason, ahead of
    /// the fault table; the other children are unaffected.
    #[test]
    fn a_child_whose_effect_cannot_be_built_fails_with_the_reason() {
        let device = SimNode::default();
        let jobs = store(fail_nodes(&["n2"]));
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch_with(&nodes, |node| {
            if node.node_id == "n2" {
                Err("n2 has no address to act on".to_owned())
            } else {
                Ok(Some(Effect::ResetSwitch { bmc_mac: mac() }))
            }
        });

        jobs.observe(&batch.parent);
        let terminal = jobs.observe(&batch.parent);
        assert_eq!(
            terminal
                .children
                .iter()
                .map(|c| (c.state, c.effect.clone(), c.error_message.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    JobState::Completed,
                    Some(Effect::ResetSwitch { bmc_mac: mac() }),
                    None
                ),
                (
                    JobState::Failed,
                    None,
                    Some("n2 has no address to act on".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn terminal_records_keep_the_job_tree_and_missing_ids_are_unknown() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device), node("rack-a", "n2", &device)];
        let batch = jobs.start_batch(&nodes, None);
        for (_, child) in &batch.children {
            jobs.observe(child);
            assert_eq!(jobs.observe(child).state, JobState::Completed);
        }
        let parent = jobs.observe(&batch.parent);
        assert_eq!(parent.state, JobState::Completed);
        assert_eq!(jobs.observe(&batch.parent), parent);
        assert_eq!(jobs.tracked(), 3);
        assert_eq!(
            jobs.observe_with(&id("rms-mock-0-99"), |_, _, _| Ok(()))
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
    }

    #[test]
    fn an_effect_failure_is_retained_and_never_retried_by_polling() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device)];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));
        jobs.observe(&batch.parent);
        let failed = jobs
            .observe_with(&batch.parent, |_, _, _| {
                Err("device unavailable".to_string())
            })
            .unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert!(
            failed
                .error_message
                .as_deref()
                .unwrap()
                .contains("device unavailable")
        );
        assert_eq!(
            jobs.observe_with(&batch.parent, |_, _, _| panic!(
                "terminal effects must not replay"
            ))
            .unwrap(),
            failed
        );
    }

    #[test]
    fn a_panicking_effect_fails_without_replaying() {
        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device)];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));
        jobs.observe(&batch.parent);
        let panic = std::panic::catch_unwind(|| {
            jobs.observe_with(&batch.parent, |_, _, _| panic!("device callback panicked"))
        });
        assert!(panic.is_err());
        let failed = jobs
            .observe_with(&batch.parent, |_, _, _| panic!("effect replayed"))
            .unwrap();
        assert_eq!(failed.state, JobState::Failed);
        assert!(
            failed
                .error_message
                .as_deref()
                .unwrap()
                .contains("completion effect did not finish")
        );
        assert_eq!(jobs.observe(&batch.parent), failed);
    }

    #[test]
    fn young_reset_records_survive_unrelated_jobs_beyond_the_count_target() {
        let config = RmsMockConfig {
            max_terminal_jobs: 3,
            ..RmsMockConfig::default()
        };
        let jobs = JobStore::with_run(&config, RUN);
        let device = SimNode::default();
        let nodes = [node("rack-a", "switch", &device)];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));
        jobs.observe(&batch.parent);
        assert_eq!(jobs.observe(&batch.parent).state, JobState::Completed);
        for _ in 0..config.max_terminal_jobs + 5 {
            let unrelated = jobs.start("other", "rack-a");
            jobs.observe(&unrelated);
            jobs.observe(&unrelated);
        }
        assert_eq!(jobs.observe(&batch.parent).state, JobState::Completed);
        assert_eq!(
            jobs.observe(&batch.children[0].1).state,
            JobState::Completed
        );
        assert_eq!(jobs.tracked(), config.max_terminal_jobs + 7);

        // Age only the reset's records; the next completion may evict them,
        // while newer records remain protected even above the count target.
        {
            let mut ledger = crate::lock(&jobs.ledger);
            for job_id in [&batch.parent, &batch.children[0].1] {
                let Some(super::Job::Terminal { completed_at, .. }) = ledger.jobs.get_mut(job_id)
                else {
                    panic!("reset record not retained");
                };
                *completed_at -= std::time::Duration::from_secs(config.min_terminal_retention_secs);
            }
        }
        let unrelated = jobs.start("other", "rack-a");
        jobs.observe(&unrelated);
        jobs.observe(&unrelated);
        assert_eq!(
            jobs.observe_with(&batch.parent, |_, _, _| Ok(()))
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
        assert_eq!(jobs.tracked(), config.max_terminal_jobs + 6);
    }

    #[test]
    fn concurrent_parent_and_child_polls_apply_the_effect_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let device = SimNode::default();
        let jobs = store(FaultConfig::default());
        let nodes = [node("rack-a", "n1", &device)];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));
        let calls = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for id in [&batch.parent, &batch.children[0].1] {
                let (jobs, calls, barrier) = (&jobs, &calls, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..3 {
                        jobs.observe_with(id, |_, _, _| {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(jobs.observe(&batch.parent).state, JobState::Completed);
    }

    #[test]
    fn an_evicted_child_cannot_make_its_parent_complete() {
        let device = SimNode::default();
        let jobs = JobStore::with_run(
            &RmsMockConfig {
                max_terminal_jobs: 1,
                min_terminal_retention_secs: 0,
                ..RmsMockConfig::default()
            },
            RUN,
        );
        let nodes = [node("rack-a", "n1", &device)];
        let batch = jobs.start_batch(&nodes, None);
        jobs.observe(&batch.children[0].1);
        jobs.observe(&batch.children[0].1);
        let newer = jobs.start("n2", "rack-a");
        jobs.observe(&newer);
        jobs.observe(&newer);
        let failed = jobs.observe(&batch.parent);
        assert_eq!(failed.state, JobState::Failed);
        assert!(
            failed
                .error_message
                .as_deref()
                .unwrap()
                .contains(&batch.children[0].1.to_string())
        );
        assert_eq!(jobs.observe(&batch.parent), failed);
        let newer = jobs.start("n3", "rack-a");
        jobs.observe(&newer);
        jobs.observe(&newer);
        assert!(
            jobs.observe_with(&batch.parent, |_, _, _| Ok(())).is_err(),
            "failed parents participate in terminal eviction"
        );
    }

    #[test]
    fn an_evicted_middle_child_does_not_freeze_the_other_children() {
        let config = RmsMockConfig {
            max_terminal_jobs: 1,
            ..RmsMockConfig::default()
        };
        let jobs = JobStore::with_run(&config, RUN);
        let device = SimNode::default();
        let nodes = [
            node("rack-a", "n1", &device),
            node("rack-a", "n2", &device),
            node("rack-a", "n3", &device),
        ];
        let batch = jobs.start_batch(&nodes, Some(Effect::ResetSwitch { bmc_mac: mac() }));
        let applied = std::sync::Mutex::new(std::collections::HashMap::new());
        let apply = |_: &str, node: &str, _: &Effect| {
            *applied.lock().unwrap().entry(node.to_owned()).or_insert(0) += 1;
            Ok(())
        };
        let middle = &batch.children[1].1;
        jobs.observe_with(middle, apply).unwrap();
        jobs.observe_with(middle, apply).unwrap();
        {
            let mut ledger = crate::lock(&jobs.ledger);
            let Some(super::Job::Terminal { completed_at, .. }) = ledger.jobs.get_mut(middle)
            else {
                panic!("middle child did not complete");
            };
            *completed_at -= std::time::Duration::from_secs(config.min_terminal_retention_secs);
        }
        let newer = jobs.start("other", "rack-a");
        jobs.observe(&newer);
        jobs.observe(&newer);
        assert!(!crate::lock(&jobs.ledger).jobs.contains_key(middle));

        let first = jobs.observe_with(&batch.parent, apply).unwrap();
        assert_eq!(
            states(&first.children),
            [JobState::Running, JobState::Failed, JobState::Running]
        );
        assert!(matches!(
            crate::lock(&jobs.ledger).jobs.get(&batch.parent),
            Some(super::Job::Parent(_))
        ));
        let terminal = jobs.observe_with(&batch.parent, apply).unwrap();
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(
            states(&terminal.children),
            [JobState::Completed, JobState::Failed, JobState::Completed]
        );
        assert!(
            terminal.children[1]
                .error_message
                .as_deref()
                .unwrap()
                .contains("record evicted")
        );
        assert_eq!(terminal.children[1].node_id.as_deref(), Some("n2"));
        assert_eq!(
            terminal.children[1].parent_job_id.as_ref(),
            Some(&batch.parent)
        );
        let repeated = jobs.observe_with(&batch.parent, apply).unwrap();
        assert_eq!(repeated.state, terminal.state);
        assert_eq!(states(&repeated.children), states(&terminal.children));
        assert_eq!(repeated.children[1], terminal.children[1]);
        assert!(matches!(
            crate::lock(&jobs.ledger).jobs.get(&batch.parent),
            Some(super::Job::Terminal { .. })
        ));
        assert_eq!(
            *applied.lock().unwrap(),
            std::collections::HashMap::from([
                ("n1".to_owned(), 1),
                ("n2".to_owned(), 1),
                ("n3".to_owned(), 1),
            ])
        );
    }

    #[test]
    fn the_hard_ceiling_evicts_young_records_in_completion_order() {
        let config = RmsMockConfig {
            max_terminal_jobs: 2,
            ..RmsMockConfig::default()
        };
        let jobs = JobStore::with_run(&config, RUN);
        let mut ids = Vec::new();
        for _ in 0..config.max_terminal_jobs * 4 {
            let id = jobs.start("n1", "rack-a");
            jobs.observe(&id);
            jobs.observe(&id);
            ids.push(id);
        }
        assert_eq!(jobs.tracked(), config.max_terminal_jobs * 4);
        assert!(!crate::lock(&jobs.ledger).retention_limit_warned);
        for _ in 0..3 {
            let id = jobs.start("n1", "rack-a");
            jobs.observe(&id);
            jobs.observe(&id);
            ids.push(id);
        }
        assert_eq!(jobs.tracked(), config.max_terminal_jobs * 4);
        assert!(crate::lock(&jobs.ledger).retention_limit_warned);
        for id in &ids[..3] {
            assert_eq!(
                jobs.observe_with(id, |_, _, _| Ok(())).unwrap_err().code(),
                tonic::Code::NotFound
            );
        }
        for id in &ids[3..] {
            assert_eq!(jobs.observe(id).state, JobState::Completed);
        }
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
        let jobs = JobStore::with_run(
            &RmsMockConfig {
                faults: fail_nodes(&["n1"]),
                max_terminal_jobs: 2,
                min_terminal_retention_secs: 0,
                ..RmsMockConfig::default()
            },
            RUN,
        );
        let fail = || {
            let id = jobs.start("n1", "rack-a");
            jobs.observe(&id);
            assert_eq!(jobs.observe(&id).state, JobState::Failed);
            id
        };
        let oldest = fail();
        let middle = fail();
        assert_eq!(jobs.tracked(), 2, "at the cap nothing is forgotten");

        // One more failure evicts the oldest, which becomes unknown;
        // the rest keep reading failed,
        // and polling them again does not count as a new failure.
        let newest = fail();
        assert_eq!(jobs.tracked(), 2);
        assert_eq!(
            jobs.observe_with(&oldest, |_, _, _| Ok(()))
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
        assert_eq!(jobs.observe(&middle).state, JobState::Failed);
        assert_eq!(jobs.observe(&newest).state, JobState::Failed);
        assert_eq!(jobs.tracked(), 2);
    }
}
